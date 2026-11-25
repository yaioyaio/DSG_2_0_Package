#!/usr/bin/env python3
"""Verify an issued project record against a stopped dedicated BuildKit worker."""

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile


DIGEST = re.compile(r"[0-9a-f]{64}")
FULL_ID = re.compile(r"[0-9a-z]{25}")
REVISION = re.compile(r"[0-9a-f]{40}")
STAGE = re.compile(r"/run/dsg-project-enrollment-[0-9a-f]{12,64}/stage")
MAX_DOCUMENT = 32 * 1024
# This is the configuration emitted by the dedicated worker qualification.
WORKER_CONFIG = (
    b'[worker.oci]\nenabled = true\nsnapshotter = "native"\nrootless = false\n'
    b'noProcessSandbox = false\ngc = false\nmax-parallelism = 1\nnetworkMode = "host"\n'
    b'[worker.containerd]\nenabled = false\n[cdi]\ndisabled = true\n[history]\nmaxEntries = 0\n'
)
DATABASES = {
    "solver_tx": "cache.db",
    "cache_tx": "runc-native/metadata_v2.db",
    "containerd_tx": "runc-native/containerdmeta.db",
    "snapshot_tx": "runc-native/snapshots/metadata.db",
    "history_tx": "history.db",
}
OPTIONAL_DATABASES = {"snapshot_tx", "history_tx"}
RECORD_FLAGS = {"record_present", "snapshot_binding", "mutable_alias",
                "blob_binding", "metadata_only_candidate"}
QUIESCENCE_FLAGS = {"filesystem_read_only", "executor_namespace_gone",
                    "mounts_clear", "external_handles_clear"}
PRODUCTION_FLAGS = {"production_deployment_verified", "exact_mutation_authorized", "receipt_storage_verified"}
CANDIDATE_FIELDS = {"id", "worker_id", "offline_record", "project_registration", "disposition",
                    "source", "before_count", "after_count", "daemon_generation",
                    "llb_sha256", "verified_layer_sha256", "evidence_sha256"}


class RegistrationError(Exception):
    pass


@dataclass(frozen=True)
class StoreSnapshot:
    directory_identities: tuple
    dedicated_marker: tuple
    namespace_marker: tuple | None
    worker_id_file: tuple
    databases: dict


def require(condition, category):
    if not condition:
        raise RegistrationError(category)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(document):
    return json.dumps(document, sort_keys=True, separators=(",", ":"), ensure_ascii=True,
                      allow_nan=False).encode("ascii")


def exact(document, keys):
    return type(document) is dict and document.keys() == keys


def matches(pattern, value):
    return type(value) is str and pattern.fullmatch(value) is not None


def positive(value):
    return type(value) is int and 0 < value < 2**64


def decode(data):
    def unique(pairs):
        document = dict(pairs)
        require(len(document) == len(pairs), "duplicate_json_key")
        return document

    def reject_constant(_):
        raise RegistrationError("invalid_json_constant")

    require(0 < len(data) <= MAX_DOCUMENT, "document_size")
    return json.loads(data.decode("ascii"), object_pairs_hook=unique, parse_constant=reject_constant)


def validate_record(record, record_id):
    require(exact(record, RECORD_FLAGS | {"id", "bindings_sha256"}), "record_schema")
    require(record["id"] == record_id and matches(DIGEST, record["bindings_sha256"]), "record_binding")
    require(all(type(record[key]) is bool for key in RECORD_FLAGS)
            and record["record_present"] and record["snapshot_binding"]
            and not record["metadata_only_candidate"], "record_disposition")


