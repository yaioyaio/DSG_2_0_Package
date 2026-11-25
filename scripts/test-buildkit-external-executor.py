#!/usr/bin/env python3
"""Bounded checks of the external journal with a create-only fixture effect."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("executor", Path(__file__).with_name("buildkit-external-executor.py"))
executor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(executor)


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

    def invoke(self, effect=None):
        return executor.execute(self.root, self.frame, effect or self.effect)

    def rejects(self, effect=None):
        with self.assertRaises((OSError, ValueError, TypeError, KeyError)):
            self.invoke(effect)

    def test_lost_reply_recovers_identical_receipt_without_effect_or_writes(self):
        reply = self.invoke()
        timestamp = self.state.stat().st_mtime_ns
        with patch.object(executor, "store", side_effect=AssertionError):
            self.assertEqual(reply, self.invoke())
        self.assertEqual(self.calls, 1)
        self.assertEqual(self.state.stat().st_mtime_ns, timestamp)

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
