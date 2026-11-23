"""Exercise registered Cargo execution and Compose reuse on temporary fixtures."""

import json
import os
from pathlib import Path
import re
import secrets
import signal
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


def docker_field(arguments, timeout):
    completed = subprocess.run(
        arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, timeout=timeout, check=False,
    )
    return completed.stdout.strip() if completed.returncode == 0 else None


def compose_up(compose, project, timeout):
    # The Docker CLI starts Compose as a plugin child, so bound the process group.
    child = subprocess.Popen(
        compose + ['up', '--detach', '--no-build', '--pull', 'never', '--no-deps',
                   '--wait', '--wait-timeout', '8', 'probe'],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        cwd=project, start_new_session=True,
    )
    try:
        return child.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait(timeout=2)
        raise RuntimeError('The isolated Compose fixture start exceeded its time limit.') from None


def write_compose_fixture(path, image_id, tag, lifetime):
    path.write_text(f'''services:
  probe:
    image: "{image_id}"
    container_name: "{tag}"
    labels:
      dsg.fixture: "{tag}"
    entrypoint: ["/bin/sh", "-c"]
    command: ["exec sleep {lifetime}"]
    mem_limit: 48m
    cpus: 0.25
    pids_limit: 16
    network_mode: none
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    restart: "no"
    logging:
      driver: none
    healthcheck:
      test: [CMD, /bin/true]
      interval: 1s
      timeout: 1s
      retries: 3
''')


