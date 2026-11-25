#!/usr/bin/env python3
"""Test project issuance, offline matching, and qualification fail-closed behavior."""

from contextlib import contextmanager
import copy
import io
import json
from pathlib import Path
import runpy
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).with_name("check-buildkit-project-registration.py")
CODE = runpy.run_path(str(SCRIPT))
QUALIFY = CODE["qualify"]
ERROR = CODE["RegistrationError"]
CANONICAL = CODE["canonical"]
DIGEST = CODE["digest"]
VALIDATE = CODE["validate_issuance"]
OBSERVE = CODE["validate_observation"]
STAGE = Path("/run/dsg-project-enrollment-0123456789ab/stage")
DATABASE_FIELDS = {"solver_tx", "cache_tx", "containerd_tx"}


def source_candidate():
    return {
        "id": "a" * 25,
        "worker_id": "b" * 25,
        "offline_record": {
            "id": "a" * 25, "record_present": True, "snapshot_binding": True,
            "mutable_alias": False, "blob_binding": True, "metadata_only_candidate": False,
            "bindings_sha256": "1" * 64,
        },
        "project_registration": {
            "evidence_scope": "isolated_project_source", "project_id": "dsg",
            "project_revision": "2" * 40, "project_source_name": "README.md",
            "project_source_sha256": "3" * 64, "project_source_bytes": 108,
            **dict.fromkeys(CODE["PRODUCTION_FLAGS"], False),
        },
        "source": "isolated scratch LLB and independently verified OCI layer",
        "disposition": "preserve", "before_count": 0, "after_count": 1,
        "llb_sha256": "4" * 64, "verified_layer_sha256": "5" * 64,
        "daemon_generation": {"pid": 1, "socket_device": 20, "socket_inode": 3, "start_ticks": 5},
    }


def seal(candidate):
    candidate = copy.deepcopy(candidate)
    candidate["evidence_sha256"] = DIGEST(CANONICAL(candidate))
    data = CANONICAL(candidate) + b"\n"
    return data, DIGEST(data)


def observation(issued, strict=False):
    observed = {
        "worker": issued["worker_id"], "record": copy.deepcopy(issued["offline_record"]),
        "graph": {}, "snapshot_tx": None,
        **dict.fromkeys(DATABASE_FIELDS, 1),
    }
    if strict:
        observed["quiescence"] = dict.fromkeys(CODE["QUIESCENCE_FLAGS"], True)
    return observed


