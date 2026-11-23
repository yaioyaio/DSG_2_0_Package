#!/usr/bin/env python3
"""Exercise detached release verification with disposable local Ed25519 fixtures."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import tempfile


VERIFY = Path(__file__).resolve().with_name("verify-release-signature.py")
RELEASE_PACKAGE = VERIFY.with_name("release-package.py")
TARGET = "aarch64-unknown-linux-musl"
PAYLOAD = {"Cargo.lock": b"lock fixture\n", "SOURCES.md": b"source fixture\n",
           "dsg": b"cli fixture\n", "dsgd": b"daemon fixture\n"}


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return (json.dumps(value, sort_keys=True) + "\n").encode()


def make_archive(path, manifest, payload=PAYLOAD):
    with path.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, content in sorted({**payload, "MANIFEST.json": manifest}.items()):
                member = tarfile.TarInfo(name)
                member.size = len(content)
                member.mode = 0o755 if name in {"dsg", "dsgd"} else 0o644
                archive.addfile(member, io.BytesIO(content))


def fixture_command(openssl, *arguments):
    subprocess.run([str(openssl), *map(str, arguments)], check=True, timeout=15,
                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def make_key(root, openssl, label):
    signing_key = root / f"fixture-{label}.pem"
    public_key = root / f"fixture-{label}.der"
    fixture_command(openssl, "genpkey", "-algorithm", "Ed25519", "-out", signing_key)
    fixture_command(openssl, "pkey", "-in", signing_key, "-pubout", "-outform", "DER", "-out", public_key)
    return signing_key, public_key


def sign(root, openssl, signing_key, path):
    signature = root / "fixture.sig"
    fixture_command(openssl, "pkeyutl", "-sign", "-rawin", "-inkey", signing_key,
                    "-in", path, "-out", signature)
    return signature.read_bytes().hex()


def run(openssl, archive, sidecar, key, *, archive_pin, key_pin, succeeds):
    result = subprocess.run([sys.executable, "-B", str(VERIFY), "--archive", str(archive),
                             "--sha256", archive_pin, "--signatures", str(sidecar), "--public-key", str(key),
                             "--public-key-sha256", key_pin, "--openssl", str(openssl)],
                            capture_output=True, timeout=40)
    if succeeds:
        if result.returncode != 0:
            raise RuntimeError(f"valid signature fixture rejected with exit status {result.returncode}")
        return json.loads(result.stdout)
    if result.returncode != 1 or result.stdout or not result.stderr.startswith(b"release signature: "):
        raise RuntimeError("invalid signature fixture did not fail closed")
    return None


def check(root, openssl):
    signing_key, public_key = make_key(root, openssl, "original")
    _, other_key = make_key(root, openssl, "alternate")
    key_pin = sha256(public_key.read_bytes())
    manifest = {"format": 1, "version": "1.0.0", "target": TARGET, "signature": "none",
                "files": {name: sha256(data) for name, data in PAYLOAD.items()}}
    archive = root / "release.tar.gz"
    manifest_path = root / "manifest.json"
    signatures = root / "signatures.json"
    archive_pin = None

    def create_fixture(manifest_bytes, payload=PAYLOAD):
        nonlocal archive_pin
        make_archive(archive, manifest_bytes, payload)
        archive_pin = sha256(archive.read_bytes())
        manifest_path.write_bytes(manifest_bytes)
        return {"format": 1, "algorithm": "Ed25519", "key_id": key_pin,
                "archive_sha256": archive_pin, "manifest_sha256": sha256(manifest_bytes),
                "archive_signature_hex": sign(root, openssl, signing_key, archive),
                "manifest_signature_hex": sign(root, openssl, signing_key, manifest_path)}

    def verify(*, succeeds=False, verifier=openssl, key=public_key, key_sha256=key_pin, archive_sha256=None):
        return run(verifier, archive, signatures, key, archive_pin=archive_sha256 or archive_pin,
                   key_pin=key_sha256, succeeds=succeeds)

    original = create_fixture(canonical(manifest))
    signatures.write_bytes(canonical(original))
    result = verify(succeeds=True)
    if result != {"signature_verified": True, "authority": "caller-pinned-key", "key_id": key_pin,
                  "archive_sha256": archive_pin, "manifest_sha256": original["manifest_sha256"],
                  "version": "1.0.0", "target": TARGET, "platform_signature_declared": "none"}:
        raise RuntimeError("valid fixture returned incorrect evidence")

    verify(archive_sha256="0" * 64)
    verify(key=other_key)
    verify(verifier=Path(openssl.name))
    other_key_pin = sha256(other_key.read_bytes())
    signatures.write_bytes(canonical({**original, "key_id": other_key_pin}))
    verify(key=other_key, key_sha256=other_key_pin)
    for field, value in (("archive_signature_hex", "0" * 128), ("manifest_signature_hex", "0" * 128),
                         ("manifest_sha256", "0" * 64)):
        signatures.write_bytes(canonical({**original, field: value}))
        verify()
    signatures.write_bytes(canonical(original) + b" " * 4096)
    verify()
    signatures.unlink()
    verify()

    altered = bytearray(archive.read_bytes())
    altered[-1] ^= 1
    archive.write_bytes(altered)
    signatures.write_bytes(canonical({**original, "archive_sha256": sha256(altered)}))
    verify(archive_sha256=sha256(altered))

    signatures.write_bytes(canonical(create_fixture(canonical(manifest).strip())))
    verify()
    signatures.write_bytes(canonical(create_fixture(canonical(manifest), {**PAYLOAD, "dsg": b"changed\n"})))
    verify()

    suffix = {"darwin": "apple-darwin", "linux": "unknown-linux-musl"}.get(sys.platform)
    architecture = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x86_64"}.get(platform.machine())
    if suffix and architecture:
        prefix = root / "install"
        prefix.mkdir(mode=0o700)

        def manage(action, *options, status=0):
            command = [sys.executable, "-B", str(RELEASE_PACKAGE), action, "--prefix", str(prefix), *options]
            result = subprocess.run(command, capture_output=True, timeout=40)
            if result.returncode != status:
                raise RuntimeError(f"package {action} returned unexpected status {result.returncode}")
            return result.stdout

        def install_options():
            return ("--archive", str(archive), "--sha256", archive_pin, "--signatures", str(signatures),
                    "--public-key", str(public_key), "--public-key-sha256", key_pin, "--openssl", str(openssl))

        local_manifest = {**manifest, "target": f"{architecture}-{suffix}"}
        signatures.write_bytes(canonical(create_fixture(canonical(local_manifest))))
        manage("install", *install_options()[:-2], status=1)
        if list(prefix.iterdir()):
            raise RuntimeError("incomplete signature options touched the installation")
        manage("install", *install_options())
        upgrade = create_fixture(canonical({**local_manifest, "version": "1.1.0"}))
        signatures.write_bytes(canonical({**upgrade, "manifest_signature_hex": "0" * 128}))
        manage("upgrade", *install_options(), status=1)
        if json.loads(manage("doctor"))["version"] != "1.0.0":
            raise RuntimeError("rejected upgrade changed the active version")
        signatures.write_bytes(canonical(upgrade))
        manage("upgrade", *install_options())
        if json.loads(manage("doctor"))["version"] != "1.1.0":
            raise RuntimeError("verified upgrade failed to activate")
        manage("uninstall")
        if list(prefix.iterdir()):
            raise RuntimeError("verified installation was not fully removed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--openssl", required=True, type=Path)
    args = parser.parse_args()
    if not args.openssl.is_absolute() or not args.openssl.is_file():
        parser.error("an explicit absolute OpenSSL executable path is required")
    with tempfile.TemporaryDirectory(prefix="dsg-signature-check-") as path:
        check(Path(path).resolve(), args.openssl)
    print("release signature fixtures passed")


if __name__ == "__main__":
    main()
