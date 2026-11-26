"""Exercise registered Cargo execution and Compose reuse on temporary fixtures."""

import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time


HEALTH_MARKER = '/dev/shm/dsg-fixture-unhealthy'
MOUNT_TARGET = '/dsg-fixture-mount'


def call(arguments, timeout=8):
    return subprocess.run(arguments, capture_output=True, text=True, timeout=timeout, check=False)


def result(invocation, expected=0, timeout=12, input_json=None):
    if input_json is None:
        completed = call(invocation, timeout)
    else:
        completed = subprocess.run(invocation, input=json.dumps(input_json), capture_output=True,
                                   text=True, timeout=timeout, check=False)
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


DETACHED_FIXTURE = '''
import os
from pathlib import Path
import time

deadline = time.monotonic() + 10

def wait_for(marker):
    while time.monotonic() < deadline and not marker.exists():
        time.sleep(0.02)

if os.fork() == 0:
    os.setsid()
    Path('detached.tmp').write_text(str(os.getpid()))
    Path('detached.tmp').replace('detached.pid')
    wait_for(Path('detached.release'))
    os._exit(0)
wait_for(Path('leader.release'))
os._exit(0)
'''


def verify_detached_lease(executable, base, global_policy, crash):
    project = base / ('lease-crash' if crash else 'lease-exit')
    project.mkdir()
    state = project / 'state'
    cli = [executable, '--root', str(project), '--state-dir', str(state),
           '--global-policy', str(global_policy), '--json']
    result(cli + ['init', '--project-id', 'detached-fixture'])
    helper = project / 'fixture.py'
    helper.write_text(DETACHED_FIXTURE)
    database = state / 'registry.sqlite3'
    detached_release = project / 'detached.release'
    leader_release = project / 'leader.release'
    readiness = project / 'detached.pid'
    contender = cli + ['exec', '--timeout', '2', '--', '/usr/bin/true']

    def assert_blocked():
        blocked = call(contender, timeout=2)
        assert blocked.returncode == 2
        assert json.loads(blocked.stderr)['error']['code'] == 'RESOURCE_ACTIVE'

    def exited(pid):
        try:
            os.getpgid(pid)
        except ProcessLookupError:
            return True
        return False

    supervisor = subprocess.Popen(cli + ['exec', '--timeout', '8', '--', sys.executable, '-IB', str(helper)],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        ready_until = time.monotonic() + 3
        with sqlite3.connect(database, timeout=0.3) as db:
            while time.monotonic() < ready_until:
                # Hold the fixture's writer lock just long enough to deliver SIGKILL
                # after a complete sample, without racing the start of the next sample.
                db.execute('BEGIN IMMEDIATE')
                lease = db.execute('SELECT id, process_id, mac_observation_complete FROM execution_lease '
                                   'WHERE released_at_ms IS NULL').fetchone()
                if lease and lease[2] == 1 and readiness.is_file():
                    lease_id, leader, _ = lease
                    detached = int(readiness.read_text())
                    observed = db.execute(
                        'SELECT pid, started_seconds FROM execution_lease_process WHERE lease_id = ?',
                        (lease_id,)).fetchall()
                    if {pid for pid, started in observed if started > 0} == {leader, detached}:
                        assert os.getpgid(leader) == leader
                        assert os.getpgid(detached) == detached and detached != leader
                        if crash:
                            supervisor.kill()
                        leader_release.touch()
                        db.commit()
                        break
                db.rollback()
                time.sleep(0.02)
            else:
                raise RuntimeError('The owned detached process was not recorded before the fixture deadline.')

        stdout, _ = supervisor.communicate(timeout=3)
        if crash:
            assert supervisor.returncode == -signal.SIGKILL
        else:
            assert supervisor.returncode == 5
            report = json.loads(stdout)['result']
            assert report['reason_code'] == 'RUN_CONTROL_UNVERIFIED'
            assert not report['run']['tree_confirmed_gone'] and not report['lease_released']
        leader_deadline = time.monotonic() + 2
        while not exited(leader) and time.monotonic() < leader_deadline:
            time.sleep(0.02)
        assert exited(leader) and os.getpgid(detached) == detached
        assert_blocked()
        # Keep the production TTL and heartbeat untouched. This isolated state
        # fixture advances expiry after proving the unexpired lease blocks entry.
        with sqlite3.connect(database, timeout=0.3) as db:
            db.execute('UPDATE execution_lease SET acquired_at_ms = 0, expires_at_ms = 1 WHERE id = ?', (lease_id,))
        assert_blocked()
        assert os.getpgid(detached) == detached
        detached_release.touch()
        detached_deadline = time.monotonic() + 2
        while not exited(detached) and time.monotonic() < detached_deadline:
            time.sleep(0.02)
        assert exited(detached)
        assert result(contender, timeout=3)['lease_released']
        with sqlite3.connect(database, timeout=0.3) as db:
            assert db.execute('SELECT COUNT(*) FROM audit_event WHERE lease_id = ? '
                              "AND reason_code = 'LEASE_RECLAIMED'", (lease_id,)).fetchone()[0] == 1
    finally:
        detached_release.touch()
        leader_release.touch()
        if supervisor.poll() is None:
            supervisor.kill()
        supervisor.communicate(timeout=2)


UNOBSERVED_DESCENDANT_FIXTURE = '''
import os
from pathlib import Path
import time

deadline = time.monotonic() + 12

def wait_for(name):
    while time.monotonic() < deadline:
        if Path(name).exists():
            return True
        time.sleep(0.02)
    return False

if not wait_for('spawn'):
    os._exit(1)
launcher = os.fork()
if launcher == 0:
    if os.fork() == 0:
        os.setsid()
        with Path('artifact.txt').open('rb') as artifact:
            Path('identity.tmp').write_text(str(os.getpid()))
            Path('identity.tmp').replace('escaped.pid')
            if wait_for('prove'):
                Path('held').write_bytes(artifact.read())
                wait_for('close')
        os._exit(0)
    os._exit(0)
os.waitpid(launcher, 0)
Path('launcher-exited').touch()
os._exit(0 if wait_for('leader-release') else 1)
'''


def verify_macos_observation_gap(executable, base, global_policy):
    project = base / 'observation-gap'
    project.mkdir()
    state = project / 'state'
    cli = [executable, '--root', str(project), '--state-dir', str(state),
           '--global-policy', str(global_policy), '--json']
    result(cli + ['init', '--project-id', 'observation-gap'])
    helper = project / 'fixture.py'
    helper.write_text(UNOBSERVED_DESCENDANT_FIXTURE)
    artifact = project / 'artifact.txt'
    sentinel = b'read-only lease observation fixture\n'
    artifact.write_bytes(sentinel)
    database = state / 'registry.sqlite3'

    def await_value(observe, purpose, timeout=2):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = observe()
            if value:
                return value
            time.sleep(0.02)
        raise RuntimeError(f'The macOS observation fixture could not confirm {purpose}.')

    def observed_leader():
        with sqlite3.connect(database, timeout=0.3) as db:
            lease = db.execute('SELECT id, process_id FROM execution_lease '
                               'WHERE released_at_ms IS NULL AND mac_observation_complete = 1').fetchone()
            if lease is not None:
                registered = db.execute('SELECT pid FROM execution_lease_process WHERE lease_id = ?',
                                        (lease[0],)).fetchall()
                if registered == [(lease[1],)]:
                    return lease

    def supervisor_stopped():
        pid, status = os.waitpid(supervisor.pid, os.WNOHANG | os.WUNTRACED)
        return pid == supervisor.pid and os.WIFSTOPPED(status)

    def escaped_identity():
        if (project / 'escaped.pid').is_file() and (project / 'launcher-exited').is_file():
            pid = int((project / 'escaped.pid').read_text())
            if pid > 1 and pid != leader and os.getpgid(pid) == pid:
                return pid

    def child_exited(pid):
        try:
            os.getpgid(pid)
        except ProcessLookupError:
            return True
        return False

    escaped = None
    paused = False
    supervisor = subprocess.Popen(cli + ['exec', '--timeout', '10', '--', sys.executable, '-IB', str(helper)],
                                  stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        lease_id, leader = await_value(observed_leader, 'the initial leader-only snapshot', timeout=3)
        assert leader != supervisor.pid and os.getpgid(leader) == leader
        # Gate this owned fixture between two observations without depending on scheduler timing.
        supervisor.send_signal(signal.SIGSTOP)
        paused = True
        await_value(supervisor_stopped, 'the stopped fixture supervisor')
        (project / 'spawn').touch()
        escaped = await_value(escaped_identity, 'the live descendant in a separate session')
        parent = call(['/bin/ps', '-p', str(escaped), '-o', 'ppid='], timeout=2)
        assert parent.returncode == 0 and parent.stdout.strip() == '1'
        assert observed_leader() == (lease_id, leader)
        (project / 'leader-release').touch()
        supervisor.send_signal(signal.SIGCONT)
        paused = False
        stdout, _ = supervisor.communicate(timeout=4)
        report = json.loads(stdout)['result']
        contender = call(cli + ['exec', '--timeout', '2', '--', '/usr/bin/true'], timeout=3)
        (project / 'prove').touch()
        await_value(lambda: (project / 'held').is_file() and (project / 'held').read_bytes() == sentinel,
                    'the descendant still holding the fixture file after the contender')
        assert os.getpgid(escaped) == escaped and artifact.read_bytes() == sentinel
        with sqlite3.connect(database, timeout=0.3) as db:
            recorded = db.execute('SELECT count(*) FROM execution_lease_process WHERE lease_id = ? AND pid = ?',
                                  (lease_id, escaped)).fetchone()[0] == 1
            released = db.execute('SELECT released_at_ms IS NOT NULL FROM execution_lease WHERE id = ?',
                                  (lease_id,)).fetchone()[0] == 1
        assert released == report['lease_released']
        tree_gone = report['run']['tree_confirmed_gone']
        if tree_gone and released and supervisor.returncode == contender.returncode == 0 and not recorded:
            outcome = 'premature_release_reproduced'
        elif not tree_gone and not released and supervisor.returncode == 5 and contender.returncode == 2:
            assert json.loads(contender.stderr)['error']['code'] == 'RESOURCE_ACTIVE'
            outcome = 'lease_preserved'
        else:
            raise RuntimeError('The macOS observation fixture produced inconsistent lease decisions.')
    finally:
        for name in ('leader-release', 'prove', 'close'):
            (project / name).touch(exist_ok=True)
        if paused:
            supervisor.send_signal(signal.SIGCONT)
        if supervisor.poll() is None:
            supervisor.kill()
        supervisor.communicate(timeout=2)
        if escaped is not None:
            await_value(lambda: child_exited(escaped), 'the owned descendant exit')
    return dict(outcome=outcome, descendant_recorded=recorded, descendant_held_file=True,
                lease_released=released, tree_confirmed_gone=tree_gone, contender_exit=contender.returncode,
                fixture_file_preserved=True, fixture_processes_remaining=0)


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


def write_compose_fixture(path, image, tag, lifetime, mount_tmpfs=False, named_volume=None):
    configuration = f'''services:
  probe:
    image: "{image['reference']}"
    platform: "{image['os']}/{image['architecture']}"
    build: ./build
    init: false
    container_name: "{tag}"
    labels:
      dsg.fixture: "{tag}"
    entrypoint: ["/bin/sh", "-c"]
    command: ["exec sleep {lifetime}"]
    mem_limit: 48m
    cpus: 0.25
    pids_limit: 16
    network_mode: none
    ipc: private
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    restart: "no"
    logging:
      driver: none
    healthcheck:
      test: [CMD, /bin/sh, -c, "test ! -e {HEALTH_MARKER}"]
      interval: 1s
      timeout: 1s
      retries: 3
'''
    if mount_tmpfs:
        configuration += f'''    volumes:
      - type: tmpfs
        target: "{MOUNT_TARGET}"
        tmpfs:
          size: 65536
'''
    if named_volume is not None:
        configuration += f'''    volumes:
      - type: volume
        source: probe_data
        target: "{MOUNT_TARGET}"
        read_only: true
        volume:
          nocopy: true
volumes:
  probe_data:
    external: true
    name: "{named_volume}"
'''
    path.write_text(configuration)


def verify_reuse(cli, context, project, runner_deadline):
    if os.name != 'posix' or context not in {'dsg-test', 'dockerguard-test'}:
        raise RuntimeError('The Compose fixture requires a registered isolated POSIX Docker context.')
    pinned_image = os.environ.get('DSG_TEST_COMPOSE_IMAGE')
    if pinned_image is not None and not re.fullmatch(r'docker\.io/library/alpine@sha256:[0-9a-f]{64}', pinned_image):
        raise RuntimeError('The optional Compose image must be an official Docker Hub Alpine digest reference.')
    requested_volumes = os.environ.get('DSG_TEST_COMPOSE_VOLUMES')
    volume_engine = os.environ.get('DSG_TEST_COMPOSE_VOLUME_ENGINE_ID')
    volume_names = (None, None)
    volume_owner = None
    if requested_volumes is not None or volume_engine is not None:
        pair = re.fullmatch(r'dsg-reuse-volume-([0-9a-f]{12})-a,dsg-reuse-volume-\1-b', requested_volumes or '')
        if pair is None or not volume_engine or pinned_image is not None:
            raise RuntimeError('The named-volume fixture requires its reserved volume pair and engine ID in a separate run.')
        volume_names = tuple(requested_volumes.split(','))
        volume_owner = pair.group(1)
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

    if volume_owner is not None and docker_field(docker + ['info', '--format', '{{.ID}}'], budget(3)) != volume_engine:
        raise RuntimeError('The named-volume fixture engine identity could not be confirmed.')
    def scan_inventory(remaining):
        observed = result(cli + ['docker-scan', '--context', context, '--max-resources', '64'],
                          timeout=remaining(25))
        if not observed['complete'] or observed['context'] != context or not observed['engine_id']:
            raise RuntimeError('The isolated Docker inventory could not be verified.')
        if observed['duplicates']:
            raise RuntimeError('The isolated Docker inventory contains duplicate Compose candidates.')
        return observed

    inventory = scan_inventory(budget)
    if volume_owner is not None and inventory['engine_id'] != volume_engine:
        raise RuntimeError('The named-volume fixture engine identity changed during inventory.')

    def confirm_inventory(remaining, fixture_id=None):
        observed = scan_inventory(remaining)
        expected_containers = [entry['id'] for entry in inventory['containers']]
        if fixture_id is not None:
            expected_containers.append(fixture_id)
        if (observed['engine_id'] != inventory['engine_id']
                or sorted(entry['id'] for entry in observed['containers']) != sorted(expected_containers)
                or sorted(entry['id'] for entry in observed['images']) != sorted(entry['id'] for entry in inventory['images'])
                or sorted(observed['volumes']) != sorted(inventory['volumes'])):
            raise RuntimeError('The isolated engine resource identities or counts changed unexpectedly.')

    images = {image['id']: image for image in inventory['images'] if image['digests']}
    inspect_image = docker + ['image', 'inspect', '--format']
    volume_count = '{{len .Config.Volumes}}'
    replacement = None
    replacement_digest = None
    if pinned_image is not None:
        replacement_digest = pinned_image.split('@', 1)[1]
        replacement_id = docker_field(inspect_image + ['{{.ID}}', '--', pinned_image], budget(4))
        replacement = images.get(replacement_id)
        if (replacement is None or replacement_digest not in replacement['digests'] or replacement['os'] != 'linux'
                or replacement['architecture'] not in {'amd64', 'arm64'}
                or docker_field(inspect_image + [volume_count, '--', replacement_id], budget(4)) != '0'):
            raise RuntimeError('The isolated engine needs the preloaded, digest-backed Alpine image without declared volumes.')
        replacement = dict(replacement, reference=pinned_image, digest=replacement_digest)

    source = None
    for reference in ('busybox:1.36', 'busybox:1.37', 'busybox:latest', 'alpine:3.20', 'alpine:3.22', 'alpine:latest'):
        image_id = docker_field(inspect_image + ['{{.ID}}', '--', reference], budget(4))
        candidate = images.get(image_id)
        if candidate is None or candidate['os'] != 'linux' or candidate['architecture'] not in {'amd64', 'arm64'}:
            continue
        if replacement is not None and (image_id == replacement['id'] or replacement_digest in candidate['digests']
                                        or candidate['os'] != replacement['os']
                                        or candidate['architecture'] != replacement['architecture']):
            continue
        if docker_field(inspect_image + [volume_count, '--', image_id], budget(4)) != '0':
            continue
        repository = reference.split(':', 1)[0]
        for digest in candidate['digests']:
            digest_reference = f'docker.io/library/{repository}@{digest}'
            if docker_field(inspect_image + ['{{.ID}}', '--', digest_reference], budget(4)) == image_id:
                source = dict(candidate, reference=digest_reference, digest=digest)
                break
        if source is not None:
            break
    if source is None:
        if replacement is not None:
            raise RuntimeError('The isolated engine needs a distinct preloaded fixture image for the same platform without declared volumes.')
        raise RuntimeError('The isolated engine needs a preloaded digest-backed BusyBox or Alpine image without declared volumes.')

    tag = 'dsg-runner-' + secrets.token_hex(6)
    yaml_path = project / 'compose.fixture.yaml'
    build_input = project / 'build' / 'Dockerfile'
    build_input.parent.mkdir()
    build_input.write_text(f'FROM {source["reference"]}\nLABEL dsg.fixture.build-input="before"\n')
    compose = docker + ['compose', '--env-file', os.devnull, '--project-directory', str(project),
                        '--project-name', tag, '--file', str(yaml_path)]
    project_filter = ['--filter', f'label=com.docker.compose.project={tag}']
    fixture_filters = project_filter + ['--filter', 'label=com.docker.compose.service=probe',
                                         '--filter', f'label=dsg.fixture={tag}']
    container_list = docker + ['container', 'ls', '--all', '--no-trunc', '--format', '{{.ID}}']

    def confirm_volume(name, remaining):
        projection = ('{{.Name}} {{.Driver}} {{.Scope}} {{len .Options}} '
                      '{{index .Labels "dsg.fixture.reuse-volume"}}')
        observed = docker_field(docker + ['volume', 'inspect', '--format', projection, '--', name], remaining(3))
        if observed != f'{name} local local 0 {volume_owner}':
            raise RuntimeError('The dedicated external fixture volume identity or ownership could not be verified.')

    def confirm_unused_volumes(remaining):
        for name in volume_names:
            confirm_volume(name, remaining)
            attached = docker_field(container_list + ['--filter', f'volume={name}'], remaining(3))
            if attached != '':
                raise RuntimeError('The dedicated external fixture volumes could not be confirmed as unused.')

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

    def confirm_owned(container, remaining):
        if owned_container(remaining) != container:
            raise RuntimeError('The observed isolated fixture ID changed.')
        fixture_label = docker_field(docker + ['container', 'inspect', '--format',
                                               '{{index .Config.Labels "dsg.fixture"}}',
                                               '--', container], remaining(3))
        if fixture_label != tag:
            raise RuntimeError('The isolated fixture label could not be confirmed.')

    def remove_owned(container, remaining):
        confirm_owned(container, remaining)
        removed = subprocess.run(docker + ['container', 'rm', '--force', '--', container],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, timeout=remaining(3), check=False)
        if removed.returncode or owned_container(remaining) is not None:
            raise RuntimeError('Isolated fixture removal could not be verified.')

    def start(lifetime, image, mount_tmpfs=False, named_volume=None):
        nonlocal start_attempted
        if owned_container(budget) != container:
            raise RuntimeError('The observed isolated fixture ID changed before startup.')
        if named_volume is not None:
            confirm_volume(named_volume, budget)
        write_compose_fixture(yaml_path, image, tag, lifetime, mount_tmpfs, named_volume)
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

    def verify(expectation, exit_code, projection=None):
        args = cli + ['docker-reuse', '--context', context, '--expectation-json', json.dumps(expectation)]
        if projection is not None:
            args.append('--compose-projection-stdin')
        return result(args, expected=exit_code, timeout=budget(22), input_json=projection)

    def expect_candidate(expectation, container_id):
        observed = verify(expectation, 0)
        if not observed['verified'] or observed['assessment']['container_id'] != container_id:
            raise RuntimeError('The isolated Compose reuse candidate ID could not be verified.')

    def expect_refusal(expectation, status, reason):
        observed = verify(expectation, 5)
        if (observed['verified'] or observed['assessment']['status'] != status
                or observed['assessment']['reason'] != reason
                or observed['reason'] != {'code': 'initial_unverified', 'assessment_reason': reason}):
            raise RuntimeError(f'The isolated Compose fixture did not return {status}/{reason} in both observations.')

    def expect_projection(expectation, container_id, baseline, desired, assessment):
        same = assessment['status'] == 'same'
        observed = verify(expectation, 0 if same else 5, dict(
            baseline_config_hash=expectation['config_hash'], baseline=baseline, desired=desired,
        ))
        if observed['verified'] != same or observed['compose'] != assessment:
            raise RuntimeError('The isolated Compose projection did not return the expected assessment.')
        verified = observed['baseline']
        if assessment['status'] == 'unknown':
            if (verified['verified'] or verified['assessment']['reason'] != 'not_healthy'
                    or verified['reason'] != {'code': 'initial_unverified', 'assessment_reason': 'not_healthy'}):
                raise RuntimeError('The isolated Compose projection did not preserve the unverified baseline.')
        elif not verified['verified'] or verified['assessment']['container_id'] != container_id:
            raise RuntimeError('The isolated Compose projection baseline did not verify the same container ID.')

    def confirm_fixture_mount(container_id, image_id, mount):
        confirm_owned(container_id, budget)
        projection = ('{{.Image}} {{.State.Health.Status}}'
                      '{{range .Mounts}} {{.Type}}:{{if eq .Type "volume"}}{{.Name}}{{end}}:{{.Destination}}:{{.RW}}{{end}}')
        kind = mount['kind']
        name = mount['volume'] or ''
        writable = str(not mount['read_only']).lower()
        expected = f'{image_id} healthy {kind}:{name}:{mount["destination"]}:{writable}'
        if kind == 'volume':
            legacy_read_only = json.dumps(f'{name}:{MOUNT_TARGET}:ro,nocopy')
            legacy_no_copy = json.dumps(f'{name}:{MOUNT_TARGET}:nocopy,ro')
            projection += ('{{range .HostConfig.Mounts}}{{if eq .Type "volume"}}'
                           '{{with .VolumeOptions}}{{if .NoCopy}} nocopy{{end}}{{end}}{{end}}{{end}}'
                           '{{range .HostConfig.Binds}}{{if or (eq . ' + legacy_read_only + ')'
                           ' (eq . ' + legacy_no_copy + ')}} nocopy{{end}}{{end}}')
            expected += ' nocopy'
        observed = docker_field(docker + ['container', 'inspect', '--format', projection,
                                          '--', container_id], budget(4))
        if observed != expected:
            raise RuntimeError('The isolated container image, health or mount could not be verified.')

    def trigger_unhealthy(container_id):
        # The marker lives in this container's private shared memory and is removed with it.
        confirm_owned(container_id, budget)
        marked = docker_field(docker + ['container', 'exec', '--', container_id, '/bin/sh', '-c',
                                        f': > {HEALTH_MARKER}'], budget(3))
        if marked != '':
            raise RuntimeError('The isolated fixture health marker could not be written.')
        health_deadline = time.monotonic() + 6
        while time.monotonic() < health_deadline:
            status = docker_field(docker + ['container', 'inspect', '--format', '{{.State.Health.Status}}',
                                            '--', container_id], budget(1))
            if status == 'unhealthy':
                return
            if status != 'healthy':
                raise RuntimeError('The isolated fixture health could not be checked.')
            time.sleep(0.25)
        raise RuntimeError('The isolated fixture did not become unhealthy within its time limit.')

    original_mounts, changed_mounts = [], []
    if volume_owner is not None:
        confirm_unused_volumes(budget)
        original_mounts, changed_mounts = (
            [{'kind': 'volume', 'volume': name, 'destination': MOUNT_TARGET, 'read_only': True}]
            for name in volume_names
        )

    container = None
    start_attempted = False
    try:
        container = start(45, source, named_volume=volume_names[0])
        config_hash = read_config_hash(container)
        expectation = dict(engine_id=inventory['engine_id'], project=tag, service='probe', number=1,
                           config_hash=config_hash, image_id=source['id'], image_digest=source['digest'],
                           mounts=original_mounts)
        if volume_owner is not None:
            confirm_fixture_mount(container, source['id'], original_mounts[0])
        expect_candidate(expectation, container)

        repeated = start(45, source, named_volume=volume_names[0])
        assert repeated == container and read_config_hash(repeated) == config_hash
        expect_candidate(expectation, container)

        service = dict(image=source['reference'], platform=f'{source["os"]}/{source["architecture"]}',
                       init=False, read_only=True, volumes=[
                           dict(type='volume', source=mount['volume'], target=mount['destination'],
                                read_only=mount['read_only'], volume=dict(nocopy=True))
                           for mount in original_mounts
                       ])
        baseline = dict(service=service, build_inputs_sha256=hashlib.sha256(build_input.read_bytes()).hexdigest())
        expect_projection(expectation, container, baseline, baseline, dict(status='same'))
        expect_projection(expectation, container, baseline, dict(baseline, service=dict(service, init=True)),
                          dict(status='changed', action='recreate'))
        build_input.write_text(f'FROM {source["reference"]}\nLABEL dsg.fixture.build-input="after"\n')
        desired = dict(baseline, build_inputs_sha256=hashlib.sha256(build_input.read_bytes()).hexdigest())
        expect_projection(expectation, container, baseline, desired, dict(status='changed', action='rebuild'))

        trigger_unhealthy(container)
        unhealthy = verify(expectation, 5)
        assert not unhealthy['verified'] and unhealthy['assessment']['reason'] == 'not_healthy'
        expect_projection(expectation, container, baseline, desired, dict(status='unknown', reason='baseline_unverified'))
        confirm_inventory(budget, fixture_id=container)

        # Remove the verified first instance before applying the changed configuration.
        original = container
        remove_owned(original, budget)
        container = None
        start_attempted = False
        container = start(46, replacement or source, mount_tmpfs=replacement is not None, named_volume=volume_names[1])
        changed_hash = read_config_hash(container)
        assert container != original and changed_hash != config_hash
        if volume_owner is not None:
            expect_refusal(expectation, 'rejected', 'configuration_changed')
            confirm_fixture_mount(container, source['id'], changed_mounts[0])
            expect_refusal(dict(expectation, config_hash=changed_hash), 'rejected', 'mount_changed')
            expect_candidate(dict(expectation, config_hash=changed_hash, mounts=changed_mounts), container)
            confirm_owned(container, budget)
        elif replacement is None:
            stale = verify(expectation, 5)
            assert not stale['verified'] and stale['assessment']['reason'] == 'configuration_changed'
        else:
            expect_refusal(expectation, 'rejected', 'configuration_changed')
            mounted = {'kind': 'tmpfs', 'volume': None, 'destination': MOUNT_TARGET, 'read_only': False}
            confirm_fixture_mount(container, replacement['id'], mounted)
            changed = dict(expectation, config_hash=changed_hash, image_id=replacement['id'],
                           image_digest=replacement['digest'])
            expect_refusal(dict(changed, image_id=source['id'], image_digest=source['digest'], mounts=[mounted]),
                           'rejected', 'image_changed')
            expect_refusal(changed, 'rejected', 'mount_changed')
            expect_refusal(dict(changed, mounts=[mounted]), 'unknown', 'unverified_mount')
            confirm_owned(container, budget)
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
            if volume_owner is not None:
                confirm_unused_volumes(cleanup_budget)
    confirm_inventory(budget)
    print('Isolated Compose reuse: repeated up retained one container; health and configuration changes were rejected.')
    print('Isolated Compose projection: same, changed/recreate, changed/rebuild and unknown/baseline_unverified passed.')
    counts = ', '.join(f'{key}={len(inventory[key])}' for key in ('containers', 'images', 'volumes', 'duplicates'))
    print(f'Isolated Compose inventory before and after fixture cleanup matched by identity: {counts}.')
    if replacement is not None:
        print('Isolated Compose image and mount changes were rejected, and matching tmpfs stayed unverified.')
    if volume_owner is not None:
        print('Isolated Compose named-volume reuse passed, the changed mount was rejected, and both external volumes were preserved.')


if __name__ == '__main__':
    # Reserve time for Docker child termination and fixture cleanup before the outer runner timeout.
    runner_deadline = time.monotonic() + 55
    executable = str(Path(sys.argv[1]).resolve())
    context = sys.argv[2] if len(sys.argv) > 2 else None
    lease_only = context == '--macos-lease-only'
    gap_only = context == '--macos-observation-gap-only'
    with tempfile.TemporaryDirectory(prefix='dsg-managed-test-') as fixture:
        base = Path(fixture)
        project = base / 'fixture'
        project.mkdir()
        global_policy = base / 'policy.json'
        global_policy.write_text('{"host_min_free_gb":0,"project_budget_gb":1}')
        if (lease_only or gap_only) and sys.platform != 'darwin':
            raise SystemExit('Detached managed lease fixtures require macOS.')
        if gap_only:
            print(json.dumps(verify_macos_observation_gap(executable, base, global_policy), sort_keys=True))
            sys.exit(0)
        if sys.platform == 'darwin' and (lease_only or context is None):
            for crash in (False, True):
                verify_detached_lease(executable, base, global_policy, crash)
            print('Managed execution (macOS): detached process blocked expired lease after normal and forced supervisor exit.')
            if lease_only:
                sys.exit(0)
        state = base / 'state'
        cli = [executable, '--root', str(project), '--state-dir', str(state), '--global-policy', str(global_policy), '--json']
        result(cli + ['init', '--project-id', 'managed-fixture'])
        if sys.platform.startswith(('darwin', 'linux')):
            os.environ['CARGO_TARGET_DIR'] = str(project / 'target')
            verify_cargo(cli, project)
        if context:
            verify_reuse(cli, context, project, runner_deadline)