def validate_issuance(data, seal):
    require(matches(DIGEST, seal) and digest(data) == seal, "external_issuance_seal")
    issued = decode(data)
    require(exact(issued, CANDIDATE_FIELDS), "issuance_schema")
    unsigned = {key: value for key, value in issued.items() if key != "evidence_sha256"}
    require(issued["evidence_sha256"] == digest(canonical(unsigned)), "issuance_evidence_digest")
    require(all(matches(FULL_ID, issued[key]) for key in ("id", "worker_id")), "issuance_full_ids")
    require(all(matches(DIGEST, issued[key]) for key in ("llb_sha256", "verified_layer_sha256")),
            "issuance_source_digest")
    require(issued["source"] == "isolated scratch LLB and independently verified OCI layer"
            and issued["disposition"] == "preserve"
            and type(issued["before_count"]) is int and issued["before_count"] == 0
            and type(issued["after_count"]) is int and issued["after_count"] == 1, "issuance_source")
    generation = issued["daemon_generation"]
    require(exact(generation, {"pid", "socket_device", "socket_inode", "start_ticks"})
            and all(positive(value) for value in generation.values()), "issuance_generation")
    validate_record(issued["offline_record"], issued["id"])
    project = issued["project_registration"]
    require(exact(project, PRODUCTION_FLAGS | {"evidence_scope", "project_id", "project_revision",
                                              "project_source_name", "project_source_sha256",
                                              "project_source_bytes"}), "project_schema")
    require(project["evidence_scope"] == "isolated_project_source" and project["project_id"] == "dsg"
            and project["project_source_name"] == "README.md"
            and matches(REVISION, project["project_revision"])
            and matches(DIGEST, project["project_source_sha256"])
            and positive(project["project_source_bytes"]) and project["project_source_bytes"] <= 16 * 1024
            and all(project[key] is False for key in PRODUCTION_FLAGS), "project_registration")
    return issued


def validate_observation(data, issued, database_fields, strict):
    observation = decode(data)
    fields = {*DATABASES, "worker", "record", "graph"}
    if "history_tx" not in database_fields:
        fields.remove("history_tx")
    if strict:
        fields.add("quiescence")
    require(exact(observation, fields) and type(observation["graph"]) is dict, "offline_schema")
    require(observation["worker"] == issued["worker_id"], "offline_worker")
    require(all(positive(observation[field]) for field in database_fields)
            and ("snapshot_tx" in database_fields or observation["snapshot_tx"] is None),
            "offline_transactions")
    record = observation["record"]
    validate_record(record, issued["id"])
    require(record == issued["offline_record"], "offline_binding_changed")
    if strict:
        quiescence = observation["quiescence"]
        require(exact(quiescence, QUIESCENCE_FLAGS) and all(value is True for value in quiescence.values()),
                "offline_quiescence")


def identity(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_gid, info.st_mode, info.st_nlink,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns)


@contextmanager
def directory(parent, name, private=False):
    fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
    try:
        info = os.fstat(fd)
        require(info.st_uid == 0 and not info.st_mode & 0o7022, "directory_owner")
        if private:
            require(info.st_mode == stat.S_IFDIR | 0o700 and info.st_gid == 0, "private_directory")
        yield fd, info
    finally:
        os.close(fd)


@contextmanager
def private_file(parent, name, device, minimum, maximum, mode=None):
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK | os.O_NOATIME
    fd = os.open(name, flags, dir_fd=parent)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and not info.st_mode & 0o7022 and info.st_uid == 0
                and info.st_nlink == 1 and info.st_dev == device
                and minimum <= info.st_size <= maximum, "protected_file")
        if mode is not None:
            require(info.st_mode == stat.S_IFREG | mode and info.st_gid == 0, "protected_file_mode")
        yield source, info
        current = os.stat(name, dir_fd=parent, follow_symlinks=False)
        require(identity(current) == identity(os.fstat(fd)) == identity(info), "protected_file_changed")


def fingerprint(parent, name, device, minimum, maximum, mode=None):
    with private_file(parent, name, device, minimum, maximum, mode) as (source, info):
        return identity(info), hashlib.file_digest(source, "sha256").hexdigest()


def read_stage_file(parent, name, device):
    with private_file(parent, name, device, 1, MAX_DOCUMENT, 0o400) as (source, info):
        data = source.read(MAX_DOCUMENT + 1)
        require(len(data) == info.st_size, "stage_input_size")
        return identity(info), data


def absent(parent, name):
    try:
        os.stat(name, dir_fd=parent, follow_symlinks=False)
    except FileNotFoundError:
        return True
    return False


