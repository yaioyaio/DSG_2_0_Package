#!/usr/bin/env python3
"""Serialize one pre-enrolled exact operation on the external execution host."""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import signal
import select
import stat
import subprocess
import sys
import time


DIGEST = re.compile(r"[0-9a-f]{64}")
FULL_ID = re.compile(r"[0-9a-z]{25}")
FRAME = re.compile(rb"DSG-EXECUTION-1 ([0-9a-f]{12,64}) ([0-9a-f]{64})\n")
SCOPE = {"project": "Project", "worker": "Worker", "record_id": "RecordID",
         "generation": "Generation", "origin": "Origin", "metadata_digest": "MetadataDigest",
         "approval": "Approval", "intent": "Intent", "custody_sha256": None}
DIRECTORY = os.O_DIRECTORY | os.O_RDONLY | os.O_NOFOLLOW
LIMIT = 8192


def require(condition):
    if not condition:
        raise ValueError("preserve")


def encode(obj):
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("ascii") + b"\n"


def decode(raw):
    def unique(pairs):
        values = dict(pairs)
        require(len(values) == len(pairs))
        return values
    return json.loads(raw.decode("ascii"), object_pairs_hook=unique)


@contextlib.contextmanager
def protected_directory(path):
    path = Path(path)
    require(path.is_absolute() and path.anchor == "/" and ".." not in path.parts)
    fd = os.open("/", DIRECTORY)
    try:
        for name in path.parts[1:]:
            parent = os.fstat(fd)
            sticky = parent.st_uid == 0 and parent.st_mode & stat.S_ISVTX
            require(parent.st_uid in (0, os.geteuid()) and (not parent.st_mode & 0o022 or sticky))
            child = os.open(name, DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = child
        yield fd
    finally:
        os.close(fd)


@contextlib.contextmanager
def entry(directory, name, flags, modes):
    fd = os.open(name, flags | os.O_NOFOLLOW | os.O_NONBLOCK, 0o400, dir_fd=directory)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid()
                and stat.S_IMODE(info.st_mode) in modes and info.st_nlink == 1)
        yield fd
    finally:
        os.close(fd)


def read(fd):
    require(0 < os.fstat(fd).st_size <= LIMIT)
    raw = os.pread(fd, LIMIT + 1, 0)
    require(0 < len(raw) <= LIMIT)
    return raw


def store(fd, value):
    raw = encode(value)
    require(len(raw) <= LIMIT and os.pwrite(fd, raw, 0) == len(raw))
    os.ftruncate(fd, len(raw))
    os.fsync(fd)


def validate_job(job, registration):
    require(type(job) is dict and job.keys() == {"version", "registration", "scope", "operator_sha256"})
    require(type(job["version"]) is int and job["version"] == 1 and job["registration"] == registration
            and DIGEST.fullmatch(job["operator_sha256"]))
    scope = job["scope"]
    require(type(scope) is dict and scope.keys() == SCOPE.keys())
    for key, value in scope.items():
        pattern = FULL_ID if key in ("worker", "record_id") else DIGEST
        require(type(value) is str and pattern.fullmatch(value))


def validate_receipt(receipt, scope):
    require(type(receipt) is dict and receipt.keys() == {"Request", "LogicalRecords", "PhysicalBytes"})
    require(type(receipt["LogicalRecords"]) is int and receipt["LogicalRecords"] == 1
            and receipt["PhysicalBytes"] is None)
    req = receipt["Request"]
    fields = {native: scope[key] for key, native in SCOPE.items() if native}
    require(type(req) is dict and req.keys() == fields.keys() | {"ApprovedAtMS", "ExpiresAtMS"})
    require(all(req[key] == value for key, value in fields.items()))
    start, end = req["ApprovedAtMS"], req["ExpiresAtMS"]
    require(type(start) is int and type(end) is int and 0 < start < end <= 2**63 - 1
            and end - start <= 1800000)


