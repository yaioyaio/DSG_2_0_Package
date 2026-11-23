#!/usr/bin/env python3
"""Build a deterministic DSG archive and manage an explicitly selected POSIX prefix."""
import argparse
from contextlib import contextmanager
import fcntl
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import stat
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BINARIES = ("dsg", "dsgd")
MODES = {"dsg": 0o755, "dsgd": 0o755, "Cargo.lock": 0o644, "SOURCES.md": 0o644}
LICENSE_NAMES = {"LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE"}
MANIFEST = "MANIFEST.json"
CURRENT = "dsg-current"
LIMIT = 256 * 1024 * 1024
TARGETS = {f'{arch}-{system}' for arch in ('aarch64', 'x86_64')
           for system in ('apple-darwin', 'unknown-linux-gnu', 'unknown-linux-musl')}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def label(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.+-]{0,95}", value)


def mode(name):
    return MODES.get(name, 0o644)


def checked_dir(path):
    path = Path(path).absolute()
    require(path.resolve(strict=True) == path and path.is_dir(), "expected an existing directory without symlinks")
    return path


def regular(path, limit=LIMIT, single=True):
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
    require(not args.adhoc or "darwin" in args.target.lower() or "macos" in args.target.lower(),
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
        for name in BINARIES:
            files[name] = regular(bindir / name, single=False)
            require(files[name], "empty binary")
            if args.adhoc:
                binary = tmp / name
                binary.write_bytes(files[name])
                binary.chmod(0o755)
                sign(binary)
                files[name] = regular(binary)
        metadata = {"format": 1, "version": args.version, "target": args.target,
                    "signature": "adhoc" if args.adhoc else "none",
                    "files": {n: digest(data) for n, data in files.items()}}
        files[MANIFEST] = (json.dumps(metadata, sort_keys=True) + "\n").encode()
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
        sidecar.write_text(f"{checksum}  {output.name}\n", encoding="ascii")
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
            and label(doc.get("version")) and doc.get("signature") in ("adhoc", "none"), "invalid manifest")
    hashes = doc.get("files")
    require(isinstance(hashes, dict) and MODES.keys() <= hashes.keys() <= MODES.keys() | LICENSE_NAMES
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
            require(e.name in (MODES.keys() | LICENSE_NAMES | {MANIFEST}) and e.name not in names
                    and e.type == tarfile.REGTYPE and not e.linkname and e.mode == mode(e.name)
                    and 0 <= e.size <= LIMIT - total, "invalid archive member")
            require(e.name != MANIFEST or e.size <= 16384, "manifest too large")
            payload = gz.read(e.size)
            require(len(payload) == e.size and gz.read(-e.size % 512) == bytes(-e.size % 512), "truncated member")
            (folder / e.name).write_bytes(payload)
            (folder / e.name).chmod(e.mode)
            names.add(e.name)
            total += e.size


def inspect(folder, expected=None):
    info = folder.stat()
    require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700,
            "unexpected release directory ownership or mode")
    require(stat.S_IMODE((folder / MANIFEST).lstat().st_mode) == mode(MANIFEST), "manifest mode changed")
    raw = regular(folder / MANIFEST, 16384)
    checksum = digest(raw)
    require(expected is None or checksum == expected, "installed manifest changed")
    doc = metadata(raw)
    require({p.name for p in folder.iterdir()} == set(doc["files"]) | {MANIFEST}, "unexpected installation contents")
    for name, hash_value in doc["files"].items():
        p = folder / name
        require(digest(regular(p)) == hash_value and stat.S_IMODE(p.lstat().st_mode) == mode(name),
                "installed file changed")
    if doc["signature"] == "adhoc":
        for name in BINARIES:
            verify_signature(folder / name)
    return checksum, doc


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
    match = re.fullmatch(r"\.dsg-release-([0-9a-f]{64})-([a-z0-9_]{8})", target)
    require(match, "unrecognized installation link")
    folder = checked_dir(prefix / target)
    checksum, doc = inspect(folder, match[1])
    return folder, checksum, doc


def remove_verified(folder, doc):
    for name in [*doc["files"], MANIFEST]:
        (folder / name).unlink()
    folder.rmdir()


def verify_host(target):
    architecture = platform.machine().lower()
    if architecture == "arm64":
        architecture = "aarch64"
    suffixes = {"darwin": ("apple-darwin",), "linux": ("unknown-linux-gnu", "unknown-linux-musl")}
    require(target in {f"{architecture}-{suffix}" for suffix in suffixes.get(sys.platform, ())},
            "release target does not match this host")


def manage(args):
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
    args = cli.parse_args()
    try:
        (build if args.command == "build" else manage)(args)
    except (OSError, ValueError, EOFError, tarfile.TarError, subprocess.SubprocessError) as err:
        cli.exit(1, f"release package: {err}\n")


if __name__ == "__main__":
    main()
