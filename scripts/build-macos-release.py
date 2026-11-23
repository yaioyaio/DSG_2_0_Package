#!/usr/bin/env python3
"""Build a local macOS release from locked dependencies in a private target folder."""

import argparse
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory


ROOT = Path(__file__).resolve().parents[1]


def compile_release(target):
    arguments = ['cargo', 'build', '--manifest-path', 'rust-scaffold/Cargo.toml',
                 '--bins', '--release', '--locked', '--offline', '--jobs', '1']
    environment = os.environ.copy()
    environment.update({
        'CARGO_TARGET_DIR': str(target),
        'CARGO_INCREMENTAL': '0',
        'CARGO_ENCODED_RUSTFLAGS': '\x1f'.join((
            '--remap-path-prefix', f'{ROOT}=/dsg/source',
            '--remap-path-prefix', f'{target}=/dsg/target',
        )),
    })
    with subprocess.Popen(arguments, cwd=ROOT, env=environment, start_new_session=True,
                          stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL) as build:
        try:
            status = build.wait(timeout=105)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            try:
                os.killpg(build.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            build.wait(timeout=8)
            raise SystemExit('macOS release build interrupted or timed out') from None
        if status != 0:
            raise SystemExit(f'macOS release build failed with exit {status}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    architecture = {'arm64': 'aarch64', 'x86_64': 'x86_64'}.get(platform.machine())
    if sys.platform != 'darwin' or architecture is None:
        parser.error('a supported macOS host is required')
    output = args.output.absolute()
    if not output.name.endswith('.tar.gz') or not output.parent.is_dir():
        parser.error('output must be a tar.gz file in an existing directory')
    if os.path.lexists(output) or os.path.lexists(output.with_name(output.name + '.sha256')):
        parser.error('the archive and its SHA-256 sidecar must be new files')
    with TemporaryDirectory(prefix='dsg-macos-release-') as temporary:
        target = Path(temporary).resolve() / 'target'
        compile_release(target)
        binaries = target / 'release'
        actual = subprocess.check_output([str(binaries / 'dsg'), '--version'], text=True, timeout=8).strip()
        if actual != f'dsg {args.version}':
            parser.error('release version must match the compiled CLI version')
        subprocess.run(
            [sys.executable, '-B', str(ROOT / 'scripts/release-package.py'), 'build',
             '--bin-dir', str(binaries), '--version', args.version, '--target', f'{architecture}-apple-darwin',
             '--output', str(output), '--adhoc'],
            check=True, timeout=45,
        )


if __name__ == '__main__':
    main()
