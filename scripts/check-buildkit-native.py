"""Compare native Linux ARM64 BuildKit observations inside a disposable guest scope."""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import shlex
import signal
import socket
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
from contextlib import contextmanager


BUILDCTL_SHA256 = "cb57dfdc462d2d2ea1ca2f6b81b801520582ba536513228eb0e5213244626814"
BUILDKITD_SHA256 = "b26e07e38e071c29896830376bcf6c939781f4dcf3d90eff10208b5ed4436566"
MAX_BINARY_BYTES = 128 * 1024 * 1024
MAX_OUTPUT_BYTES = 64 * 1024
SYSTEM_PATH = "/usr/sbin:/usr/bin:/sbin:/bin"
CACHE_ID = re.compile(r"[a-z0-9]{25}")
OCI_MANIFEST = "application/vnd.oci.image.manifest.v1+json"
OCI_LAYER = "application/vnd.oci.image.layer.v1.tar"
OWNED_FILE = "dsg-owned-payload"


class CheckFailed(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise CheckFailed(reason)


def preflight(arguments):
    require(len(arguments) == 4, "arguments")
    require(sys.platform == "linux" and os.uname().machine == "aarch64", "platform")
    require(os.geteuid() == 0, "guest_uid")
    workdir, dsg, daemon, client = map(Path, arguments)
    require(workdir.is_absolute() and workdir.resolve(strict=True) == workdir, "workdir_path")
    info = workdir.stat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and stat.S_IMODE(info.st_mode) == 0o700,
            "workdir_permissions")
    for path in (dsg, daemon, client):
        require(path.is_relative_to(workdir) and path.resolve(strict=True) == path, "binary_path")
        info = path.stat()
        require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022
                and info.st_mode & 0o111 and 0 < info.st_size <= MAX_BINARY_BYTES, "binary_permissions")
    require(len({dsg, daemon, client}) == 3 and client.name == "buildctl", "binary_names")

    interfaces = {
        line.split(":", 1)[0].strip()
        for line in Path("/proc/net/dev").read_text(encoding="ascii").splitlines()
        if ":" in line
    }
    ipv4_routes = Path("/proc/net/route").read_text(encoding="ascii").splitlines()[1:]
    ipv6_routes = Path("/proc/net/ipv6_route").read_text(encoding="ascii").splitlines()
    require(interfaces == {"lo"} and all(line.split()[0] == "lo" for line in ipv4_routes)
            and all(line.split()[-1] == "lo" for line in ipv6_routes), "network_isolation")
    with Path("/proc/self/mountinfo").open(encoding="ascii") as mountinfo:
        root_mount = next((line.partition(" - ")[0].split()[6:]
                           for line in mountinfo if line.split()[4] == "/"), None)
    require(root_mount == [], "mount_isolation")

    for path, expected, step in ((daemon, BUILDKITD_SHA256, "official_daemon_hash"),
                                 (client, BUILDCTL_SHA256, "official_client_hash")):
        with path.open("rb") as binary:
            require(hashlib.file_digest(binary, "sha256").hexdigest() == expected, step)
    return workdir, dsg, daemon, client


@contextmanager
def process(argv, directory, environment, output, *, stdin=subprocess.DEVNULL,
            error_output=subprocess.DEVNULL):
    child = subprocess.Popen(argv, cwd=directory, env=environment, start_new_session=True,
                             stdin=stdin, stdout=output, stderr=error_output)
    try:
        yield child
    finally:
        try:
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                    child.wait(timeout=3)
                except ProcessLookupError:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=2)
        finally:
            if child.stdout is not None:
                child.stdout.close()


def command(directory, environment, step, argv, *, timeout=8, exit_code=0, capture=True,
            stdin=subprocess.DEVNULL):
    output = subprocess.PIPE if capture else subprocess.DEVNULL
    with process(argv, directory, environment, output, stdin=stdin) as child:
        try:
            stdout, _ = child.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise CheckFailed(step + "_timeout") from None
        require(child.returncode == exit_code, step + "_exit")
    if capture:
        require(len(stdout) <= MAX_OUTPUT_BYTES, step + "_output")
    return stdout


def document(raw, step):
    try:
        return json.loads(raw)
    except (UnicodeError, ValueError):
        raise CheckFailed(step + "_json") from None


