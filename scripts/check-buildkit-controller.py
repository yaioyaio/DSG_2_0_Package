#!/usr/bin/env python3
"""Qualify one socket-activated DSG request in a synthetic Linux ARM64 rehearsal."""

import argparse
import json
import os
from pathlib import Path
import re
import signal
import socket
import stat
import subprocess
import sys

DENIAL = {"status": "preserved", "reason": "controller_admission"}
# SO_PASSPIDFD was introduced in Linux 6.5; older Python lacks the symbolic name.
SO_PASSPIDFD = getattr(socket, "SO_PASSPIDFD", 76)


class QualificationFailure(Exception):
    def __init__(self, category, **details):
        self.summary = {"failure": category, **details}


def demand(condition, reason, **details):
    if not condition:
        raise QualificationFailure(reason, **details)


def read_document(stage, name, mode, limit):
    fd = os.open(stage / name, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        demand(info.st_mode == stat.S_IFREG | mode and info.st_uid == 0 and info.st_nlink == 1
               and 0 < info.st_size <= limit, "file_shape")
        data = source.read(limit + 1)
        demand(len(data) == info.st_size, "file_changed")
        return json.loads(data)


def exchange(binary, registration, uid, packet):
    # Abstract addresses stay inside the current private network namespace. Connect
    # after dropping privileges; a pre-fork socketpair would retain root peer creds.
    address = b"\0dsg-buildkit-check-" + os.urandom(16)
    child_steps = ("prepare", "drop_groups", "drop_gid", "drop_uid", "socket", "connect", "send", "receive", "report")
    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        listener.setsockopt(socket.SOL_SOCKET, SO_PASSPIDFD, 1)
        listener.bind(address)
        listener.listen(1)
        listener.settimeout(4)
        reader, writer = os.pipe()
        child = os.fork()
        if child == 0:
            step = "prepare"
            try:
                os.close(reader)
                listener.close()
                step = "drop_groups"
                os.setgroups([])
                step = "drop_gid"
                os.setgid(uid)
                step = "drop_uid"
                os.setuid(uid)
                step = "socket"
                with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
                    peer.settimeout(45)
                    step = "connect"
                    peer.connect(address)
                    step = "send"
                    demand(peer.send(packet) == len(packet), "request_write")
                    step = "receive"
                    reply, _, flags, _ = peer.recvmsg(1536)
                    demand(not flags & socket.MSG_TRUNC, "oversize_reply")
                    step = "report"
                    os.write(writer, reply)
            except (OSError, QualificationFailure) as error:
                if isinstance(error, OSError):
                    os.write(writer, str(error.errno or 0).encode("ascii"))
                os._exit(10 + child_steps.index(step))
            os._exit(0)
        os.close(writer)
        def check_child(wait_flags, controller_code):
            nonlocal child
            pid, wait_status = os.waitpid(child, wait_flags)
            if pid:
                child = None
                code = os.waitstatus_to_exitcode(wait_status)
                if code:
                    field = os.read(reader, 16)
                    client_step = child_steps[code - 10] if 10 <= code < 10 + len(child_steps) else "exit"
                    raise QualificationFailure("role_client", client_step=client_step, client_exit=code,
                                               errno=int(field) if field.isdigit() else 0, controller_exit=controller_code)

        try:
            try:
                connection, _ = listener.accept()
            except OSError:
                check_child(os.WNOHANG, None)
                raise
            with connection:
                controller = subprocess.Popen(
                    [binary, "buildkit-exact-controller", registration], stdin=connection,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd="/", env={},
                )
            with controller:
                try:
                    status = controller.wait(timeout=40)
                except subprocess.TimeoutExpired:
                    controller.kill()
                    controller.wait(timeout=3)
                    raise QualificationFailure("controller_timeout")
            check_child(0, status)
            reply = os.read(reader, 1537)
            try:
                return status, json.loads(reply) if reply else None
            except (ValueError, UnicodeError):
                raise QualificationFailure("reply_format", controller_exit=status) from None
        finally:
            os.close(reader)
            if child is not None:
                try:
                    os.kill(child, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                os.waitpid(child, 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dsg", type=Path)
    parser.add_argument("registration")
    parser.add_argument("operation", choices=("check-denials", "approve", "apply", "receipt"))
    args = parser.parse_args()
    demand(os.getuid() == os.geteuid() == 0 and os.uname().machine == "aarch64", "linux_root_arm64")
    demand(re.fullmatch("[a-f0-9]{12,64}", args.registration), "registration")
    demand(args.dsg.is_absolute() and args.dsg.resolve(strict=True) == args.dsg, "dsg_path")
    stage = Path("/run") / ("dsg-approval-" + args.registration) / "stage"
    demand(stage.resolve(strict=True) == stage and stage.lstat().st_uid == 0 and
           stat.S_IMODE(stage.lstat().st_mode) == 0o700, "stage")
    evidence = read_document(stage, "custody.json", 0o600, 32 * 1024)
    demand(evidence["purpose"] == "rehearsal", "requires_rehearsal")
    policy = read_document(stage, "controller-policy.json", 0o400, 4096)
    approve, execute = policy["approver_uid"], policy["executor_uid"]
    demand(all(type(uid) is int and 0 < uid < 2**32 - 1 for uid in (approve, execute)) and
           approve != execute, "role_policy")
    scope = policy["scope"]

    def send(operation, uid, selectors, extra=None):
        request = {"version": 1, "operation": operation, "scope": selectors, **(extra or {})}
        return exchange(args.dsg, args.registration, uid, json.dumps(request).encode("ascii"))

    if args.operation == "check-denials":
        changed = dict(scope)
        changed["intent"] = ("0" if scope["intent"][0] != "0" else "1") + scope["intent"][1:]
        rejected = [
            ("approve", execute, scope, {}), ("apply", approve, scope, {}),
            ("receipt", approve, scope, {}), ("approve", 0, scope, {}),
            ("receipt", execute, changed, {}), ("receipt", execute, scope, {"uid": approve}),
        ]
        cases = ("approve_by_executor", "apply_by_approver", "receipt_by_approver", "root_sender", "other_intent", "asserted_uid")
        for case, params in zip(cases, rejected, strict=True):
            try:
                code, result = send(*params)
                response = "controller_denial" if result == DENIAL else "empty" if result is None else "other_json"
                demand(code == 5 and result == DENIAL, "admission_not_confirmed", controller_exit=code, response=response)
            except QualificationFailure as error:
                error.summary["case"] = case
                raise
            except OSError as error:
                raise QualificationFailure("socket_or_launch", case=case, errno=error.errno) from None
        print(json.dumps({"controller_denials": len(rejected)}))
        return
    status, result = send(args.operation, approve if args.operation == "approve" else execute, scope)
    if status == 5 and result is None:
        print(json.dumps({"status": "native_preserve"}))
        sys.exit(5)
    demand(status == 0 and isinstance(result, dict) and set(result) == {"status", "receipt"},
           "native_result_missing", controller_exit=status, controller_denial=result == DENIAL)
    demand(result["status"] == {"approve": "approved", "apply": "applied", "receipt": "settled"}[args.operation],
           "native_phase")
    if args.operation == "approve":
        demand(result["receipt"] is None, "approval_result")
    else:
        receipt = result["receipt"]
        demand(receipt["LogicalRecords"] == 1 and receipt["PhysicalBytes"] is None, "logical_receipt")
        fields = {"Project": "project", "Worker": "worker", "RecordID": "record_id", "Generation": "generation",
                  "Origin": "origin", "MetadataDigest": "metadata_digest", "Approval": "approval", "Intent": "intent"}
        demand(all(receipt["Request"][key] == scope[name] for key, name in fields.items()), "receipt_binding")
    print(json.dumps(result, separators=(",", ":"), sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except QualificationFailure as error:
        sys.exit(json.dumps(error.summary, sort_keys=True))
    except OSError as error:
        sys.exit(json.dumps({"failure": "os_error", "errno": error.errno}))
    except (KeyError, TypeError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(json.dumps({"failure": "input_or_transport", "category": type(error).__name__}))
