#!/usr/bin/env python3
"""Verify a DSG archive and its exact internal manifest with a caller-pinned Ed25519 key."""
import argparse
import json
import os
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tarfile
import tempfile


SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")
SPKI_SIZE = len(SPKI_PREFIX) + 32
SIDECAR_LIMIT = 4096
MANIFEST_LIMIT = 16384
SIDECAR_FIELDS = {"format", "algorithm", "key_id", "archive_sha256", "manifest_sha256",
                  "archive_signature_hex", "manifest_signature_hex"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def pin(value):
    require(re.fullmatch(r"[0-9a-fA-F]{64}", value), "expected a SHA-256 pin")
    return value.lower()


def bounded_file(package, name, limit):
    path = Path(name).absolute()
    package["checked_dir"](path.parent)
    return package["regular"](path, limit)


def read_signatures(package, path, archive_sha256, key_id):
    raw = bounded_file(package, path, SIDECAR_LIMIT)
    sidecar = json.loads(raw)
    require(isinstance(sidecar, dict) and sidecar.keys() == SIDECAR_FIELDS
            and raw == package["manifest_bytes"](sidecar)
            and type(sidecar["format"]) is int and sidecar["format"] == 1
            and sidecar["algorithm"] == "Ed25519", "invalid detached signatures format")
    require(sidecar["archive_sha256"] == archive_sha256 and sidecar["key_id"] == key_id,
            "detached signatures do not match the archive and key pins")
    for field, length in (("manifest_sha256", 64), ("archive_signature_hex", 128),
                          ("manifest_signature_hex", 128)):
        value = sidecar[field]
        require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]+", value) and len(value) == length,
                "invalid detached signature or manifest digest encoding")
    return sidecar


def public_verifier_path(value):
    path = Path(value)
    require(path.is_absolute(), "an explicit absolute OpenSSL path is required")
    path = path.resolve(strict=True)
    require(path.is_file() and os.access(path, os.X_OK)
            and (sys.platform != "win32" or path.suffix.lower() == ".exe"),
            "OpenSSL must be an executable file")
    return path


def verifier_environment(work):
    config = work / "openssl.cnf"
    config.touch()
    modules = work / "modules"
    modules.mkdir()
    environment = {"LC_ALL": "C", "OPENSSL_CONF": str(config),
                   "OPENSSL_MODULES": str(modules), "OPENSSL_ENGINES": str(modules)}
    if sys.platform == "win32" and "SystemRoot" in os.environ:
        environment["SystemRoot"] = os.environ["SystemRoot"]
    return environment


def verify_message(openssl, work, environment, message, signature):
    detached = work / "signature.bin"
    detached.write_bytes(bytes.fromhex(signature))
    command = [str(openssl), "pkeyutl", "-provider", "default", "-verify", "-pubin", "-keyform", "DER",
               "-inkey", str(work / "public.der"), "-rawin", "-in", str(message), "-sigfile", str(detached)]
    result = subprocess.run(command, cwd=work, env=environment, stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    require(result.returncode == 0, "Ed25519 signature rejected")


def read_manifest(package, archive, archive_sha256, folder):
    folder.mkdir(mode=0o700)
    package["unpack"](argparse.Namespace(archive=archive, sha256=archive_sha256), folder)
    raw = package["regular"](folder / package["MANIFEST"], MANIFEST_LIMIT)
    manifest = package["metadata"](raw)
    require(manifest.keys() == {"format", "version", "target", "signature", "files"}
            and type(manifest["format"]) is int and raw == package["manifest_bytes"](manifest),
            "invalid canonical package manifest")
    require({path.name for path in folder.iterdir()} == set(manifest["files"]) | {package["MANIFEST"]},
            "package members do not match the manifest")
    for name, expected in manifest["files"].items():
        require(package["digest"](package["regular"](folder / name)) == expected,
                "package member digest does not match the manifest")
    return raw, manifest


def verify(args):
    """Return verification evidence without invoking target code or reporting publisher identity."""
    package = runpy.run_path(str(Path(__file__).with_name("release-package.py")))
    try:
        archive_sha256 = pin(args.sha256)
        key_id = pin(args.public_key_sha256)
        openssl = public_verifier_path(args.openssl)
        public_key = bounded_file(package, args.public_key, SPKI_SIZE)
        require(len(public_key) == SPKI_SIZE and public_key.startswith(SPKI_PREFIX)
                and package["digest"](public_key) == key_id, "public Ed25519 SPKI key pin mismatch")
        sidecar = read_signatures(package, args.signatures, archive_sha256, key_id)
        with tempfile.TemporaryDirectory(prefix="dsg-release-signature-") as temp:
            work = Path(temp).resolve()
            archive = work / "release.tar.gz"
            data = bounded_file(package, args.archive, package["LIMIT"])
            require(package["digest"](data) == archive_sha256, "archive SHA-256 pin mismatch")
            archive.write_bytes(data)
            del data
            (work / "public.der").write_bytes(public_key)
            environment = verifier_environment(work)
            verify_message(openssl, work, environment, archive, sidecar["archive_signature_hex"])
            raw, manifest = read_manifest(package, archive, archive_sha256, work / "package")
            manifest_sha256 = package["digest"](raw)
            require(manifest_sha256 == sidecar["manifest_sha256"], "manifest SHA-256 mismatch")
            verify_message(openssl, work, environment, work / "package" / package["MANIFEST"],
                           sidecar["manifest_signature_hex"])
    except (OSError, UnicodeError, EOFError, RecursionError, tarfile.TarError, subprocess.SubprocessError) as error:
        raise ValueError("could not read or verify bounded release evidence") from error
    return {"signature_verified": True, "authority": "caller-pinned-key", "archive_sha256": archive_sha256,
            "manifest_sha256": manifest_sha256, "key_id": key_id, "target": manifest["target"],
            "version": manifest["version"], "platform_signature_declared": manifest["signature"]}


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--archive", required=True)
    cli.add_argument("--sha256", required=True, help="independently approved archive SHA-256")
    cli.add_argument("--signatures", required=True, help="detached signatures JSON")
    cli.add_argument("--public-key", required=True, help="Ed25519 public key in SPKI DER encoding")
    cli.add_argument("--public-key-sha256", required=True, help="independently approved SPKI DER SHA-256")
    cli.add_argument("--openssl", required=True, help="absolute path to caller-trusted OpenSSL 3 executable")
    args = cli.parse_args()
    try:
        result = verify(args)
    except ValueError as error:
        cli.exit(1, f"release signature: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
