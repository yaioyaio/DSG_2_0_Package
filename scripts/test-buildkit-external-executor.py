#!/usr/bin/env python3
"""Bounded checks of the external journal with a create-only fixture effect."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("executor", Path(__file__).with_name("buildkit-external-executor.py"))
executor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(executor)
authority_spec = importlib.util.spec_from_file_location("authority", Path(__file__).with_name("buildkit-freshness-authority.py"))
authority = importlib.util.module_from_spec(authority_spec)
authority_spec.loader.exec_module(authority)


class ExternalExecution(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / "execution-state.json"
        self.jobfile = self.root / "job.json"
        self.claim = self.root / "execution-claimed"
        self.calls = 0
        self.scope = {key: "b" * (25 if key in ("worker", "record_id") else 64) for key in executor.SCOPE}
        self.scope.update(custody_sha256="c" * 64, intent="d" * 64, approval="e" * 64)
        self.job = {"version": 1, "scope": self.scope, "operator_sha256": "c" * 64, "registration": "a" * 12}
        self.make_job()
        self.request = {native: self.scope[key] for key, native in executor.SCOPE.items() if native}
        self.request.update(ApprovedAtMS=1000, ExpiresAtMS=2000)
        self.receipt = {"Request": self.request, "LogicalRecords": 1, "PhysicalBytes": None}

    def make_job(self):
        raw = executor.encode(self.job)
        self.jobfile.touch(mode=0o600, exist_ok=True)
        self.jobfile.chmod(0o600)
        self.jobfile.write_bytes(raw)
        self.jobfile.chmod(0o400)
        digest = hashlib.sha256(raw).hexdigest()
        self.ready = {"version": 1, "job_sha256": digest, "status": "ready"}
        self.state.write_bytes(executor.encode(self.ready))
        self.state.chmod(0o600)
        self.frame = f"DSG-EXECUTION-1 {self.job['registration']} {digest}\n".encode()

    def effect(self, job):
        self.calls += 1
        self.assertEqual(self.job, job)
        self.assertEqual("pending", json.loads(self.state.read_bytes())["status"])
        self.assertTrue(self.claim.is_file())
        with (self.root / "fixture-effect").open("xb") as target:
            target.write(b"one")
        return {"status": "applied", "receipt": self.receipt}

    def reserve(self, registration, scope_digest, job_digest):
        self.assertEqual("pending", json.loads(self.state.read_bytes())["status"])
        self.assertTrue(self.claim.is_file())
        self.assertEqual((registration, job_digest), (self.job["registration"], self.ready["job_sha256"]))
        expected = f"dsg-external-execution-fence-v1\n{registration}\n{'c'*64}\n{'d'*64}\n{'e'*64}\n"
        self.assertEqual(scope_digest, hashlib.sha256(expected.encode("ascii")).hexdigest())
        return True

    def invoke(self, effect=None, reserve=None):
        return executor.execute(self.root, self.frame, effect or self.effect, reserve or self.reserve)

    def rejects(self, effect=None):
        with self.assertRaises((OSError, ValueError, TypeError, KeyError)):
            self.invoke(effect)

    def test_lost_reply_recovers_identical_receipt_without_effect_or_writes(self):
        reply = self.invoke()
        timestamp = self.state.stat().st_mtime_ns
        with patch.object(executor, "store", side_effect=AssertionError), patch.object(self, "reserve", side_effect=AssertionError):
            self.assertEqual(reply, self.invoke())
        self.assertEqual(self.calls, 1)
        self.assertEqual(self.state.stat().st_mtime_ns, timestamp)

    def test_combined_rollback_independent_authority_retains_claim(self):
        with tempfile.TemporaryDirectory() as retained:
            directory = authority.open_state_directory(Path(retained).resolve())
            self.addCleanup(os.close, directory)
            registration = self.job["registration"]
            expected = {
                "version": 1,
                "registration": registration,
                "scope_sha256": hashlib.sha256(
                    f"dsg-external-execution-fence-v1\n{registration}\n{'c'*64}\n{'d'*64}\n{'e'*64}\n".encode("ascii")
                ).hexdigest(),
                "checkpoint_sha256": self.ready["job_sha256"],
            }
            expectation = Path(retained) / "expected.json"
            expectation.write_bytes(executor.encode(expected))
            expectation.chmod(0o400)

            def reserve(registration, scope_digest, job_digest):
                self.reserve(registration, scope_digest, job_digest)
                client, peer = socket.socketpair()
                def serve():
                    with peer:
                        authority.serve_connection(peer, directory)
                worker = threading.Thread(target=serve)
                worker.start()
                try:
                    with client:
                        return executor.fence_exchange(client, registration, scope_digest, job_digest,
                                                       time.monotonic() + 2)
                finally:
                    worker.join(3)
                    self.assertFalse(worker.is_alive())
            self.invoke(reserve=reserve)
            marker = Path(retained) / "claimed"
            before = marker.stat()
            self.state.write_bytes(executor.encode(self.ready))
            self.claim.unlink()
            with self.assertRaises(ValueError):
                self.invoke(reserve=reserve)
            self.assertTrue(os.path.samestat(before, marker.stat()))
            self.assertEqual(marker.read_bytes(), authority.CLAIM_RECORD)
            self.assertEqual(self.calls, 1)
            self.assertEqual("pending", json.loads(self.state.read_bytes())["status"])

    def test_pending_sync_failure_never_contacts_authority(self):
        with patch.object(executor.os, "fsync", side_effect=[None, None, OSError]), \
                patch.object(self, "reserve", side_effect=AssertionError):
            self.rejects()
        self.assertEqual(self.calls, 0)

    def test_unavailable_or_denied_reservation_prevents_dispatch(self):
        for answer in (False, None, 1, TimeoutError()):
            with self.subTest(answer=answer):
                if self.claim.exists():
                    self.claim.unlink()
                self.state.write_bytes(executor.encode(self.ready))
                def unavailable(*args):
                    if isinstance(answer, Exception):
                        raise answer
                    return answer
                with self.assertRaises((TimeoutError, ValueError)):
                    self.invoke(reserve=unavailable)
                self.rejects()
                self.assertEqual(self.calls, 0)

    def test_exchange_requires_exact_nonce_and_eof(self):
        nonce = "00" * 32
        response = f"DSG-FRESHNESS-1 CLAIMED {nonce}\n".encode("ascii")
        for reply, eof in ((response[:-1], True), (response + b"x", True),
                           (response.replace(b"0", b"1"), True), (response, False)):
            with self.subTest(reply=reply, eof=eof):
                client, peer = socket.socketpair()
                with client, peer, patch.object(executor.os, "urandom", return_value=bytes(32)):
                    peer.sendall(reply)
                    if eof:
                        peer.shutdown(socket.SHUT_WR)
                        self.assertFalse(executor.fence_exchange(client, "a"*12, "b"*64, "c"*64,
                                                                 time.monotonic() + 1))
                    else:
                        with self.assertRaises(TimeoutError):
                            executor.fence_exchange(client, "a"*12, "b"*64, "c"*64, time.monotonic() + 0.1)

    def test_unsynced_settled_record_cannot_be_acknowledged(self):
        reply = self.invoke()
        with patch.object(executor.os, "fsync", side_effect=OSError):
            self.rejects()
        self.assertEqual(reply, self.invoke())
        self.assertEqual(self.calls, 1)

    def test_reset_ready_even_on_recreated_state_requires_preservation(self):
        self.invoke()
        self.state.unlink()
        self.state.write_bytes(executor.encode(self.ready))
        self.state.chmod(0o600)
        self.rejects()
        self.assertEqual(self.calls, 1)

    def test_missing_claim_rejects_settled(self):
        self.invoke()
        self.claim.unlink()
        self.rejects()
        self.assertEqual(self.calls, 1)

    def test_corrupt_claim_rejects_settled(self):
        self.invoke()
        self.claim.chmod(0o600)
        self.claim.write_bytes(b"{}")
        self.claim.chmod(0o400)
        self.rejects()
        self.assertEqual(self.calls, 1)

    def test_missing_or_torn_state_rejects(self):
        self.state.unlink()
        self.rejects()
        self.state.write_bytes(b'{"status"')
        self.state.chmod(0o600)
        self.rejects()
        self.assertEqual(self.calls, 0)

    def test_ambiguous_after_effect(self):
        def fail(job):
            self.effect(job)
            raise TimeoutError
        self.rejects(fail)
        self.rejects()
        self.assertEqual(self.calls, 1)

    def test_input_only_accepts_exact_enrolled_frame(self):
        frame = self.frame
        for bad in (frame.rstrip(), frame + frame, frame + b"arg", frame.replace(b"aaa", b"ddd"),
                    frame[:-3] + b"ZZ\n"):
            self.frame = bad
            self.rejects()
        self.assertEqual(self.calls, 0)
        self.assertFalse(self.claim.exists())

    def test_invalid_registered_scope(self):
        self.job["scope"]["worker"] = "partial"
        self.make_job()
        self.rejects()
        self.assertFalse(self.claim.exists())

    def test_mismatched_receipt_preserves_pending(self):
        self.receipt["Request"]["Origin"] = "0" * 64
        self.rejects()
        self.rejects()
        self.assertEqual(self.calls, 1)
        self.assertEqual("pending", json.loads(self.state.read_bytes())["status"])

    def test_concurrent_request_cannot_dispatch(self):
        entered = threading.Event()
        finish = threading.Event()
        result = []

        def hold(job):
            entered.set()
            self.assertTrue(finish.wait(3))
            return self.effect(job)

        worker = threading.Thread(target=lambda: result.append(self.invoke(hold)))
        worker.start()
        try:
            self.assertTrue(entered.wait(3))
            self.rejects()
        finally:
            finish.set()
            worker.join(3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(result, [self.invoke()])
        self.assertEqual(self.calls, 1)


if __name__ == "__main__":
    unittest.main()