def official_inventory(raw):
    records = document(raw, "official_du")
    if records is None:  # The Go template serializes an empty nil slice as null.
        return {}
    require(isinstance(records, list), "official_du_shape")
    inventory = {}
    for record in records:
        require(isinstance(record, dict), "official_du_record")
        identifier = record.get("id")
        require(isinstance(identifier, str) and CACHE_ID.fullmatch(identifier)
                and identifier not in inventory, "official_du_id")
        require(all(type(record.get(key)) is bool for key in ("inUse", "shared", "mutable"))
                and isinstance(record.get("recordType"), str) and record["recordType"]
                and type(record.get("size")) is int and record["size"] >= 0
                and type(record.get("usageCount")) is int and record["usageCount"] >= 0
                and isinstance(record.get("createdAt"), str) and "lastUsedAt" in record
                and (record["lastUsedAt"] is None or isinstance(record["lastUsedAt"], str)),
                "official_du_fields")
        require("parents" in record, "official_du_parents")
        parents = [] if record["parents"] is None else record["parents"]
        require(isinstance(parents, list)
                and all(isinstance(parent, str) and CACHE_ID.fullmatch(parent) for parent in parents),
                "official_du_parents")
        inventory[identifier] = {key: record[key] for key in (
            "inUse", "shared", "mutable", "recordType", "size", "usageCount", "createdAt", "lastUsedAt"
        )}
        inventory[identifier]["parents"] = sorted(parents)
    return inventory


def official_counts(inventory):
    counts = dict(total=len(inventory), reclaimable=0, shared=0, mutable=0, candidate_count=0)
    reasons = dict(active=0, shared=0, mutable=0, unknown_ownership=0)
    for record in inventory.values():
        in_use, shared, mutable = flags = [record[key] for key in ("inUse", "shared", "mutable")]
        counts["reclaimable"] += int(not in_use)
        counts["shared"] += int(shared)
        counts["mutable"] += int(mutable)
        reason = next((name for name, enabled in zip(("active", "shared", "mutable"), flags)
                       if enabled), "unknown_ownership")
        reasons[reason] += 1
    counts["preserved_by_reason"] = reasons
    return counts


def socket_generation(path, pid):
    directory = path.parent.stat()
    node = path.lstat()
    require(directory.st_uid == 0 and stat.S_IMODE(directory.st_mode) == 0o700
            and stat.S_ISSOCK(node.st_mode) and node.st_uid == 0, "fixture_socket_identity")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as peer:
        peer.settimeout(2)
        peer.connect(str(path))
        peer_pid, peer_uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    remaining_fields = Path(f"/proc/{pid}/stat").read_text(encoding="ascii").rsplit(")", 1)[1].split()
    require(peer_pid == pid and peer_uid == 0 and len(remaining_fields) >= 20
            and remaining_fields[19].isdecimal(), "fixture_peer_identity")
    current = path.lstat()
    require((node.st_dev, node.st_ino) == (current.st_dev, current.st_ino), "fixture_socket_changed")
    return {"pid": pid, "start_ticks": int(remaining_fields[19]), "socket_device": node.st_dev,
            "socket_inode": node.st_ino}


def varint(value):
    encoded = bytearray()
    value &= (1 << 64) - 1
    while value >= 128:
        encoded.append((value & 127) | 128)
        value >>= 7
    encoded.append(value)
    return bytes(encoded)


def protobuf_bytes(field, data):
    return varint((field << 3) | 2) + varint(len(data)) + data


def protobuf_integer(field, value):
    return varint(field << 3) + varint(value)


def scratch_definition(payload):
    # v0.35.1 solver/pb/ops.proto: one FileActionMkFile without primary or secondary inputs.
    mkfile = (protobuf_bytes(1, ("/" + OWNED_FILE).encode("ascii"))
              + protobuf_integer(2, 0o644) + protobuf_bytes(3, payload))
    action = protobuf_integer(1, -1) + protobuf_integer(2, -1) + protobuf_bytes(5, mkfile)
    operation = protobuf_bytes(4, protobuf_bytes(2, action))
    # client/llb/state.go: the last Op selects output index zero of the FileOp by digest.
    digest = ("sha256:" + hashlib.sha256(operation).hexdigest()).encode("ascii")
    output = protobuf_bytes(1, protobuf_bytes(1, digest))
    return protobuf_bytes(1, operation) + protobuf_bytes(1, output)


