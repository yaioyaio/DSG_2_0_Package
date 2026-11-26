"""Check the macOS boundary against the existing read-only dockerguard-test engine."""
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import socket
from socketserver import UnixStreamServer
import stat
import subprocess
import sys
import tempfile
import threading

helpers = runpy.run_path(str(Path(__file__).with_name('check-enforcement.py')))
env = {'PATH': '/usr/bin:/bin', 'LANG': 'C'}


def cli(*args):
    result = subprocess.run(args, env=env, timeout=8, capture_output=True)
    if result.returncode or len(result.stdout) > 256:
        raise ValueError('Docker projection unavailable')
    return result.stdout.decode('ascii').strip()


def fixture_port(docker, root):
    context = 'dockerguard-test'
    key = hashlib.sha256(context.encode('ascii')).hexdigest()
    contexts = Path.home() / '.docker' / 'contexts'
    # Reject TLS material by presence. Only the selected metadata reaches Docker.
    tls = contexts / 'tls' / key
    if tls.is_symlink() or (tls.exists() and (not tls.is_dir() or next(tls.iterdir(), None) is not None)):
        raise ValueError('Fixture requires a context without TLS material')
    source = contexts / 'meta' / key / 'meta.json'
    meta = source.lstat()
    if not stat.S_ISREG(meta.st_mode) or not 0 < meta.st_size <= 16 << 10:
        raise ValueError('Fixture context metadata unavailable')
    metadata = root / 'metadata'
    target = metadata / 'contexts' / 'meta' / key
    target.mkdir(mode=0o700, parents=True)
    (target / 'meta.json').symlink_to(source)
    projection = '{{.Endpoints.docker.Host}} {{.Endpoints.docker.SkipTLSVerify}} {{len .TLSMaterial}}'
    value = cli(docker, '--config', metadata, 'context', 'inspect', '--format', projection,
                context)
    match = re.fullmatch(r'tcp://127\.0\.0\.1:([1-9][0-9]{0,4}) false 0', value)
    if not match or int(match[1]) > 65535:
        raise ValueError('Fixture requires a plaintext IPv4 loopback context')
    return int(match[1])


