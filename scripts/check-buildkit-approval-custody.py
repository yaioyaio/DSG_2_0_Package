#!/usr/bin/env python3
"""Retain a newly qualified BuildKit record on one private Linux ext4 mount."""

import argparse
import ctypes
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import re
import runpy
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time


STAGE_PATTERN = re.compile(r"/run/dsg-approval-[0-9a-f]{12,64}/stage")
DIGEST_PATTERN = re.compile(r"[0-9a-f]{64}")
NATIVE_STORES = ("solver.db", "metadata_v2.db", "containerd.db", "snapshots/metadata.db", "history.db")
SOURCE_NAME = "native-source.json"
EVIDENCE_NAME = "custody.json"
EXECUTABLE = "dsg-cache-tests"
MAX_DOCUMENT = 32 * 1024
MAX_LIFETIME_SECONDS = 3600


def require(condition, category):
    if not condition:
        raise RuntimeError(category)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False).encode("ascii")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def stage_arg(value):
    if not STAGE_PATTERN.fullmatch(value):
        raise argparse.ArgumentTypeError("expected /run/dsg-approval-<hex>/stage")
    return Path(value)


def milliseconds(clock):
    return time.clock_gettime_ns(clock) // 1_000_000


def boot_hash():
    value = Path("/proc/sys/kernel/random/boot_id").read_bytes().strip()
    require(re.fullmatch(rb"[0-9a-f-]{36}", value), "kernel_boot_identity")
    return sha256(value)


def keeper_identity(pid):
    process = Path(f"/proc/{pid}")
    raw = (process / "stat").read_bytes()
    fields = raw[raw.rindex(b")") + 1:].split()
    require(len(fields) >= 20 and fields[0] not in (b"Z", b"X", b"x", b"T", b"t"), "keeper_not_running")
    return {"pid": pid, "start_ticks": int(fields[19]), "boot_sha256": boot_hash(),
            "mount_namespace": (process / "ns/mnt").stat().st_ino,
            "network_namespace": (process / "ns/net").stat().st_ino}


def private_mount(pid, path, fs, access):
    with Path(f"/proc/{pid}/mountinfo").open("rb") as source:
        data = source.read(1_048_577)
    require(len(data) <= 1_048_576, "mountinfo_limit")
    expected = os.fsencode(path)
    found = []
    for line in data.splitlines():
        options, _, backing = line.partition(b" - ")
        left, right = options.split(), backing.split()
        if len(left) < 6 or left[4] != expected:
            continue
        require(len(left) == 6 and len(right) == 3 and left[3] == b"/" and right[0] == fs,
                "private_mount")
        require({access, b"nodev", b"nosuid"} <= set(left[5].split(b",")) and access in right[2].split(b","),
                "mount_access")
        found.append((int(left[0]), left[2].decode("ascii")))
    require(len(found) == 1, "mount_count")
    return found[0]


def unique_mount_id(path):
    # Stable Linux statx ABI. stx_mask is at 0 and stx_mnt_id is at 144.
    # STATX_MNT_ID_UNIQUE needs Linux 6.8 or later and never reuses an ID.
    statx = ctypes.CDLL(None, use_errno=True).statx
    statx.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_uint, ctypes.c_void_p)
    statx.restype = ctypes.c_int
    buffer = (ctypes.c_uint64 * 32)()
    mask = 0x4000
    result = statx(-100, os.fsencode(path), 0x100 | 0x800, mask, ctypes.byref(buffer))
    require(result == 0 and struct.unpack_from("=I", buffer)[0] & mask, "unique_mount_identity")
    unique_id = struct.unpack_from("=Q", buffer, 144)[0]
    require(unique_id > 0, "unique_mount_identity")
    return unique_id


def mounted_identity(stage, visible_stage, pid, read_only=True):
    mount = stage / "candidate"
    visible_mount = visible_stage / "candidate"
    root = visible_mount / "worker"
    mount_id, device = private_mount(pid, mount, b"ext4", b"ro" if read_only else b"rw")
    info = root.lstat()
    fs = os.statvfs(root)
    require(info.st_uid == 0 and info.st_mode == stat.S_IFDIR | 0o700, "private_worker_root")
    require(root.stat().st_dev == visible_mount.stat().st_dev != visible_stage.stat().st_dev,
            "dedicated_ext4_device")
    require(device == f"{os.major(info.st_dev)}:{os.minor(info.st_dev)}"
            and bool(fs.f_flag & os.ST_RDONLY) == read_only, "ext4_device_access")
    return {"mount_id": mount_id, "unique_mount_id": unique_mount_id(root), "device": info.st_dev,
            "fsid": fs.f_fsid, "worker_root_inode": info.st_ino}


