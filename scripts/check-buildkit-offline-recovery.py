#!/usr/bin/env python3
"""Qualify one dedicated BuildKit root on a disposable Linux ext4 filesystem."""

from contextlib import contextmanager
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import socket
import stat
import struct
import subprocess
import sys
import time
from types import SimpleNamespace

SYSTEM_PATH = "/usr/sbin:/usr/bin:/sbin:/bin"
BOUNDARY_ERRORS = (
    "quiescence_enrollment", "quiescence_namespace", "quiescence_filesystem", "quiescence_mounts",
    "quiescence_handles", "quiescence_visibility", "quiescence_limits",
)


def command(workspace, args, timeout=8):
    return subprocess.run(
        args, cwd=workspace, env={"PATH": SYSTEM_PATH, "LANG": "C", "HOME": str(workspace),
                                 "XDG_RUNTIME_DIR": str(workspace), "TMPDIR": str(workspace)},
        stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout,
    )


def mount_command(workspace, *args):
    result = command(workspace, args, timeout=10)
    if result.returncode:
        raise RuntimeError(f"fixture_{Path(args[0]).name}")


@contextmanager
def private_store(workspace):
    image, mountpoint = workspace / "trial.ext4", workspace / "native"
    with image.open("xb") as temporary:
        temporary.truncate(64 * 1024**2)
    metadata = image.lstat()
    assert stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and metadata.st_nlink == 1
    mountpoint.mkdir(mode=0o700)
    mount_command(workspace, "mkfs.ext4", "-q", "-F", "-m", "0", "-E",
                  "lazy_itable_init=0,lazy_journal_init=0", image)
    mount_command(workspace, "mount", "-t", "ext4", "-o", "loop,nodev,nosuid", image, mountpoint)
    try:
        mount_command(workspace, "mount", "--make-private", mountpoint)
        assert mountpoint.is_mount() and mountpoint.stat().st_dev != workspace.stat().st_dev
        store = mountpoint / "worker"
        store.mkdir(mode=0o700)
        yield mountpoint, store
    finally:
        mount_command(workspace, "umount", mountpoint)
        assert not mountpoint.is_mount()
        for _ in range(3):
            loop = command(workspace, ("losetup", "-j", image))
            if loop.returncode == 0 and not loop.stdout.strip():
                break
            time.sleep(0.2)
        else:
            raise AssertionError("owned_loop_still_attached")


def wait_for_worker(proc, sock):
    deadline = time.monotonic() + 9
    while time.monotonic() < deadline:
        assert proc.poll() is None, "strict_daemon_exited"
        if sock.is_socket():
            with socket.socket(socket.AF_UNIX) as peer:
                peer.settimeout(0.3)
                try:
                    peer.connect(str(sock))
                except (ConnectionRefusedError, TimeoutError):
                    time.sleep(0.1)
                    continue
                pid, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                fields = {}
                for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                    name, _, value = line.partition(":")
                    if name in ("PPid", "NSpid"):
                        fields[name] = [int(part) for part in value.split()]
                assert uid == 0 and fields["PPid"] == [proc.pid]
                assert len(fields["NSpid"]) > 1 and fields["NSpid"][-1] == 1
                return SimpleNamespace(pid=pid)
        time.sleep(0.1)
    raise RuntimeError("strict_daemon_deadline")


def rejection(workspace, args, reasons=BOUNDARY_ERRORS):
    result = command(workspace, args)
    observed = [reason for reason in reasons if reason.encode() in result.stderr]
    assert result.returncode != 0 and not result.stdout.strip() and len(observed) == 1, "unclassified_denial"
    return observed[0]


