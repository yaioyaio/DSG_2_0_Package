"""Exercise the Unix read-only broker against a disposable, Docker-shaped engine."""

from collections import Counter
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import socket
from socketserver import UnixStreamServer
import subprocess
import sys
import tempfile
import threading
import time
import uuid


class Engine:
    def __init__(self, path):
        self.path = path
        self.engine_id = 'fixture-engine'
        self.routes = Counter()
        self.server = None
        self.thread = None

    def start(self):
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def route(self):
                route = self.path.partition('?')[0]
                if route.startswith('/v1.') and route.count('/') >= 2:
                    route = route.split('/', 2)[2]
                    route = '/' + route
                return route

            def parse_request(self):
                if not super().parse_request():
                    return False
                fixture.routes[(self.command, self.route())] += 1
                return True

            def do_HEAD(self):
                self.do_GET()

            def do_GET(self):
                route = self.route()
                if route == '/_ping':
                    body = b'OK'
                else:
                    result = {'/info': {'ID': fixture.engine_id}, '/containers/json': [],
                              '/images/json': [], '/volumes': {'Volumes': [], 'Warnings': []}}
                    if route not in result:
                        self.send_error(404)
                        return
                    body = json.dumps(result[route]).encode()
                self.send_response(200)
                self.send_header('API-Version', '1.47')
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)

        self.server = UnixStreamServer(str(self.path), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def stop(self):
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join(timeout=2)
            self.path.unlink()
            self.server = None


def request(path, method, route):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(6)
        client.connect(str(path))
        try:
            client.sendall(f'{method} {route} HTTP/1.1\r\nHost: localhost\r\n\r\n'.encode())
            with client.makefile('rb') as stream:
                headers, _, body = stream.read(1 << 20).partition(b'\r\n\r\n')
        except (BrokenPipeError, ConnectionResetError):
            return 0, b''
    if not headers:
        return 0, b''
    return int(headers.split(b' ', 2)[1]), body


def wait_socket(path):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        try:
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(path))
            return
        except (ConnectionRefusedError, FileNotFoundError):
            time.sleep(0.05)
    raise AssertionError('Fixture broker failed to open its socket')


@contextmanager
def process(args):
    env = {'PATH': '/usr/bin:/bin', 'LANG': 'C'}
    with subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL, start_new_session=True) as child:
        try:
            yield child
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    assert child.wait(timeout=8) == 0
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=3)
                    raise AssertionError('Fixture broker did not stop') from None


def seatbelt_boundary(listen, upstream):
    profile = ('(version 1) (allow default) (deny network-outbound (remote unix-socket (path '
               + json.dumps(str(upstream)) + ')))')
    worker = '''import socket, sys
with socket.socket(socket.AF_UNIX) as conn:
    conn.settimeout(4)
    try:
        conn.connect(sys.argv[1])
    except PermissionError:
        raise SystemExit(19)
    if len(sys.argv) > 2:
        conn.sendall(b'GET /info HTTP/1.1\\r\\nHost: localhost\\r\\n\\r\\n')
        assert b'200 OK' in conn.recv(256)
'''
    command = ['/usr/bin/sandbox-exec', '-p', profile, sys.executable, '-B', '-c', worker]
    for suffix, code in [([str(upstream)], 19), ([str(listen), 'request'], 0)]:
        probe = subprocess.run(command + suffix, timeout=6, check=False, capture_output=True)
        assert probe.returncode == code, f'OS socket boundary exit {probe.returncode}, expected {code}'


