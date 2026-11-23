#!/usr/bin/env python3
"""Exercise local release generation and portable lifecycle with two explicit binary paths."""
import argparse
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
WINDOWS_TARGETS = ("aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc")


def run(*arguments, success=True):
    result = subprocess.run([sys.executable, "-B", str(SCRIPT), *map(str, arguments)],
                            capture_output=True, timeout=90)
    if (result.returncode == 0) != success:
        raise RuntimeError(f"{arguments[0]} returned unexpected status {result.returncode}")
    return result.stdout.decode().strip()


def checksum(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def install_concurrently(prefix, archive):
    command = [sys.executable, "-B", str(SCRIPT), "install", "--prefix", str(prefix),
               "--archive", str(archive), "--sha256", checksum(archive)]
    processes = []
    try:
        for _ in range(2):
            processes.append(subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        assert sorted(process.wait(timeout=60) for process in processes) == [0, 1]
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)


def check(root, binaries, target):
    inputs = root / "inputs"
    inputs.mkdir()
    is_windows = sys.platform == "win32"
    windows_package = target in WINDOWS_TARGETS
    names = ("dsg.exe", "dsgd.exe") if windows_package else ("dsg", "dsgd")
    for name, binary in zip(names, binaries):
        shutil.copyfile(binary, inputs / name)
    originals = [checksum(path) for path in binaries]
    prefix = root / "prefix"
    prefix.mkdir(mode=0o700)
    keep = prefix / "user-file"
    keep.write_text("preserve")
    cur = prefix / "dsg-current"
    signed = target.endswith("-apple-darwin")

    def build(filename, version):
        archive = root / filename
        result = run("build", "--bin-dir", inputs, "--target", target, "--version", version,
                     "--output", archive, *(["--adhoc"] if signed else []))
        assert result == checksum(archive)
        assert archive.with_name(filename + ".sha256").read_text() == f"{result}  {filename}\n"
        return archive

    def lifecycle(command, archive, expected=True, supplied=None):
        run(command, "--prefix", prefix, "--archive", archive, "--sha256", supplied or checksum(archive),
            success=expected)

    def doctor():
        return json.loads(run("doctor", "--prefix", prefix))

    def release_directory():
        return Path(doctor()["path"]) if is_windows else cur.resolve()

    first = build("first.tar.gz", "1.0.0")
    same = build("repeat.tar.gz", "1.0.0")
    assert checksum(first) == checksum(same)
    assert [checksum(path) for path in binaries] == originals
    if windows_package:
        other = next(candidate for candidate in WINDOWS_TARGETS if candidate != target)
        run("build", "--bin-dir", inputs, "--target", other, "--version", "1.0.0",
            "--output", root / "wrong-machine.tar.gz", success=False)
        with tarfile.open(first, "r:gz") as archive:
            doc = json.load(archive.extractfile("MANIFEST.json"))
            assert doc["target"] == target and set(names) <= set(doc["files"])
            for name, original in zip(names, originals):
                assert hashlib.sha256(archive.extractfile(name).read()).hexdigest() == original
        if not is_windows:
            lifecycle("install", first, False)
            assert list(prefix.iterdir()) == [keep]
            return
    cur.write_text("existing user data")
    lifecycle("install", first, False)
    assert cur.read_text() == "existing user data"
    cur.unlink()
    lifecycle("install", first, False, "0" * 64)
    assert not os.path.lexists(cur)
    for i, (name, kind) in enumerate([("../escape", tarfile.REGTYPE), (names[0], tarfile.SYMTYPE),
                                      (names[0], tarfile.LNKTYPE)]):
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
    if is_windows:
        install_concurrently(prefix, first)
    else:
        lifecycle("install", first)
    assert doctor()["version"] == "1.0.0"
    original = release_directory()
    subprocess.run([str(original / names[0]), "--version"], check=True, timeout=8, stdout=subprocess.DEVNULL)
    next_release = build("next.tar.gz", "1.1.0")
    extra = original / "user-added"
    extra.write_text("preserve")
    lifecycle("upgrade", next_release, False)
    run("uninstall", "--prefix", prefix, success=False)
    assert extra.read_text() == "preserve"
    extra.unlink()
    assert release_directory() == original
    os.link(original / names[0], prefix / "extra-link")
    run("doctor", "--prefix", prefix, success=False)
    (prefix / "extra-link").unlink()
    if is_windows:
        junction = root / "junction"
        command = ["cmd.exe", "/d", "/c", "mklink", "/J", str(junction), str(prefix)]
        subprocess.run(command, check=True, timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            run("doctor", "--prefix", junction, success=False)
        finally:
            junction.rmdir()
        stream = Path(str(original / names[0]) + ":dsg-user-data")
        stream.write_bytes(b"preserve")
        run("uninstall", "--prefix", prefix, success=False)
        assert stream.read_bytes() == b"preserve"
        stream.unlink()
        directory_stream = Path(str(original) + ":dsg-user-data")
        directory_stream.write_bytes(b"preserve")
        run("uninstall", "--prefix", prefix, success=False)
        assert directory_stream.read_bytes() == b"preserve"
        directory_stream.unlink()
    # Test altered archive data with a valid external archive hash and stale inner manifest.
    altered = root / "altered.tar.gz"
    with tarfile.open(first, "r:gz") as src, tarfile.open(altered, "w:gz") as out:
        for member in src:
            stream = src.extractfile(member)
            data = stream.read()
            if member.name == names[0]:
                data = bytes([data[0] ^ 1]) + data[1:]
            out.addfile(member, io.BytesIO(data))
    lifecycle("upgrade", altered, False)
    assert release_directory() == original
    lifecycle("upgrade", next_release)
    assert doctor()["version"] == "1.1.0" and not original.exists()
    installed = release_directory()
    subprocess.run([str(installed / names[1]), "--help"], check=True, timeout=8, stdout=subprocess.DEVNULL)
    lifecycle("upgrade", next_release)
    for name in names:
        if signed:
            subprocess.run(["/usr/bin/codesign", "--verify", "--strict", str(installed / name)], check=True, timeout=15)
        else:
            assert checksum(installed / name) == checksum(inputs / name)
    if is_windows:
        with (installed / names[0]).open("rb"):
            lifecycle("upgrade", first, False)
            assert installed.exists()
            assert doctor()["version"] == "1.0.0" and doctor()["pending_cleanup"]
        lifecycle("upgrade", first)
        assert not installed.exists() and not doctor()["pending_cleanup"]
        installed = release_directory()
        with (installed / names[0]).open("rb"):
            run("uninstall", "--prefix", prefix, success=False)
            assert installed.exists()
    run("uninstall", "--prefix", prefix)
    assert list(prefix.iterdir()) == [keep] and keep.read_text() == "preserve"


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("--target")
    cli.add_argument("dsg", type=Path)
    cli.add_argument("dsgd", type=Path)
    args = cli.parse_args()
    architecture = platform.machine().lower()
    architecture = {"arm64": "aarch64", "amd64": "x86_64"}.get(architecture, architecture)
    suffixes = {"darwin": "apple-darwin", "linux": "unknown-linux-musl", "win32": "pc-windows-msvc"}
    target = args.target or architecture + "-" + suffixes[sys.platform]
    with tempfile.TemporaryDirectory() as work:
        check(Path(work).resolve(), [args.dsg, args.dsgd], target)
    if sys.platform != "win32" and target in WINDOWS_TARGETS:
        print("Windows archive checks passed; native Windows is required for lifecycle checks")
    else:
        print("release package lifecycle checks passed")
