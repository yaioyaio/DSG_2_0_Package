#!/usr/bin/env python3
"""Build a deterministic DSG archive and manage an explicitly selected prefix."""
import argparse
from contextlib import ExitStack, contextmanager
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import runpy
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes
    import msvcrt
else:
    import fcntl

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("dsg", "dsgd")
WINDOWS_BINARIES = ("dsg.exe", "dsgd.exe")
WINDOWS_MACHINES = {"aarch64-pc-windows-msvc": 0xAA64, "x86_64-pc-windows-msvc": 0x8664}
EXECUTABLES = set(BINARIES + WINDOWS_BINARIES)
REQUIRED_FILES = {"Cargo.lock", "SOURCES.md"}
LICENSE_NAMES = {"LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE"}
MANIFEST = "MANIFEST.json"
CURRENT = "dsg-current"
LIMIT = 256 * 1024 * 1024
TARGETS = {f'{arch}-{system}' for arch in ('aarch64', 'x86_64')
           for system in ('apple-darwin', 'unknown-linux-gnu', 'unknown-linux-musl')}
TARGETS.update(WINDOWS_MACHINES)
RELEASE_NAME = re.compile(r"\.dsg-release-([0-9a-f]{64})-([a-z0-9_]{8})")


if sys.platform == "win32":
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)

    def windows_api(name, result, *types):
        function = getattr(kernel, name)
        function.restype = result
        function.argtypes = types
        return function

    class WindowsFileInfo(ctypes.Structure):
        _fields_ = [("attributes", wintypes.DWORD), ("created", wintypes.FILETIME),
                    ("accessed", wintypes.FILETIME), ("modified", wintypes.FILETIME),
                    ("volume", wintypes.DWORD), ("size_high", wintypes.DWORD),
                    ("size_low", wintypes.DWORD), ("links", wintypes.DWORD),
                    ("index_high", wintypes.DWORD), ("index_low", wintypes.DWORD)]

        def identity(self):
            return self.volume, self.index_high, self.index_low

    class WindowsStreamInfo(ctypes.Structure):
        _fields_ = [("size", ctypes.c_int64), ("name", wintypes.WCHAR * 296)]

    create_file = windows_api("CreateFileW", wintypes.HANDLE, wintypes.LPCWSTR,
                              wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                              wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE)
    file_information = windows_api("GetFileInformationByHandle", wintypes.BOOL,
                                   wintypes.HANDLE, ctypes.POINTER(WindowsFileInfo))
    drive_type = windows_api("GetDriveTypeW", wintypes.UINT, wintypes.LPCWSTR)
    first_stream = windows_api("FindFirstStreamW", wintypes.HANDLE, wintypes.LPCWSTR,
                               ctypes.c_int, ctypes.POINTER(WindowsStreamInfo), wintypes.DWORD)
    close_search = windows_api("FindClose", wintypes.BOOL, wintypes.HANDLE)
    stream_information = windows_api("GetFileInformationByHandleEx", wintypes.BOOL,
                                     wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID,
                                     wintypes.DWORD)
    set_information = windows_api("SetFileInformationByHandle", wintypes.BOOL,
                                  wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID,
                                  wintypes.DWORD)
    close_handle = windows_api("CloseHandle", wintypes.BOOL, wintypes.HANDLE)
    create_mutex = windows_api("CreateMutexW", wintypes.HANDLE, wintypes.LPVOID,
                               wintypes.BOOL, wintypes.LPCWSTR)
    wait_handle = windows_api("WaitForSingleObject", wintypes.DWORD, wintypes.HANDLE,
                              wintypes.DWORD)
    release_mutex = windows_api("ReleaseMutex", wintypes.BOOL, wintypes.HANDLE)
    current_process = windows_api("GetCurrentProcess", wintypes.HANDLE)
    native_machine = windows_api("IsWow64Process2", wintypes.BOOL, wintypes.HANDLE,
                                 ctypes.POINTER(wintypes.USHORT),
                                 ctypes.POINTER(wintypes.USHORT))


