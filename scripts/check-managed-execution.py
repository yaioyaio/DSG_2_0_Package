"""Exercise registered Cargo execution and Compose reuse on temporary fixtures."""

import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time


def call(arguments, timeout=8):
    return subprocess.run(arguments, capture_output=True, text=True, timeout=timeout, check=False)


def result(invocation, expected=0, timeout=12):
    completed = call(invocation, timeout)
    if completed.returncode != expected or not completed.stdout:
        raise RuntimeError(f'{invocation[0].rsplit(os.sep, 1)[-1]} returned {completed.returncode}; expected {expected}.')
    return json.loads(completed.stdout)['result']


def verify_cargo(cli, project):
    (project / 'src').mkdir()
    (project / 'Cargo.toml').write_text('[package]\nname = "dsg_fixture"\nversion = "0.1.0"\nedition = "2021"\n')
    (project / 'src/main.rs').write_text('fn main() {}\n')
    run = result(cli + ['exec', '--timeout', '20', '--', 'cargo', 'check', '--offline', '--jobs', '1'], timeout=25)
    assert run['run']['outcome'] == 'success' and run['lease_released']
    assert (project / 'target' / 'debug').exists()
    timed = result(cli + ['exec', '--timeout', '1', '--', 'sleep', '3'], expected=124)
    assert timed['run']['outcome'] == 'timed_out' and timed['lease_released']
    following = result(cli + ['exec', '--timeout', '3', '--', 'true'])
    assert following['run']['outcome'] == 'success' and following['lease_released']
    print('Managed execution: Cargo fixture, timeout and same-project lease reacquisition passed.')


def verify_reuse(cli, context):
    docker = ['docker', '--context', context]
    for key in ('DOCKER_HOST', 'DOCKER_CONTEXT', 'DOCKER_API_VERSION'):
        os.environ.pop(key, None)
    inventory = result(cli + ['docker-scan', '--context', context, '--max-resources', '64'], timeout=25)
    assert inventory['complete']
    images = {image['id']: image for image in inventory['images'] if image['digests']}
    source = None
    for reference in ('busybox:1.36', 'busybox:1.37', 'busybox:latest', 'alpine:3.20', 'alpine:3.22', 'alpine:latest', 'redis:7-alpine'):
        known = call(docker + ['image', 'inspect', '--format', '{{.ID}}', '--', reference])
        if known.returncode == 0 and known.stdout.strip() in images:
            source = images[known.stdout.strip()]
            break
    if source is None:
        raise RuntimeError('The isolated engine needs a preloaded digest-backed public BusyBox or Alpine fixture image.')

    tag = 'dsg-runner-' + secrets.token_hex(6)
    config_hash = 'a' * 64
    selector = docker + ['container', 'ls', '--all', '--no-trunc', '--filter', f'label=dsg.fixture={tag}', '--format', '{{.ID}}']
    assert not call(selector).stdout.strip()
    try:
        started = call(docker + [
            'run', '--detach', '--name', tag, '--label', f'dsg.fixture={tag}',
            '--label', f'com.docker.compose.project={tag}', '--label', 'com.docker.compose.service=probe',
            '--label', 'com.docker.compose.container-number=1', '--label', 'com.docker.compose.oneoff=False',
            '--label', f'com.docker.compose.config-hash={config_hash}', '--memory', '48m',
            '--cpus', '0.25', '--pids-limit', '16', '--network', 'none', '--read-only',
            '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges', '--restart', 'no',
            '--log-driver', 'none', '--health-cmd', 'true', '--health-interval', '1s',
            '--health-timeout', '1s', '--health-retries', '3', '--pull', 'never',
            '--entrypoint', '/bin/sh', source['id'], '-c', 'exec sleep 50',
        ], timeout=12)
        if started.returncode:
            raise RuntimeError('The isolated Docker fixture did not start.')
        container = started.stdout.strip()
        assert len(container) == 64 and all(character in '0123456789abcdef' for character in container)
        healthy_deadline = time.monotonic() + 12
        while time.monotonic() < healthy_deadline:
            health = call(docker + ['container', 'inspect', '--format', '{{with .State.Health}}{{.Status}}{{end}}', '--', container], timeout=3)
            if health.returncode == 0 and health.stdout.strip() == 'healthy':
                break
            time.sleep(0.25)
        else:
            raise RuntimeError('The isolated Docker fixture did not become healthy.')

        expectation = dict(engine_id=inventory['engine_id'], project=tag, service='probe', number=1,
                           config_hash=config_hash, image_id=source['id'], image_digest=source['digests'][0], mounts=[])
        def verify(expected, exit_code):
            args = cli + ['docker-reuse', '--context', context, '--expectation-json', json.dumps(expected)]
            return result(args, expected=exit_code, timeout=22)

        for _ in range(2):
            observed = verify(expectation, 0)
            assert observed['verified'] and observed['assessment']['container_id'] == container
        assert not verify(dict(expectation, config_hash='b' * 64), 5)['verified']
        listing = call(selector)
        assert listing.returncode == 0 and listing.stdout.splitlines() == [container]
        print('Isolated Compose reuse: same healthy container returned twice, changed hash rejected, additional containers=0.')
    finally:
        listing = call(selector)
        if listing.returncode:
            raise RuntimeError('Unable to enumerate the isolated fixture for cleanup.')
        for container in listing.stdout.splitlines():
            assert len(container) == 64 and all(character in '0123456789abcdef' for character in container)
            removed = call(docker + ['container', 'rm', '--force', '--', container])
            assert removed.returncode == 0, 'Isolated fixture removal failed.'
        after = call(selector)
        assert after.returncode == 0 and not after.stdout.strip()


if __name__ == '__main__':
    executable = str(Path(sys.argv[1]).resolve())
    context = sys.argv[2] if len(sys.argv) > 2 else None
    with tempfile.TemporaryDirectory(prefix='dsg-managed-test-') as fixture:
        base = Path(fixture)
        project = base / 'fixture'
        project.mkdir()
        global_policy = base / 'policy.json'
        global_policy.write_text('{"host_min_free_gb":0,"project_budget_gb":1}')
        state = base / 'state'
        cli = [executable, '--root', str(project), '--state-dir', str(state), '--global-policy', str(global_policy), '--json']
        result(cli + ['init', '--project-id', 'managed-fixture'])
        if sys.platform.startswith(('darwin', 'linux')):
            os.environ['CARGO_TARGET_DIR'] = str(project / 'target')
            verify_cargo(cli, project)
        if context:
            verify_reuse(cli, context)
