"""Verify the local read-only dashboard on a disposable project."""

from http.client import HTTPConnection, HTTPResponse
import json
import os
from pathlib import Path
from queue import Queue
import re
import socket
import subprocess
import sys
import tempfile
from threading import Thread
import time


DOCKER_COUNT_FIELDS = ('container_count', 'image_count', 'volume_count', 'duplicate_groups')


def expected_docker_counts(renderer):
    raw = os.environ.get('DSG_TEST_DASHBOARD_COUNTS')
    if raw is None:
        return None
    if os.environ.get('DSG_TEST_DOCKER_CONTEXT') != 'dockerguard-test':
        raise AssertionError('Live dashboard verification requires the dockerguard-test context')
    if sys.platform != 'darwin' or not renderer:
        raise AssertionError('Live dashboard verification requires the macOS WebKit renderer')
    try:
        counts = json.loads(raw)
    except ValueError:
        raise AssertionError('Live dashboard expected counts must be valid JSON') from None
    if (not isinstance(counts, dict) or counts.keys() != set(DOCKER_COUNT_FIELDS)
            or any(type(count) is not int or not 0 <= count <= 64 for count in counts.values())):
        raise AssertionError('Live dashboard verification requires four bounded integer counts')
    return counts


def request(port, route, method='GET', *, timeout=8, **headers):
    connection = HTTPConnection('127.0.0.1', port, timeout=timeout)
    try:
        connection.request(method, route, headers=headers)
        response = connection.getresponse()
        return response.status, response.headers, response.read(1 << 20)
    finally:
        connection.close()


def verify(dsg, fixture):
    renderer = os.environ.get('DSG_TEST_RENDERER')
    docker_counts = expected_docker_counts(renderer)
    live = docker_counts is not None
    project, state = fixture / 'project', fixture / 'unopened-state'
    output = project / 'dist' / 'synthetic.txt'
    output.parent.mkdir(parents=True)
    output.write_bytes(b'local dashboard fixture\n')
    aged = time.time() - 9 * 86400
    os.utime(output.parent, (aged, aged))
    args = [dsg, '--root', str(project), '--state-dir', str(state), '--json',
            'dashboard', '--lifetime', '90' if live else '30']
    if live:
        args.extend(['--docker-context', 'dockerguard-test'])
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
            with socket.create_connection(('127.0.0.1', port), timeout=8) as client:
                # A connection can arrive before its first request bytes.
                time.sleep(0.15)
                client.sendall(f'GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n'.encode('ascii'))
                with HTTPResponse(client) as response:
                    response.begin()
                    assert response.status == 200 and b'/styles.css' in response.read(1 << 20)
            for asset, content in [('/styles.css', 'text/css'), ('/app.js', 'text/javascript')]:
                info = request(port, asset)
                assert info[0] == 200 and info[1]['Content-Type'].startswith(content) and info[2]
            status, headers, payload = request(port, '/api/overview', timeout=25 if live else 8, Origin=url)
            assert status == 200 and headers['Cross-Origin-Resource-Policy'] == 'same-origin'
            overview = json.loads(payload)
            assert overview['health']['complete'] and overview['health']['total_bytes'] == output.stat().st_size
            assert overview['cleanup']['dry_run'] and overview['cleanup']['candidate_count'] == 1
            assert overview['cleanup']['candidate_bytes'] == output.stat().st_size
            assert str(project).encode() not in payload
            if docker_counts is None:
                assert overview['docker'] is None
            else:
                docker = overview['docker']
                assert (isinstance(docker, dict) and docker.get('complete') is True
                        and docker.get('status') == 'complete'), 'Dashboard API did not report a complete Docker inventory'
                assert all(type(docker.get(key)) is int and docker[key] == count
                           for key, count in docker_counts.items()), 'Dashboard API counts differ from observation'
            assert request(port, '/api/overview', Host='untrusted.invalid')[0] == 403
            assert request(port, '/api/overview', Origin='https://untrusted.invalid')[0] == 403
            assert request(port, '/api/overview', **{'Sec-Fetch-Site': 'cross-site'})[0] == 403
            assert request(port, '/api/overview', 'POST')[0] == 405
            assert request(port, '/api/cleanup/apply')[0] == 404
            if renderer:
                if sys.platform != 'darwin':
                    raise AssertionError('The WebKit rendering fixture requires macOS')
                rendering = [renderer, url + '/']
                if docker_counts is not None:
                    rendering.extend(str(docker_counts[key]) for key in DOCKER_COUNT_FIELDS)
                try:
                    subprocess.run(rendering, timeout=60 if live else 20, check=True)
                except subprocess.TimeoutExpired:
                    raise AssertionError('Dashboard renderer exceeded its time limit') from None
                except subprocess.CalledProcessError:
                    raise AssertionError('Dashboard renderer did not pass') from None
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
    if live:
        print('Dashboard: independent Docker counts matched the API and both WebKit layouts.')


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='dsg-dashboard-') as scratch:
        verify(str(Path(sys.argv[1]).resolve(strict=True)), Path(scratch).resolve())