FILE_READ_ATTRIBUTES = 0x80
GENERIC_READ = 0x80000000
DELETE_ACCESS = 0x10000
FILE_SHARE_READ = 1
FILE_SHARE_WRITE = 2
OPEN_EXISTING = 3
FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000
FILE_FLAG_BACKUP_SEMANTICS = 0x02000000
FILE_STREAM_INFO = 7
FILE_DISPOSITION_INFO = 4


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def label(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.+-]{0,95}", value)


def mode(name):
    return 0o755 if name in EXECUTABLES else 0o644


def binary_names(target):
    return WINDOWS_BINARIES if target in WINDOWS_MACHINES else BINARIES


def verify_binary(data, target):
    require(data, "empty binary")
    if target in WINDOWS_MACHINES:
        require(len(data) >= 64 and data[:2] == b"MZ", "invalid Windows executable")
        start = struct.unpack_from("<I", data, 60)[0]
        require(64 <= start <= len(data) - 26 and data[start:start + 4] == b"PE\0\0",
                "invalid Windows PE header")
        machine, _, _, _, _, optional_size, characteristics = struct.unpack_from("<HHIIIHH", data, start + 4)
        require(machine == WINDOWS_MACHINES[target], "Windows executable architecture mismatch")
        require(optional_size >= 112 and start + 24 + optional_size <= len(data)
                and struct.unpack_from("<H", data, start + 24)[0] == 0x20B
                and characteristics & 0x0002 and not characteristics & 0x2000, "invalid Windows executable")


def open_windows(path, access, share):
    handle = create_file(str(path), access, share, None, OPEN_EXISTING,
                         FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    return handle


def windows_info(handle):
    info = WindowsFileInfo()
    if not file_information(handle, ctypes.byref(info)):
        raise ctypes.WinError(ctypes.get_last_error())
    require(not info.attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT, "unexpected Windows reparse point")
    return info


@contextmanager
def windows_directories(path):
    require(re.fullmatch(r"[A-Za-z]:\\", path.anchor) and all(
        part not in (".", "..") and ":" not in part and not part.endswith((".", " "))
        for part in path.parts[1:]), "expected a local Windows path without aliases")
    require(drive_type(path.anchor) == 3, "expected a fixed Windows drive")
    with ExitStack() as stack:
        for directory in [*reversed(path.parents), path]:
            handle = open_windows(directory, FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE)
            stack.callback(close_handle, handle)
            info = windows_info(handle)
            require(info.attributes & stat.FILE_ATTRIBUTE_DIRECTORY, "expected an existing directory")
        yield info


def verify_windows_streams(handle):
    # An alternate stream may hold user data even when the primary content is unchanged.
    streams = ctypes.create_string_buffer(65536)
    if not stream_information(handle, FILE_STREAM_INFO, streams, len(streams)):
        raise ctypes.WinError(ctypes.get_last_error())
    next_entry, name_size = struct.unpack_from("<II", streams)
    require(next_entry == 0 and name_size == len("::$DATA".encode("utf-16-le"))
            and streams[24:24 + name_size] == "::$DATA".encode("utf-16-le"), "unexpected Windows file stream")


def verify_windows_directory_streams(path):
    stream = WindowsStreamInfo()
    search = first_stream(str(path), 0, ctypes.byref(stream), 0)
    if search == ctypes.c_void_p(-1).value:
        error = ctypes.get_last_error()
        if error == 38:  # ERROR_HANDLE_EOF: a directory with no named data streams.
            return
        raise ctypes.WinError(error)
    close_search(search)
    raise ValueError("unexpected Windows directory stream")


@contextmanager
def windows_regular(path, limit=LIMIT, single=True, managed=False, delete=False):
    with windows_directories(path.parent):
        access = GENERIC_READ | (DELETE_ACCESS if delete else 0)
        handle = open_windows(path, access, FILE_SHARE_READ)
        try:
            info = windows_info(handle)
            require(not info.attributes & (stat.FILE_ATTRIBUTE_DIRECTORY | stat.FILE_ATTRIBUTE_DEVICE),
                    "unexpected file type")
            size = (info.size_high << 32) | info.size_low
            require(size <= limit, "unexpected file size")
            require(not single or info.links == 1, "unexpected hard link")
            if managed:
                verify_windows_streams(handle)
            fd = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
        except BaseException:
            close_handle(handle)
            raise
        with os.fdopen(fd, "rb") as stream:
            data = stream.read(limit + 1)
            require(len(data) == size, "file changed during read")
            yield data, handle


def checked_dir(path):
    path = Path(path).absolute()
    if sys.platform == "win32":
        with windows_directories(path):
            return path
    require(path.resolve(strict=True) == path and path.is_dir(), "expected an existing directory without symlinks")
    return path


def regular(path, limit=LIMIT, single=True, managed=False):
    if sys.platform == "win32":
        with windows_regular(path, limit, single, managed) as (data, _):
            return data
    with os.fdopen(os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW), "rb") as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_size <= limit, "unexpected file type or size")
        require(not single or info.st_nlink == 1, "unexpected hard link")
        data = stream.read(limit + 1)
        require(len(data) == info.st_size, "file changed during read")
        return data


