"""Verify Linux systemd and broker boundaries inside a disposable Docker guest."""

import argparse
from collections import Counter
from contextlib import ExitStack
import http.client
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import socket
from socketserver import UnixStreamServer
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit


ENV = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'LANG': 'C'}
MAX_BODY = 1 << 20
MAX_DIAGNOSTICS = 64 << 10
ENGINE_ID = re.compile(r'[A-Za-z0-9:-]{8,128}')
DOCKER_ROUTES = {'/_ping', '/version', '/info', '/containers/json', '/images/json', '/volumes'}
FAILURE_SIGNALS = {
    'daemon-start': rb'failed to start daemon',
    'runtime-binary': rb'(?:runc|shim)[^\n]*(?:not found|no such file)|executable file not found',
    'config': rb'(?:config|directive)[^\n]*(?:unknown|invalid|multiple)|unknown flag|unable to configure',
    'containerd-connection': rb'(?:dial|connect)[^\n]*containerd|containerd[^\n]*(?:deadline exceeded|connection refused)',
    'storage-driver': rb'error initializing graphdriver|failed to mount overlay|driver not supported',
    'network-controller': rb'error initializing network controller|failed to register "bridge"|failed to create nat chain',
    'read-only-filesystem': rb'read-only file system',
    'docker-runtime-dir': rb'(?:/var)?/run/docker[/ :\x22]',
    'docker-data-dir': rb'/var/lib/docker[/ :\x22]',
    'sysctl-dir': rb'/proc/sys/',
    'permission': rb'permission denied|operation not permitted',
    'missing-path': rb'no such file or directory|executable file not found',
    'socket-bind': rb'(?:listen unix|bind:)[^\n]*(?:invalid argument|address already in use)',
    'broker-unavailable': rb'dsgd_unavailable',
}
PROBE = r'''
import http.client
import json
import os
import socket
import sys

expected_uid, path, method, route = sys.argv[1:]
if os.geteuid() != int(expected_uid) or os.getgroups():
    raise SystemExit(3)
with socket.socket(socket.AF_UNIX) as conn:
    conn.settimeout(8)
    try:
        conn.connect(path)
    except PermissionError:
        print(json.dumps([-1, ""]))
        raise SystemExit(0)
    try:
        conn.sendall(f"{method} {route} HTTP/1.1\r\nHost: localhost\r\n\r\n".encode())
        reply = http.client.HTTPResponse(conn)
        reply.begin()
        body = reply.read(1048577)
        if len(body) > 1048576:
            raise SystemExit(4)
        print(json.dumps([reply.status, body.decode("ascii")]))
    except (BrokenPipeError, ConnectionResetError, http.client.RemoteDisconnected):
        print(json.dumps([0, ""]))
'''


