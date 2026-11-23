"""Check the macOS Seatbelt boundary using the existing dockerguard-test engine."""
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
    # Only the context store is visible to the CLI, including TLS-material presence.
    metadata = root / 'metadata'
    metadata.mkdir(mode=0o700)
    (metadata / 'contexts').symlink_to(Path.home() / '.docker' / 'contexts')
    projection = '{{.Endpoints.docker.Host}} {{.Endpoints.docker.SkipTLSVerify}} {{len .TLSMaterial}}'
    value = cli(docker, '--config', metadata, 'context', 'inspect', '--format', projection,
                'dockerguard-test')
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


def verify(binary, root):
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
        relay.port, relay.forwarded = port, 0
        thread = threading.Thread(target=relay.serve_forever, daemon=True)
        thread.start()
        try:
            engine = cli(docker, '--config', root / 'cfg', '--host', 'unix://' + str(upstream),
                         'info', '--format', '{{.ID}}')
            if not re.fullmatch(r'[a-zA-Z0-9:-]{8,128}', engine):
                raise ValueError('Fixture engine identity unavailable')
            args = [binary, '--listen', str(listen), '--upstream', str(upstream),
                    '--docker-executable', str(docker), '--empty-config-dir', str(root / 'cfg'),
                    '--engine-id', engine, '--allow-uid', str(os.getuid())]
            with helpers['process'](args):
                helpers['wait_socket'](listen)
                previous = relay.forwarded
                code, _ = helpers['request'](listen, 'POST', '/v1.47/containers/create')
                assert code in (400, 403, 404, 405) and relay.forwarded == previous
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
                result = subprocess.run(['/usr/bin/sandbox-exec', '-p', profile, sys.executable,
                                         '-I', '-B', '-c', worker, str(upstream), str(port), str(listen)],
                                        env=env, timeout=12, capture_output=True)
                assert result.returncode == 0 and relay.forwarded > previous
            assert not listen.exists()
        finally:
            relay.shutdown()
            thread.join(timeout=4)
    print('macOS live fixture: direct Unix and TCP denied, broker info passed, mutation forwarded=0.')


if __name__ == '__main__':
    if sys.platform != 'darwin' or sys.flags.optimize or len(sys.argv) != 2:
        sys.exit('Requires macOS, assertions enabled, and the dsgd executable.')
    try:
        with tempfile.TemporaryDirectory(prefix='dsg-live-') as scratch:
            verify(str(Path(sys.argv[1]).resolve(strict=True)), Path(scratch).resolve())
    except Exception as exc:
        sys.exit('Live fixture check failed (' + type(exc).__name__ + ').')