def sign(path):
    require(sys.platform == "darwin", "ad-hoc signing requires macOS")
    subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none",
                    "--identifier", "local.dsg." + path.name, str(path)], check=True)
    verify_signature(path)


def verify_signature(path):
    require(sys.platform == "darwin", "macOS signature verification requires macOS")
    subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(path)], check=True)


def publish(source, dest):
    try:
        os.link(source, dest)
    except FileExistsError:
        require(regular(dest) == regular(source), "different output already exists")


def build(args):
    require(label(args.version) and args.target in TARGETS, "invalid target or version")
    require(not args.adhoc or args.target.endswith("-apple-darwin"),
            "ad-hoc signing requires a macOS target")
    bindir = checked_dir(args.bin_dir)
    output = Path(args.output).absolute()
    checked_dir(output.parent)
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.+-]*\.tar\.gz", output.name), "expected a .tar.gz filename")
    with tempfile.TemporaryDirectory(dir=output.parent) as temp:
        tmp = Path(temp)
        files = {"Cargo.lock": regular(ROOT / "rust-scaffold/Cargo.lock"),
                 "SOURCES.md": regular(ROOT / "docs/SOURCES.md")}
        for name in sorted(LICENSE_NAMES):
            if (ROOT / name).exists():
                files[name] = regular(ROOT / name)
        for name in binary_names(args.target):
            files[name] = regular(bindir / name, single=False)
            verify_binary(files[name], args.target)
            if args.adhoc:
                binary = tmp / name
                binary.write_bytes(files[name])
                binary.chmod(0o755)
                sign(binary)
                files[name] = regular(binary)
        metadata = {"format": 1, "version": args.version, "target": args.target,
                    "signature": "adhoc" if args.adhoc else "none",
                    "files": {n: digest(data) for n, data in files.items()}}
        files[MANIFEST] = manifest_bytes(metadata)
        require(sum(map(len, files.values())) < LIMIT, "package too large")
        archive = tmp / "archive"
        with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                for name, data in sorted(files.items()):
                    entry = tarfile.TarInfo(name)
                    entry.mode, entry.size = mode(name), len(data)
                    tar.addfile(entry, io.BytesIO(data))
        checksum = digest(regular(archive))
        sidecar = tmp / "checksum"
        sidecar.write_bytes(f"{checksum}  {output.name}\n".encode("ascii"))
        side = output.with_name(output.name + ".sha256")
        if os.path.lexists(side):
            require(regular(side, 1024) == sidecar.read_bytes(), "checksum output already exists")
        publish(archive, output)
        publish(sidecar, side)
        print(checksum)


