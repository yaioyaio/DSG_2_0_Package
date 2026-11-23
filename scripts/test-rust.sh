#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../rust-scaffold"
exec python3 - <<'PY'
import json
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
    docker_context = os.environ.get('DSG_TEST_DOCKER_CONTEXT')
    compose_fixture = os.environ.get('DSG_TEST_COMPOSE_FIXTURE') == '1'
    if compose_fixture and docker_context not in {'dsg-test', 'dockerguard-test'}:
        raise SystemExit('Compose fixture requires a registered isolated test context.')
    if docker_context:
        if docker_context not in {'dsg-test', 'dockerguard-test'}:
            raise SystemExit('Real inventory smoke requires a registered isolated test context.')
        project = Path(fixture) / 'inventory-fixture'
        project.mkdir()
        smoke = subprocess.run([
            str(executable), '--root', str(project), '--json', 'docker-scan',
            '--context', docker_context, '--max-resources', '64',
        ], capture_output=True, text=True, timeout=25, check=False)
        if not smoke.stdout:
            raise SystemExit(f'Isolated Docker inventory exit {smoke.returncode}.')
        inventory = json.loads(smoke.stdout)['result']
        if not smoke.returncode == 0 or not inventory['complete']:
            print(f'Isolated Docker inventory incomplete. Issue count: {len(inventory["issues"])}.')
            raise SystemExit(1)
        assert inventory['context'] == docker_context and inventory['engine_id']
        print('Isolated Docker inventory:', ', '.join(f'{key}={len(inventory[key])}'
              for key in ('containers', 'images', 'volumes', 'duplicates')))
    invocation = ['python3', '../scripts/check-managed-execution.py', str(executable)]
    if compose_fixture:
        invocation.append(docker_context)
    subprocess.run(invocation, timeout=70, check=True)
    subprocess.run(['python3', '../scripts/check-cleanup.py', str(executable)], timeout=65, check=True)
PY