class Relay(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def setup(self):
        self.request.settimeout(3)
        super().setup()

    def parse_request(self):
        self.server.received += 1
        return super().parse_request()

    def do_GET(self):
        if not re.fullmatch(r'(?:/v1\.[0-9]{1,3})?/(?:_ping|info|version)', self.path):
            self.send_error(403)
            return
        if self.server.forwarded >= 48:
            self.send_error(503)
            return
        self.server.forwarded += 1
        conn = http.client.HTTPConnection('127.0.0.1', self.server.port, timeout=3)
        try:
            conn.request(self.command, self.path)
            response = conn.getresponse()
            body = response.read((1 << 20) + 1)
            if len(body) > 1 << 20 or response.status != 200:
                self.send_error(502)
                return
            self.send_response(200)
            for key in ('Api-Version', 'Ostype', 'Content-Type'):
                if response.getheader(key):
                    self.send_header(key, response.getheader(key))
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        finally:
            conn.close()

    do_HEAD = do_GET


class QuietServer(UnixStreamServer):
    def handle_error(self, *_):
        pass


def observe_engine(docker, config, upstream):
    value = cli(docker, '--config', config, '--host', 'unix://' + str(upstream),
                'info', '--format', '{{.ID}} {{.Containers}} {{.Images}}')
    match = re.fullmatch(r'([a-zA-Z0-9:-]{8,128}) ([0-9]{1,10}) ([0-9]{1,10})', value)
    if not match:
        raise ValueError('Fixture engine identity or aggregate counts unavailable')
    return match[1], int(match[2]), int(match[3])


def seatbelt_boundary(relay, listen, upstream):
    profile = ('(version 1)(allow default)(deny network-outbound)'
               '(allow network-outbound (remote unix-socket (path '
               + json.dumps(str(listen)) + ')))')
    worker = """import socket, sys
for family, addr in [(socket.AF_UNIX, sys.argv[1]), (socket.AF_INET, ('127.0.0.1', int(sys.argv[2])))]:
    with socket.socket(family) as client:
        client.settimeout(3)
        try:
            client.connect(addr)
        except PermissionError:
            continue
        raise SystemExit(2)
with socket.socket(socket.AF_UNIX) as client:
    client.settimeout(4)
    client.connect(sys.argv[3])
    client.sendall(b'GET /info HTTP/1.1\\r\\nHost: localhost\\r\\n\\r\\n')
    assert client.makefile('rb').readline(128).startswith(b'HTTP/1.1 200 ')
"""
    previous = relay.forwarded
    result = subprocess.run(['/usr/bin/sandbox-exec', '-p', profile, sys.executable,
                             '-I', '-B', '-c', worker, str(upstream), str(relay.port), str(listen)],
                            env=env, timeout=12, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    assert result.returncode == 0 and relay.forwarded > previous, 'Fixture Seatbelt boundary probe failed'


def verify(binary, root, launchd=False):
    docker = root / 'docker'
    source = Path(shutil.which('docker')).resolve(strict=True)
    assert source.is_file() and 0 < source.stat().st_size <= 96 << 20
    shutil.copyfile(source, docker)
    docker.chmod(0o700)
    port = fixture_port(docker, root)
    for name in ('cfg', 'client', 'engine'):
        (root / name).mkdir(mode=0o700)
    upstream = root / 'engine/u.sock'
    listen = root / 'client/d.sock'
    with QuietServer(str(upstream), Relay) as relay:
        upstream.chmod(0o600)
        relay.port = port
        relay.received = relay.forwarded = 0
        thread = threading.Thread(target=relay.serve_forever, daemon=True)
        thread.start()
        try:
            baseline = observe_engine(docker, root / 'cfg', upstream)
            engine, containers, images = baseline
            args = [binary, '--listen', str(listen), '--upstream', str(upstream),
                    '--docker-executable', str(docker), '--empty-config-dir', str(root / 'cfg'),
                    '--engine-id', engine, '--allow-uid', str(os.getuid())]
            def check_online():
                rejected = [('POST', '/v1.47/containers/create'), ('DELETE', '/v1.47/volumes/fixture'),
                            ('GET', '/v1.47/containers/fixture/json'), ('GET', '/info?format=full')]
                for method, route in rejected:
                    before = relay.received, relay.forwarded
                    code, _ = helpers['request'](listen, method, route)
                    assert code in (400, 403, 404, 405), 'Fixture broker accepted a forbidden request'
                    assert (relay.received, relay.forwarded) == before, 'Forbidden request reached the relay'
                code, body = helpers['request'](listen, 'GET', '/info')
                assert code == 200 and json.loads(body) == {'ID': engine}, 'Fixture broker identity changed'
                seatbelt_boundary(relay, listen, upstream)
                assert observe_engine(docker, root / 'cfg', upstream) == baseline, 'Fixture engine counts changed'

            if launchd:
                helpers['launchd_fixture'](root, args, listen, check_online)
            else:
                with helpers['process'](args):
                    helpers['wait_socket'](listen)
                    check_online()
            assert not listen.exists(), 'Fixture broker left its listen socket'
            assert next((root / 'cfg').iterdir(), None) is None, 'Fixture Docker config did not stay empty'
        finally:
            relay.shutdown()
            thread.join(timeout=4)
    upstream.unlink()
    if launchd:
        print('macOS live launchd: one crash, offline socket, different PID after restart, job and plist removed.')
    print('macOS live fixture: direct Unix and TCP denied, broker info passed, forbidden requests received/forwarded=0/0.')
    print(f'Engine identity and aggregate counts matched: containers={containers}, images={images}.')


if __name__ == '__main__':
    if sys.platform != 'darwin' or sys.flags.optimize or len(sys.argv) != 2:
        sys.exit('Requires macOS, assertions enabled, and the dsgd executable.')
    try:
        with tempfile.TemporaryDirectory(prefix='dsg-live-') as scratch:
            verify(str(Path(sys.argv[1]).resolve(strict=True)), Path(scratch).resolve(),
                   launchd=os.environ.get('DSG_TEST_LAUNCHD') == '1')
    except AssertionError as exc:
        sys.exit(str(exc) or 'Live fixture assertion failed.')
    except Exception as exc:
        sys.exit('Live fixture check failed (' + type(exc).__name__ + ').')