def metadata(raw):
    doc = json.loads(raw)
    require(isinstance(doc, dict) and doc.get("format") == 1 and label(doc.get("target"))
            and doc["target"] in TARGETS
            and label(doc.get("version")) and doc.get("signature") in ("adhoc", "none")
            and (doc["signature"] != "adhoc" or doc["target"].endswith("-apple-darwin")), "invalid manifest")
    hashes = doc.get("files")
    required = REQUIRED_FILES | set(binary_names(doc["target"]))
    require(isinstance(hashes, dict) and required <= hashes.keys() <= required | LICENSE_NAMES
            and all(isinstance(h, str) and re.fullmatch("[0-9a-f]{64}", h) for h in hashes.values()),
            "invalid file inventory")
    return doc


def unpack(args, folder):
    archive = Path(args.archive).absolute()
    checked_dir(archive.parent)
    require(archive.name.endswith(".tar.gz"), "expected .tar.gz")
    raw = regular(archive)
    require(re.fullmatch("[0-9a-fA-F]{64}", args.sha256) and digest(raw) == args.sha256.lower(),
            "archive SHA256 mismatch")
    # Stream only the fixed USTAR layout we produce. Reject extension records before
    # a general tar reader can allocate memory based on untrusted extension lengths.
    require(len(raw) <= LIMIT, "archive too large")
    names = set()
    total = 0
    with gzip.GzipFile(fileobj=io.BytesIO(raw)) as gz:
        while True:
            block = gz.read(512)
            if block == bytes(512):
                trailer = gz.read(tarfile.RECORDSIZE + 1)
                require(512 <= len(trailer) <= tarfile.RECORDSIZE and not any(trailer), "invalid archive trailer")
                break
            e = tarfile.TarInfo.frombuf(block, "ascii", "strict")
            require(e.name in (EXECUTABLES | REQUIRED_FILES | LICENSE_NAMES | {MANIFEST}) and e.name not in names
                    and e.type == tarfile.REGTYPE and not e.linkname and e.mode == mode(e.name)
                    and 0 <= e.size <= LIMIT - total, "invalid archive member")
            require(e.name != MANIFEST or e.size <= 16384, "manifest too large")
            payload = gz.read(e.size)
            require(len(payload) == e.size and gz.read(-e.size % 512) == bytes(-e.size % 512), "truncated member")
            with (folder / e.name).open("xb") as entry:
                entry.write(payload)
            if sys.platform != "win32":
                (folder / e.name).chmod(e.mode)
            names.add(e.name)
            total += e.size


def inspect(folder, expected=None):
    if sys.platform == "win32":
        with windows_directories(folder):
            return inspect_files(folder, expected)
    checked_dir(folder)
    return inspect_files(folder, expected)


def inspect_files(folder, expected):
    if sys.platform == "win32":
        verify_windows_directory_streams(folder)
    else:
        info = folder.stat()
        require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700,
                "unexpected release directory ownership or mode")
        require(stat.S_IMODE((folder / MANIFEST).lstat().st_mode) == mode(MANIFEST),
                "manifest mode changed")
    raw = regular(folder / MANIFEST, 16384, managed=True)
    checksum = digest(raw)
    require(expected is None or checksum == expected, "installed manifest changed")
    doc = metadata(raw)
    if doc["target"] in WINDOWS_MACHINES:
        require(raw == manifest_bytes(doc), "unexpected Windows manifest encoding")
    require({p.name for p in folder.iterdir()} == set(doc["files"]) | {MANIFEST}, "unexpected installation contents")
    for name, hash_value in doc["files"].items():
        p = folder / name
        data = regular(p, managed=True)
        require(digest(data) == hash_value, "installed file changed")
        if sys.platform != "win32":
            require(stat.S_IMODE(p.lstat().st_mode) == mode(name), "installed file permissions changed")
        if name in EXECUTABLES:
            verify_binary(data, doc["target"])
    if doc["signature"] == "adhoc":
        for name in BINARIES:
            verify_signature(folder / name)
    return checksum, doc


def manifest_bytes(doc):
    return (json.dumps(doc, sort_keys=True) + "\n").encode()


