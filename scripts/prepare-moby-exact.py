#!/usr/bin/env python3
"""Prepare a new Moby source tree with the DSG exact image overlay."""
import argparse
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import tarfile

TAG = "docker-v29.1.3"
COMMIT = "fbf3ed25f893e6ce21336f1101590e40a13934f4"
ARCHIVE_URL = f"https://codeload.github.com/moby/moby/tar.gz/refs/tags/{TAG}"
ARCHIVE_SHA256 = "a3999adc2099b493d924fe7e1d07dfb4c7cb2887ea5c84c4de95554ee9ac8a07"
ARCHIVE_LIMIT = 64 << 20
EXPANDED_LIMIT = 512 << 20
MEMBER_LIMIT = 120_000
OVERLAY_FILE_LIMIT = 1 << 20
OVERLAY_PACKAGES = ("cmd/dockerd", "daemon/dsgimage")


def sensitive_path(path):
    suffixes = (".env", ".crt", ".key", ".pem", ".pfx", ".p12", ".der")
    for name in path.parts:
        name = name.lower()
        if name.endswith(suffixes) or ".env." in name:
            return True
        if ".deploy." in name and name.endswith("env"):
            return True
        if name in {"id_dsa", "id_ecdsa", "id_ed25519", "id_rsa"}:
            return True
    return False


def read_archive(archive):
    if sensitive_path(archive):
        raise SystemExit("invalid Moby source archive path")
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    with os.fdopen(os.open(archive, flags), "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > ARCHIVE_LIMIT:
            raise SystemExit("invalid Moby source archive file")
        data = handle.read(ARCHIVE_LIMIT + 1)
    if len(data) > ARCHIVE_LIMIT or hashlib.sha256(data).hexdigest() != ARCHIVE_SHA256:
        raise SystemExit(f"official Moby {TAG} source archive digest mismatch")
    return data


def extract_source(data, destination):
    total_bytes = 0
    with tarfile.open(fileobj=io.BytesIO(data), mode="r|gz") as tar:
        for count, item in enumerate(tar, 1):
            total_bytes += item.size
            if count > MEMBER_LIMIT or item.size < 0 or total_bytes > EXPANDED_LIMIT:
                raise SystemExit("Moby source extraction limit exceeded")
            path = PurePosixPath(item.name)
            if path.is_absolute() or ".." in path.parts or path.parts[:1] != (f"moby-{TAG}",):
                raise SystemExit("invalid upstream source path")
            if item.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or sensitive_path(path):
                continue
            if len(path.parts) < 2:
                raise SystemExit("invalid upstream source member")
            target = destination.joinpath(*path.parts[1:])
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            with tar.extractfile(item) as source, target.open("xb") as output:
                shutil.copyfileobj(source, output, length=1 << 20)


def overlay_source(destination):
    overlay = Path(__file__).resolve().parents[1] / "integrations/moby-exact"
    for package in OVERLAY_PACKAGES:
        directory = overlay / package
        if directory.resolve() != directory:
            raise SystemExit("invalid DSG Moby overlay path")
        sources = sorted(directory.glob("dsg_*.go"))
        if not 1 <= len(sources) <= 64:
            raise SystemExit("invalid DSG Moby overlay file count")
        target = destination / package
        target.mkdir(mode=0o700, parents=True, exist_ok=True)
        for source in sources:
            info = source.lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > OVERLAY_FILE_LIMIT:
                raise SystemExit("invalid DSG Moby overlay file")
            with source.open("rb") as handle, (target / source.name).open("xb") as output:
                shutil.copyfileobj(handle, output, length=1 << 20)


def prepare(archive, destination):
    data = read_archive(archive)
    destination.mkdir(mode=0o700)
    extract_source(data, destination)
    overlay_source(destination)
    daemon = destination / "daemon/command/daemon.go"
    source = daemon.read_text()
    anchor = "\t// Create the daemon root before we create ANY other files"
    if source.count(anchor) != 1:
        raise SystemExit("pinned Moby daemon root anchor mismatch")
    guard = (
        '\tif runtime.GOOS == "linux" && runtime.GOARCH == "arm64" {\n'
        '\t\troot, err := filepath.EvalSymlinks(cli.Config.Root)\n'
        '\t\tif err == nil {\n'
        '\t\t\t_, err := os.Lstat(filepath.Join(filepath.Dir(root), "dsg-dedicated-image-store"))\n'
        '\t\t\tif !os.IsNotExist(err) {\n'
        '\t\t\t\treturn errors.New("dedicated DSG image storage cannot start the standard daemon")\n'
        '\t\t\t}\n'
        '\t\t} else if !os.IsNotExist(err) {\n'
        '\t\t\treturn err\n'
        '\t\t}\n'
        '\t}\n\n'
    )
    daemon.write_text(source.replace(anchor, guard + anchor))
    print(f"prepared official Moby {TAG} ({COMMIT}) with the DSG exact image overlay")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, help=f"pinned source archive from {ARCHIVE_URL}")
    parser.add_argument("destination", type=Path, help="new source directory in caller-owned scratch")
    args = parser.parse_args()
    prepare(args.archive, args.destination)