def qualify(workspace, daemon_hash, client_hash):
    assert sys.platform == "linux" and os.uname().machine == "aarch64" and os.geteuid() == 0
    assert workspace.resolve(strict=True) == workspace and workspace.is_mount()
    assert workspace.stat().st_uid == 0 and stat.S_IMODE(workspace.stat().st_mode) == 0o700
    assert os.readlink("/proc/self/ns/pid") == os.readlink("/proc/1/ns/pid")
    for kind in ("net", "mnt"):
        assert os.readlink(f"/proc/self/ns/{kind}") != os.readlink(f"/proc/1/ns/{kind}")
    assert not Path("/run/docker.sock").is_socket() and not Path("/run/containerd/containerd.sock").is_socket()
    fixture = runpy.run_path(str(workspace / "check-dedicated-buildkit-fence.py"))
    daemon = fixture["check_binary"](workspace, "buildkitd", daemon_hash)
    client = fixture["check_binary"](workspace, "buildctl", client_hash)
    config, sock = workspace / "buildkitd.toml", workspace / "native.sock"
    config.write_text(
        '[worker.oci]\nenabled = true\nsnapshotter = "native"\nrootless = false\ngc = false\n'
        'noProcessSandbox = false\nmax-parallelism = 1\nnetworkMode = "host"\n'
        '[worker.containerd]\nenabled = false\n[cdi]\ndisabled = true\n[history]\nmaxEntries = 0\n',
        encoding="ascii",
    )
    addr = f"unix://{sock}"
    with private_store(workspace) as (mountpoint, store):
        args = (daemon, "--dsg-dedicated-worker", "--dsg-require-quiescence", "--config", config,
                "--root", store, "--addr", addr, "--otel-socket-path", workspace / "trace.sock")
        inspect = (daemon, "--dsg-inspect-dedicated-worker", "--dsg-require-quiescence",
                   "--config", config, "--root", store)
        assert rejection(workspace, args) == "quiescence_namespace"
        isolated = ("unshare", "--pid", "--fork", "--kill-child", "--mount-proc", *args)
        with fixture["child"](isolated, workspace) as running:
            peer = wait_for_worker(running, sock)
            candidate = fixture["create_cache_evidence"](workspace, client, addr, peer, sock)
        check = (*inspect, "--dsg-record-id", candidate["id"])
        writable_rejection = rejection(workspace, check)
        assert writable_rejection == "quiescence_filesystem"
        mount_command(workspace, "mount", "-o", "remount,ro", mountpoint)
        database = store / "runc-native/metadata_v2.db"
        try:
            fd = os.open(database, os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW)
        except OSError as issue:
            assert issue.errno == errno.EROFS
        else:
            os.close(fd)
            raise AssertionError("root_write_open_allowed")
        with database.open("rb"):
            assert rejection(workspace, check) == "quiescence_handles"

        alias = workspace / "native-alias"
        alias.mkdir(mode=0o700)
        mount_command(workspace, "mount", "--bind", mountpoint, alias)
        try:
            assert rejection(workspace, check) == "quiescence_mounts"
        finally:
            mount_command(workspace, "umount", alias)
        before = fixture["database_digests"](store)
        observed = command(workspace, check, timeout=12)
        if observed.returncode:
            failure = next((reason for reason in BOUNDARY_ERRORS if reason.encode() in observed.stderr), "unknown")
            raise AssertionError("strict_read_" + failure)
        assert 0 < len(observed.stdout) <= 32768
        evidence = json.loads(observed.stdout)
        assert evidence["record"]["id"] == candidate["id"] and evidence["record"]["record_present"]
        expected = ("filesystem_read_only", "executor_namespace_gone", "mounts_clear", "external_handles_clear")
        assert all(evidence["quiescence"][name] is True for name in expected)
        graph = evidence["graph"]
        assert graph["disposition"] == "preserve" and graph["unverified"]
        assert re.fullmatch(r"[a-f0-9]{64}", graph["references_sha256"])
        assert graph["cache"]["committed"] and graph["solver"]["result_nodes"] > 0
        assert fixture["database_digests"](store) == before
        candidate["offline"] = evidence
        candidate["disposition"] = "preserve"
        snapshot = json.dumps(candidate, sort_keys=True, separators=(",", ":")).encode()
        candidate["evidence_sha256"] = hashlib.sha256(snapshot).hexdigest()
    return {
        "daemon_started": True, "separate_pid_namespace": True, "owned_loop_released": True,
        "root_writable_mount_denied": True, "root_write_open_denied": True, "independent_handle_denied": True,
        "bind_alias_denied": True, "unchanged_database_count": len(before),
        "read_only_candidate": candidate, "actual_cache_deletion_requests": 0, "prune_requests": 0,
    }


if __name__ == "__main__":
    os.umask(0o077)
    try:
        result = qualify(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except (AssertionError, OSError, KeyError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        location = error.__traceback__
        local_line = location.tb_lineno
        while location is not None:
            if location.tb_frame.f_code.co_filename == __file__:
                local_line = location.tb_lineno
            location = location.tb_next
        category = str(error) if re.fullmatch(r"[a-z_]{1,80}", str(error)) else None
        print(json.dumps({"fixture_error": type(error).__name__, "line": local_line,
                          "category": category, "errno": getattr(error, "errno", None)}), flush=True)
        raise SystemExit(1) from None
    print(json.dumps(result, sort_keys=True), flush=True)