@contextmanager
def windows_lock(prefix):
    folder = Path(prefix).absolute()
    with windows_directories(folder) as info:
        identifier = "-".join(f"{value:08x}" for value in info.identity())
        mutex = create_mutex(None, False, "Global\\DSG.Release." + identifier)
        if not mutex:
            raise ctypes.WinError(ctypes.get_last_error())
        acquired = False
        try:
            status = wait_handle(mutex, 0)
            acquired = status in (0, 0x80)  # WAIT_OBJECT_0 and WAIT_ABANDONED
            require(acquired, "Windows installation is locked by another operation")
            yield folder
        finally:
            if acquired:
                release_mutex(mutex)
            close_handle(mutex)


@contextmanager
def locked(prefix):
    folder = checked_dir(prefix)
    info = folder.stat()
    require(info.st_uid == os.geteuid() and not info.st_mode & 0o022, "prefix must be private to its owner for writing")
    fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield folder
    finally:
        os.close(fd)


def active(prefix):
    link = prefix / CURRENT
    require(link.is_symlink(), "no managed installation or existing path conflict")
    target = os.readlink(link)
    match = RELEASE_NAME.fullmatch(target)
    require(match, "unrecognized installation link")
    folder = checked_dir(prefix / target)
    checksum, doc = inspect(folder, match[1])
    return folder, checksum, doc


def remove_verified(folder, doc):
    for name in [*doc["files"], MANIFEST]:
        (folder / name).unlink()
    folder.rmdir()


def verify_host(target):
    if sys.platform == "win32":
        process_arch, native_arch = wintypes.USHORT(), wintypes.USHORT()
        if not native_machine(current_process(), ctypes.byref(process_arch), ctypes.byref(native_arch)):
            raise ctypes.WinError(ctypes.get_last_error())
        require(WINDOWS_MACHINES.get(target) == native_arch.value,
                "release target does not match this host")
        return
    architecture = platform.machine().lower()
    if architecture == "arm64":
        architecture = "aarch64"
    suffixes = {"darwin": ("apple-darwin",), "linux": ("unknown-linux-gnu", "unknown-linux-musl")}
    require(target in {f"{architecture}-{suffix}" for suffix in suffixes.get(sys.platform, ())},
            "release target does not match this host")


def manage_posix(args):
    with locked(args.prefix) as prefix:
        current = prefix / CURRENT
        previous = None
        if args.command == "install":
            require(not os.path.lexists(current), "installation path already exists")
        else:
            previous, oldsum, old = active(prefix)
            if args.command == "doctor":
                print(json.dumps({"status": "ok", "version": old["version"], "target": old["target"],
                                  "signature": old["signature"]}, sort_keys=True))
                return
            if args.command == "uninstall":
                current.unlink()
                remove_verified(previous, old)
                return
        with tempfile.TemporaryDirectory(prefix=".dsg-stage-", dir=prefix) as temp:
            stage = Path(temp)
            content = stage / "content"
            content.mkdir(mode=0o700)
            content.chmod(0o700)
            unpack(args, content)
            checksum, doc = inspect(content)
            verify_host(doc["target"])
            if previous:
                require(doc["target"] == old["target"], "upgrade target mismatch")
                if oldsum == checksum:
                    return
            # The stage's unique suffix avoids adopting existing files or failed installations.
            dest = prefix / (".dsg-release-" + checksum + "-" + stage.name[-8:])
            require(not os.path.lexists(dest), "release path collision")
            content.rename(dest)
            try:
                if previous is None:
                    current.symlink_to(dest.name)
                else:
                    require(current.is_symlink() and os.readlink(current) == previous.name,
                            "current installation changed during upgrade")
                    pending = stage / "pointer"
                    pending.symlink_to(dest.name)
                    os.replace(pending, current)
            except BaseException:
                if not current.is_symlink() or os.readlink(current) != dest.name:
                    remove_verified(dest, doc)
                raise
            if previous:
                remove_verified(previous, old)


def delete_windows_handle(handle):
    delete = ctypes.c_ubyte(1)  # FILE_DISPOSITION_INFO.DeleteFile is a Win32 BOOLEAN.
    if not set_information(handle, FILE_DISPOSITION_INFO, ctypes.byref(delete), ctypes.sizeof(delete)):
        raise ctypes.WinError(ctypes.get_last_error())


