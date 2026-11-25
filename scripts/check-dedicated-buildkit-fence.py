#!/usr/bin/env python3
"""Verify the pinned BuildKit daemon's dedicated owner lock in an isolated Linux VM."""

from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import signal
import socket
import stat
import struct
import subprocess
import sys
import time


@contextmanager
def directory_lock(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield os.fstat(fd).st_dev, os.fstat(fd).st_ino
    finally:
        os.close(fd)


@contextmanager
def child(args, workspace):
    environment = {key: str(workspace) for key in ("HOME", "TMPDIR", "XDG_RUNTIME_DIR")}
    environment.update(PATH="/usr/sbin:/usr/bin:/sbin:/bin", LANG="C", GOMAXPROCS="1")
    with subprocess.Popen(args, cwd=workspace, env=environment, start_new_session=True,
                          stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as proc:
        try:
            yield proc
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait(timeout=3)


def check_binary(workspace, name, checksum):
    path = workspace / name
    info = path.lstat()
    assert re.fullmatch(r"[a-f0-9]{64}", checksum)
    assert stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
    assert stat.S_IMODE(info.st_mode) == 0o500 and 0 < info.st_size <= 100 * 1024 * 1024
    with path.open("rb") as data:
        assert hashlib.file_digest(data, "sha256").hexdigest() == checksum
    return path


def database_digests(store):
    digests = {}
    for name in ("cache.db", "runc-native/metadata_v2.db", "runc-native/containerdmeta.db",
                 "runc-native/snapshots/metadata.db"):
        path = store / name
        if path.exists():
            assert path.is_file() and not path.is_symlink()
            with path.open("rb") as database:
                digests[name] = hashlib.file_digest(database, "sha256").hexdigest()
    assert len(digests) in (3, 4)
    return digests


def await_listener(proc, path):
    deadline = time.monotonic() + 9
    while time.monotonic() < deadline:
        assert proc.poll() is None, "daemon exited before the socket was ready"
        if path.is_socket():
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(0.3)
                try:
                    client.connect(str(path))
                except (ConnectionRefusedError, TimeoutError):
                    continue
                peer = client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
                pid, uid, _ = struct.unpack("3i", peer)
                assert (pid, uid) == (proc.pid, 0)
                return
        time.sleep(0.1)
    raise AssertionError("daemon socket deadline")


def create_cache_evidence(workspace, client, addr, daemon, sock):
    fixture = runpy.run_path(str(workspace / "check-buildkit-native.py"))
    environment = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LANG": "C", "HOME": str(workspace),
                   "TMPDIR": str(workspace), "XDG_RUNTIME_DIR": str(workspace)}
    ctl = (client, "--addr", addr, "--timeout", "15")

    def inventory(*options):
        output = fixture["command"](workspace, environment, "worker_inventory",
                                    (*ctl, "du", "--format", "{{json .}}", *options), timeout=15)
        return fixture["official_inventory"](output)

    generation = fixture["socket_generation"](sock, daemon.pid)
    before = inventory()
    assert not before, "only a new isolated store may run the producer"
    payload = b"DSG dedicated worker synthetic project " + os.urandom(16).hex().encode("ascii") + b"\n"
    definition = fixture["scratch_definition"](payload)
    llb, image = workspace / "source.llb", workspace / "source.oci.tar"
    llb.write_bytes(definition)
    with llb.open("rb") as source:
        fixture["command"](workspace, environment, "dedicated_owned_producer",
                           (*ctl, "build", "--output", f"type=oci,dest={image},compression=uncompressed",
                            "--progress", "plain"), timeout=22, capture=False, stdin=source)
    layer = fixture["verify_owned_export"](image, payload)
    current = inventory()
    added = current.keys() - before.keys()
    assert len(added) == 1 and all(current.get(key) == value for key, value in before.items())
    exact_id = next(iter(added))
    assert inventory() == current and inventory("--filter", f"id=={exact_id}") == {exact_id: current[exact_id]}
    assert fixture["socket_generation"](sock, daemon.pid) == generation
    return {"id": exact_id, "source": "isolated scratch LLB and independently verified OCI layer",
            "llb_sha256": hashlib.sha256(definition).hexdigest(), "verified_layer_sha256": layer,
            "daemon_generation": generation, "before_count": len(before), "after_count": len(current)}


def qualify(workspace, daemon_hash, client_hash):
    assert sys.platform == "linux" and os.uname().machine == "aarch64" and os.geteuid() == 0
    assert workspace == workspace.resolve(strict=True) and workspace.is_mount()
    assert workspace.stat().st_uid == 0 and stat.S_IMODE(workspace.stat().st_mode) == 0o700
    for kind in ("net", "mnt"):
        assert os.readlink(f"/proc/self/ns/{kind}") != os.readlink(f"/proc/1/ns/{kind}")
    interfaces = {line.split(":", 1)[0].strip() for line in Path("/proc/net/dev").read_text().splitlines() if ":" in line}
    assert interfaces == {"lo"}
    assert not Path("/run/docker.sock").is_socket() and not Path("/run/containerd/containerd.sock").is_socket()

    daemon = check_binary(workspace, "buildkitd", daemon_hash)
    client = check_binary(workspace, "buildctl", client_hash)
    store, other = workspace / "store", workspace / "other"
    store.mkdir(mode=0o700)
    other.mkdir(mode=0o700)
    config, sock = workspace / "buildkitd.toml", workspace / "buildkitd.sock"
    config.write_text(
        '[worker.oci]\nenabled = true\nsnapshotter = "native"\nrootless = false\n'
        'noProcessSandbox = false\ngc = false\nmax-parallelism = 1\nnetworkMode = "host"\n'
        '[worker.containerd]\nenabled = false\n[cdi]\ndisabled = true\n[history]\nmaxEntries = 0\n',
        encoding="ascii",
    )
    addr = f"unix://{sock}"
    args = (daemon, "--dsg-dedicated-worker", "--oci-worker=true", "--oci-worker-snapshotter=native",
            "--containerd-worker=false", "--config", config, "--root", store, "--addr", addr,
            "--otel-socket-path", workspace / "trace.sock")
    inspection = (daemon, "--dsg-inspect-dedicated-worker", "--config", config, "--root", store)

    with directory_lock(store) as identity, child(args, workspace) as rejected:
        assert rejected.wait(timeout=8) != 0
        assert not sock.exists() and not list(store.iterdir()), "rejected daemon initialized the worker"

    with child(args, workspace) as running:
        await_listener(running, sock)
        try:
            with directory_lock(store):
                raise AssertionError("administration acquired the live daemon's store")
        except BlockingIOError:
            pass
        with child(inspection, workspace) as rejected:
            assert rejected.wait(timeout=8) != 0
        with directory_lock(other) as other_identity:
            assert other_identity != identity
        details = subprocess.run(
            (client, "--addr", addr, "--timeout", "6", "debug", "workers", "--format", "{{json .}}"),
            env={"PATH": "/usr/bin:/bin", "LANG": "C", "HOME": str(workspace)}, cwd=workspace,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=8, check=True,
        )
        assert 0 < len(details.stdout) <= 32768
        workers = json.loads(details.stdout)
        assert len(workers) == 1 and workers[0]["labels"]["org.mobyproject.buildkit.worker.snapshotter"] == "native"
        assert workers[0]["gcPolicy"] in (None, [])
        worker_id = workers[0]["id"]
        assert re.fullmatch(r"[a-z0-9]{25}", worker_id)
        candidate = create_cache_evidence(workspace, client, addr, running, sock)

    with directory_lock(store) as recovered:
        assert recovered == identity
        digests = database_digests(store)
    without_mode = (args[0], *args[2:])
    with child(without_mode, workspace) as rejected:
        assert rejected.wait(timeout=8) != 0
    def inspect(*options):
        result = subprocess.run(
            (*inspection, *options), env={"PATH": "/usr/bin:/bin", "LANG": "C", "HOME": str(workspace)},
            cwd=workspace, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=8,
        )
        if result.returncode:
            reason = "record" if options else "worker"
            category = "preserve" if b"exact effects unproven" in result.stderr else "other"
            raise AssertionError(f"offline_{reason}_{category}")
        return result

    inspected_store = inspect()
    assert json.loads(inspected_store.stdout)["worker"] == worker_id
    offline = inspect("--dsg-record-id", candidate["id"])
    assert 0 < len(offline.stdout) <= 4096
    observation = json.loads(offline.stdout)
    assert observation["worker"] == worker_id
    assert all(isinstance(observation[field], int) and observation[field] > 0
               for field in ("solver_tx", "cache_tx", "containerd_tx"))
    candidate["worker_id"] = worker_id
    candidate["offline_record"] = observation["record"]
    assert observation["record"]["id"] == candidate["id"] and observation["record"]["record_present"] is True
    assert observation["record"]["snapshot_binding"] is True
    assert observation["record"]["metadata_only_candidate"] is False
    assert re.fullmatch(r"[a-f0-9]{64}", observation["record"]["bindings_sha256"])
    candidate["disposition"] = "preserve"
    evidence = json.dumps(candidate, sort_keys=True, separators=(",", ":")).encode("utf-8")
    candidate["evidence_sha256"] = hashlib.sha256(evidence).hexdigest()
    with directory_lock(store) as recovered:
        assert recovered == identity and database_digests(store) == digests
    assert not list(other.iterdir())
    return {
        "daemon_started": True, "locked_daemon_start_denied": True, "live_maintenance_lock_denied": True,
        "live_inspection_denied": True, "flagless_start_denied": True, "offline_worker_matches": True,
        "offline_database_digests_unchanged": len(digests),
        "cache_read_only_evidence": candidate,
        "independent_store_unlocked": True, "stable_lock_reopened": True, "verified_native_workers": 1,
        "actual_cache_deletion_requests": 0, "prune_requests": 0,
    }


if __name__ == "__main__":
    os.umask(0o077)
    try:
        result = qualify(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except (AssertionError, OSError, KeyError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        location = error.__traceback__
        fixture_line = location.tb_lineno
        while location is not None:
            if location.tb_frame.f_code.co_filename == __file__:
                fixture_line = location.tb_lineno
            location = location.tb_next
        category = str(error) if re.fullmatch(r"[a-z_]{1,60}", str(error)) else None
        print(json.dumps({"fixture_error": type(error).__name__, "errno": getattr(error, "errno", None),
                          "line": fixture_line, "child_exit": getattr(error, "returncode", None),
                          "category": category}), flush=True)
        raise SystemExit(1) from None
    print(json.dumps(result, sort_keys=True), flush=True)
