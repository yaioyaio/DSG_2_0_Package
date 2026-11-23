#!/usr/bin/env python3
"""Exercise local release generation and portable lifecycle with two explicit binary paths."""
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile


SCRIPT = Path(__file__).resolve().with_name("release-package.py")


def run(*arguments, success=True):
    result = subprocess.run([sys.executable, "-B", str(SCRIPT), *map(str, arguments)],
                            capture_output=True, timeout=90)
    if (result.returncode == 0) != success:
        raise RuntimeError(f"{arguments[0]} returned unexpected status {result.returncode}")
    return result.stdout.decode().strip()


def checksum(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(root, binaries):
    inputs = root / "inputs"
    inputs.mkdir()
    for name, binary in zip(("dsg", "dsgd"), binaries):
        shutil.copyfile(binary, inputs / name)
    originals = [checksum(path) for path in binaries]
    prefix = root / "prefix"
    prefix.mkdir(mode=0o700)
    keep = prefix / "user-file"
    keep.write_text("preserve")
    cur = prefix / "dsg-current"
    is_mac = sys.platform == "darwin"
    arch = "aarch64" if platform.machine() == "arm64" else platform.machine()
    target = arch + ("-apple-darwin" if is_mac else "-unknown-linux-musl")
    def build(filename, version):
        archive = root / filename
        result = run("build", "--bin-dir", inputs, "--target", target, "--version", version,
                     "--output", archive, *(["--adhoc"] if is_mac else []))
        assert result == checksum(archive)
        assert archive.with_name(filename + ".sha256").read_text() == f"{result}  {filename}\n"
        return archive

    def lifecycle(command, archive, expected=True, supplied=None):
        run(command, "--prefix", prefix, "--archive", archive, "--sha256", supplied or checksum(archive),
            success=expected)

    first = build("first.tar.gz", "1.0.0")
    same = build("repeat.tar.gz", "1.0.0")
    assert checksum(first) == checksum(same)
    assert [checksum(path) for path in binaries] == originals
    cur.write_text("existing user data")
    lifecycle("install", first, False)
    assert cur.read_text() == "existing user data"
    cur.unlink()
    lifecycle("install", first, False, "0" * 64)
    assert not os.path.lexists(cur)
    for i, (name, kind) in enumerate([("../escape", tarfile.REGTYPE), ("dsg", tarfile.SYMTYPE),
                                      ("dsg", tarfile.LNKTYPE)]):
        damaged = root / f"invalid-{i}.tar.gz"
        with tarfile.open(damaged, "w:gz") as tar:
            item = tarfile.TarInfo(name)
            item.type = kind
            item.mode = 0o755
            if kind != tarfile.REGTYPE:
                item.linkname = "../user-file"
            tar.addfile(item)
        lifecycle("install", damaged, False)
        assert not (prefix / "escape").exists() and not os.path.lexists(cur)
    lifecycle("install", first)
    doctor = lambda: json.loads(run("doctor", "--prefix", prefix))
    assert doctor()["version"] == "1.0.0"
    subprocess.run([str(cur / 'dsg'), '--version'], check=True, timeout=8, stdout=subprocess.DEVNULL)
    original = cur.resolve()
    next_release = build("next.tar.gz", "1.1.0")
    extra = cur / "user-added"
    extra.write_text("preserve")
    lifecycle("upgrade", next_release, False)
    run("uninstall", "--prefix", prefix, success=False)
    assert extra.read_text() == "preserve" and cur.resolve() == original
    extra.unlink()
    os.link(cur / "dsg", prefix / "extra-link")
    run("doctor", "--prefix", prefix, success=False)
    (prefix / "extra-link").unlink()
    # Test altered archive data with a valid external archive hash and stale inner manifest.
    altered = root / "altered.tar.gz"
    with tarfile.open(first, "r:gz") as src, tarfile.open(altered, "w:gz") as out:
        for member in src:
            stream = src.extractfile(member)
            data = stream.read()
            if member.name == "dsg":
                data = bytes([data[0] ^ 1]) + data[1:]
            out.addfile(member, io.BytesIO(data))
    lifecycle("upgrade", altered, False)
    assert cur.resolve() == original
    lifecycle("upgrade", next_release)
    assert doctor()["version"] == "1.1.0" and not original.exists()
    subprocess.run([str(cur / 'dsgd'), '--help'], check=True, timeout=8, stdout=subprocess.DEVNULL)
    lifecycle("upgrade", next_release)
    for name in ("dsg", "dsgd"):
        if is_mac:
            subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(cur / name)], check=True, timeout=15)
        else:
            assert checksum(cur / name) == checksum(inputs / name)
    run("uninstall", "--prefix", prefix)
    assert list(prefix.iterdir()) == [keep] and keep.read_text() == "preserve"


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: python3 -B scripts/check-release-package.py <dsg> <dsgd>")
    with tempfile.TemporaryDirectory() as work:
        check(Path(work).resolve(), list(map(Path, sys.argv[1:])))
    print("release package lifecycle checks passed")