def windows_state(raw):
    state = json.loads(raw)
    require(isinstance(state, dict) and state.keys() == {"format", "active", "retired"}
            and state["format"] == 1, "unrecognized Windows installation state")
    active_name, retired = state["active"], state["retired"]
    require(active_name is None or isinstance(active_name, str) and RELEASE_NAME.fullmatch(active_name),
            "unrecognized Windows installation path")
    if retired is not None:
        require(isinstance(retired, dict) and retired.keys() == {"name", "manifest", "identity"}
                and isinstance(retired["name"], str) and retired["name"] != active_name,
                "unrecognized Windows cleanup state")
        identity = retired["identity"]
        require(isinstance(identity, list) and len(identity) == 3
                and all(type(item) is int and 0 <= item <= 0xFFFFFFFF for item in identity),
                "invalid Windows cleanup directory identifier")
        check = RELEASE_NAME.fullmatch(retired["name"])
        raw_manifest = manifest_bytes(retired["manifest"])
        require(check and digest(raw_manifest) == check[1]
                and metadata(raw_manifest)["target"] in WINDOWS_MACHINES, "invalid Windows cleanup manifest")
    require(active_name is not None or retired is not None, "empty Windows installation state")
    return state


def write_windows_state(pointer, state, previous, stage):
    if state is None:
        with windows_regular(pointer, 32768, managed=True, delete=True) as (raw, handle):
            require(raw == previous, "Windows installation state changed")
            delete_windows_handle(handle)
        return None
    data = manifest_bytes(state)
    pending = stage / "pointer"
    with pending.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    if previous is None:
        # Windows rename fails if the destination exists.
        pending.rename(pointer)
    else:
        require(regular(pointer, 32768, managed=True) == previous, "Windows installation state changed")
        os.replace(pending, pointer)
    return data


def exists_windows(path):
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    return True


def retire_windows(prefix, retired):
    folder = prefix / retired["name"]
    manifest = retired["manifest"]
    hashes = {**manifest["files"], MANIFEST: digest(manifest_bytes(manifest))}
    if not exists_windows(folder):
        return
    with windows_directories(folder) as info:
        identity = info.identity()
        require(list(identity) == retired["identity"], "retired Windows directory changed")
        verify_windows_directory_streams(folder)
        names = {path.name for path in folder.iterdir()}
        require(names <= hashes.keys(), "unexpected file in retired Windows installation")
        with ExitStack() as files:
            handles = []
            for name in sorted(names):
                limit = 16384 if name == MANIFEST else LIMIT
                raw, handle = files.enter_context(windows_regular(folder / name, limit, managed=True, delete=True))
                require(digest(raw) == hashes[name], "retired Windows installation file changed")
                handles.append(handle)
            require(names == {path.name for path in folder.iterdir()}, "retired Windows installation changed")
            for handle in handles:
                delete_windows_handle(handle)
    # Delete the inspected directory by identity. The OS also rejects a nonempty directory.
    handle = open_windows(folder, DELETE_ACCESS | FILE_READ_ATTRIBUTES, FILE_SHARE_READ)
    try:
        require(windows_info(handle).identity() == identity, "retired Windows directory changed")
        delete_windows_handle(handle)
    finally:
        close_handle(handle)


def finish_windows_cleanup(prefix, pointer, state, raw, stage):
    try:
        retire_windows(prefix, state["retired"])
        result = {"format": 1, "active": state["active"], "retired": None} if state["active"] else None
        return write_windows_state(pointer, result, raw, stage)
    except (OSError, ValueError) as err:
        raise ValueError("Windows release cleanup pending; retry after closing binaries and checking the prefix") from err