def archive_bytes(archive, name):
    members = [member for member in archive.getmembers() if member.name.removeprefix("./") == name]
    require(len(members) == 1 and members[0].isfile() and members[0].size <= MAX_OUTPUT_BYTES,
            "owned_archive_member")
    with archive.extractfile(members[0]) as file:
        return file.read(MAX_OUTPUT_BYTES + 1)


def oci_blob(archive, descriptors, media_type):
    require(isinstance(descriptors, list) and len(descriptors) == 1 and isinstance(descriptors[0], dict),
            "owned_oci_descriptor")
    descriptor = descriptors[0]
    digest = descriptor.get("digest")
    size = descriptor.get("size")
    require(descriptor.get("mediaType") == media_type and isinstance(digest, str)
            and re.fullmatch(r"sha256:[a-f0-9]{64}", digest) and type(size) is int
            and 0 < size <= MAX_OUTPUT_BYTES, "owned_oci_blob")
    data = archive_bytes(archive, "blobs/sha256/" + digest[7:])
    require(len(data) == size and hashlib.sha256(data).hexdigest() == digest[7:], "owned_oci_integrity")
    return data


def object_document(raw, step):
    data = document(raw, step)
    require(isinstance(data, dict), step + "_shape")
    return data


def verify_owned_export(path, payload):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_OUTPUT_BYTES, "owned_archive_size")
    try:
        with tarfile.open(path, mode="r:") as archive:
            index = object_document(archive_bytes(archive, "index.json"), "owned_oci_index")
            manifest = object_document(oci_blob(archive, index.get("manifests"), OCI_MANIFEST),
                                       "owned_oci_manifest")
            layer = oci_blob(archive, manifest.get("layers"), OCI_LAYER)
        with tarfile.open(fileobj=io.BytesIO(layer), mode="r:") as archive:
            require(archive_bytes(archive, OWNED_FILE) == payload, "owned_oci_payload")
    except (tarfile.TarError, EOFError):
        raise CheckFailed("owned_archive_invalid") from None
    return hashlib.sha256(layer).hexdigest()


def single_worker(raw):
    workers = document(raw, "official_workers")
    require(isinstance(workers, list) and len(workers) == 1 and isinstance(workers[0], dict),
            "fixture_one_worker")
    identifier = workers[0].get("id")
    require(isinstance(identifier, str) and CACHE_ID.fullmatch(identifier), "fixture_worker_id")
    require("gcPolicy" in workers[0] and workers[0]["gcPolicy"] in (None, []), "fixture_worker_gc_enabled")
    return identifier


def owned_record_rejection(baseline, current):
    if any(current.get(identifier) != record for identifier, record in baseline.items()):
        return "baseline_changed"
    added = current.keys() - baseline.keys()
    if len(added) != 1:
        return "producer_did_not_add_exactly_one_visible_id"
    identifier = next(iter(added))
    record = current[identifier]
    if record["inUse"]:
        return "record_in_use"
    if record["shared"]:
        return "record_shared"
    if record["mutable"]:
        return "record_mutable"
    if record["recordType"] != "regular":
        return "record_not_regular"
    if record["parents"] or any(identifier in item["parents"] for item in current.values()):
        return "record_has_dependency"
    if record["size"] == 0:
        return "record_size_unproven"
    return None


def product_result(raw):
    response = document(raw, "product")
    require(isinstance(response, dict) and response.get("schema_version") == 1
            and response.get("command") == "buildkit-local-cache"
            and isinstance(response.get("result"), dict), "product_envelope")
    return response["result"]


def await_socket(daemon, path, diagnostics, evidence):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if daemon.poll() is not None:
            diagnostics.seek(0)
            output = diagnostics.read(MAX_OUTPUT_BYTES).lower()
            markers = (b"read-only file system", b"permission denied", b"operation not permitted",
                       b"invalid network mode", b"no worker found", b"no such file or directory",
                       b"toml:", b"not a mountpoint", b"exec format error", b"unknown flag",
                       b"address already in use", b"mode must be", b"not mounted")
            evidence["daemon_exit"] = daemon.returncode
            evidence["daemon_error_markers"] = {
                index + 1: output.count(marker) for index, marker in enumerate(markers) if marker in output
            }
            raise CheckFailed("daemon_early_exit")
        if path.is_socket():
            return
        time.sleep(0.1)
    raise CheckFailed("daemon_socket_timeout")


