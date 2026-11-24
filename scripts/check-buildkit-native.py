"""Compare native Linux ARM64 BuildKit observations inside a disposable guest scope."""

import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import stat
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager


BUILDCTL_SHA256 = "cb57dfdc462d2d2ea1ca2f6b81b801520582ba536513228eb0e5213244626814"
MAX_BINARY_BYTES = 128 * 1024 * 1024
MAX_OUTPUT_BYTES = 64 * 1024
SYSTEM_PATH = "/usr/sbin:/usr/bin:/sbin:/bin"


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

    with client.open("rb") as binary:
        require(hashlib.file_digest(binary, "sha256").hexdigest() == BUILDCTL_SHA256, "official_client_hash")
    return workdir, dsg, daemon, client


@contextmanager
def process(argv, directory, environment, output, *, error_output=subprocess.DEVNULL):
    child = subprocess.Popen(argv, cwd=directory, env=environment, start_new_session=True,
                             stdin=subprocess.DEVNULL, stdout=output, stderr=error_output)
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


def command(directory, environment, step, argv, *, timeout=8, exit_code=0, capture=True):
    output = subprocess.PIPE if capture else subprocess.DEVNULL
    with process(argv, directory, environment, output) as child:
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


def official_usage(raw):
    records = document(raw, "official_du")
    if records is None:  # The Go template serializes an empty nil slice as null.
        records = []
    require(isinstance(records, list), "official_du_shape")
    counts = dict(total=len(records), reclaimable=0, shared=0, mutable=0, candidate_count=0)
    reasons = dict(active=0, shared=0, mutable=0, unknown_ownership=0)
    ids = set()
    for record in records:
        require(isinstance(record, dict), "official_du_record")
        identifier = record.get("id")
        flags = [record.get(key) for key in ("inUse", "shared", "mutable")]
        require(isinstance(identifier, str) and identifier and identifier not in ids
                and all(type(flag) is bool for flag in flags), "official_du_fields")
        ids.add(identifier)
        in_use, shared, mutable = flags
        counts["reclaimable"] += int(not in_use)
        counts["shared"] += int(shared)
        counts["mutable"] += int(mutable)
        reason = next((name for name, enabled in zip(("active", "shared", "mutable"), flags)
                       if enabled), "unknown_ownership")
        reasons[reason] += 1
    counts["preserved_by_reason"] = reasons
    return counts


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

            def official(step):
                return official_usage(command(directory, environment, step,
                                              (*ctl, "du", "--format", "{{json .}}"), timeout=17))

            def product(step, *, socket=path, pid=daemon.pid, client=buildctl, exit_code=0):
                argv = (dsg, "--json", "buildkit-local-cache", "--socket", socket,
                        "--expected-pid", str(pid), "--buildctl", client)
                return product_result(command(directory, environment, step, argv, timeout=22,
                                              exit_code=exit_code))

            def compare(step, counts):
                require(product(step) == {"status": "complete", "counts": counts,
                                          "disposition": "preserve", "reason": "unknown_ownership"},
                        step + "_mismatch")

            before = official("official_before")
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
            after = official("official_after")
            require(after["total"] > 0, "cache_still_empty")
            compare("product_after", after)
            require(official("official_recheck") == after, "cache_changed_during_observation")
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
            require(official("official_final") == after, "cache_changed_after_rejections")
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
                "build_completed": False, "rejected": []}
    try:
        signal.alarm(95)
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