def launchd_fixture(root, args, listen, online=None):
    label = 'local.dsg.test.' + uuid.uuid4().hex
    domain = f'gui/{os.getuid()}'
    service = f'{domain}/{label}'
    plist = root / 'test.plist'
    plist.write_bytes(plistlib.dumps({'Label': label, 'ProgramArguments': args,
                                     'RunAtLoad': True, 'KeepAlive': False,
                                     'ThrottleInterval': 1}))

    def call(*argv, check=True):
        try:
            result = subprocess.run(['/bin/launchctl', *argv], timeout=15, check=False,
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            raise AssertionError(f'Fixture launchd {argv[0]} timed out') from None
        if check and result.returncode:
            raise AssertionError(f'Fixture launchd {argv[0]} exited with {result.returncode}')
        return result

    def job_pid():
        # `list` supplies PID, status and label without the job environment.
        for line in call('list').stdout.splitlines():
            fields = line.split()
            if len(fields) == 3 and fields[2] == label.encode('ascii'):
                return 0 if fields[0] == b'-' else int(fields[0])
        return None

    def check_online():
        wait_socket(listen)
        pid = job_pid()
        assert pid is not None and pid > 0, 'Fixture launchd job has no running PID'
        assert request(listen, 'GET', '/_ping') == (200, b'OK'), 'Fixture launchd broker ping failed'
        if online is not None:
            online()
        return pid

    started = False
    try:
        call('bootstrap', domain, str(plist))
        started = True
        first_pid = check_online()
        call('kill', 'SIGKILL', service)
        deadline = time.monotonic() + 6
        while job_pid() != 0:
            assert time.monotonic() < deadline, 'Fixture launchd job did not stay registered and stopped'
            time.sleep(0.2)
        for _ in range(2):
            try:
                request(listen, 'GET', '/_ping')
            except (ConnectionRefusedError, FileNotFoundError):
                pass
            else:
                raise AssertionError('Stopped fixture launchd broker still accepted a connection')
            time.sleep(0.2)
        assert job_pid() == 0, 'Fixture launchd job unexpectedly restarted'
        call('kickstart', service)
        assert check_online() != first_pid, 'Fixture launchd restart reused the original PID'
    finally:
        try:
            if started:
                call('bootout', service)
            else:
                call('bootout', service, check=False)
        finally:
            plist.unlink()
        deadline = time.monotonic() + 6
        while listen.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not listen.exists(), 'Fixture launchd stop left its listen socket'
        assert job_pid() is None and not plist.exists(), 'Fixture launchd job or plist remains'


def verify(binary, docker, root):
    inbound = root / 'in'
    outbound = root / 'out'
    config = root / 'cfg'
    for directory in (inbound, outbound, config):
        directory.mkdir(mode=0o700)
    listen = inbound / 'd.sock'
    upstream = outbound / 'u.sock'
    args = [binary, '--listen', str(listen), '--upstream', str(upstream),
            '--docker-executable', docker, '--empty-config-dir', str(config),
            '--engine-id', 'fixture-engine', '--allow-uid']
    owner = str(os.getuid())
    engine = Engine(upstream)
    engine.start()
    try:
        with process(args + [str(os.getuid() ^ 1)]):
            wait_socket(listen)
            assert request(listen, 'GET', '/info') == (0, b'') and not engine.routes
        assert not listen.exists()

        with process(args + [owner]) as child:
            wait_socket(listen)
            rejected = [('POST', '/v1.47/containers/create'), ('DELETE', '/v1.47/volumes/fixture'),
                        ('GET', '/v1.47/containers/fixture/json'), ('GET', '/info?format=full')]
            for method, route in rejected:
                assert request(listen, method, route)[0] in (400, 403, 404, 405)
            assert not engine.routes
            assert json.loads(request(listen, 'GET', '/v1.47/info')[1]) == {'ID': engine.engine_id}
            assert request(listen, 'GET', '/_ping') == (200, b'OK')
            code, payload = request(listen, 'GET', '/dsg/v1/inventory')
            assert code == 200 and json.loads(payload)['complete']
            assert set(engine.routes.values()) and all(method in {'GET', 'HEAD'} for method, _ in engine.routes)
            duplicate = subprocess.run(args + [owner], timeout=6, capture_output=True)
            assert duplicate.returncode == 5 and request(listen, 'GET', '/_ping')[0] == 200
            if sys.platform == 'darwin':
                seatbelt_boundary(listen, upstream)
            engine.stop()
            assert request(listen, 'GET', '/info')[0] == 503
            engine.engine_id = 'replacement-engine'
            engine.start()
            assert request(listen, 'GET', '/info')[0] == 503
            engine.stop()
            engine.engine_id = 'fixture-engine'
            engine.start()
            assert request(listen, 'GET', '/info')[0] == 200
            child.send_signal(signal.SIGKILL)
            assert child.wait(timeout=5) == -signal.SIGKILL and listen.exists()

        with process(args + [owner]):
            wait_socket(listen)
            assert request(listen, 'GET', '/info')[0] == 200
        assert not listen.exists() and upstream.exists() and not list(config.iterdir())
        print('Unix broker: rejected operations sent upstream=0, peer rejection, engine pin, recovery and socket cleanup passed.')
        if sys.platform == 'darwin' and os.environ.get('DSG_TEST_LAUNCHD') == '1':
            launchd_fixture(root, args + [owner], listen)
            print('macOS launchd: bootstrap, one crash/restart, and bootout passed.')
    finally:
        engine.stop()


if __name__ == '__main__' and sys.platform in ('darwin', 'linux'):
    docker = shutil.which('docker')
    if not docker:
        print('Unix broker fixture skipped: Docker CLI unavailable.')
        sys.exit(0)
    runtime_dir = os.environ.get('DSG_TEST_RUNTIME_DIR')
    if sys.platform == 'linux' and runtime_dir is None:
        runtime_dir = Path.home()
    with tempfile.TemporaryDirectory(prefix='dsgd-', dir=runtime_dir) as scratch:
        source = Path(docker).resolve(strict=True)
        root = Path(scratch).resolve()
        fixture_docker = root / 'docker'
        assert source.is_file() and 0 < source.stat().st_size <= 96 << 20
        shutil.copyfile(source, fixture_docker)
        fixture_docker.chmod(0o700)
        verify(str(Path(sys.argv[1]).resolve(strict=True)), str(fixture_docker), root)