def manage_windows(args):
    with windows_lock(args.prefix) as prefix:
        pointer = prefix / CURRENT
        installed = exists_windows(pointer)
        if args.command == "install":
            require(not installed, "installation path already exists")
        else:
            require(installed, "no managed installation")
        raw = regular(pointer, 32768, managed=True) if installed else None
        state = windows_state(raw) if raw is not None else None
        active_name = state["active"] if state is not None else None
        previous = None
        if active_name is not None:
            previous = prefix / active_name
            with windows_directories(previous) as info:
                old_id = list(info.identity())
                oldsum, old = inspect_files(previous, RELEASE_NAME.fullmatch(active_name)[1])
            verify_host(old["target"])
        if args.command == "doctor":
            require(previous is not None, "Windows release removal is pending; retry uninstall")
            print(json.dumps({"status": "ok", "path": str(previous), "version": old["version"],
                              "target": old["target"], "signature": old["signature"],
                              "pending_cleanup": state["retired"] is not None}, sort_keys=True))
            return
        with tempfile.TemporaryDirectory(prefix=".dsg-stage-", dir=prefix) as temp:
            stage = Path(temp)
            if state is not None and state["retired"] is not None:
                raw = finish_windows_cleanup(prefix, pointer, state, raw, stage)
                if previous is None:
                    require(args.command == "uninstall", "Windows release removed; use install")
                    return
            retired = ({"name": active_name, "manifest": old, "identity": old_id}
                       if previous is not None else None)
            if args.command == "uninstall":
                removed = {"format": 1, "active": None, "retired": retired}
                updated = write_windows_state(pointer, removed, raw, stage)
                finish_windows_cleanup(prefix, pointer, removed, updated, stage)
                return
            content = stage / "content"
            content.mkdir()
            unpack(args, content)
            checksum, doc = inspect(content)
            verify_host(doc["target"])
            if previous is not None:
                require(doc["target"] == old["target"], "upgrade target mismatch")
                if oldsum == checksum:
                    return
                with windows_directories(previous) as info:
                    require(list(info.identity()) == old_id, "Windows installation changed during upgrade")
                    inspect_files(previous, oldsum)
            dest = prefix / (".dsg-release-" + checksum + "-" + stage.name[-8:])
            with windows_directories(content) as info:
                new_id = list(info.identity())
            content.rename(dest)
            updated_state = {"format": 1, "active": dest.name, "retired": retired}
            try:
                updated = write_windows_state(pointer, updated_state, raw, stage)
            except Exception:
                retire_windows(prefix, {"name": dest.name, "manifest": doc, "identity": new_id})
                raise
            if retired is not None:
                finish_windows_cleanup(prefix, pointer, updated_state, updated, stage)


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    commands = cli.add_subparsers(dest="command", required=True)
    builder = commands.add_parser("build")
    for arg in ["bin-dir", "target", "version", "output"]:
        builder.add_argument("--" + arg, required=True)
    builder.add_argument("--adhoc", action="store_true")
    for command in ["install", "upgrade", "doctor", "uninstall"]:
        action = commands.add_parser(command)
        action.add_argument("--prefix", required=True)
        if command in ("install", "upgrade"):
            action.add_argument("--archive", required=True)
            action.add_argument("--sha256", required=True)
            action.add_argument("--signatures", help="detached Ed25519 signatures JSON")
            action.add_argument("--public-key", help="Ed25519 public key in SPKI DER encoding")
            action.add_argument("--public-key-sha256", help="independently approved public key SHA-256")
            action.add_argument("--openssl", help="absolute path to caller-trusted OpenSSL 3")
    args = cli.parse_args()
    try:
        if args.command in ("install", "upgrade"):
            signature_options = (args.signatures, args.public_key, args.public_key_sha256, args.openssl)
            if any(option is not None for option in signature_options):
                require(all(signature_options), "provide all four detached signature verification options")
                verifier = runpy.run_path(str(Path(__file__).with_name("verify-release-signature.py")))
                verifier["verify"](args)
        manager = manage_windows if sys.platform == "win32" else manage_posix
        (build if args.command == "build" else manager)(args)
    except (OSError, ValueError, EOFError, tarfile.TarError, subprocess.SubprocessError) as err:
        cli.exit(1, f"release package: {err}\n")


if __name__ == "__main__":
    main()