def store_snapshot(stage, stage_info, strict):
    require(absent(stage, "buildkitd.sock") and absent(stage, "trace.sock"), "live_socket")
    with directory(stage, "store", private=True) as (store, store_info):
        # The patched inspection holds this same flock while accessing Bolt.
        # Release this probe lock before invoking it and reacquire afterward.
        fcntl.flock(store, fcntl.LOCK_EX | fcntl.LOCK_NB)
        device = store_info.st_dev
        if not strict:
            require(device == stage_info.st_dev, "dedicated_store_device")
        marker = fingerprint(store, ".dsg-dedicated-worker-v1", device, 0, 0, 0o600)
        namespace = ".dsg-pid-namespace-v1"
        if strict:
            ns_marker = fingerprint(store, namespace, device, 1, 4096, 0o600)
            require(os.fstatvfs(store).f_flag & os.ST_RDONLY, "strict_store_read_only")
        else:
            require(absent(store, namespace), "strict_enrollment_requires_strict_inspection")
            ns_marker = None
        with directory(store, "runc-native") as (worker, worker_info):
            with directory(worker, "snapshots") as (snapshots, snapshot_info):
                require(worker_info.st_dev == snapshot_info.st_dev == device, "native_store_device")
                worker_id = fingerprint(worker, "workerid", device, 25, 25)
                database_folders = dict(zip(DATABASES, (store, worker, worker, snapshots, store), strict=True))
                databases = {}
                for field, path in DATABASES.items():
                    parent, name = database_folders[field], path.rsplit("/", 1)[-1]
                    if field not in OPTIONAL_DATABASES or not absent(parent, name):
                        databases[field] = fingerprint(parent, name, device, 8192, 128 << 20)
                return StoreSnapshot(
                    (identity(store_info), identity(worker_info), identity(snapshot_info)),
                    marker, ns_marker, worker_id, databases,
                )


def check_boundary(workspace):
    require(sys.platform == "linux" and os.uname().machine == "aarch64"
            and os.getuid() == os.geteuid() == 0, "linux_arm64_root")
    require(STAGE.fullmatch(str(workspace)) and workspace == workspace.resolve(strict=True)
            and workspace.is_mount(), "private_stage_mount")
    for kind in ("net", "mnt"):
        require(os.readlink(f"/proc/self/ns/{kind}") != os.readlink(f"/proc/1/ns/{kind}"),
                "private_namespaces")
    interfaces = {line.split(":", 1)[0].strip() for line in Path("/proc/net/dev").read_text().splitlines()
                  if ":" in line}
    require(interfaces == {"lo"} and not Path("/run/docker.sock").is_socket()
            and not Path("/run/containerd/containerd.sock").is_socket(), "private_connectivity")
    with Path("/proc/self/mountinfo").open("rb") as source:
        mountinfo = source.read((1 << 20) + 1)
    require(len(mountinfo) <= 1 << 20, "mountinfo_size")
    mounts = []
    for line in mountinfo.splitlines():
        left, _, right = line.partition(b" - ")
        fields, backing = left.split(), right.split()
        if len(fields) >= 6 and fields[4] == os.fsencode(workspace):
            mounts.append((fields, backing))
    require(len(mounts) == 1, "private_stage_mount")
    fields, backing = mounts[0]
    require(len(fields) == 6 and fields[3] == b"/" and len(backing) == 3 and backing[0] == b"tmpfs"
            and {b"nodev", b"nosuid"} <= set(fields[5].split(b",")), "private_stage_mount")


def inspect(workspace, binary_fd, config_fd, record, strict):
    args = [f"/proc/self/fd/{binary_fd}", "--dsg-inspect-dedicated-worker",
            "--config", f"/proc/self/fd/{config_fd}", "--root", str(workspace / "store"),
            "--dsg-record-id", record]
    if strict:
        args.append("--dsg-require-quiescence")
    # BuildKit's process initialization may allocate runtime files even when
    # the selected command only reads Bolt. Keep those files out of the seal.
    with tempfile.TemporaryDirectory(prefix="dsg-readonly-inspect-", dir="/tmp") as runtime:
        environment = {"TMPDIR": runtime, "XDG_RUNTIME_DIR": runtime,
                       "PATH": "/usr/bin:/bin", "LANG": "C", "GOMAXPROCS": "1"}
        result = subprocess.run(args, cwd=runtime, env=environment, pass_fds=(binary_fd, config_fd),
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                check=False, timeout=12)
    require(result.returncode == 0, "offline_inspection_denied")
    return result.stdout


