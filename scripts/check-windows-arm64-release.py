#!/usr/bin/env python3
"""Qualify Windows packages in an ARM64 Windows guest on local NTFS."""
import argparse
import ctypes
from ctypes import wintypes
from dataclasses import dataclass
import json
from pathlib import Path
import re
import runpy
import subprocess
import sys
import tarfile
import tempfile


SCRIPTS = Path(__file__).resolve().parent
INSTALLER = SCRIPTS / "release-package.py"
ARM64 = "aarch64-pc-windows-msvc"
X64 = "x86_64-pc-windows-msvc"
ARCHIVE_LIMIT = 64 * 1024 * 1024
HOST_MISMATCH = b"release package: release target does not match this host"
USER_FILE = b"Windows release qualification fixture\n"


class QualificationError(ValueError):
    pass


@dataclass(frozen=True)
class Package:
    archive: Path
    sha256: str
    content: Path
    manifest_hash: str
    manifest: dict


def require(condition, message):
    if not condition:
        raise QualificationError(message)


def read_package(installer, original, root, target):
    original = original.absolute()
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.+-]*\.tar\.gz", original.name),
            "invalid archive name")
    sidecar = original.with_name(original.name + ".sha256")
    payload = installer["regular"](sidecar, 512, managed=True)
    match = re.fullmatch(rb"([0-9a-fA-F]{64})  " + re.escape(original.name.encode("ascii")) + rb"\r?\n",
                         payload)
    require(match is not None, "invalid archive checksum file")
    sha256 = match[1].decode("ascii").lower()
    data = installer["regular"](original, ARCHIVE_LIMIT, managed=True)
    require(installer["digest"](data) == sha256, "archive checksum mismatch")
    root.mkdir()
    archive = root / original.name
    archive.write_bytes(data)
    del data
    content = root / "content"
    content.mkdir()
    installer["unpack"](argparse.Namespace(archive=archive, sha256=sha256), content)
    manifest_hash, manifest = installer["inspect"](content)
    require(manifest["target"] == target, "unexpected package target")
    return Package(archive, sha256, content, manifest_hash, manifest)


def guest_environment(installer, scratch):
    filesystem = ctypes.create_unicode_buffer(32)
    volume_information = installer["windows_api"](
        "GetVolumeInformationW", wintypes.BOOL, wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD,
        wintypes.LPDWORD, wintypes.LPDWORD, wintypes.LPDWORD, wintypes.LPWSTR, wintypes.DWORD,
    )
    with installer["windows_directories"](scratch):
        require(volume_information(scratch.anchor, None, 0, None, None, None, filesystem, len(filesystem))
                and filesystem.value == "NTFS", "qualification requires a local NTFS scratch directory")
    windows_directory = installer["windows_api"]("GetWindowsDirectoryW", wintypes.UINT,
                                                 wintypes.LPWSTR, wintypes.UINT)
    system_root = ctypes.create_unicode_buffer(32768)
    require(0 < windows_directory(system_root, len(system_root)) < len(system_root),
            "Windows system directory unavailable")
    return {"SystemRoot": system_root.value, "WINDIR": system_root.value,
            "TEMP": str(scratch), "TMP": str(scratch)}


def run(environment, stage, *command, timeout=30, reject_host=False):
    try:
        result = subprocess.run(command, capture_output=True, timeout=timeout, env=environment)
    except (OSError, subprocess.TimeoutExpired):
        raise QualificationError(f"{stage} process start or timeout failure") from None
    if reject_host:
        require(result.returncode == 1 and result.stderr.strip() == HOST_MISMATCH,
                f"{stage}: expected native architecture rejection")
    else:
        require(result.returncode == 0, f"{stage}: unexpected process exit {result.returncode}")
    return result.stdout


def lifecycle(environment, command, prefix, package=None, *, reject_host=False, python=sys.executable):
    arguments = [python, "-B", str(INSTALLER), command, "--prefix", str(prefix)]
    if package is not None:
        arguments.extend(["--archive", str(package.archive), "--sha256", package.sha256])
    return run(environment, f"installer {command}", *arguments, reject_host=reject_host)


def check_binaries(environment, folder, package):
    dsg = str(folder / "dsg.exe")
    dsgd = str(folder / "dsgd.exe")
    output = run(environment, "dsg version", dsg, "--version", timeout=10)
    require(output.strip() == f"dsg {package.manifest['version']}".encode("ascii"), "unexpected CLI version")
    output = run(environment, "dsgd help", dsgd, "--help", timeout=10)
    require(b"Usage: dsgd" in output, "unexpected daemon help")
    run(environment, "dashboard", sys.executable, "-B", str(SCRIPTS / "check-dashboard.py"), dsg, timeout=50)


