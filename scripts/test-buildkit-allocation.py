#!/usr/bin/env python3
"""Focused, unprivileged tests for BuildKit allocation accounting."""

from copy import deepcopy
import os
from pathlib import Path
import runpy
import socket
import tempfile
import time
import unittest
from unittest.mock import mock_open, patch


allocation = runpy.run_path(str(Path(__file__).with_name("check-buildkit-allocation.py")))
compare = allocation["compare"]
inventory = allocation["inventory"]
MeasurementError = allocation["MeasurementError"]
MARKER = allocation["MARKER"]


def entry(inode, blocks, *paths, kind="file"):
    return {"inode": inode, "kind": kind, "blocks_512": blocks, "paths": list(paths)}


def snapshot(free, entries, directory_blocks=8):
    return {
        "identity": {"boot_sha256": "f" * 64, "mount_namespace": 1, "mount_id": 2, "unique_mount_id": 3,
                     "device": 7, "root_inode": 2, "marker_inode": 3, "fsid": 1, "block_bytes": 4096,
                     "total_blocks": 128},
        "free_blocks": free,
        "read_only": True,
        "entries": [entry(2, directory_blocks, ".", kind="directory"), entry(3, 0, MARKER), *entries],
    }


class AllocationDiffTest(unittest.TestCase):
    def test_growing_overwrite_unchanged_blocks_removal_and_addition(self):
        before = snapshot(100, [entry(5, 8, "same-size"), entry(8, 8, "solver.db"),
                                entry(9, 16, "payload"), entry(10, 8, "old", "old-alias")])
        after = snapshot(94, [entry(5, 8, "same-size"), entry(8, 24, "solver.db"),
                               entry(10, 8, "old-alias"), entry(11, 24, "new-db")], directory_blocks=16)
        result = compare(before, after)
        rows = {row["inode"]: row for row in result["changes"]}
        self.assertEqual({inode: row["delta_bytes"] for inode, row in rows.items()},
                         {2: 4096, 8: 8192, 9: -8192, 10: 0, 11: 12288})
        self.assertEqual(rows[10]["before_paths"], ["old", "old-alias"])
        self.assertEqual(rows[10]["after_paths"], ["old-alias"])
        self.assertEqual(result["file_delta_bytes"], 12288)
        self.assertEqual(result["directory_delta_bytes"], 4096)
        self.assertEqual(result["observed_delta_bytes"], 16384)
        self.assertEqual(result["filesystem_delta_bytes"], 24576)
        self.assertEqual(result["unattributed_delta_bytes"], 8192)

    def test_hard_links_count_once_on_change_and_removal(self):
        one_name = snapshot(100, [entry(5, 8, "payload")])
        two_names = snapshot(100, [entry(5, 8, "payload", "payload-copy")])
        deleted = snapshot(101, [])
        self.assertEqual(compare(one_name, two_names)["file_delta_bytes"], 0)
        result = compare(two_names, deleted)
        self.assertEqual(result["file_delta_bytes"], -4096)
        self.assertEqual(result["unattributed_delta_bytes"], 0)
        self.assertEqual(len(result["changes"]), 1)

    def test_inode_reuse_with_disjoint_names_reports_both_endpoints(self):
        before = snapshot(100, [entry(5, 8, "old")])
        after = snapshot(100, [entry(5, 8, "new")])
        result = compare(before, after)
        self.assertEqual([(row["before_paths"], row["after_paths"], row["delta_bytes"])
                          for row in result["changes"]], [(["old"], [], -4096), ([], ["new"], 4096)])

    def test_vfs_entries_keep_inode_and_residual_accounting(self):
        before = snapshot(100, [entry(5, 8, "pointer", "alias", kind="symlink"),
                                entry(6, 0, "endpoint", kind="socket")])
        after = snapshot(101, [entry(6, 8, "endpoint")])
        with self.assertRaisesRegex(MeasurementError, "invalid snapshot inode"):
            compare(before, after)
        result = compare(before, after, allow_vfs_entries=True)
        self.assertEqual([(row["kind"], row["delta_bytes"]) for row in result["changes"]],
                         [("symlink", -4096), ("socket", 0), ("file", 4096)])
        self.assertEqual(result["file_delta_bytes"], 0)
        self.assertEqual(result["unattributed_delta_bytes"], -4096)

    def test_only_known_unallocated_vfs_quota_device_is_accepted(self):
        parents = [entry(4, 8, "docker-data", kind="directory"),
                   entry(5, 8, "docker-data/volumes", kind="directory"),
                   entry(6, 8, "docker-data/vfs", kind="directory")]
        nodes = sorted(allocation["VFS_QUOTA_NODES"])
        fixture = snapshot(100, [*parents, *(entry(n, 0, node, kind="block device")
                                             for n, node in enumerate(nodes, 7))])
        self.assertEqual(compare(fixture, fixture, allow_vfs_entries=True)["changes"], [])
        with self.assertRaisesRegex(MeasurementError, "invalid snapshot inode"):
            compare(fixture, fixture)
        for name, blocks in ((nodes[0], 8), ("docker-data/volumes/other", 0)):
            with self.subTest(path=name, blocks=blocks):
                wrong = snapshot(100, [*parents, entry(7, blocks, name, kind="block device")])
                with self.assertRaisesRegex(MeasurementError, "invalid VFS quota device"):
                    compare(wrong, wrong, allow_vfs_entries=True)

    def test_remount_preserves_identity_but_new_mount_is_rejected(self):
        before = snapshot(100, [])
        after = deepcopy(before)
        after["read_only"] = False
        self.assertEqual(compare(before, after)["filesystem_delta_bytes"], 0)
        for field in ("mount_namespace", "mount_id", "unique_mount_id", "root_inode", "marker_inode", "fsid", "device"):
            with self.subTest(field=field):
                changed = deepcopy(after)
                changed["identity"][field] += 2
                if field == "root_inode":
                    changed["entries"][0]["inode"] = changed["identity"][field]
                if field == "marker_inode":
                    changed["entries"][1]["inode"] = changed["identity"][field]
                with self.assertRaisesRegex(MeasurementError, "different mounts"):
                    compare(before, changed)

    def test_malformed_snapshot_paths_are_rejected(self):
        for name in ("sub/../outside", "disconnected/child", "copy.env.local", "/escape", "bad\nname", "."):
            with self.subTest(name=name):
                data = snapshot(100, [entry(7, 8, name)])
                with self.assertRaises(MeasurementError):
                    compare(data, data)

    def test_duplicate_snapshot_keys_are_rejected(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "before.json"
            path.write_text('{"identity":{},"identity":{}}', encoding="ascii")
            with self.assertRaisesRegex(MeasurementError, "duplicate snapshot JSON key"):
                allocation["read_snapshot"](path)


class AllocationInventoryTest(unittest.TestCase):
    def read(self, path, *, allow_vfs_entries=False):
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
        try:
            return inventory(fd, time.monotonic() + 5, allow_vfs_entries=allow_vfs_entries)
        finally:
            os.close(fd)

    def test_walk_reports_directory_and_allocations_once(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "files").mkdir()
            payload = root / "files/payload"
            payload.write_bytes(b"a" * 8192)
            os.link(payload, root / "alias")
            entries = {item["inode"]: item for item in self.read(root)}
            st = payload.stat()
            self.assertEqual(entries[st.st_ino], entry(st.st_ino, st.st_blocks, "alias", "files/payload"))
            self.assertEqual(entries[root.stat().st_ino]["paths"], ["."])
            self.assertEqual(entries[(root / "files").stat().st_ino]["kind"], "directory")

    def test_incomplete_hard_links_are_rejected(self):
        with tempfile.TemporaryDirectory() as outer:
            parent = Path(outer)
            (parent / "fixture").mkdir()
            (parent / "origin").touch()
            os.link(parent / "origin", parent / "fixture/link")
            with self.assertRaisesRegex(MeasurementError, "incomplete fixture hard links"):
                self.read(parent / "fixture")

    def test_symlink_and_fifo_are_rejected(self):
        for kind in ("link", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as root:
                path = Path(root) / "special"
                if kind == "link":
                    path.symlink_to("absent", target_is_directory=True)
                else:
                    os.mkfifo(path)
                with self.assertRaisesRegex(MeasurementError, "unsupported fixture entry"):
                    self.read(root)
                if kind == "fifo":
                    with self.assertRaisesRegex(MeasurementError, "unsupported fixture entry: fifo"):
                        self.read(root, allow_vfs_entries=True)

    def test_vfs_inventory_uses_lstat_for_symlinks_and_sockets(self):
        with tempfile.TemporaryDirectory(dir="/tmp") as path, socket.socket(socket.AF_UNIX) as endpoint:
            root = Path(path)
            socket_path = root / "endpoint"
            endpoint.bind(str(socket_path))
            with self.assertRaisesRegex(MeasurementError, "unsupported fixture entry: socket"):
                self.read(root)
            symlink = root / "pointer"
            symlink.symlink_to("absent", target_is_directory=True)
            entries = {item["inode"]: item for item in self.read(root, allow_vfs_entries=True)}
            for item, kind in ((symlink, "symlink"), (socket_path, "socket")):
                info = item.lstat()
                self.assertEqual(entries[info.st_ino], entry(info.st_ino, info.st_blocks, item.name, kind=kind))

    def test_private_ext4_mount_record_rejects_alias_and_submount(self):
        mount_record = allocation["mount_record"]
        device = os.makedev(7, 8)
        target = b"42 12 7:8 / /fixture rw,nosuid,nodev - ext4 /dev/loop8 rw\n"
        with patch("builtins.open", mock_open(read_data=target)):
            self.assertEqual(mount_record("/fixture", device), 42)
        rejected = [
            target + b"43 12 7:8 / /other rw,nosuid,nodev - ext4 /dev/loop8 rw\n",
            target + b"43 42 0:12 / /fixture/sub rw - tmpfs tmpfs rw\n",
            target.replace(b" - ", b" shared:3 - "),
            target.replace(b"/dev/loop8", b"/dev/sda1"),
        ]
        for records in rejected:
            with patch("builtins.open", mock_open(read_data=records)), self.assertRaises(MeasurementError):
                mount_record("/fixture", device)


if __name__ == "__main__":
    unittest.main()
