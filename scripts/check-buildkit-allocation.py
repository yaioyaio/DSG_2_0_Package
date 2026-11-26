#!/usr/bin/env python3
"""Compare allocated bytes on one disposable Ubuntu ext4 fixture."""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import sys
import time


MARKER = ".dsg-allocation-fixture"
MAX_ENTRIES = 4096
MAX_DEPTH = 16
MAX_RELATIVE_PATH = 1024
MAX_DOCUMENT = 8 << 20
MAX_FILESYSTEM = 128 << 20
CAPTURE_SECONDS = 5
COMPONENT = re.compile(r"[A-Za-z0-9_.+-]{1,255}\Z")
DIR_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
IDENTITY_NUMBERS = ("mount_namespace", "mount_id", "unique_mount_id", "device",
                    "root_inode", "marker_inode", "fsid", "block_bytes", "total_blocks")
ENTRY_KIND = {stat.S_IFDIR: "directory", stat.S_IFREG: "file", stat.S_IFLNK: "symlink",
              stat.S_IFSOCK: "socket", stat.S_IFIFO: "fifo", stat.S_IFBLK: "block device",
              stat.S_IFCHR: "character device"}
DEFAULT_KINDS = {"directory", "file"}
VFS_KINDS = DEFAULT_KINDS | {"symlink", "socket", "block device"}
VFS_QUOTA_NODES = {"docker-data/vfs/backingFsBlockDev", "docker-data/volumes/backingFsBlockDev"}


class MeasurementError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise MeasurementError(reason)


def safe_component(name):
    lower = name.lower()
    secret_suffixes = (".env", ".key", ".pem", ".p12", ".pfx", ".crt", ".der")
    return (COMPONENT.fullmatch(name) and name not in (".", "..") and
            ".env." not in lower and not lower.endswith(secret_suffixes) and
            not ("deploy." in lower and lower.endswith("env")))


def unique_mount_id(fd):
    # Linux statx ABI, shared with the custody checker. This mask requires Linux 6.8+.
    call = ctypes.CDLL(None, use_errno=True).statx
    call.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_uint, ctypes.c_void_p)
    call.restype = ctypes.c_int
    buffer = (ctypes.c_uint64 * 32)()
    mask = 0x4000
    require(call(fd, b"", 0x1000, mask, ctypes.byref(buffer)) == 0 and
            struct.unpack_from("=I", buffer)[0] & mask, "unique mount ID unavailable")
    return struct.unpack_from("=Q", buffer, 144)[0]


def mount_record(path, device):
    with open("/proc/self/mountinfo", "rb") as source:
        data = source.read(MAX_DOCUMENT + 1)
    require(len(data) <= MAX_DOCUMENT, "mount table limit")
    device_name = f"{os.major(device)}:{os.minor(device)}".encode("ascii")
    records = []
    for line in data.splitlines():
        left, separator, right = line.partition(b" - ")
        fields, mount = left.split(), right.split()
        require(separator and len(fields) >= 6 and len(mount) == 3, "invalid mount table")
        records.append((fields, mount))
    matches = [(fields, mount) for fields, mount in records
               if fields[2] == device_name or fields[4] == os.fsencode(path)]
    require(len(matches) == 1, "mount point or device is ambiguous")
    fields, mount = matches[0]
    require(fields[2] == device_name and fields[3] == b"/" and
            fields[4] == os.fsencode(path) and len(fields) == 6 and
            {b"nodev", b"nosuid"} <= set(fields[5].split(b",")) and
            mount[0] == b"ext4" and re.fullmatch(rb"/dev/loop[0-9]+", mount[1]) and
            all(other[1] != fields[0] for other, _ in records), "not a private fixture ext4 mount")
    return int(fields[0])


