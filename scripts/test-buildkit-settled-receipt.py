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
    def test_archive_and_independent_pin_loss(self):
        raw = json.dumps(finalized()).encode()
        pin_bytes = hashlib.sha256(raw).hexdigest().encode("ascii") + b"\n"
        for archive_present, pin_present in ((True, True), (False, True), (True, False), (False, False)):
            with self.subTest(archive=archive_present, pin=pin_present), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                archive = root / "archive" / "AUTHORIZED-RESULT.json"
                pin_path = root / "supervisor" / "ARCHIVE-SHA256"
                archive.parent.mkdir()
                pin_path.parent.mkdir()
                archive.write_bytes(raw)
                pin_path.write_bytes(pin_bytes)
                if not archive_present:
                    archive.unlink()
                if not pin_present:
                    pin_path.unlink()

                # The supervisor supplies its retained value and never derives a replacement from the archive.
                try:
                    pin = pin_path.read_text(encoding="ascii").strip()
                except FileNotFoundError:
                    pin = ""
                expected_files = {path: path.stat().st_ino for path in (archive, pin_path) if path.exists()}
                args = ["check", "--archive", str(archive), "--archive-sha256", pin,
                        "--custody-sha256", digest, "--record-id", record]
                output = io.StringIO()
                with patch("sys.argv", args), contextlib.redirect_stdout(output):
                    code = checker["main"]()

                response = json.loads(output.getvalue())
                if archive_present and pin_present:
                    self.assertEqual(code, 0)
                    self.assertEqual(response["status"], "archived_settled_receipt")
                    self.assertEqual(response["mutation_calls"], 0)
                else:
                    self.assertEqual(code, 1)
                    self.assertEqual(response, {"status": "preserve", "mutation_calls": 0})
                self.assertEqual(set(root.rglob("*")), {archive.parent, pin_path.parent, *expected_files})
                for path, original in ((archive, raw), (pin_path, pin_bytes)):
                    if path in expected_files:
                        self.assertEqual(path.stat().st_ino, expected_files[path])
                        self.assertEqual(path.read_bytes(), original)

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

    def test_operating_controller_requires_both_archive_witnesses(self):
        for fault in (None, "missing-checkpoint", "missing-ledger_witness", "malformed-checkpoint",
                      "malformed-ledger_witness", "divergent-ledger_witness", "wrong-pin", "wrong-custody",
                      "wrong-record", "writable-mount"):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as temporary:
                path = Path(temporary) / "AUTHORIZED-RESULT.json"
                result = finalized()
                result["purpose"] = "operating_controller"
                post = result["attempt"]["postconditions"]
                hashes = post["completion_store_hashes"]
                hashes.update(checkpoint=digest, ledger_witness=digest)
                if fault == "divergent-ledger_witness":
                    hashes["ledger_witness"] = hashlib.sha256(b"other phase").hexdigest()
                if fault and fault.startswith("missing-"):
                    del hashes[fault.removeprefix("missing-")]
                if fault and fault.startswith("malformed-"):
                    hashes[fault.removeprefix("malformed-")] = "invalid"
                if fault == "writable-mount":
                    post["same_read_only_mount_after_recovery"] = False
                raw = json.dumps(result).encode()
                path.write_bytes(raw)
                pin = "0" * 64 if fault == "wrong-pin" else hashlib.sha256(raw).hexdigest()
                args = ["check", "--archive", str(path), "--archive-sha256", pin,
                        "--custody-sha256", "f" * 64 if fault == "wrong-custody" else digest,
                        "--record-id", "a" * 25 if fault == "wrong-record" else record]
                output = io.StringIO()
                with patch("sys.argv", args), contextlib.redirect_stdout(output):
                    code = checker["main"]()
                reply = json.loads(output.getvalue())
                self.assertEqual(reply["mutation_calls"], 0)
                if fault is None:
                    self.assertEqual(code, 0)
                    self.assertEqual(reply["status"], "archived_settled_receipt")
                    self.assertEqual(reply["purpose"], "operating_controller")
                else:
                    self.assertEqual(code, 1)
                    self.assertEqual(reply, {"status": "preserve", "mutation_calls": 0})

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
