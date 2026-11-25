import json
import os
from pathlib import Path
import runpy
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("buildkit-freshness-authority.py")
AUTHORITY = runpy.run_path(str(SCRIPT))
EXPECTED = {
    "version": 1,
    "registration": "a" * 64,
    "scope_sha256": "b" * 64,
    "checkpoint_sha256": "c" * 64,
}
NONCE = b"d" * 64
REQUEST = b" ".join((b"DSG-FRESHNESS-1", EXPECTED["registration"].encode(),
                     EXPECTED["scope_sha256"].encode(), EXPECTED["checkpoint_sha256"].encode(), NONCE)) + b"\n"
APPROVAL = b"DSG-FRESHNESS-1 CLAIMED " + NONCE + b"\n"


class FreshnessAuthorityTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="dsg-freshness-", dir=Path("/tmp").resolve())
        self.addCleanup(temporary.cleanup)
        self.state = Path(temporary.name)
        self.expected = self.state / "expected.json"
        self.claimed = self.state / "claimed"
        self.provision(json.dumps(EXPECTED).encode())

    def provision(self, raw, mode=0o400):
        self.expected.unlink(missing_ok=True)
        self.expected.write_bytes(raw)
        self.expected.chmod(mode)

    def invoke(self, frame, state=None):
        return subprocess.run(
            [sys.executable, SCRIPT, "--state-dir", state or self.state, "--stdio"],
            input=frame, capture_output=True, timeout=5,
        )

    def assert_denied(self, frame=REQUEST, state=None):
        result = self.invoke(frame, state)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (1, b"", b""))

    def test_replay_preserves_provisioned_expectation_and_original_marker(self):
        original_expected = self.expected.stat().st_ino, self.expected.read_bytes()
        result = self.invoke(REQUEST)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, APPROVAL, b""))
        self.assertEqual(stat.S_IMODE(self.claimed.stat().st_mode), 0o400)
        original_marker = self.claimed.stat().st_ino, self.claimed.read_bytes()

        self.assert_denied(REQUEST.replace(NONCE, b"e" * 64))
        self.assertEqual((self.expected.stat().st_ino, self.expected.read_bytes()), original_expected)
        self.assertEqual((self.claimed.stat().st_ino, self.claimed.read_bytes()), original_marker)

    def test_wrong_witness_components_and_invalid_frames_create_no_claim(self):
        malformed = (
            REQUEST.replace(b"a" * 64, b"e" * 12),
            REQUEST.replace(b"b" * 64, b"e" * 64),
            REQUEST.replace(b"c" * 64, b"e" * 64),
            REQUEST.replace(NONCE, b"D" * 64),
            REQUEST[:-1],
            REQUEST + b"\n",
            REQUEST + REQUEST,
            REQUEST + b"x" * 1024,
        )
        for frame in malformed:
            with self.subTest(length=len(frame)):
                self.assert_denied(frame)
                self.assertFalse(self.claimed.exists())

    def test_missing_or_untrusted_expectation_never_creates_state(self):
        self.expected.unlink()
        self.assert_denied()
        self.assertEqual(list(self.state.iterdir()), [])
        raw = json.dumps(EXPECTED).encode()
        for invalid, mode in ((raw, 0o600), (raw, 0o444), (raw + b" " * 512, 0o400),
                              (b'{"version":1,' + raw[1:], 0o400),
                              (raw.replace(b'1,', b'true,', 1), 0o400)):
            with self.subTest(mode=mode, length=len(invalid)):
                self.provision(invalid, mode)
                self.assert_denied()
                self.assertFalse(self.claimed.exists())

        self.expected.unlink()
        target = self.state / "operator-record"
        target.write_bytes(raw)
        target.chmod(0o400)
        self.expected.symlink_to(target)
        self.assert_denied()
        self.assertFalse(self.claimed.exists())
        self.expected.unlink()
        os.link(target, self.expected)
        self.assert_denied()
        self.assertFalse(self.claimed.exists())

    def test_protection_of_state_path_and_claim_file(self):
        self.state.chmod(0o750)
        self.assert_denied()
        self.state.chmod(0o700)
        nested = self.state / "nested"
        nested.mkdir(mode=0o700)
        alias = self.state / "alias"
        alias.symlink_to(nested, target_is_directory=True)
        self.assert_denied(state=alias)
        self.assertEqual(list(nested.iterdir()), [])
        self.claimed.symlink_to("absent")
        self.assert_denied()
        self.assertTrue(self.claimed.is_symlink())
        self.assertFalse((self.state / "absent").exists())

    def test_failed_sync_sends_no_approval_and_marker_prevents_retry(self):
        directory = AUTHORITY["open_state_directory"](self.state)
        self.addCleanup(os.close, directory)
        for failure in (1, 2):
            client, server = socket.socketpair()
            with self.subTest(sync_number=failure), client, server:
                self.claimed.unlink(missing_ok=True)
                client.sendall(REQUEST)
                client.shutdown(socket.SHUT_WR)
                with patch("os.fsync", side_effect=[None] * (failure - 1) + [OSError()]):
                    self.assertIsNone(AUTHORITY["serve_connection"](server, directory))
                server.close()
                self.assertEqual(client.recv(1024), b"")
                self.assertTrue(self.claimed.exists())
                self.assert_denied()

    def test_unix_listener_uses_protected_socket_and_one_approval(self):
        endpoint = self.state / "authority.sock"
        outside = self.state.parent / endpoint.name
        args = [sys.executable, SCRIPT, "--state-dir", self.state, "--socket"]
        denied = subprocess.run([*args, outside], capture_output=True, timeout=5)
        self.assertEqual((denied.returncode, denied.stdout, denied.stderr), (1, b"", b""))

        with subprocess.Popen([*args, endpoint], stdout=subprocess.PIPE, stderr=subprocess.PIPE) as authority:
            try:
                self.assertEqual(self.request_unix(authority, endpoint, REQUEST + b"\n"), b"")
                self.assertEqual(stat.S_IMODE(endpoint.stat().st_mode), 0o600)
                self.assertFalse(self.claimed.exists())
                self.assertEqual(self.request_unix(authority, endpoint, REQUEST), APPROVAL)
                self.assertEqual(authority.communicate(timeout=5), (b"", b""))
                self.assertEqual(authority.returncode, 0)
            finally:
                if authority.poll() is None:
                    authority.terminate()
                    authority.wait(timeout=5)
        self.assertFalse(endpoint.exists())
        self.assertTrue(self.claimed.exists())

    def request_unix(self, authority, endpoint, frame):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(4)
            deadline = time.monotonic() + 4
            while True:
                try:
                    client.connect(str(endpoint))
                    break
                except (FileNotFoundError, ConnectionRefusedError):
                    if authority.poll() is not None or time.monotonic() >= deadline:
                        raise
                    time.sleep(0.01)
            client.sendall(frame)
            client.shutdown(socket.SHUT_WR)
            with client.makefile("rb") as response:
                return response.read(1024)


if __name__ == "__main__":
    unittest.main()
