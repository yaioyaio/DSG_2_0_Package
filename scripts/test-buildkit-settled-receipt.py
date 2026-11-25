import contextlib
import copy
import hashlib
import io
import json
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

checker = runpy.run_path(str(Path(__file__).with_name("check-buildkit-settled-receipt.py")))
digest = hashlib.sha256(b"test witness").hexdigest()
record = "r" * 25


def finalized():
    request = dict.fromkeys(("Project", "Generation", "MetadataDigest", "Origin", "Intent", "Approval"), digest)
    request.update(RecordID=record, Worker="w" * 25, ApprovedAtMS=10, ExpiresAtMS=100)
    response = {"code": 0, "receipt": {"Request": request, "LogicalRecords": 1, "PhysicalBytes": None}}
    stores = checker["NATIVE"]
    post = {"byte_identical_after_receipt_only": True, "same_read_only_mount_after_recovery": True,
            "database_files_checked": 5, "database_files_changed": sorted(stores - {"history.db"}),
            "completion_store_hashes": {"authority": digest, "journal": digest, "native": dict.fromkeys(stores, digest)}}
    return {"attempt": {"application": copy.deepcopy(response), "recovered": response, "postconditions": post},
            "custody_sha256": digest, "record_id": record, "purpose": "candidate",
            "guest_response_code": 0, "exact_mutation_calls": 1, "receipt_recovery_mutation_calls": 0}


class SettledReceiptTest(unittest.TestCase):
    def test_controller_rehearsal(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "SETTLED.json"
            result = finalized()
            for purpose in ("controller_rehearsal", "unknown"):
                with self.subTest(purpose=purpose):
                    result["purpose"] = purpose
                    raw = json.dumps(result).encode()
                    path.write_bytes(raw)
                    pin = hashlib.sha256(raw).hexdigest()
                    if purpose == "controller_rehearsal":
                        reply = checker["inspect_archive"](path, pin, digest, record)
                        self.assertEqual(reply["purpose"], purpose)
                        self.assertEqual(reply["mutation_calls"], 0)
                        with self.assertRaises(ValueError):
                            checker["inspect_archive"](path, "0" * 64, digest, record)
                    else:
                        with self.assertRaises(ValueError):
                            checker["inspect_archive"](path, pin, digest, record)

    def test_archived_receipt(self):
        for fault in ("none", "wrong-pin", "wrong-custody", "wrong-record", "unsettled", "foreign-receipt",
                      "incomplete-stores", "duplicate-field", "boolean-count", "expired-intent", "truncated", "missing"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "AUTHORIZED-RESULT.json"
                result = finalized()
                attempt = result["attempt"]
                if fault == "unsettled":
                    attempt["recovered"]["code"] = 1
                if fault == "foreign-receipt":
                    attempt["recovered"]["receipt"]["Request"]["Approval"] = "f" * 64
                if fault == "incomplete-stores":
                    del attempt["postconditions"]["completion_store_hashes"]["native"]["solver.db"]
                if fault == "boolean-count":
                    result["exact_mutation_calls"] = True
                if fault == "expired-intent":
                    for op in ("application", "recovered"):
                        attempt[op]["receipt"]["Request"]["ExpiresAtMS"] = 9
                raw = json.dumps(result).encode()
                if fault == "duplicate-field":
                    raw = b'{"purpose":"candidate",' + raw[1:]
                if fault == "truncated":
                    raw = raw[:-5]
                if fault != "missing":
                    path.write_bytes(raw)
                pin = "0" * 64 if fault == "wrong-pin" else hashlib.sha256(raw).hexdigest()
                args = ["check", "--archive", str(path), "--archive-sha256", pin,
                        "--custody-sha256", "f" * 64 if fault == "wrong-custody" else digest,
                        "--record-id", "a" * 25 if fault == "wrong-record" else record]
                output = io.StringIO()
                with patch("sys.argv", args), contextlib.redirect_stdout(output):
                    code = checker["main"]()
                response = json.loads(output.getvalue())
                self.assertEqual(code, 0 if fault == "none" else 1)
                self.assertEqual(response["status"], "archived_settled_receipt" if fault == "none" else "preserve")
                self.assertEqual(response["mutation_calls"], 0)


if __name__ == "__main__":
    unittest.main()