def mount_sample(fd, root):
    info = os.fstat(fd)
    live = root.lstat()
    require((live.st_dev, live.st_ino) == (info.st_dev, info.st_ino) and
            info.st_uid == 0 and info.st_mode & 0o022 == 0, "mount point changed or has unsafe ownership")
    fs = os.fstatvfs(fd)
    require(0 < fs.f_frsize <= 65536 and 0 <= fs.f_bfree <= fs.f_blocks and
            0 < fs.f_blocks * fs.f_frsize <= MAX_FILESYSTEM, "fixture filesystem size limit")
    mount_id = mount_record(root, info.st_dev)
    with open(f"/proc/self/fdinfo/{fd}", "rb") as source:
        pinned = re.findall(rb"^mnt_id:\s*(\d+)$", source.read(4096), re.M)
    require(pinned == [str(mount_id).encode("ascii")], "fixture directory is on a different mount")
    unique_id = unique_mount_id(fd)
    require(unique_id > 0, "invalid unique mount ID")
    with open("/proc/sys/kernel/random/boot_id", "rb") as source:
        boot = source.read(64)
    require(re.fullmatch(rb"[a-f0-9-]{36}\n", boot), "invalid boot identity")
    identity = {
        "boot_sha256": hashlib.sha256(boot).hexdigest(),
        "mount_namespace": os.stat("/proc/self/ns/mnt").st_ino,
        "mount_id": mount_id,
        "unique_mount_id": unique_id,
        "device": info.st_dev,
        "root_inode": info.st_ino,
        "fsid": fs.f_fsid,
        "block_bytes": fs.f_frsize,
        "total_blocks": fs.f_blocks,
    }
    return identity, fs.f_bfree, bool(fs.f_flag & os.ST_RDONLY)


def entry_state(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_uid, info.st_nlink,
            info.st_size, info.st_blocks, info.st_mtime_ns, info.st_ctime_ns, info.st_rdev)


def inventory(root_fd, deadline, *, allow_vfs_entries=False):
    """Walk a quiescent fixture using directory FDs and metadata only."""
    anchor = os.fstat(root_fd)
    allowed_kinds = VFS_KINDS if allow_vfs_entries else DEFAULT_KINDS
    seen = {}
    count = 0

    def visit(fd, rel, info, depth, parent=None, name=None):
        nonlocal count
        count += 1
        require(count <= MAX_ENTRIES and depth <= MAX_DEPTH and len(rel) <= MAX_RELATIVE_PATH and
                time.monotonic() < deadline, "fixture walk limit")
        require(info.st_dev == anchor.st_dev and info.st_ino > 0 and info.st_blocks >= 0,
                "invalid fixture inode")
        state = entry_state(info)
        kind = ENTRY_KIND.get(stat.S_IFMT(info.st_mode), "unknown")
        require(kind in allowed_kinds, f"unsupported fixture entry: {kind}")
        if kind == "block device":
            # Moby leaves its quota probe on the private loop. Account for the
            # directory entry without opening the device or following a link.
            require(rel in VFS_QUOTA_NODES and info.st_rdev == anchor.st_dev and
                    info.st_uid == 0 and info.st_nlink == 1 and info.st_blocks == 0 and
                    info.st_size == 0 and stat.S_IMODE(info.st_mode) == 0o600,
                    "unrecognized VFS quota device")
        directory = kind == "directory"
        if info.st_ino in seen:
            previous, paths = seen[info.st_ino]
            require(not directory and state == entry_state(previous), "ambiguous fixture inode")
            paths.append(rel)
        else:
            seen[info.st_ino] = info, [rel]

        if directory:
            require(entry_state(os.fstat(fd)) == state, "fixture directory changed")
            with os.scandir(fd) as entries:
                for entry in entries:
                    require(count < MAX_ENTRIES and time.monotonic() < deadline, "fixture walk limit")
                    require(safe_component(entry.name), "unsupported fixture name")
                    path = entry.name if rel == "." else f"{rel}/{entry.name}"
                    child = os.stat(entry.name, dir_fd=fd, follow_symlinks=False)
                    if stat.S_ISDIR(child.st_mode):
                        child_fd = os.open(entry.name, DIR_FLAGS | getattr(os, "O_NOATIME", 0), dir_fd=fd)
                        try:
                            visit(child_fd, path, child, depth + 1, fd, entry.name)
                        finally:
                            os.close(child_fd)
                    else:
                        visit(fd, path, child, depth + 1, fd, entry.name)
            require(entry_state(os.fstat(fd)) == state, "fixture directory changed")
        if parent is not None:
            require(entry_state(os.stat(name, dir_fd=parent, follow_symlinks=False)) == state,
                    "fixture entry changed")

    visit(root_fd, ".", anchor, 0)
    result = []
    for inode, (info, paths) in sorted(seen.items()):
        kind = ENTRY_KIND[stat.S_IFMT(info.st_mode)]
        require(kind == "directory" or info.st_nlink == len(paths), "incomplete fixture hard links")
        result.append({"inode": inode, "kind": kind,
                       "blocks_512": info.st_blocks, "paths": sorted(paths)})
    return result