def qualify(workspace, daemon_sha256, issuance_sha256, strict=False):
    check_boundary(workspace)
    require(matches(DIGEST, daemon_sha256), "daemon_pin")
    with directory(None, workspace, private=True) as (stage, stage_info):
        device = stage_info.st_dev
        issuance_info, raw = read_stage_file(stage, "project-issuance.json", device)
        issued = validate_issuance(raw, issuance_sha256)
        with private_file(stage, "buildkitd", device, 1, 100 << 20, 0o500) as (binary, _):
            require(hashlib.file_digest(binary, "sha256").hexdigest() == daemon_sha256, "daemon_pin")
            with private_file(stage, "buildkitd.toml", device, 1, 4096, 0o600) as (config, _):
                require(config.read(4097) == WORKER_CONFIG, "dedicated_worker_config")
                before = store_snapshot(stage, stage_info, strict)
                require(before.worker_id_file[1] == digest(issued["worker_id"].encode("ascii")), "store_worker")
                try:
                    output = inspect(workspace, binary.fileno(), config.fileno(), issued["id"], strict)
                finally:
                    require(store_snapshot(stage, stage_info, strict) == before, "store_changed")
                    require(read_stage_file(stage, "project-issuance.json", device) == (issuance_info, raw),
                            "issuance_changed")
                    require(identity(stage_info) == identity(workspace.lstat()), "stage_changed")
                validate_observation(output, issued, before.databases, strict)
    result = {
        "schema": "dsg-buildkit-project-read-only-registration-v1",
        "project_registration": issued["project_registration"],
        "project_ownership_verified_read_only": True,
        "worker_id": issued["worker_id"],
        "record_id": issued["id"],
        "bindings_sha256": issued["offline_record"]["bindings_sha256"],
        "issuance_sha256": issuance_sha256,
        "issuance_evidence_sha256": issued["evidence_sha256"],
        "buildkitd_sha256": daemon_sha256,
        "worker_database_sha256": {DATABASES[field]: database[1] for field, database in before.databases.items()},
        "store_identity_and_database_bytes_unchanged": True,
        "qualification_inputs_unchanged": True,
        "offline_boundary": "strict_quiescence" if strict else "dedicated_worker_flock",
        "strict_quiescence_verified": strict,
        "disposition": "preserve",
        "daemon_started": False,
        "actual_cache_deletion_requests": 0,
        "prune_requests": 0,
        **dict.fromkeys(PRODUCTION_FLAGS, False),
        "production_operating_accounts_verified": False,
        "production_exclusive_mutation_verified": False,
    }
    return {**result, "evidence_sha256": digest(canonical(result))}


class SafeArguments(argparse.ArgumentParser):
    def error(self, message):
        raise RegistrationError("arguments")


def main():
    parser = SafeArguments(description=__doc__, allow_abbrev=False)
    parser.add_argument("workspace", type=Path)
    parser.add_argument("daemon_sha256")
    parser.add_argument("issuance_sha256")
    parser.add_argument("--require-quiescence", action="store_true")
    try:
        args = parser.parse_args()
        result = qualify(args.workspace, args.daemon_sha256, args.issuance_sha256, args.require_quiescence)
    except RegistrationError as error:
        print(json.dumps({"registration_error": str(error)}), flush=True)
        return 1
    except (OSError, ValueError, RecursionError, subprocess.SubprocessError) as error:
        print(json.dumps({"registration_error": type(error).__name__, "errno": getattr(error, "errno", None)}),
              flush=True)
        return 1
    print(json.dumps(result, sort_keys=True), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