def execute(state_dir, frame, effect):
    match = FRAME.fullmatch(frame)
    require(match)
    registration, digest = (part.decode("ascii") for part in match.groups())
    with protected_directory(state_dir) as directory:
        info = os.fstat(directory)
        require(info.st_uid == os.geteuid() and stat.S_IMODE(info.st_mode) == 0o700)
        with entry(directory, "job.json", os.O_RDONLY, {0o400}) as fd:
            raw = read(fd)
        require(hashlib.sha256(raw).hexdigest() == digest)
        job = decode(raw)
        validate_job(job, registration)
        base = {"version": 1, "job_sha256": digest}
        with entry(directory, "execution-state.json", os.O_RDWR, {0o600}) as fd:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            info = os.fstat(fd)
            claim = {**base, "device": info.st_dev, "inode": info.st_ino}
            state = decode(read(fd))
            require(type(state) is dict and type(state.get("version")) is int)
            if state.get("status") == "settled":
                require(state == {**base, "status": "settled", "receipt": state["receipt"]})
                with entry(directory, "execution-claimed", os.O_RDONLY, {0o400}) as marker:
                    require(read(marker) == encode(claim))
                validate_receipt(state["receipt"], job["scope"])
                # A previous writer may have exited after writing but before fsync returned.
                os.fsync(fd)
                return {"status": "settled", "receipt": state["receipt"]}
            require(state == {**base, "status": "ready"})
            with entry(directory, "execution-claimed", os.O_CREAT | os.O_EXCL | os.O_WRONLY, {0o400}) as marker:
                store(marker, claim)
            os.fsync(directory)
            store(fd, {**base, "status": "pending"})
            result = effect(job)
            require(type(result) is dict and result.keys() == {"status", "receipt"} and result["status"] == "applied")
            validate_receipt(result["receipt"], job["scope"])
            reply = {"status": "settled", "receipt": result["receipt"]}
            store(fd, {**base, **reply})
            return reply


def child_limits():
    resource.setrlimit(resource.RLIMIT_CPU, (50, 50))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def native(operator, job):
    path = Path(operator)
    require(sys.platform.startswith("linux") and os.geteuid() == 0 and path.name == "dsg-buildkit-exact")
    with protected_directory(path.parent) as directory:
        require(os.fstat(directory).st_uid == 0 and not os.fstat(directory).st_mode & 0o022)
        with entry(directory, path.name, os.O_RDONLY, {0o500, 0o550, 0o555, 0o700, 0o750, 0o755}) as fd:
            pinned = os.fstat(fd)
            require(0 < pinned.st_size <= 512 * 1024 * 1024)
            digest = hashlib.sha256()
            remaining = pinned.st_size
            while remaining:
                chunk = os.read(fd, min(remaining, 1024 * 1024))
                require(chunk)
                digest.update(chunk)
                remaining -= len(chunk)
            measured = os.fstat(fd)
            require((measured.st_size, measured.st_mtime_ns, measured.st_ctime_ns)
                    == (pinned.st_size, pinned.st_mtime_ns, pinned.st_ctime_ns)
                    and digest.hexdigest() == job["operator_sha256"])
            deadline = time.monotonic() + 55
            with subprocess.Popen([f"/proc/self/fd/{fd}", "apply", job["registration"]],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  pass_fds=(fd,), env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"},
                                  cwd="/", start_new_session=True, preexec_fn=child_limits) as process:
                try:
                    process.stdin.write(encode(job["scope"]))
                    process.stdin.close()
                    output = bytearray()
                    while len(output) <= LIMIT:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
                            raise TimeoutError
                        block = os.read(process.stdout.fileno(), LIMIT + 1 - len(output))
                        if not block:
                            break
                        output.extend(block)
                    require(len(output) <= LIMIT)
                    require(process.wait(timeout=max(0, deadline - time.monotonic())) == 0)
                    return decode(output)
                except BaseException:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    raise


def timed_out(signum, frame):
    raise TimeoutError


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--state-dir", required=True, type=Path)
    parser.add_argument("--operator", required=True, type=Path)
    parser.add_argument("--stdio", action="store_true", required=True)
    args = parser.parse_args()
    try:
        signal.signal(signal.SIGALRM, timed_out)
        signal.alarm(3)
        frame = sys.stdin.buffer.read(160)
        require(len(frame) < 160)
        signal.alarm(64)
        require(sys.platform.startswith("linux") and os.geteuid() == 0 and args.operator.is_absolute())
        result = execute(args.state_dir, frame, lambda job: native(args.operator, job))
        status = 0
    except (OSError, ValueError, TypeError, KeyError, RecursionError, subprocess.SubprocessError):
        result, status = {"status": "preserve"}, 1
    finally:
        signal.alarm(0)
    try:
        signal.alarm(3)
        output = encode(result)
        return status if os.write(sys.stdout.fileno(), output) == len(output) else 1
    except OSError:
        return 1
    finally:
        signal.alarm(0)


if __name__ == "__main__":
    sys.exit(main())
