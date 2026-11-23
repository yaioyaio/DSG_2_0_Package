"""Verify protocol wiring and, when selected, run native CLIs against local fixtures."""

from contextlib import closing, contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from threading import Thread
from urllib.parse import urlsplit


AGENTS = {
    'claude': ('PreToolUse', 'Bash', '.claude/settings.json'),
    'codex': ('PreToolUse', 'Bash', '.codex/hooks.json'),
    'gemini': ('BeforeTool', 'run_shell_command', '.gemini/settings.json'),
}
NETWORK_PROFILE = '(deny network-outbound) (allow network-outbound (remote ip "localhost:*"))'


def execute(args, *, cwd=None, env=None, stdin=None, timeout=12):
    with subprocess.Popen(args, cwd=cwd, env=env, text=True, stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True) as run:
        try:
            stdout, stderr = run.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(run.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            run.communicate(timeout=4)
            raise AssertionError('Agent fixture exceeded its time limit') from None
        if run.returncode:
            raise AssertionError(f'Fixture CLI exited with status {run.returncode}')
        return stdout, stderr


def configure(dsg, root, agent, action):
    text, _ = execute([dsg, '--root', str(root), '--json', 'agents', action, '--agent', agent])
    return json.loads(text)['result']['changed']


def verify_contracts(dsg, fixture):
    for agent, (event, tool, config) in AGENTS.items():
        root = fixture / agent
        root.mkdir()
        path = root / config
        path.parent.mkdir()
        existing = {'fixture': {'nested': ['preserve']}}
        path.write_text(json.dumps(existing))
        assert configure(dsg, root, agent, 'install')
        assert not configure(dsg, root, agent, 'install')
        assert json.loads(path.read_text())['fixture'] == existing['fixture']
        for command, expected in [('docker volume prune', True), ('docker volume ls', False)]:
            payload = {'hook_event_name': event, 'tool_name': tool, 'tool_input': {'command': command}}
            output, stderr = execute([dsg, 'hook', '--agent', agent], stdin=json.dumps(payload))
            verdict = json.loads(output)
            assert not stderr and (verdict != {}) == expected
            if expected:
                assert 'PROTECTED_VOLUME' in output and command not in output
        invalid, _ = execute([dsg, 'hook', '--agent', agent], stdin='{"unexpected":1}')
        assert 'HOOK_INPUT_INVALID' in invalid
        assert configure(dsg, root, agent, 'uninstall')
        assert not configure(dsg, root, agent, 'uninstall')
        assert json.loads(path.read_text())['fixture'] == existing['fixture']

    def message(method, params, request_id=None):
        request = {'jsonrpc': '2.0', 'method': method, 'params': params}
        if request_id is not None:
            request['id'] = request_id
        return json.dumps(request)

    root = fixture / 'mcp'
    root.mkdir()
    state = root / 'unexpected-state'
    requests = [
        message('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                               'clientInfo': {'name': 'fixture', 'version': '1'}}, 1),
        message('notifications/initialized', {}),
        message('tools/list', {}, 2),
        message('tools/call', {'name': 'dsg_get_gc_plan', 'arguments': {}}, 3),
        message('tools/call', {'name': 'dsg_check_command',
                               'arguments': {'argv': ['docker', 'volume', 'prune']}}, 4),
        message('tools/call', {'name': 'dsg_gc_apply', 'arguments': {}}, 5),
        message('tools/call', {'name': 'dsg_get_execution_status', 'arguments': {}}, 6),
    ]
    output, stderr = execute([dsg, '--root', str(root), '--state-dir', str(state), 'mcp'],
                             stdin='\n'.join(requests) + '\n')
    replies = {reply['id']: reply for reply in map(json.loads, output.splitlines())}
    assert not stderr and not state.exists() and not list(root.iterdir())
    assert replies[1]['result']['protocolVersion'] == '2025-06-18'
    listed = replies[2]['result']['tools']
    assert len(listed) == 8 and all(tool['annotations']['readOnlyHint'] for tool in listed)
    assert not replies[3]['result'].get('isError', False)
    assert 'PROTECTED_VOLUME' in json.dumps(replies[4])
    assert replies[5].get('error') or replies[5]['result'].get('isError')
    print('Agent contracts: install, preserve config, allow/deny, invalid input, uninstall and write-free MCP passed.')
    if sys.platform == 'darwin':
        verify_state_tools(dsg, fixture, message, requests[:2])


def verify_state_tools(dsg, fixture, message, handshake):
    root = fixture / 'state-project'
    output = root / 'dist'
    output.mkdir(parents=True)
    (output / 'fixture.txt').write_text('synthetic')
    aged = time.time() - 9 * 86400
    os.utime(output, (aged, aged))
    state = fixture / 'state-observation'
    other = fixture / 'other-project'
    other.mkdir()

    def command(project):
        return [dsg, '--root', str(project), '--state-dir', str(state), '--json']

    def invoke(project, *args):
        output, _ = execute(command(project) + list(args))
        return json.loads(output)['result']

    def snapshot():
        entries = {}
        for path in state.iterdir():
            info = path.lstat()
            entries[path.name] = (info.st_ino, info.st_size, info.st_mtime_ns)
        return entries

    def observe(project, name, arguments=None):
        requests = handshake + [message('tools/call', {
            'name': name, 'arguments': arguments or {},
        }, 10)]
        before = snapshot()
        responses, stderr = execute(command(project) + ['mcp'], stdin='\n'.join(requests) + '\n')
        assert not stderr and snapshot() == before
        reply = json.loads(responses.splitlines()[-1])
        result = reply.get('result', {})
        if result.get('isError') or 'error' in reply:
            return 'unavailable'
        return json.loads(result['content'][0]['text'])['status']

    invoke(root, 'init')
    invoke(other, 'init')
    invoke(root, 'gc-own', '--candidate', 'dist', '--confirm-generated')
    plan = invoke(root, 'gc-plan')['record']['plan_id']
    approval = {'plan_id': plan}
    assert observe(root, 'dsg_get_execution_status') == 'none'
    assert observe(root, 'dsg_get_gc_approval_status', approval) == 'awaiting_approval'
    with closing(sqlite3.connect(state / 'registry.sqlite3')) as anchor:
        anchor.execute('BEGIN')
        anchor.execute('SELECT COUNT(*) FROM project').fetchone()
        assert observe(root, 'dsg_get_execution_status') == 'none'
        assert observe(root, 'dsg_get_gc_approval_status', approval) == 'awaiting_approval'
        subprocess.run(command(root) + ['gc-approve', '--plan', str(plan), '--candidate', 'dist', '--confirm'],
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=10, check=True)
        assert observe(root, 'dsg_get_gc_approval_status', approval) == 'approved_recorded'
        assert observe(other, 'dsg_get_gc_approval_status', approval) == 'unavailable'
    assert observe(root, 'dsg_get_execution_status') == 'none'
    assert observe(root, 'dsg_get_gc_approval_status', approval) == 'approved_recorded'
    print('MCP state: project isolation, approval records, and state files unchanged by reads.', flush=True)


def sse(payloads, *, named=False):
    return ''.join((f'event: {payload["type"]}\n' if named else '')
                   + 'data: ' + json.dumps(payload) + '\n\n' for payload in payloads).encode()


def codex_reply(command, round_number):
    response = f'fixture-response-{round_number}'
    if round_number == 1:
        item = {'type': 'function_call', 'call_id': 'fixture-tool-call', 'name': 'exec_command',
                'arguments': json.dumps({'cmd': command})}
    else:
        item = {'type': 'message', 'role': 'assistant', 'id': 'fixture-message',
                'content': [{'type': 'output_text', 'text': 'Fixture finished.'}]}
    usage = {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}
    return sse([{'type': 'response.created', 'response': {'id': response}},
                {'type': 'response.output_item.done', 'item': item},
                {'type': 'response.completed', 'response': {'id': response, 'usage': usage}}])


def claude_reply(command, round_number):
    tool = round_number == 1
    initial = {'type': 'message_start', 'message': {
        'id': f'msg_fixture_{round_number}', 'type': 'message', 'role': 'assistant',
        'model': 'claude-sonnet-4-6', 'content': [], 'stop_reason': None, 'stop_sequence': None,
        'usage': {'input_tokens': 1, 'output_tokens': 0},
    }}
    content = ({'type': 'tool_use', 'id': 'toolu_fixture', 'name': 'Bash', 'input': {}}
               if tool else {'type': 'text', 'text': ''})
    delta = ({'type': 'input_json_delta', 'partial_json': json.dumps({
        'command': command, 'description': 'Execute a local fixture stub',
    })} if tool else {'type': 'text_delta', 'text': 'Fixture finished.'})
    return sse([initial, {'type': 'content_block_start', 'index': 0, 'content_block': content},
                {'type': 'content_block_delta', 'index': 0, 'delta': delta},
                {'type': 'content_block_stop', 'index': 0},
                {'type': 'message_delta', 'delta': {
                    'stop_reason': 'tool_use' if tool else 'end_turn', 'stop_sequence': None},
                 'usage': {'output_tokens': 1}},
                {'type': 'message_stop'}], named=True)


@contextmanager
def local_gateway(agent, command):
    exchanges = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_HEAD(self):
            self.send_response(200)
            self.end_headers()

        def do_POST(self):
            path = urlsplit(self.path).path
            size = int(self.headers.get('Content-Length', '0'))
            if size > 512 << 10 or len(exchanges) >= 4:
                self.send_error(413)
                return
            body = self.rfile.read(size)
            if path.endswith('/count_tokens'):
                payload, content_type = b'{"input_tokens":1}', 'application/json'
            elif path in ('/v1/responses', '/v1/messages'):
                exchanges.append(body)
                respond = codex_reply if agent == 'codex' else claude_reply
                payload, content_type = respond(command, len(exchanges)), 'text/event-stream'
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    with HTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield f'http://127.0.0.1:{server.server_port}', exchanges
        finally:
            server.shutdown()
            thread.join(timeout=2)


def native_case(dsg, root, agent, scenario):
    root.mkdir()
    home = root / 'home'
    home.mkdir()
    marker = root / 'shell-executed'
    docker = root / 'docker'
    docker.write_text(f'#!/bin/sh\nprintf fixture > {shlex.quote(str(marker))}\n')
    docker.chmod(0o700)
    deny = scenario == 'deny'
    broken = scenario == 'hook-error'
    command = shlex.quote(str(docker)) + (' volume prune' if deny or broken else ' volume ls')
    assert configure(dsg, root, agent, 'install')
    event, _, config = AGENTS[agent]
    path = root / config
    settings = json.loads(path.read_text())
    error_marker = root / 'hook-error-executed'
    if broken:
        failure = root / 'fixture-failing-hook'
        failure.write_text(f'#!/bin/sh\nprintf fixture > {shlex.quote(str(error_marker))}\nexit 17\n')
        failure.chmod(0o700)
        settings['hooks'][event][0]['hooks'][0]['command'] = shlex.quote(str(failure))
        path.write_text(json.dumps(settings))
    env = {'HOME': str(home), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'TMPDIR': str(home),
           'SHELL': '/bin/bash',
           'LANG': 'en_US.UTF-8', 'CI': '1', 'TERM': 'dumb', 'NO_COLOR': '1'}
    profile = ('(version 1) (allow default) ' + NETWORK_PROFILE + ' (deny file-write*) '
               + '(allow file-write* (literal "/dev/null") (subpath '
               + json.dumps(str(root.resolve())) + '))')
    sandbox = ['/usr/bin/sandbox-exec', '-p', profile]
    if agent == 'gemini':
        node = os.environ['DSG_TEST_NODE']
        entry = os.environ['DSG_TEST_GEMINI_ENTRY']
        settings.update({'security': {'auth': {'selectedType': 'gemini-api-key'},
                                      'folderTrust': {'enabled': False}},
                         'telemetry': {'enabled': False}, 'hooksConfig': {'enabled': True},
                         'tools': {'shell': {'enableInteractiveShell': False}}})
        path.write_text(json.dumps(settings))
        env.update(GEMINI_CLI_HOME=str(home), GEMINI_API_KEY='fixture-only',
                   GEMINI_CLI_TRUST_WORKSPACE='true')
        replay = root / 'replay.jsonl'
        tool = {'functionCall': {'name': 'run_shell_command', 'args': {
            'command': command, 'description': 'Execute a local fixture stub',
        }}}
        records = [{'method': 'generateContentStream', 'response': [{
            'candidates': [{'content': {'role': 'model', 'parts': [part]},
                            'finishReason': 'STOP', 'index': 0}],
            'usageMetadata': {'promptTokenCount': 1, 'candidatesTokenCount': 1, 'totalTokenCount': 2},
        }]} for part in [tool, {'text': 'Fixture finished.'}]]
        replay.write_text(''.join(json.dumps(record) + '\n' for record in records))
        args = [node, entry, '--fake-responses', str(replay), '--approval-mode', 'yolo',
                '--model', 'gemini-2.5-flash',
                '--output-format', 'stream-json', '-p', 'Run the single local fixture tool.']
        stdout, stderr = execute(sandbox + args, cwd=root, env=env, timeout=22)
        verified = 'PROTECTED_VOLUME' in (stdout + stderr) if deny else 'Fixture finished.' in stdout
    else:
        executable = shutil.which(agent, path=os.environ.get('PATH'))
        if not executable:
            raise AssertionError(f'{agent} fixture executable not available')
        with local_gateway(agent, command) as (address, exchanges):
            if agent == 'codex':
                env.update(CODEX_HOME=str(home), CODEX_SQLITE_HOME=str(home), OPENAI_API_KEY='fixture-only')
                args = [executable, 'exec', '--json', '--ephemeral', '--skip-git-repo-check', '--trust',
                        '--dangerously-bypass-hook-trust', '--sandbox', 'danger-full-access',
                        '-c', 'approval_policy="never"',
                        '-c', 'openai_base_url=' + json.dumps(address + '/v1'),
                        'Run the single local fixture tool.']
            else:
                env.update(CLAUDE_CONFIG_DIR=str(home), CLAUDE_CODE_TMPDIR=str(home),
                           ANTHROPIC_AUTH_TOKEN='fixture-only', ANTHROPIC_BASE_URL=address,
                           DISABLE_NONESSENTIAL_TRAFFIC='1')
                args = [executable, '-p', '--no-session-persistence', '--strict-mcp-config',
                        '--setting-sources', 'project', '--tools', 'Bash', '--allowedTools', 'Bash',
                        '--verbose', '--output-format', 'stream-json', '--include-hook-events',
                        '--model', 'claude-sonnet-4-6', 'Run the single local fixture tool.']
            stdout, stderr = execute(sandbox + args, cwd=root, env=env, timeout=22)
        verified = len(exchanges) == 2 and (not deny or b'PROTECTED_VOLUME' in exchanges[-1])
    expected_marker = not (deny or (broken and agent == 'claude'))
    if not verified or marker.exists() != expected_marker or error_marker.exists() != broken:
        indicators = ('Fixture finished.', 'PROTECTED_VOLUME', 'HOOK_INPUT_INVALID',
                      'HOOK_EVENT_UNSUPPORTED', 'HOOK_COMMAND_UNKNOWN', 'is_error',
                      'Operation not permitted', 'Permission denied', 'cannot be accessed',
                      'sandbox', 'EPERM', 'EACCES', 'ENOENT', 'hook_started', 'hook_response',
                      'Could not', 'hook failed', 'prevented', 'denied', 'exit code', 'timed out',
                      'No such file', 'shell snapshot', 'Shell snapshot', 'blocked',
                      'Invalid JSON', 'invalid JSON', 'No replacement', 'untrusted')
        found = [indicator for indicator in indicators if indicator in stdout + stderr]
        rounds = len(exchanges) if agent != 'gemini' else 'replay'
        raise AssertionError(f'{agent} native {scenario}, rounds={rounds}, '
                             f'marker={marker.exists()}, hook_error={error_marker.exists()}, '
                             f'indicators={found}')


if __name__ == '__main__' and os.name == 'posix':
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    selection = os.environ.get('DSG_TEST_AGENT_NATIVE', '')
    with tempfile.TemporaryDirectory(prefix='dsg-agent-fixtures-') as scratch:
        fixture = Path(scratch).resolve()
        verify_contracts(binary, fixture)
        if selection:
            assert sys.platform == 'darwin' and selection in {*AGENTS, 'all'}
            for agent in AGENTS if selection == 'all' else [selection]:
                for scenario in ('allow', 'deny', 'hook-error'):
                    native_case(binary, fixture / f'native-{agent}-{scenario}', agent, scenario)
                print(f'Native {agent}: allow, deny and hook-error paths passed.')