def verify_reuse(cli, context, project, runner_deadline):
    if os.name != 'posix' or context not in {'dsg-test', 'dockerguard-test'}:
        raise RuntimeError('The Compose fixture requires a registered isolated POSIX Docker context.')
    docker = ['docker', '--context', context]
    overrides = {'DOCKER_HOST', 'DOCKER_CONTEXT', 'DOCKER_API_VERSION', 'DOCKER_DEFAULT_PLATFORM',
                 'DOCKER_TLS', 'DOCKER_TLS_VERIFY', 'DOCKER_CERT_PATH'}
    for key in tuple(os.environ):
        if key in overrides or key.startswith('COMPOSE_'):
            del os.environ[key]

    deadline = min(time.monotonic() + 52, runner_deadline)

    def budget(limit):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('The isolated Compose fixture verification exceeded its time limit.')
        return min(limit, remaining)

    inventory = result(cli + ['docker-scan', '--context', context, '--max-resources', '64'], timeout=budget(25))
    if not inventory['complete'] or not inventory['engine_id']:
        raise RuntimeError('The isolated Docker inventory could not be verified.')
    images = {image['id']: image for image in inventory['images'] if image['digests']}
    source = None
    for reference in ('busybox:1.36', 'busybox:1.37', 'busybox:latest', 'alpine:3.20', 'alpine:3.22', 'alpine:latest'):
        inspect = docker + ['image', 'inspect', '--format']
        image_id = docker_field(inspect + ['{{.ID}}', '--', reference], budget(4))
        if image_id in images and docker_field(inspect + ['{{len .Config.Volumes}}', '--', image_id], budget(4)) == '0':
            source = images[image_id]
            break
    if source is None:
        raise RuntimeError('The isolated engine needs a preloaded digest-backed BusyBox or Alpine image without declared volumes.')

    tag = 'dsg-runner-' + secrets.token_hex(6)
    yaml_path = project / 'compose.fixture.yaml'
    compose = docker + ['compose', '--env-file', os.devnull, '--project-directory', str(project),
                        '--project-name', tag, '--file', str(yaml_path)]
    project_filter = ['--filter', f'label=com.docker.compose.project={tag}']
    fixture_filters = project_filter + ['--filter', 'label=com.docker.compose.service=probe',
                                         '--filter', f'label=dsg.fixture={tag}']
    container_list = docker + ['container', 'ls', '--all', '--no-trunc', '--format', '{{.ID}}']

    def owned_container(remaining):
        engine = docker_field(docker + ['info', '--format', '{{.ID}}'], remaining(3))
        if engine != inventory['engine_id']:
            raise RuntimeError('The isolated engine identity could not be reverified.')
        project_ids = docker_field(container_list + project_filter, remaining(3))
        fixture_ids = docker_field(container_list + fixture_filters, remaining(3))
        if project_ids is None or project_ids != fixture_ids:
            raise RuntimeError('Ownership of the isolated Compose project could not be confirmed.')
        if project_ids and not re.fullmatch(r'[0-9a-f]{64}', project_ids):
            raise RuntimeError('The isolated Compose project has an unexpected container count or ID.')
        return project_ids or None

    def remove_owned(container, remaining):
        if owned_container(remaining) != container:
            raise RuntimeError('The observed isolated fixture ID changed before removal.')
        fixture_label = docker_field(docker + ['container', 'inspect', '--format',
                                               '{{index .Config.Labels "dsg.fixture"}}',
                                               '--', container], remaining(3))
        if fixture_label != tag:
            raise RuntimeError('The fixture label could not be confirmed before removal.')
        removed = subprocess.run(docker + ['container', 'rm', '--force', '--', container],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, timeout=remaining(3), check=False)
        if removed.returncode or owned_container(remaining) is not None:
            raise RuntimeError('Isolated fixture removal could not be verified.')

    def start(lifetime):
        nonlocal start_attempted
        if owned_container(budget) != container:
            raise RuntimeError('The observed isolated fixture ID changed before startup.')
        write_compose_fixture(yaml_path, source['id'], tag, lifetime)
        timeout = budget(11)
        start_attempted = True
        if compose_up(compose, project, timeout):
            raise RuntimeError('The isolated Compose fixture did not become ready.')
        container_id = owned_container(budget)
        if container_id is None:
            raise RuntimeError('The isolated Compose fixture ID was missing after startup.')
        return container_id

    def read_config_hash(container_id):
        label = docker_field(docker + ['container', 'inspect', '--format',
                                       '{{index .Config.Labels "com.docker.compose.config-hash"}}',
                                       '--', container_id], budget(4))
        if label is None or not re.fullmatch(r'[0-9a-f]{64}', label):
            raise RuntimeError('The real Compose configuration hash could not be verified.')
        return label

    def verify(expectation, exit_code):
        args = cli + ['docker-reuse', '--context', context, '--expectation-json', json.dumps(expectation)]
        return result(args, expected=exit_code, timeout=budget(22))

    container = None
    start_attempted = False
    try:
        container = start(45)
        config_hash = read_config_hash(container)
        expectation = dict(engine_id=inventory['engine_id'], project=tag, service='probe', number=1,
                           config_hash=config_hash, image_id=source['id'], image_digest=source['digests'][0], mounts=[])
        observed = verify(expectation, 0)
        assert observed['verified'] and observed['assessment']['container_id'] == container

        repeated = start(45)
        assert repeated == container and read_config_hash(repeated) == config_hash
        observed = verify(expectation, 0)
        assert observed['verified'] and observed['assessment']['container_id'] == container

        # Remove the verified first instance before applying the changed configuration.
        original = container
        remove_owned(original, budget)
        container = None
        start_attempted = False
        container = start(46)
        assert container != original and read_config_hash(container) != config_hash
        stale = verify(expectation, 5)
        assert not stale['verified'] and stale['assessment']['reason'] == 'configuration_changed'
    finally:
        if start_attempted:
            cleanup_deadline = time.monotonic() + 10

            def cleanup_budget(limit):
                remaining = cleanup_deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError('Isolated fixture cleanup exceeded its time limit.')
                return min(limit, remaining)

            current = owned_container(cleanup_budget)
            if current is not None:
                remove_owned(container or current, cleanup_budget)
    print('Isolated Compose reuse: repeated up retained one container and the original configuration expectation was then rejected.')


if __name__ == '__main__':
    # Reserve time for Docker child termination and fixture cleanup before the outer runner timeout.
    runner_deadline = time.monotonic() + 55
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
            verify_reuse(cli, context, project, runner_deadline)