def capture(path, *, allow_vfs_entries=False):
    require(sys.platform == "linux" and os.geteuid() == 0, "snapshot requires Linux root")
    root = Path(os.path.abspath(path))
    require(root != Path("/") and root.resolve(strict=True) == root and
            all(safe_component(part) for part in root.parts[1:]), "invalid fixture mount path")
    fd = os.open(root, DIR_FLAGS | os.O_NOATIME)
    try:
        before = mount_sample(fd, root)
        marker = os.stat(MARKER, dir_fd=fd, follow_symlinks=False)
        require(stat.S_ISREG(marker.st_mode) and stat.S_IMODE(marker.st_mode) == 0o600 and
                marker.st_dev == os.fstat(fd).st_dev and marker.st_uid == 0 and
                marker.st_nlink == 1 and marker.st_size == 0, "invalid fixture marker")
        entries = inventory(fd, time.monotonic() + CAPTURE_SECONDS, allow_vfs_entries=allow_vfs_entries)
        require(entry_state(marker) == entry_state(os.stat(MARKER, dir_fd=fd, follow_symlinks=False)),
                "fixture marker changed during snapshot")
        require(before == mount_sample(fd, root), "fixture changed during snapshot")
    finally:
        os.close(fd)
    identity, free, read_only = before
    identity["marker_inode"] = marker.st_ino
    return {"identity": identity, "free_blocks": free, "read_only": read_only, "entries": entries}


def integers(obj, keys):
    return all(type(obj.get(key)) is int and obj[key] >= 0 for key in keys)


def snapshot_entries(snapshot, *, allow_vfs_entries=False):
    identity = snapshot["identity"]
    require(set(snapshot) == {"identity", "free_blocks", "read_only", "entries"} and
            set(identity) == {*IDENTITY_NUMBERS, "boot_sha256"} and integers(identity, IDENTITY_NUMBERS) and
            re.fullmatch(r"[a-f0-9]{64}", identity["boot_sha256"]) and
            all(identity[key] > 0 for key in ("mount_namespace", "mount_id", "unique_mount_id",
                                              "root_inode", "marker_inode")) and
            0 < identity["block_bytes"] <= 65536 and
            0 < identity["block_bytes"] * identity["total_blocks"] <= MAX_FILESYSTEM and
            integers(snapshot, ("free_blocks",)) and snapshot["free_blocks"] <= identity["total_blocks"] and
            type(snapshot["read_only"]) is bool, "invalid snapshot filesystem")
    require(isinstance(snapshot["entries"], list) and 0 < len(snapshot["entries"]) <= MAX_ENTRIES,
            "invalid snapshot entry count")
    allowed_kinds = VFS_KINDS if allow_vfs_entries else DEFAULT_KINDS
    entries, paths = {}, {}
    for entry in snapshot["entries"]:
        kind, links = entry["kind"], entry["paths"]
        require(set(entry) == {"inode", "kind", "paths", "blocks_512"} and
                isinstance(kind, str) and kind in allowed_kinds and integers(entry, ("inode", "blocks_512")) and
                entry["inode"] > 0 and entry["inode"] not in entries and
                entry["blocks_512"] * 512 <= MAX_FILESYSTEM and
                isinstance(links, list) and 0 < len(links) <= MAX_ENTRIES - len(paths) and
                (kind != "directory" or len(links) == 1), "invalid snapshot inode")
        if kind == "block device":
            require(len(links) == 1 and isinstance(links[0], str) and links[0] in VFS_QUOTA_NODES and
                    entry["blocks_512"] == 0,
                    "invalid VFS quota device snapshot")
        for path in links:
            require(isinstance(path, str) and len(path) <= MAX_RELATIVE_PATH and path not in paths and
                    ((path == "." and kind == "directory" and entry["inode"] == identity["root_inode"]) or
                     (0 < len(path.split("/")) <= MAX_DEPTH and all(safe_component(p) for p in path.split("/")))),
                    "invalid snapshot path")
            paths[path] = kind
        entries[entry["inode"]] = entry
    marker = entries.get(identity["marker_inode"])
    require(paths.get(".") == "directory" and marker is not None and
            marker["kind"] == "file" and marker["paths"] == [MARKER] and
            all(paths.get(path.rpartition("/")[0] or ".") == "directory" for path in paths),
            "incomplete fixture snapshot")
    return entries


