#!/usr/bin/env python3
"""Prepare a release for an external signer or assemble verified Ed25519 signatures."""
import argparse
import json
from pathlib import Path
import runpy
import subprocess
import tarfile
import tempfile


def publish(package, output, data):
    path = Path(output).absolute()
    package["checked_dir"](path.parent)
    with tempfile.TemporaryDirectory(prefix=".dsg-signing-", dir=path.parent) as temp:
        staged = Path(temp) / "output"
        staged.write_bytes(data)
        package["publish"](staged, path)


def prepare(args, package, verifier):
    archive_sha256 = verifier["pin"](args.sha256)
    with tempfile.TemporaryDirectory(prefix="dsg-signing-request-") as temp:
        raw, manifest = verifier["read_manifest"](
            package, args.archive, archive_sha256, Path(temp).resolve() / "package")
    publish(package, args.manifest_output, raw)
    return {"archive_sha256": archive_sha256, "manifest_sha256": package["digest"](raw),
            "target": manifest["target"], "version": manifest["version"]}


def assemble(args, package, verifier):
    sidecar = {"format": 1, "algorithm": "Ed25519", "key_id": verifier["pin"](args.public_key_sha256),
               "archive_sha256": verifier["pin"](args.sha256),
               "manifest_sha256": verifier["pin"](args.manifest_sha256)}
    for kind, path in (("archive", args.archive_signature), ("manifest", args.manifest_signature)):
        raw = verifier["bounded_file"](package, path, 64)
        verifier["require"](len(raw) == 64, "expected a raw 64-byte Ed25519 signature")
        sidecar[f"{kind}_signature_hex"] = raw.hex()
    data = package["manifest_bytes"](sidecar)
    with tempfile.TemporaryDirectory(prefix="dsg-signing-response-") as temp:
        staged = Path(temp).resolve() / "signatures.json"
        staged.write_bytes(data)
        args.signatures = staged
        evidence = verifier["verify"](args)
    publish(package, args.output, data)
    return evidence


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    commands = cli.add_subparsers(dest="command", required=True)
    request = commands.add_parser("prepare", help="export the exact internal manifest for signing")
    response = commands.add_parser("assemble", help="verify and publish externally produced signatures")
    for action in (request, response):
        action.add_argument("--archive", required=True)
        action.add_argument("--sha256", required=True, help="independently approved archive SHA-256")
    request.add_argument("--manifest-output", required=True)
    response.add_argument("--manifest-sha256", required=True, help="independently approved manifest SHA-256")
    response.add_argument("--archive-signature", required=True, help="raw 64-byte Ed25519 signature")
    response.add_argument("--manifest-signature", required=True, help="raw 64-byte Ed25519 signature")
    response.add_argument("--public-key", required=True, help="Ed25519 public key in SPKI DER encoding")
    response.add_argument("--public-key-sha256", required=True, help="independently approved SPKI DER SHA-256")
    response.add_argument("--openssl", required=True, help="absolute path to caller-trusted OpenSSL 3 executable")
    response.add_argument("--macos-team-id", help="require the pinned Apple Developer ID Application team")
    response.add_argument("--output", required=True)
    args = cli.parse_args()
    try:
        directory = Path(__file__).parent
        package = runpy.run_path(str(directory / "release-package.py"))
        verifier = runpy.run_path(str(directory / "verify-release-signature.py"))
        operation = prepare if args.command == "prepare" else assemble
        evidence = operation(args, package, verifier)
    except ValueError as error:
        cli.exit(1, f"release signing handoff: {error}\n")
    except (OSError, EOFError, RecursionError, tarfile.TarError, subprocess.SubprocessError):
        cli.exit(1, "release signing handoff: could not read, verify, or publish bounded release files\n")
    print(json.dumps(evidence, sort_keys=True))


if __name__ == "__main__":
    main()