def installed_release(installer, environment, prefix, package):
    state = json.loads(lifecycle(environment, "doctor", prefix))
    active = Path(state["path"])
    require(active.parent == prefix and installer["RELEASE_NAME"].fullmatch(active.name) is not None,
            "unexpected active release path")
    require(state == {"status": "ok", "path": str(active), "version": package.manifest["version"],
                      "target": ARM64, "signature": package.manifest["signature"], "pending_cleanup": False},
            "unexpected installer diagnostics")
    require(installer["inspect"](active, package.manifest_hash)[1] == package.manifest,
            "unexpected installed manifest")
    require({entry.name for entry in prefix.iterdir()} == {"user-file", installer["CURRENT"], active.name},
            "unexpected installation contents")
    return active


def check_x64_python(environment, python):
    require(python.is_absolute() and python.name.lower() == "python.exe",
            "x64 Python must be an absolute path to python.exe")
    output = run(environment, "x64 Python architecture", python, "-I", "-B", "-c",
                 "import sysconfig; print(sysconfig.get_platform())")
    require(output.strip() == b"win-amd64", "x64 Python must report win-amd64")


def check_lifecycle(installer, environment, scratch, legacy, current, x64, x64_python=None):
    require(legacy.manifest["version"] == current.manifest["version"]
            and legacy.manifest_hash != current.manifest_hash,
            "ARM64 inputs must be distinct releases with the same version")
    prefix = scratch / "prefix"
    prefix.mkdir()
    sentinel = prefix / "user-file"
    sentinel.write_bytes(USER_FILE)

    interpreters = (sys.executable,) if x64_python is None else (sys.executable, x64_python)
    for python in interpreters:
        lifecycle(environment, "install", prefix, x64, reject_host=True, python=python)
        require(list(prefix.iterdir()) == [sentinel] and sentinel.read_bytes() == USER_FILE,
                "rejected x64 install changed the prefix")
    lifecycle(environment, "install", prefix, legacy)
    previous = installed_release(installer, environment, prefix, legacy)

    pointer = prefix / installer["CURRENT"]
    original_state = installer["regular"](pointer, 32768, managed=True)
    for python in interpreters:
        lifecycle(environment, "upgrade", prefix, x64, reject_host=True, python=python)
        require(installed_release(installer, environment, prefix, legacy) == previous
                and installer["regular"](pointer, 32768, managed=True) == original_state
                and sentinel.read_bytes() == USER_FILE, "rejected x64 upgrade changed the ARM64 installation")

    lifecycle(environment, "upgrade", prefix, current)
    active = installed_release(installer, environment, prefix, current)
    require(active != previous and not previous.exists(), "previous release was not retired")
    check_binaries(environment, active, current)
    lifecycle(environment, "uninstall", prefix)
    require(list(prefix.iterdir()) == [sentinel] and sentinel.read_bytes() == USER_FILE,
            "uninstall did not preserve the user file")


def main():
    cli = argparse.ArgumentParser(description=__doc__)
    for argument in ("legacy-arm64", "current-arm64", "x64"):
        cli.add_argument(f"--{argument}", required=True, type=Path,
                         help="archive path with sibling <archive>.sha256")
    cli.add_argument("--x64-python", type=Path,
                     help="absolute path to portable x64 python.exe for the installer rejection check")
    args = cli.parse_args()
    if sys.platform != "win32":
        cli.exit(1, "Windows release qualification requires ARM64 Windows.\n")

    stage = "guest preflight"
    try:
        installer = runpy.run_path(str(INSTALLER))
        installer["verify_host"](ARM64)
        with tempfile.TemporaryDirectory(prefix="dsg-win-arm64-release-") as name:
            scratch = Path(name).absolute()
            environment = guest_environment(installer, scratch)
            if args.x64_python is not None:
                stage = "x64 Python preflight"
                check_x64_python(environment, args.x64_python)
            stage = "legacy ARM64 package integrity"
            legacy = read_package(installer, args.legacy_arm64, scratch / "legacy", ARM64)
            stage = "current ARM64 package integrity"
            current = read_package(installer, args.current_arm64, scratch / "current", ARM64)
            stage = "x64 package integrity"
            x64 = read_package(installer, args.x64, scratch / "x64", X64)
            stage = "x64 emulation"
            check_binaries(environment, x64.content, x64)
            stage = "ARM64 lifecycle and x64 installer boundary"
            check_lifecycle(installer, environment, scratch, legacy, current, x64, args.x64_python)
            stage = "scratch cleanup"
    except QualificationError as error:
        cli.exit(1, f"Windows release qualification failed during {stage} ({error}).\n")
    except (OSError, ValueError, EOFError, tarfile.TarError, KeyError):
        cli.exit(1, f"Windows release qualification failed during {stage}.\n")

    print("Passed: Windows ARM64 NTFS install, same-version upgrade, diagnostics, execution, dashboard and removal.")
    print("Passed: x64 archive and PE integrity, emulated execution and dashboard, native installer rejection.")
    if args.x64_python is not None:
        print("Passed: x64 Python cannot install or upgrade the x64 package on ARM64 Windows.")
    print("Scope: Windows ARM64 guest. Native Windows x64 package lifecycle remains unverified.")


if __name__ == "__main__":
    main()
