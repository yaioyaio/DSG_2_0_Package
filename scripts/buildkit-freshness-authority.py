#!/usr/bin/env python3
"""Consume an operator-provisioned freshness expectation outside the snapshot."""

import argparse
import json
import os
from pathlib import Path
import re
import select
import signal
import socket
import stat
import sys
import time


EXPECTED = "expected.json"
CLAIMED = "claimed"
RECORD_LIMIT = 512
FRAME_LIMIT = len(b"DSG-FRESHNESS-1    \n") + 4 * 64
IO_TIMEOUT = 3
SOCKET_LIFETIME = 60
SOCKET_REQUEST_LIMIT = 8
REGISTRATION = re.compile(rb"[a-f0-9]{12,64}")
DIGEST = re.compile(rb"[a-f0-9]{64}")
REQUEST = re.compile(
    rb"DSG-FRESHNESS-1 ([a-f0-9]{12,64}) ([a-f0-9]{64}) ([a-f0-9]{64}) ([a-f0-9]{64})\n"
)
CLAIM_RECORD = b"DSG-FRESHNESS-1 CLAIMED\n"
DIRECTORY_FLAGS = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW


def open_state_directory(path):
    if not path.is_absolute() or path.anchor != "/" or ".." in path.parts or path == Path("/"):
        raise ValueError

    uid = os.geteuid()
    directory = os.open("/", DIRECTORY_FLAGS)
    try:
        for component in path.parts[1:]:
            parent = os.fstat(directory)
            root_sticky = parent.st_uid == 0 and parent.st_mode & stat.S_ISVTX
            if parent.st_uid not in (0, uid) or (parent.st_mode & 0o022 and not root_sticky):
                raise ValueError
            opened = os.open(component, DIRECTORY_FLAGS, dir_fd=directory)
            os.close(directory)
            directory = opened

        state = os.fstat(directory)
        if state.st_uid != uid or stat.S_IMODE(state.st_mode) != 0o700:
            raise ValueError
        return directory
    except BaseException:
        os.close(directory)
        raise


def unique_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError
        result[key] = value
    return result


def load_expectation(directory):
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    with os.fdopen(os.open(EXPECTED, flags, dir_fd=directory), "rb") as source:
        entry = os.fstat(source.fileno())
        if (not stat.S_ISREG(entry.st_mode) or entry.st_uid != os.geteuid()
                or stat.S_IMODE(entry.st_mode) != 0o400 or entry.st_nlink != 1):
            raise ValueError
        raw = source.read(RECORD_LIMIT + 1)

    if len(raw) > RECORD_LIMIT:
        raise ValueError
    expected = json.loads(raw.decode("ascii"), object_pairs_hook=unique_fields)
    fields = ("registration", "scope_sha256", "checkpoint_sha256")
    if (not isinstance(expected, dict) or set(expected) != {"version", *fields}
            or type(expected["version"]) is not int or expected["version"] != 1):
        raise ValueError

    encoded = []
    for field, pattern in zip(fields, (REGISTRATION, DIGEST, DIGEST)):
        value = expected[field]
        if not isinstance(value, str):
            raise ValueError
        value = value.encode("ascii")
        if not pattern.fullmatch(value):
            raise ValueError
        encoded.append(value)
    return tuple(encoded)


def read_request(source):
    deadline = time.monotonic() + IO_TIMEOUT
    frame = bytearray()
    while len(frame) <= FRAME_LIMIT:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([source], [], [], remaining)[0]:
            raise TimeoutError
        chunk = os.read(source, FRAME_LIMIT + 1 - len(frame))
        if not chunk:
            return REQUEST.fullmatch(frame)
        frame.extend(chunk)
    return None


def claim(directory):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
    marker = os.open(CLAIMED, flags, mode=0o400, dir_fd=directory)
    # A failure after creation leaves the marker in place and prevents a retry.
    try:
        if os.write(marker, CLAIM_RECORD) != len(CLAIM_RECORD):
            raise OSError
        os.fsync(marker)
    finally:
        os.close(marker)
    os.fsync(directory)


def serve_request(source, destination, directory):
    try:
        request = read_request(source)
        if request is None or request.groups()[:3] != load_expectation(directory):
            return None
        claim(directory)
    except (OSError, ValueError, RecursionError):
        return None

    response = b"DSG-FRESHNESS-1 CLAIMED " + request[4] + b"\n"
    try:
        if select.select([], [destination], [], IO_TIMEOUT)[1]:
            return os.write(destination, response) == len(response)
    except OSError:
        pass
    return False


def serve_connection(stream, directory):
    return serve_request(stream.fileno(), stream.fileno(), directory)


def serve_unix(socket_path, directory):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        previous_umask = os.umask(0o177)
        try:
            listener.bind(str(socket_path))
        finally:
            os.umask(previous_umask)

        entry = os.stat(socket_path.name, dir_fd=directory, follow_symlinks=False)
        try:
            if not stat.S_ISSOCK(entry.st_mode) or stat.S_IMODE(entry.st_mode) != 0o600:
                raise ValueError
            listener.listen(1)
            deadline = time.monotonic() + SOCKET_LIFETIME
            for _ in range(SOCKET_REQUEST_LIMIT):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                listener.settimeout(remaining)
                connection, _ = listener.accept()
                with connection:
                    acknowledged = serve_connection(connection, directory)
                    if acknowledged is not None:
                        return acknowledged
            return False
        finally:
            try:
                current = os.stat(socket_path.name, dir_fd=directory, follow_symlinks=False)
                if os.path.samestat(current, entry):
                    os.unlink(socket_path.name, dir_fd=directory)
            except FileNotFoundError:
                pass


def terminate(_signal, _frame):
    raise SystemExit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__, epilog="End every request with EOF.")
    parser.add_argument("--state-dir", required=True, type=Path)
    behavior = parser.add_mutually_exclusive_group(required=True)
    behavior.add_argument("--socket", type=Path)
    behavior.add_argument("--stdio", action="store_true")
    args = parser.parse_args()
    if args.socket and (args.socket.parent != args.state_dir or args.socket.name in (EXPECTED, CLAIMED)):
        return 1

    signal.signal(signal.SIGTERM, terminate)
    try:
        directory = open_state_directory(args.state_dir)
        try:
            if args.stdio:
                return 0 if serve_request(sys.stdin.fileno(), sys.stdout.fileno(), directory) else 1
            return 0 if serve_unix(args.socket, directory) else 1
        finally:
            os.close(directory)
    except (OSError, ValueError, KeyboardInterrupt):
        return 1


if __name__ == "__main__":
    sys.exit(main())