def observe(arguments, evidence):
    workdir, dsg, buildkitd, buildctl = preflight(arguments)
    with tempfile.TemporaryDirectory(prefix="bk-", dir=workdir) as temporary:
        directory = Path(temporary)
        path = directory / "b.sock"
        require(len(os.fsencode(path)) < 108, "socket_path_length")
        environment = {"PATH": SYSTEM_PATH, "LANG": "C", "HOME": temporary,
                       "TMPDIR": temporary, "XDG_RUNTIME_DIR": temporary}
        version = command(directory, environment, "daemon_version", (buildkitd, "--version"))
        require(b"v0.35.1" in version.split(), "daemon_release")

        source = directory / "context"
        source.mkdir(mode=0o700)
        (source / "Dockerfile").write_text("FROM scratch\nCOPY payload /payload\n", encoding="ascii")
        payload = b"DSG BuildKit native fixture\n"
        (source / "payload").write_bytes(payload)
        config = directory / "buildkitd.toml"
        config.write_text(
            '[worker.oci]\nenabled = true\nsnapshotter = "native"\nrootless = false\n'
            'noProcessSandbox = false\ngc = false\nmax-parallelism = 1\nnetworkMode = "host"\n'
            '[worker.containerd]\nenabled = false\n[cdi]\ndisabled = true\n'
            '[history]\nmaxEntries = 0\n',
            encoding="ascii",
        )
        addr = "unix://" + str(path)
        ctl = (buildctl, "--addr", addr, "--timeout", "15")
        daemon_args = (buildkitd, "--config", config, "--root", directory / "store", "--addr", addr,
                       "--otel-socket-path", directory / "trace.sock")
        with (tempfile.TemporaryFile(dir=directory) as diagnostics,
              process(daemon_args, directory, environment, subprocess.DEVNULL,
                      error_output=diagnostics) as daemon):
            evidence["daemon_started"] = True
            await_socket(daemon, path, diagnostics, evidence)

            def official(step, *options):
                return official_inventory(command(directory, environment, step,
                                                  (*ctl, "du", "--format", "{{json .}}", *options),
                                                  timeout=17))

            def worker(step):
                return single_worker(command(directory, environment, step,
                                             (*ctl, "debug", "workers", "--format", "{{json .}}"), timeout=17))

            def product(step, *, socket=path, pid=daemon.pid, client=buildctl, exit_code=0):
                argv = (dsg, "--json", "buildkit-local-cache", "--socket", socket,
                        "--expected-pid", str(pid), "--buildctl", client)
                return product_result(command(directory, environment, step, argv, timeout=22,
                                              exit_code=exit_code))

            def compare(step, counts):
                require(product(step) == {"status": "complete", "counts": counts,
                                          "disposition": "preserve", "reason": "unknown_ownership"},
                        step + "_mismatch")

            generation = socket_generation(path, daemon.pid)
            worker_id = worker("official_worker_before")
            before = official_counts(official("official_before"))
            require(before["total"] == 0, "initial_cache_not_empty")
            compare("product_before", before)
            evidence["before"] = before

            output = directory / "output"
            evidence["build_attempted"] = True
            command(directory, environment, "fixture_build",
                    (*ctl, "build", "--frontend", "dockerfile.v0", "--local", f"context={source}",
                     "--local", f"dockerfile={source}", "--output", f"type=local,dest={output}",
                     "--progress", "plain"), timeout=25, capture=False)
            exported = output / "payload"
            require(exported.stat().st_size == len(payload) and exported.read_bytes() == payload, "fixture_output")
            evidence["build_completed"] = True
            after = official_counts(official("official_after"))
            require(after["total"] > 0, "cache_still_empty")
            compare("product_after", after)
            require(official_counts(official("official_recheck")) == after, "cache_changed_during_observation")
            evidence["after"] = after

            wrong_client_dir = directory / "wrong-client"
            wrong_client_dir.mkdir(mode=0o700)
            marker = directory / "wrong-client-executed"
            wrong_client = wrong_client_dir / "buildctl"
            wrong_client.write_text(f"#!/bin/sh\n: > {shlex.quote(str(marker))}\n", encoding="utf-8")
            wrong_client.chmod(0o700)
            symlink = directory / "socket-link"
            symlink.symlink_to(path)
            rejected = {"status": "unknown", "counts": None, "disposition": "preserve",
                        "reason": "incomplete_inventory", "failure": "invalid_selector"}
            for step, overrides in (("wrong_pid", {"pid": os.getpid()}),
                                    ("wrong_client_hash", {"client": wrong_client}),
                                    ("socket_symlink", {"socket": symlink})):
                require(product(step, exit_code=5, **overrides) == rejected, step + "_mismatch")
                evidence["rejected"].append(step)
            require(not marker.exists(), "wrong_client_executed")
            baseline = official("official_final")
            require(official_counts(baseline) == after, "cache_changed_after_rejections")

            owned = evidence["owned_fixture"]
            owned.update({"scope": "disposable_single_worker", "worker_id": worker_id,
                          "daemon_generation": generation, "before_ids": sorted(baseline)})
            payload = b"DSG synthetic project cache " + os.urandom(16).hex().encode("ascii") + b"\n"
            definition = scratch_definition(payload)
            llb = directory / "owned.llb"
            llb.write_bytes(definition)
            image = directory / "owned.oci.tar"
            owned["producer_attempted"] = True
            with llb.open("rb") as definition_file:
                command(directory, environment, "owned_producer",
                        (*ctl, "build", "--output", f"type=oci,dest={image},compression=uncompressed",
                         "--progress", "plain"), timeout=25, capture=False, stdin=definition_file)
            owned["layer_sha256"] = verify_owned_export(image, payload)
            owned["definition_sha256"] = hashlib.sha256(definition).hexdigest()
            owned["producer_completed"] = True

            current = official("official_owned_after")
            compare("product_owned_after", official_counts(current))
            stable = current == official("official_owned_recheck")
            require(worker("official_worker_after") == worker_id and socket_generation(path, daemon.pid) == generation,
                    "fixture_daemon_generation_changed")
            owned["after_ids"] = sorted(current)
            owned["full_inventory_stable"] = stable
            reason = owned_record_rejection(baseline, current) if stable else "inventory_changed_during_observation"
            if reason is None:
                identifier = next(iter(current.keys() - baseline.keys()))
                # ParseAll in v0.35.1 ORs separate filter strings. One exact ID is the entire filter.
                selector = f"id=={identifier}"
                filtered = official("official_owned_exact_id", "--filter", selector)
                final = official("official_owned_final")
                require(socket_generation(path, daemon.pid) == generation, "fixture_daemon_generation_changed")
                if filtered != {identifier: current[identifier]} or final != current:
                    reason = "exact_id_readback_mismatch"
                else:
                    owned.update({"ownership_proof": "verified", "exact_id_selector": selector,
                                  "record": {"id": identifier, **current[identifier]}})
                    # control.Prune cancels image cache leases and runs unfiltered solver metadata cleanup.
                    reason = "stock_prune_global_cache_effects"
            owned["reason"] = reason
        evidence["daemon_stopped"] = daemon.returncode is not None
    evidence["fixture_removed"] = not directory.exists()


def interrupt(number, _frame):
    raise CheckFailed("runner_deadline" if number == signal.SIGALRM else "interrupted")


def main():
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    signal.signal(signal.SIGALRM, interrupt)
    os.umask(0o077)
    evidence = {"status": "failed", "daemon_started": False, "build_attempted": False,
                "build_completed": False, "rejected": [], "prune_requests": 0,
                "owned_fixture": {"producer_attempted": False, "producer_completed": False,
                                  "ownership_proof": "unverified", "disposition": "preserve",
                                  "reason": "fixture_incomplete"}}
    try:
        signal.alarm(120)
        observe(sys.argv[1:], evidence)
        evidence["status"] = "passed"
    except CheckFailed as error:
        evidence["failure"] = str(error)
    except subprocess.TimeoutExpired:
        evidence["failure"] = "shutdown_timeout"
    except (OSError, RuntimeError, ValueError):
        evidence["failure"] = "fixture_system_error"
    finally:
        signal.alarm(0)
    print(json.dumps(evidence, sort_keys=True, separators=(",", ":")))
    return int(evidence["status"] != "passed")


if __name__ == "__main__":
    sys.exit(main())