class RegistrationTests(unittest.TestCase):
    def setUp(self):
        self.source = source_candidate()
        self.raw, self.seal = seal(self.source)
        self.issued = VALIDATE(self.raw, self.seal)

    def assertDenied(self, category, method, *args):
        with self.assertRaisesRegex(ERROR, f"^{category}$"):
            method(*args)

    def test_prior_issuance_and_exact_observation(self):
        OBSERVE(CANONICAL(observation(self.issued)), self.issued, DATABASE_FIELDS, False)
        OBSERVE(CANONICAL(observation(self.issued, True)), self.issued, DATABASE_FIELDS, True)

    def test_matching_inner_hash_does_not_replace_external_seal(self):
        self.source["project_registration"]["project_source_sha256"] = "6" * 64
        new_raw, _ = seal(self.source)
        self.assertDenied("external_issuance_seal", VALIDATE, new_raw, self.seal)
        self.assertDenied("external_issuance_seal", VALIDATE, self.raw, self.issued["evidence_sha256"])

    def test_forwarded_inner_digest_is_checked(self):
        changed = {**self.issued, "evidence_sha256": "0" * 64}
        raw = CANONICAL(changed)
        self.assertDenied("issuance_evidence_digest", VALIDATE, raw, DIGEST(raw))

    def test_duplicate_keys_are_denied_even_with_matching_seal(self):
        raw = self.raw.replace(b'"id":', b'"id":"duplicate","id":', 1)
        self.assertDenied("duplicate_json_key", VALIDATE, raw, DIGEST(raw))

    def test_production_flags_and_unexpected_fields_are_denied(self):
        project = self.source["project_registration"]
        project["exact_mutation_authorized"] = True
        self.assertDenied("project_registration", VALIDATE, *seal(self.source))
        project["exact_mutation_authorized"] = False
        project["extra"] = "/path/should/not/be/reported"
        self.assertDenied("project_schema", VALIDATE, *seal(self.source))

    def test_full_worker_and_record_ids_are_required(self):
        for key in ("worker_id", "id"):
            with self.subTest(key=key):
                candidate = copy.deepcopy(self.source)
                candidate[key] = candidate[key][:-1]
                self.assertDenied("issuance_full_ids", VALIDATE, *seal(candidate))

    def test_record_and_worker_mismatches_are_denied(self):
        for key, category in (("worker", "offline_worker"), ("id", "record_binding"),
                              ("bindings_sha256", "offline_binding_changed")):
            with self.subTest(key=key):
                observed = observation(self.issued)
                target = observed if key == "worker" else observed["record"]
                target[key] = "7" * (64 if key == "bindings_sha256" else 25)
                self.assertDenied(category, OBSERVE, CANONICAL(observed), self.issued, DATABASE_FIELDS, False)

    def test_strict_boundary_is_explicit_and_all_checks_are_required(self):
        point = observation(self.issued, True)
        self.assertDenied("offline_schema", OBSERVE, CANONICAL(point), self.issued, DATABASE_FIELDS, False)
        point["quiescence"]["mounts_clear"] = False
        self.assertDenied("offline_quiescence", OBSERVE, CANONICAL(point), self.issued, DATABASE_FIELDS, True)
        point.pop("quiescence")
        self.assertDenied("offline_schema", OBSERVE, CANONICAL(point), self.issued, DATABASE_FIELDS, True)

    def test_transaction_set_matches_native_database_set(self):
        observed = observation(self.issued)
        fields = DATABASE_FIELDS | {"snapshot_tx", "history_tx"}
        observed.update(snapshot_tx=2, history_tx=3)
        OBSERVE(CANONICAL(observed), self.issued, fields, False)
        observed["history_tx"] = True
        self.assertDenied("offline_transactions", OBSERVE, CANONICAL(observed), self.issued, fields, False)

    def run_qualification(self, *, changed=False, strict=False, denial=False):
        class Source(io.BytesIO):
            def __init__(self, data, descriptor):
                super().__init__(data)
                self.descriptor = descriptor

            def fileno(self):
                return self.descriptor

        @contextmanager
        def open_file(_, name, *_rest):
            source = Source(b"pinned binary", 7) if name == "buildkitd" else Source(CODE["WORKER_CONFIG"], 8)
            yield source, None

        @contextmanager
        def directory(*_args, **_kwargs):
            yield 5, SimpleNamespace(st_dev=17)

        snapshot = CODE["StoreSnapshot"]
        worker_id_file = ("id", DIGEST(self.issued["worker_id"].encode("ascii")))
        databases = {field: (field, "6" * 64) for field in DATABASE_FIELDS}
        old = snapshot(("store", "worker", "snapshot"), ("marker",), None, worker_id_file, databases)
        changed_ids = ("different_store", "worker", "snapshot")
        new = snapshot(changed_ids, ("marker",), None, worker_id_file, databases) if changed else old

        def inspect(workspace, daemon, config, record, strict_arg):
            self.assertEqual((workspace, daemon, config, record, strict_arg),
                             (STAGE, 7, 8, self.issued["id"], strict))
            if denial:
                raise ERROR("offline_inspection_denied")
            return CANONICAL(observation(self.issued, strict))

        patches = {
            "check_boundary": lambda _workspace: None,
            "directory": directory,
            "read_stage_file": lambda *_args: (("issuance",), self.raw),
            "private_file": open_file,
            "identity": lambda _info: ("stage",),
            "inspect": inspect,
            "store_snapshot": Mock(side_effect=(old, new)),
        }
        with patch.dict(QUALIFY.__globals__, patches), patch.object(Path, "lstat", return_value=None):
            return QUALIFY(STAGE, DIGEST(b"pinned binary"), self.seal, strict)

    def test_preflight_or_postflight_change_cannot_emit_evidence(self):
        with self.assertRaisesRegex(ERROR, "^store_changed$"):
            self.run_qualification(changed=True)
        with self.assertRaisesRegex(ERROR, "^store_changed$"):
            self.run_qualification(changed=True, denial=True)
        with self.assertRaisesRegex(ERROR, "^offline_inspection_denied$"):
            self.run_qualification(denial=True)

    def test_registration_claim_is_read_only_and_self_hashed(self):
        for strict, boundary in ((False, "dedicated_worker_flock"), (True, "strict_quiescence")):
            with self.subTest(strict=strict):
                proof = self.run_qualification(strict=strict)
                self.assertTrue(proof["project_ownership_verified_read_only"])
                self.assertTrue(proof["store_identity_and_database_bytes_unchanged"])
                self.assertEqual(proof["offline_boundary"], boundary)
                self.assertIs(proof["strict_quiescence_verified"], strict)
                for flag in CODE["PRODUCTION_FLAGS"] | {
                    "production_operating_accounts_verified", "production_exclusive_mutation_verified"
                }:
                    self.assertIs(proof[flag], False)
                self.assertEqual(proof["actual_cache_deletion_requests"], 0)
                self.assertEqual(proof.pop("evidence_sha256"), DIGEST(CANONICAL(proof)))
                self.assertNotIn(str(STAGE), json.dumps(proof))


if __name__ == "__main__":
    unittest.main()