def compare(before, after, *, allow_vfs_entries=False):
    preceding = snapshot_entries(before, allow_vfs_entries=allow_vfs_entries)
    following = snapshot_entries(after, allow_vfs_entries=allow_vfs_entries)
    require(before["identity"] == after["identity"], "snapshots are from different mounts")
    identity = before["identity"]
    block = identity["block_bytes"]
    used_before = (identity["total_blocks"] - before["free_blocks"]) * block
    used_after = (identity["total_blocks"] - after["free_blocks"]) * block
    subtotals = {"file": 0, "directory": 0}
    changes = []

    def add_change(old, new):
        kind = (new or old)["kind"]
        old_bytes = old["blocks_512"] * 512 if old else 0
        new_bytes = new["blocks_512"] * 512 if new else 0
        delta = new_bytes - old_bytes
        # Preserve the file/directory subtotals while reporting the actual inode type.
        subtotals["directory" if kind == "directory" else "file"] += delta
        changes.append({"inode": (new or old)["inode"], "kind": kind,
                        "before_paths": sorted(old["paths"]) if old else [],
                        "after_paths": sorted(new["paths"]) if new else [],
                        "before_bytes": old_bytes, "after_bytes": new_bytes, "delta_bytes": delta})

    for inode in sorted(preceding.keys() | following.keys()):
        old, new = preceding.get(inode), following.get(inode)
        if old == new:
            continue
        if old and new and (old["kind"] != new["kind"] or set(old["paths"]).isdisjoint(new["paths"])):
            # Report disjoint names as two endpoints. Linux may reuse the inode between samples.
            add_change(old, None)
            add_change(None, new)
        else:
            add_change(old, new)

    filesystem_delta = used_after - used_before
    observed_delta = sum(subtotals.values())
    return {"identity": identity, "filesystem_before_bytes": used_before, "filesystem_after_bytes": used_after,
            "filesystem_delta_bytes": filesystem_delta, "file_delta_bytes": subtotals["file"],
            "directory_delta_bytes": subtotals["directory"], "observed_delta_bytes": observed_delta,
            "unattributed_delta_bytes": filesystem_delta - observed_delta, "changes": changes}


def read_snapshot(path):
    path = Path(os.path.abspath(path))
    resolved = path.resolve(strict=True)
    require(path.suffix == ".json" and resolved.suffix == ".json" and
            all(safe_component(part) for name in (path, resolved) for part in name.parts[1:]),
            "invalid snapshot document name")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_size <= MAX_DOCUMENT,
                "invalid snapshot document")
        raw = source.read(MAX_DOCUMENT + 1)
    require(len(raw) <= MAX_DOCUMENT, "snapshot document limit")
    return json.loads(raw, object_pairs_hook=unique_json_object)


def unique_json_object(pairs):
    obj = {}
    for key, value in pairs:
        require(key not in obj, "duplicate snapshot JSON key")
        obj[key] = value
    return obj


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    snapshot = subparsers.add_parser("snapshot")
    snapshot.add_argument("mount")
    difference = subparsers.add_parser("compare")
    difference.add_argument("before")
    difference.add_argument("after")
    args = parser.parse_args()
    try:
        if args.command == "snapshot":
            result = capture(args.mount)
        else:
            result = compare(read_snapshot(args.before), read_snapshot(args.after))
    except MeasurementError as error:
        parser.exit(2, f"allocation measurement: {error}\n")
    except (OSError, AttributeError, KeyError, TypeError, ValueError, RuntimeError):
        parser.exit(2, "allocation measurement: fixture or snapshot unavailable or invalid\n")
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
