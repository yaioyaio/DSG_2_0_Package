#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../rust-scaffold"
exec python3 - <<'PY'
import os
from pathlib import Path
import signal
import subprocess
import tempfile

def interrupt(_signum, _frame):
    raise KeyboardInterrupt

signal.signal(signal.SIGTERM, interrupt)

with tempfile.TemporaryDirectory(prefix='dsg-rust-test-') as fixture:
    target = Path(fixture) / 'target'
    os.environ.update(CARGO_BUILD_JOBS='1', CARGO_INCREMENTAL='0', RUST_TEST_THREADS='1',
                      CARGO_TARGET_DIR=str(target), PYTHONDONTWRITEBYTECODE='1')
    command = ['cargo', 'test', '--workspace', '--locked', '--offline', '--jobs', '1']
    with subprocess.Popen(command, start_new_session=(os.name != 'nt')) as runner:
        try:
            status = runner.wait(timeout=95)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            if os.name == 'nt':
                subprocess.run(['taskkill', '/T', '/F', '/PID', str(runner.pid)], timeout=8, check=False)
            else:
                try:
                    os.killpg(runner.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            runner.wait(timeout=8)
            raise SystemExit('Rust validation interrupted or exceeded 95 seconds.')
    if status:
        raise SystemExit(status)
    executable = target / 'debug' / ('dsg.exe' if os.name == 'nt' else 'dsg')
    subprocess.run(['python3', '../scripts/check-foundation-parity.py', str(executable)], timeout=25, check=True)
PY
