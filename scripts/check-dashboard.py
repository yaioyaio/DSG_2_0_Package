"""Verify the local read-only dashboard on a disposable project."""

from http.client import HTTPConnection
import json
import os
from pathlib import Path
from queue import Queue
import re
import subprocess
import sys
import tempfile
from threading import Thread
import time


def request(port, route, method='GET', **headers):
    connection = HTTPConnection('127.0.0.1', port, timeout=8)
    try:
        connection.request(method, route, headers=headers)
        response = connection.getresponse()
        return response.status, response.headers, response.read(1 << 20)
    finally:
        connection.close()


def verify(dsg, fixture):
    project, state = fixture / 'project', fixture / 'unopened-state'
    output = project / 'dist' / 'synthetic.txt'
    output.parent.mkdir(parents=True)
    output.write_bytes(b'local dashboard fixture\n')
    aged = time.time() - 9 * 86400
    os.utime(output.parent, (aged, aged))
    args = [dsg, '--root', str(project), '--state-dir', str(state), '--json',
            'dashboard', '--lifetime', '30']
    ready = Queue(maxsize=1)
    with subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True) as server:
        reader = Thread(target=lambda: ready.put(server.stdout.readline(2048)), daemon=True)
        reader.start()
        try:
            url = json.loads(ready.get(timeout=5))['result']['url']
            match = re.fullmatch(r'http://127\.0\.0\.1:([1-9][0-9]{0,4})', url)
            assert match and 0 < int(match[1]) <= 65535
            port = int(match[1])
            status, headers, html = request(port, '/')
            assert status == 200 and b'/styles.css' in html and b'/app.js' in html
            assert "frame-ancestors 'none'" in headers['Content-Security-Policy']
            assert headers['Cache-Control'] == 'no-store'
            for asset, content in [('/styles.css', 'text/css'), ('/app.js', 'text/javascript')]:
                info = request(port, asset)
                assert info[0] == 200 and info[1]['Content-Type'].startswith(content) and info[2]
            status, headers, payload = request(port, '/api/overview', Origin=url)
            assert status == 200 and headers['Cross-Origin-Resource-Policy'] == 'same-origin'
            overview = json.loads(payload)
            assert overview['health']['complete'] and overview['health']['total_bytes'] == output.stat().st_size
            assert overview['cleanup']['dry_run'] and overview['cleanup']['candidate_count'] == 1
            assert overview['cleanup']['candidate_bytes'] == output.stat().st_size
            assert overview['docker'] is None and str(project).encode() not in payload
            assert request(port, '/api/overview', Host='untrusted.invalid')[0] == 403
            assert request(port, '/api/overview', Origin='https://untrusted.invalid')[0] == 403
            assert request(port, '/api/overview', **{'Sec-Fetch-Site': 'cross-site'})[0] == 403
            assert request(port, '/api/overview', 'POST')[0] == 405
            assert request(port, '/api/cleanup/apply')[0] == 404
            renderer = os.environ.get('DSG_TEST_RENDERER')
            if renderer:
                if sys.platform != 'darwin':
                    raise AssertionError('The WebKit rendering fixture requires macOS')
                subprocess.run([renderer, url + '/'], timeout=20, check=True)
            assert not state.exists() and output.read_bytes() == b'local dashboard fixture\n'
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=2)
                    raise AssertionError('Dashboard did not stop') from None
            reader.join(timeout=1)
    if os.name == 'posix':
        assert server.returncode == 0
    print('Dashboard: local API, assets, origin guard, read-only observation and teardown passed.')


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='dsg-dashboard-') as scratch:
        verify(str(Path(sys.argv[1]).resolve(strict=True)), Path(scratch).resolve())