def open_private(path, mode, maximum, device=None):
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    source = os.fdopen(fd, "rb")
    try:
        info = os.fstat(fd)
        require(info.st_mode == stat.S_IFREG | mode and info.st_uid == 0 and info.st_nlink == 1,
                "private_regular_file")
        require(0 < info.st_size <= maximum and (device is None or info.st_dev == device), "private_file_bounds")
        return source
    except BaseException:
        source.close()
        raise


def read_private(path, maximum):
    with open_private(path, 0o600, maximum) as source:
        data = source.read(maximum + 1)
        require(0 < len(data) <= maximum, "private_file_bounds")
        return data


def digest_private(path, mode, maximum, device=None):
    with open_private(path, mode, maximum, device) as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def publish(stage, name, value, maximum):
    data = canonical(value) + b"\n"
    require(len(data) <= maximum, "output_bounds")
    with tempfile.NamedTemporaryFile(dir=stage, prefix=".custody-", delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            os.link(temporary, stage / name, follow_symlinks=False)
        finally:
            temporary.unlink()
    fd = os.open(stage, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    return sha256(data)


def go_qualification(helpers, stage, worker, mode, temporary=None):
    run = (stage / EXECUTABLE, "-test.v", "-test.run=^TestDSGNativeSameMountCustody$", "-test.timeout=25s",
           "--", mode, worker)
    if mode == "dsg-produce-approval":
        run = ("unshare", "--pid", "--fork", "--kill-child", *run)
        envelope = b"DSG_APPROVAL_PRODUCED"
    else:
        envelope = b"DSG_APPROVAL_INSPECTED"
    result = helpers["command"](stage, run, timeout=30, temporary=temporary)
    if result.returncode:
        site = re.search(rb"dsg_(?:approval_custody|native_reclamation)_linux_test\.go:(\d{1,4}):", result.stdout)
        suffix = "_" + site[1].decode("ascii") if site else ""
        raise RuntimeError(mode.replace("-", "_") + suffix)
    require(0 < len(result.stdout) <= MAX_DOCUMENT, "probe_output_bounds")
    proofs = re.findall(rb"\b" + envelope + rb" (\{[^\r\n]+\})$", result.stdout, flags=re.M)
    require(len(proofs) == 1, "probe_proof")
    return json.loads(proofs[0])


def prove_origin(helpers, worker, produced):
    source, input_plan = produced["source"], produced["input"]
    request = input_plan["Plan"]["Request"]
    require(all(source[name] == request[name] for name in ("Project", "Worker", "RecordID", "Origin")), "source_selectors")
    require(all(re.fullmatch(r"[a-z0-9]{25}", source[name]) for name in ("Worker", "RecordID")), "source_exact_ids")
    require(all(DIGEST_PATTERN.fullmatch(source[name]) for name in ("Project", "Origin", "PlanSHA256")), "source_registration")
    generation = source["PayloadGeneration"]
    layer = source["LayerSHA256"]
    payload = source["PayloadSHA256"]
    require(all(DIGEST_PATTERN.fullmatch(value) for value in (generation, layer, payload)), "source_digests")
    expected = f"native deletion fixture {generation}/snapshot/target".encode("ascii")
    require(sha256(expected) == payload and sha256(b"manifestless-native-payload-v1\x00" + expected) == source["Origin"],
            "source_origin_digest")
    target = input_plan["Layout"]["Backing"]["TargetID"]
    require(re.fullmatch(r"[1-9][0-9]{0,2}", target), "source_snapshot_allocation")
    flat = worker / "fresh-native-deletion"
    filename = "dsg-owned-payload"
    device = worker.stat().st_dev
    require(helpers["read_generated_file"](flat / "snapshots/snapshots" / target / filename, device) == expected,
            "source_snapshot_payload")
    raw = helpers["read_generated_file"](flat / "content/blobs/sha256" / layer, device)
    require(sha256(raw) == layer, "source_layer_digest")
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:") as archive:
        entries = archive.getmembers()
        require(len(entries) == 1, "source_tar_entries")
        entry = entries[0]
        require(entry.isfile() and entry.name.removeprefix("./") == filename and entry.size == len(expected), "source_tar_file")
        with archive.extractfile(entry) as content:
            require(content.read(len(expected) + 1) == expected, "source_tar_payload")
    return {"layer_sha256": layer, "payload_sha256": payload, "source_origin_sha256": source["Origin"]}


def offline_digests(worker):
    flat = worker / "fresh-native-deletion"
    device = worker.stat().st_dev
    return {name: digest_private(flat / name, 0o600, 64 * 1024**2, device) for name in NATIVE_STORES}


def cross_check(keeper, mounted, authority):
    observed = authority["Identity"]
    require(observed["BootSHA256"] == keeper["boot_sha256"] and int(observed["MountNS"]) == keeper["mount_namespace"],
            "probe_namespace_identity")
    require(int(observed["MountID"]) == mounted["mount_id"] and int(observed["UniqueMountID"]) == mounted["unique_mount_id"]
            and int(observed["RootInode"]) == mounted["worker_root_inode"] and int(observed["Device"]) == mounted["device"],
            "probe_mount_identity")


def live(evidence):
    return (milliseconds(time.CLOCK_REALTIME) < evidence["expires_at_unix_ms"]
            and milliseconds(time.CLOCK_BOOTTIME) < evidence["expires_at_boottime_ms"])


def hold(args):
    stage = args.stage
    require(args.lifetime_seconds in range(30, MAX_LIFETIME_SECONDS + 1), "invalid_lifetime")
    require(DIGEST_PATTERN.fullmatch(args.runtime_sha256), "invalid_runtime_digest")
    require(stage.resolve(strict=True) == stage, "stage_path")
    private_mount(os.getpid(), stage, b"tmpfs", b"rw")
    helpers_path = stage / "check-buildkit-offline-recovery.py"
    with open_private(helpers_path, 0o400, MAX_DOCUMENT):
        helpers = runpy.run_path(str(helpers_path))
    helpers["require_private_boundary"](stage)
    require(digest_private(stage / EXECUTABLE, 0o500, 100 * 1024**2) == args.runtime_sha256, "runtime_digest")

    keeper = keeper_identity(os.getpid())
    ttl_ms = args.lifetime_seconds * 1000
    created_wall = milliseconds(time.CLOCK_REALTIME)
    created_boot = milliseconds(time.CLOCK_BOOTTIME)
    evidence = {"schema": "dsg-buildkit-approval-custody-v1", "purpose": args.purpose, "keeper": keeper,
                "runtime_sha256": args.runtime_sha256, "created_at_unix_ms": created_wall,
                "expires_at_unix_ms": created_wall + ttl_ms, "expires_at_boottime_ms": created_boot + ttl_ms}
    if args.operator_sha256 is not None:
        require(DIGEST_PATTERN.fullmatch(args.operator_sha256) and
                digest_private(stage / "dsg-buildkit-exact", 0o500, 100 * 1024**2) == args.operator_sha256,
                "operator_digest")
        evidence["operator_sha256"] = args.operator_sha256
    stopped = False

    def stop(_signum, _frame):
        nonlocal stopped
        stopped = True

    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, stop)

    # The open file is outside ext4. The keeper never pins an ext4 descriptor.
    with (stage / ".keeper.lock").open("xb") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with helpers["private_store"](stage, "candidate") as (mount, worker):
            created_mount = mounted_identity(stage, stage, os.getpid(), read_only=False)
            temporary = mount / "producer-tmp"
            temporary.mkdir(mode=0o700)
            producer_proc = stage / "producer-proc"
            producer_proc.mkdir(mode=0o700)
            try:
                produced = go_qualification(helpers, stage, worker, "dsg-produce-approval", temporary)
            finally:
                if producer_proc.is_mount():
                    private_mount(os.getpid(), producer_proc, b"proc", b"rw")
                    helpers["mount_command"](stage, "umount", producer_proc)
                producer_proc.rmdir()
            temporary.rmdir()
            helpers["mount_command"](stage, "mount", "-o", "remount,ro,nodev,nosuid", mount)
            mounted = mounted_identity(stage, stage, os.getpid())
            require(mounted == created_mount, "producer_mount_replaced")
            digest = publish(stage, SOURCE_NAME, produced["input"], 16 * 1024)
            databases = offline_digests(worker)
            authority = go_qualification(helpers, stage, worker, "dsg-inspect-approval")
            require(authority["Source"] == produced["source"], "probe_source_identity")
            require(authority["Record"]["DatabasesSHA256"] == [databases[name] for name in NATIVE_STORES],
                    "probe_database_hashes")
            cross_check(keeper, mounted, authority)
            provenance = prove_origin(helpers, worker, produced)
            require(go_qualification(helpers, stage, worker, "dsg-inspect-approval") == authority, "repeat_probe_changed")
            require(offline_digests(worker) == databases and mounted_identity(stage, stage, os.getpid()) == mounted,
                    "qualification_changed")
            evidence.update(authority=authority, mount=mounted, provenance=provenance, native_source_sha256=digest,
                            native_databases_sha256=databases, actual_cache_deletion_requests=0, prune_requests=0)
            require(live(evidence) and not stopped, "expired_during_qualification")
            evidence["evidence_sha256"] = sha256(canonical(evidence))
            publish(stage, EVIDENCE_NAME, evidence, MAX_DOCUMENT)
            print(json.dumps({"custody": "ready", "evidence_sha256": evidence["evidence_sha256"]}), flush=True)
            while not stopped and live(evidence):
                time.sleep(1)
    print(json.dumps({"custody": "closed", "reason": "signal" if stopped else "expired"}), flush=True)


def status(args):
    pid = args.keeper_pid
    require(pid > 1, "keeper_pid")
    before = keeper_identity(pid)
    visible_stage = Path(f"/proc/{pid}/root") / args.stage.relative_to("/")
    private_mount(pid, args.stage, b"tmpfs", b"rw")
    raw = read_private(visible_stage / EVIDENCE_NAME, MAX_DOCUMENT)
    evidence = json.loads(raw)
    digest = evidence.pop("evidence_sha256")
    require(digest == sha256(canonical(evidence)) and evidence["schema"] == "dsg-buildkit-approval-custody-v1",
            "custody_digest")
    require(evidence["keeper"] == before and live(evidence), "custody_expired_or_replaced")
    require(mounted_identity(args.stage, visible_stage, pid) == evidence["mount"], "custody_mount_changed")
    require(sha256(read_private(visible_stage / SOURCE_NAME, 16 * 1024)) == evidence["native_source_sha256"],
            "custody_source_changed")
    runtime = digest_private(visible_stage / EXECUTABLE, 0o500, 100 * 1024**2)
    require(runtime == evidence["runtime_sha256"], "custody_runtime_changed")
    if "operator_sha256" in evidence:
        require(digest_private(visible_stage / "dsg-buildkit-exact", 0o500, 100 * 1024**2)
                == evidence["operator_sha256"], "custody_operator_changed")
    require(keeper_identity(pid) == before and live(evidence), "custody_expired_or_replaced")
    require(canonical(dict(evidence, evidence_sha256=digest)) + b"\n" == raw, "custody_encoding")
    print(raw.decode("ascii"), end="", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    keep = commands.add_parser("hold")
    keep.add_argument("stage", type=stage_arg)
    keep.add_argument("runtime_sha256")
    keep.add_argument("--operator-sha256", help="digest of the separate fixed-name exact-record operator")
    keep.add_argument("--lifetime-seconds", type=int, default=1800)
    keep.add_argument("--purpose", choices=("approval", "rehearsal"), default="approval")
    inspect = commands.add_parser("status")
    inspect.add_argument("stage", type=stage_arg)
    inspect.add_argument("keeper_pid", type=int)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        require(sys.platform == "linux" and os.uname().machine == "aarch64" and os.geteuid() == 0 and not sys.flags.optimize,
                "linux_arm64_root_required")
        (hold if args.command == "hold" else status)(args)
    except (AssertionError, AttributeError, KeyError, OSError, RuntimeError, TypeError, ValueError,
            subprocess.SubprocessError, tarfile.TarError) as error:
        category = str(error) if re.fullmatch(r"[a-z_0-9]{1,80}", str(error)) else None
        print(json.dumps({"custody_error": type(error).__name__, "category": category,
                          "errno": getattr(error, "errno", None)}), flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
