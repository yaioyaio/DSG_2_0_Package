import hashlib
import os
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch


CHECKER = Path(__file__).with_name("check-dedicated-buildkit-fence.py")
API = runpy.run_path(str(CHECKER))
PAYLOAD = b"# Isolated DSG project-source test\n"
DIGEST = hashlib.sha256(PAYLOAD).hexdigest()
REVISION = "a" * 40


class RootStat:
    """Simulate root ownership while keeping the filesystem observations real."""

    st_uid = 0

    def __init__(self, actual):
        self.actual = actual

    def __getattr__(self, name):
        return getattr(self.actual, name)


class ProjectSourceTest(unittest.TestCase):
    def setUp(self):
        self.workspace = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.source = self.workspace / "README.md"
        self.source.write_bytes(PAYLOAD)
        self.source.chmod(0o400)

    def read(self, **overrides):
        params = dict(workspace=self.workspace, source=self.source, checksum=DIGEST, project_id="dsg", revision=REVISION)
        params.update(overrides)
        fstat, stat = os.fstat, os.stat
        with patch.object(os, "fstat", side_effect=lambda fd: RootStat(fstat(fd))), patch.object(
            os, "stat", side_effect=lambda *args, **kwargs: RootStat(stat(*args, **kwargs))
        ):
            return API["read_project_source"](**params)

    def test_pinned_bytes_and_revision_are_returned_as_isolated_evidence(self):
        payload, evidence = self.read()
        self.assertEqual(payload, PAYLOAD)
        self.assertEqual(evidence["evidence_scope"], "isolated_project_source")
        self.assertEqual(evidence["project_id"], "dsg")
        self.assertEqual(evidence["project_revision"], REVISION)
        self.assertEqual(evidence["project_source_name"], "README.md")
        self.assertEqual(evidence["project_source_sha256"], DIGEST)
        self.assertEqual(evidence["project_source_bytes"], len(PAYLOAD))
        for field in ("production_deployment_verified", "exact_mutation_authorized", "receipt_storage_verified"):
            self.assertIs(evidence[field], False)

    def test_source_selectors_fail_closed(self):
        for field, value, reason in (
            ("checksum", "0" * 64, "project_source_digest"),
            ("revision", "a" * 39, "project_identity"),
            ("project_id", "other", "project_identity"),
            ("source", self.workspace / "other" / self.source.name, "project_source_path"),
        ):
            with self.subTest(field=field), self.assertRaisesRegex(AssertionError, reason):
                self.read(**{field: value})

    def test_writable_source_is_rejected(self):
        self.source.chmod(0o600)
        with self.assertRaisesRegex(AssertionError, "project_source_permissions"):
            self.read()

    def test_hard_link_and_symlink_are_rejected(self):
        alias = self.workspace / "ALIAS.md"
        os.link(self.source, alias)
        with self.assertRaisesRegex(AssertionError, "project_source_boundary"):
            self.read()
        self.source.unlink()
        self.source.symlink_to(alias)
        with self.assertRaises(OSError):
            self.read()

    def test_source_size_is_bounded(self):
        self.source.chmod(0o600)
        self.source.write_bytes(b"x" * (API["PROJECT_SOURCE_LIMIT"] + 1))
        self.source.chmod(0o400)
        with self.assertRaisesRegex(AssertionError, "project_source_size"):
            self.read()


class ProjectArgumentTest(unittest.TestCase):
    def test_three_positional_arguments_keep_the_synthetic_default(self):
        with patch("sys.argv", [str(CHECKER), "/isolated", "daemon", "client"]):
            self.assertEqual(API["arguments"](), (Path("/isolated"), "daemon", "client", None))

    def test_project_options_are_all_required_together(self):
        required = [str(CHECKER), "/isolated", "daemon", "client"]
        source = "/isolated/README.md"
        options = ["--project-source", source, "--project-source-sha256", DIGEST,
                   "--project-id", "dsg", "--project-revision", REVISION]
        with patch("sys.argv", required + options):
            self.assertEqual(API["arguments"]()[3], (Path(source), DIGEST, "dsg", REVISION))
        with patch("sys.argv", required + options[:-2]), self.assertRaisesRegex(ValueError, "project_arguments"):
            API["arguments"]()


if __name__ == "__main__":
    unittest.main()
