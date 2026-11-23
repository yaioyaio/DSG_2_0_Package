#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../rust-scaffold"
exec python3 - <<'PY'
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
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
    dashboard_counts = None
    compose_fixture = os.environ.get('DSG_TEST_COMPOSE_FIXTURE') == '1'
    if compose_fixture and docker_context not in {'dsg-test', 'dockerguard-test'}:
        raise SystemExit('Compose fixture requires a registered isolated test context.')
    if docker_context:
        if docker_context not in {'dsg-test', 'dockerguard-test'}:
            raise SystemExit('Real inventory smoke requires a registered isolated test context.')
        project = Path(fixture) / 'inventory-fixture'
        project.mkdir()
        context_output = subprocess.run([str(executable), '--json', 'docker-contexts'],
                                        capture_output=True, text=True, timeout=8, check=True)
        contexts = json.loads(context_output.stdout)['result']
        assert contexts['complete'] and docker_context in contexts['contexts']
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
        if docker_context == 'dockerguard-test':
            dashboard_counts = {
                'container_count': len(inventory['containers']),
                'image_count': len(inventory['images']),
                'volume_count': len(inventory['volumes']),
                'duplicate_groups': len(inventory['duplicates']),
            }
        cache_output = subprocess.run([str(executable), '--json', 'docker-build-cache',
                                       '--context', docker_context],
                                      capture_output=True, text=True, timeout=14, check=True)
        cache = json.loads(cache_output.stdout)['result']
        assert cache['context'] == docker_context and cache['engine_id'] == inventory['engine_id']
        assert cache['status'] == 'complete' and cache['disposition'] == 'preserve'
        print(f"Embedded builder cache: total={cache['total_count']}, active={cache['active_count']}, preserved.")
        if inventory['images']:
            args = [str(executable), '--root', str(project), '--state-dir', str(project / '.dsg'), '--json']
            preview = subprocess.run(args + ['gc-plan', '--docker-context', docker_context,
                                             '--docker-engine', inventory['engine_id'],
                                             '--docker-image', inventory['images'][0]['id']],
                                     capture_output=True, text=True, timeout=30, check=True)
            image_plan = json.loads(preview.stdout)['result']
            assert image_plan['plan']['docker_images']['disposition'] == 'preserve'
            print('Isolated Docker image plan: selected engine observed and existing image preserved.')
    invocation = ['python3', '../scripts/check-managed-execution.py', str(executable)]
    if compose_fixture:
        invocation.append(docker_context)
    subprocess.run(invocation, timeout=70, check=True)
    if os.environ.get('DSG_TEST_BUILD_PROVIDERS') == '1':
        subprocess.run(['python3', '-IB', '../scripts/check-build-providers.py', str(executable)],
                       timeout=150, check=True)
    subprocess.run(['python3', '../scripts/check-cleanup.py', str(executable)], timeout=65, check=True)
    subprocess.run(['python3', '../scripts/check-agent-integration.py', str(executable)], timeout=155, check=True)
    daemon = target / 'debug' / ('dsgd.exe' if os.name == 'nt' else 'dsgd')
    subprocess.run(['python3', '../scripts/check-enforcement.py', str(daemon)], timeout=75, check=True)
    if os.environ.get('DSG_TEST_LIVE_ENFORCEMENT') == '1':
        subprocess.run(['python3', '-B', '../scripts/check-live-enforcement.py', str(daemon)],
                       timeout=55, check=True)
    dashboard_environment = os.environ.copy()
    dashboard_environment.pop('DSG_TEST_DASHBOARD_COUNTS', None)
    dashboard_timeout = 35
    if os.environ.get('DSG_TEST_WEBKIT') == '1':
        if sys.platform != 'darwin':
            raise SystemExit('WebKit rendering verification currently requires macOS.')
        renderer = Path(fixture) / 'dashboard-render-check'
        subprocess.run(['/usr/bin/xcrun', 'swiftc', '-module-cache-path', str(Path(fixture) / 'swift-cache'),
                        '../scripts/check-dashboard-rendering.swift', '-o', str(renderer)],
                       timeout=45, check=True)
        dashboard_environment['DSG_TEST_RENDERER'] = str(renderer)
        if dashboard_counts is not None:
            dashboard_environment['DSG_TEST_DASHBOARD_COUNTS'] = json.dumps(dashboard_counts)
            dashboard_timeout = 105
    subprocess.run(['python3', '-B', '../scripts/check-dashboard.py', str(executable)],
                   env=dashboard_environment, timeout=dashboard_timeout, check=True)
    if os.name == 'posix':
        subprocess.run(['python3', '-B', '../scripts/check-release-package.py', str(executable), str(daemon)],
                       timeout=70, check=True)
PY