class CheckFailed(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise CheckFailed(message)


def command(label, *args, timeout=10, check=True, **kwargs):
    try:
        result = subprocess.run(args, env=ENV, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, timeout=timeout, **kwargs)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CheckFailed(f'{label}: {type(exc).__name__}') from None
    if check and result.returncode:
        raise CheckFailed(f'{label}: exit {result.returncode}')
    return result


def guest_tool(name):
    binary = shutil.which(name, path=ENV['PATH'])
    require(binary, f'Guest tool unavailable: {name}')
    return binary


def executable(value, label, from_media=False):
    path = Path(value)
    require(path.is_absolute(), f'{label} must be an absolute path')
    path = path.resolve(strict=True)
    require(path.is_file() and os.access(path, os.X_OK), f'{label} is not executable')
    if from_media:
        require(os.statvfs(path).f_flag & os.ST_RDONLY, f'{label} must be on read-only media')
    return path


def wait_for(description, condition, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = condition()
        if result:
            return result
        time.sleep(0.15)
    raise CheckFailed(description)


class Diagnostics:
    """Read bounded output from a private pipe and emit only fixed labels and counts."""

    def __init__(self, path, stack):
        self.path = path
        os.mkfifo(path, 0o600)
        self.fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
        stack.callback(self.close)
        self.sample = bytearray()

    def collect(self):
        remaining = MAX_DIAGNOSTICS - len(self.sample)
        if self.fd is not None and remaining:
            try:
                self.sample.extend(os.read(self.fd, remaining))
            except BlockingIOError:
                pass

    def close(self):
        try:
            self.collect()
        finally:
            os.close(self.fd)
            self.fd = None

    def summary(self):
        self.collect()
        sample = self.sample.lower()
        categories = []
        for label, pattern in FAILURE_SIGNALS.items():
            count = len(re.findall(pattern, sample))
            if count:
                categories.append(f'{label}={count}')
        categories.append(f'sampled-bytes={len(sample)}')
        if len(sample) == MAX_DIAGNOSTICS:
            categories.append('sample-limit=1')
        return ', '.join(categories)


class Unit:
    def __init__(self, systemctl, name, pending):
        self.systemctl = systemctl
        self.name = name
        self.pending = pending
        self.diagnostics = None

    def control(self, operation, *args, check=True):
        return command(f'{self.name} {operation}', self.systemctl, operation, *args, self.name,
                       check=check)

    def status(self):
        if self.diagnostics is not None:
            self.diagnostics.collect()
        raw = self.control('show', '--property=LoadState,ActiveState,SubState,MainPID,NRestarts',
                           check=False).stdout.decode('ascii')
        return dict(line.split('=', 1) for line in raw.splitlines() if '=' in line)

    def start(self, stack, runner, args, memory, tasks, home, properties=(), binary_dir=None):
        require(self.status().get('LoadState') == 'not-found', f'{self.name} already exists')
        self.diagnostics = Diagnostics(home.parent / f'{self.name}.output', stack)
        self.pending.add(self.name)
        stack.callback(self.stop)
        path = ENV['PATH'] if binary_dir is None else f'{binary_dir}:{ENV["PATH"]}'
        limits = ('User=root', 'Group=root', 'PrivateNetwork=yes', 'CPUQuota=100%',
                  f'MemoryMax={memory}', f'TasksMax={tasks}', 'LimitNOFILE=512',
                  'LimitFSIZE=16M', 'LimitCORE=0', 'TimeoutStartSec=10s', 'TimeoutStopSec=8s',
                  'RuntimeMaxSec=100s', 'KillMode=control-group', 'UMask=0077',
                  f'StandardOutput=append:{self.diagnostics.path}', 'StandardError=inherit',
                  'ProtectSystem=strict')
        command(f'{self.name} registration', runner, '--quiet', '--collect', '--service-type=exec',
                '--unit', self.name, *(f'--property={value}' for value in (*limits, *properties)),
                '--', guest_tool('env'), '-i', f'PATH={path}', 'LANG=C',
                f'HOME={home}', f'TMPDIR={home}', *args)

    def running(self):
        state = self.status()
        if state.get('ActiveState') == 'active':
            return int(state.get('MainPID', '0')), int(state.get('NRestarts', '0'))
        if state.get('LoadState') == 'not-found' or state.get('ActiveState') == 'failed':
            raise CheckFailed(f'{self.name} exited before its readiness check')
        return None

    def stop(self):
        if self.name not in self.pending:
            return
        self.control('stop', check=False)
        wait_for(f'{self.name} did not unload after stop',
                 lambda: self.status().get('LoadState') == 'not-found', 4)
        self.pending.remove(self.name)

    def summary(self):
        return self.diagnostics.summary() if self.diagnostics is not None else None


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__('localhost', timeout=3)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(str(self.path))


class Relay(UnixStreamServer):
    """Count attempts before any relay filtering or forwarding to the live daemon."""

    def __init__(self, address, upstream):
        self.routes = Counter()
        self.upstream = upstream
        super().__init__(str(address), RelayRequest)

    def handle_error(self, *_):
        pass


class RelayRequest(BaseHTTPRequestHandler):
    def setup(self):
        self.request.settimeout(3)
        super().setup()

    def log_message(self, *_):
        pass

    def relay(self):
        self.server.routes[(self.command, self.path)] += 1
        route = urlsplit(self.path).path
        route = re.sub(r'^/v1\.[0-9]{1,3}/', '/', route, count=1)
        if (sum(self.server.routes.values()) > 96 or not self.path.startswith('/')
                or self.command not in {'GET', 'HEAD'} or route not in DOCKER_ROUTES):
            self.send_error(403)
            return
        conn = UnixHTTP(self.server.upstream)
        try:
            conn.request(self.command, self.path)
            response = conn.getresponse()
            body = response.read(MAX_BODY + 1)
            if len(body) > MAX_BODY:
                self.send_error(502)
                return
            self.send_response(response.status)
            for name in ('Api-Version', 'Ostype', 'Docker-Experimental', 'Content-Type'):
                if response.getheader(name):
                    self.send_header(name, response.getheader(name))
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        finally:
            conn.close()

    do_GET = do_HEAD = do_POST = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = relay


def start_relay(stack, path, upstream):
    relay = stack.enter_context(Relay(path, upstream))
    path.chmod(0o600)
    thread = threading.Thread(target=relay.serve_forever, kwargs={'poll_interval': 0.1}, daemon=True)
    thread.start()

    def stop():
        relay.shutdown()
        thread.join(timeout=4)
        require(not thread.is_alive(), 'Fixture relay failed to stop')

    stack.callback(stop)
    return relay


def probe(identity, path, method, route):
    uid, gid = identity
    result = command('Unix peer probe', sys.executable, '-IB', '-c', PROBE, str(uid), path, method,
                     route, user=uid, group=gid, extra_groups=[])
    return json.loads(result.stdout)


def socket_ready(path):
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            conn.settimeout(1)
            conn.connect(str(path))
            return True
    except (FileNotFoundError, ConnectionRefusedError):
        return False


def engine_ready(unit, path):
    if not unit.running():
        return False
    conn = UnixHTTP(path)
    try:
        conn.request('GET', '/_ping')
        reply = conn.getresponse()
        return reply.status == 200 and reply.read(3) == b'OK'
    except (OSError, http.client.HTTPException):
        return False
    finally:
        conn.close()


def verify_engine_network(pid):
    require(os.readlink(f'/proc/{pid}/ns/net') != os.readlink('/proc/self/ns/net'),
            'Docker daemon did not get a private network namespace')
    for table in ('tcp', 'tcp6'):
        try:
            with Path(f'/proc/{pid}/net/{table}').open() as entries:
                next(entries)
                require(not any(line.split()[3] == '0A' for line in entries),
                        'Docker private network has a TCP listener')
        except FileNotFoundError:
            if table == 'tcp':
                raise


def fixture_binary(value, destination, label, max_bytes):
    source = executable(value, label)
    require(0 < source.stat().st_size <= max_bytes, f'{label} exceeds fixture size limit')
    shutil.copyfile(source, destination)
    destination.chmod(0o700)
    return destination


def check_broker(unit, path, upstream, relay_path, relay, engine, identity):
    root_identity = (0, 0)
    require(probe(identity, upstream, 'GET', '/info') == [-1, ''],
            'Unprivileged peer could reach the Docker socket')
    require(probe(identity, relay_path, 'GET', '/info') == [-1, ''],
            'Unprivileged peer could reach the upstream relay')
    baseline = relay.routes.copy()
    require(probe(root_identity, path, 'GET', '/info') == [0, ''],
            'Broker accepted a UID outside its allowlist')
    require(relay.routes == baseline, 'Disallowed UID generated an upstream request')
    for method, target in (('POST', '/v1.47/containers/create'), ('DELETE', '/v1.47/volumes/fixture'),
                           ('HEAD', '/info'), ('GET', '/info?format=full'),
                           ('GET', '/v1.47/containers/fixture/json')):
        require(probe(identity, path, method, target)[0] in (400, 403, 404, 405),
                'Broker accepted a non-allowlisted request')
    require(relay.routes == baseline, 'Disallowed request reached the upstream relay')

    for target in ('/info', '/v1.47/info'):
        status, payload = probe(identity, path, 'GET', target)
        require(status == 200 and json.loads(payload) == {'ID': engine},
                'Broker did not return the pinned Docker engine identity')
    require(probe(identity, path, 'GET', '/_ping') == [200, 'OK'], 'Broker ping failed')
    status, payload = probe(identity, path, 'GET', '/dsg/v1/inventory')
    require(status == 200, 'Broker inventory failed')
    inventory = json.loads(payload)
    require(inventory.get('complete') and inventory.get('engine_id') == engine
            and all(inventory.get(key) == [] for key in ('containers', 'images', 'volumes')),
            'Docker inventory was incomplete or contained resources')
    require(sum(relay.routes.values()) > sum(baseline.values()), 'No request reached the live engine')

    before = unit.running()
    require(before and before[0] > 0 and before[1] == 0, 'Invalid broker state before crash')
    unit.control('kill', '--kill-who=main', '--signal=SIGKILL')
    wait_for('Broker did not observe SIGKILL before restart',
             lambda: unit.status().get('SubState') == 'auto-restart', 3)
    require(path.is_socket(), 'Crash did not leave the fixture-owned broker socket')

    def restarted():
        after = unit.running()
        return after and after[0] > 0 and after[0] != before[0] and after[1] == 1 and socket_ready(path)

    wait_for('Broker did not restart exactly once after SIGKILL', restarted, 12)
    require(probe(identity, path, 'GET', '/_ping') == [200, 'OK'], 'Restarted broker ping failed')
    unit.stop()
    require(not path.exists(), 'Normal stop did not remove the broker socket')
    print('Linux systemd: unprivileged Docker access denied; allowed UID read-only queries passed; '
          'rejected UID/routes forwarded=0; SIGKILL restart=1; normal stop removed socket.')


def verify(options, root, pending, units):
    runner = guest_tool('systemd-run')
    systemctl = guest_tool('systemctl')
    identity = pwd.getpwnam('nobody')
    require(identity.pw_uid != 0 and identity.pw_gid != 0, 'A non-root nobody account is required')
    cli = fixture_binary(options.docker or guest_tool('docker'), root / 'docker', 'Docker CLI', 96 << 20)
    broker = fixture_binary(options.broker, root / 'dsgd', 'Broker', 40 << 20)
    dockerd = executable(options.dockerd or guest_tool('dockerd'), 'dockerd',
                         from_media=options.dockerd is not None)
    containerd_path = options.containerd or (
        dockerd.with_name('containerd') if options.dockerd else guest_tool('containerd'))
    containerd = executable(containerd_path, 'containerd',
                            from_media=options.containerd is not None or options.dockerd is not None)
    private = root / 'engine'
    public = root / 'client'
    config = root / 'cfg'
    home = root / 'home'
    for path, mode in ((private, 0o700), (public, 0o755), (config, 0o700), (home, 0o700)):
        path.mkdir(mode=mode)
        path.chmod(mode)
    upstream = private / 'docker.sock'
    relay_path = private / 'relay.sock'
    listen = public / 'broker.sock'
    containerd_sock = private / 'containerd.sock'
    docker_config = private / 'docker.json'
    docker_config.write_text('{"features":{"containerd-snapshotter":false}}', encoding='ascii')
    containerd_config = private / 'containerd.toml'
    containerd_config.write_text('version = 2\n', encoding='ascii')
    def unit(role):
        instance = Unit(systemctl, f'{root.name}-{role}.service', pending)
        units[role] = instance
        return instance

    with ExitStack() as stack:
        engine_properties = ('PrivateMounts=yes', f'ReadWritePaths={private} {home}')
        media_dir = dockerd.parent if options.dockerd else None
        runtime = unit('runtime')
        runtime.start(stack, runner, [containerd, '--config', containerd_config,
                      '--root', private / 'containerd-data', '--state', private / 'containerd-state',
                      '--address', containerd_sock, '--log-level', 'error'], '192M', 64,
                      home, engine_properties, binary_dir=media_dir)
        wait_for('Private containerd socket never became ready',
                 lambda: runtime.running() and socket_ready(containerd_sock), 8)
        engine = unit('engine')
        engine.start(stack, runner, [dockerd, '--config-file', docker_config,
                     '--containerd', containerd_sock, '--data-root', private / 'data',
                     '--exec-root', private / 'exec', '--pidfile', private / 'dockerd.pid',
                     '--host', f'unix://{upstream}', '--group', 'root', '--log-level', 'error',
                     '--storage-driver', 'vfs', '--bridge', 'none', '--iptables=false',
                     '--ip6tables=false', '--ip-forward=false', '--ip-masq=false', '--userland-proxy=false',
                     '--shutdown-timeout', '4'], '384M', 96, home,
                     (*engine_properties, 'TemporaryFileSystem=/run/docker:rw,size=16M,nodev,nosuid'),
                     binary_dir=media_dir)
        wait_for('Actual Docker engine never replied on its private socket',
                 lambda: engine_ready(engine, upstream), 16)
        verify_engine_network(engine.running()[0])
        relay = start_relay(stack, relay_path, upstream)
        engine_id = command('Live Docker identity and resource counts', cli, '--config', config,
                            '--host', f'unix://{relay_path}', 'info', '--format',
                            '{{.ID}} {{.Containers}} {{.Images}}').stdout.decode('ascii').split()
        require(len(engine_id) == 3 and ENGINE_ID.fullmatch(engine_id[0])
                and engine_id[1:] == ['0', '0'], 'Docker engine identity or empty state not verified')
        print('Live Docker: one private Unix engine, TCP listeners=0, containers=0, images=0.')
        broker_unit = unit('broker')
        broker_unit.start(stack, runner, [broker, '--listen', listen, '--upstream', relay_path,
                          '--docker-executable', cli, '--empty-config-dir', config,
                          '--engine-id', engine_id[0], '--allow-uid', str(identity.pw_uid),
                          '--max-resources', '1'], '128M', 32, home,
                          ('Restart=on-failure', 'RestartSec=2s', 'StartLimitIntervalSec=60s',
                           'StartLimitBurst=2', 'NoNewPrivileges=yes', 'PrivateDevices=yes',
                           'ProtectHome=yes', f'ReadWritePaths={public}',
                           'RestrictAddressFamilies=AF_UNIX'))
        wait_for('Broker Unix socket never became ready',
                 lambda: broker_unit.running() and socket_ready(listen), 8)
        check_broker(broker_unit, listen, upstream, relay_path, relay, engine_id[0],
                     (identity.pw_uid, identity.pw_gid))
        require(not any(config.iterdir()), 'Docker client altered its empty configuration directory')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--disposable-guest', action='store_true')
    parser.add_argument('--dockerd', help='absolute path to dockerd on read-only guest media')
    parser.add_argument('--containerd', help='absolute path to containerd on read-only guest media')
    parser.add_argument('--docker', help='absolute path to the guest Docker CLI')
    parser.add_argument('broker', help='absolute path to the Linux dsgd executable')
    options = parser.parse_args()
    if not (options.disposable_guest and sys.platform == 'linux' and os.geteuid() == 0
            and Path('/proc/1/comm').read_text(encoding='ascii').strip() == 'systemd'):
        parser.error('requires explicit --disposable-guest and root in a Linux systemd guest')

    def interrupted(signum, _):
        raise CheckFailed(f'Fixture interrupted by signal {signum}')

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    mount, umount = guest_tool('mount'), guest_tool('umount')
    root = Path(tempfile.mkdtemp(prefix='dsg-', dir='/run'))
    pending = set()
    units = {}
    try:
        command('Fixture tmpfs mount', mount, '-t', 'tmpfs', '-o',
                'size=192m,nr_inodes=8192,mode=0755,nodev,nosuid', 'tmpfs', root)
        verify(options, root, pending, units)
    except Exception as exc:
        summaries = []
        for role, unit in units.items():
            summary = unit.summary()
            if summary is not None:
                summaries.append(f'{role} [{summary}]')
        if summaries:
            reason = str(exc) if isinstance(exc, CheckFailed) else type(exc).__name__
            raise CheckFailed(f'{reason}. Fixture diagnostics: ' + '; '.join(summaries)) from None
        raise
    finally:
        require(not pending, f'Fixture units not confirmed unloaded; preserved {root}')
        if root.is_mount():
            command('Fixture tmpfs unmount', umount, root)
        root.rmdir()
    print('Guest fixture cleanup: three transient units unloaded; private tmpfs removed.')


if __name__ == '__main__':
    try:
        main()
    except CheckFailed as exc:
        sys.exit(str(exc))
    except Exception as exc:
        sys.exit(f'Linux fixture check failed ({type(exc).__name__}).')
