"""Verify registered providers with isolated projects and optional staged dependencies."""

import argparse
import json
from contextlib import closing
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import subprocess
import sys
from tempfile import TemporaryDirectory, mkdtemp
import time
from zipfile import ZipFile


PROJECTS = {
    'cargo': ('test', {
        'Cargo.toml': '[package]\nname = "dsg_fixture"\nversion = "0.1.0"\nedition = "2021"\n',
        'src/lib.rs': '#[test]\nfn fixture() { assert_eq!(2 + 2, 4); }\n',
    }),
    'node': ('test', {
        'fixture.test.js': "const test = require('node:test'); test('fixture', () => {});\n",
    }),
    'pnpm': ('build', {
        'package.json': '{"name":"dsg-fixture","private":true,"scripts":{"build":"node fixture.js"}}\n',
        'fixture.js': "require('node:fs').writeFileSync('built.txt', 'fixture');\n",
    }),
    'gradle': ('build', {
        'settings.gradle': "rootProject.name = 'dsg-fixture'\n",
        'build.gradle': "plugins { id 'java' }\n",
        'src/main/java/Fixture.java': 'public final class Fixture {}\n',
    }),
    'uv': ('test', {
        'test_fixture.py': 'import unittest\nclass Fixture(unittest.TestCase):\n'
                           '    def test_example(self): self.assertEqual(2 + 2, 4)\n',
    }),
    'go': ('test', {
        'go.mod': 'module example.invalid/fixture\n\ngo 1.20\n',
        'fixture_test.go': 'package fixture\nimport "testing"\nfunc TestFixture(t *testing.T) {}\n',
    }),
}

NEXT_VERSIONS = {'next': '16.4.6', 'react': '19.3.1', 'react-dom': '19.3.1'}

NEXT_SMOKE = '''const { spawn } = require('node:child_process');
const { once } = require('node:events');
const { createServer } = require('node:net');
const { setTimeout: delay } = require('node:timers/promises');

async function smoke(marker) {
  const reservation = createServer().listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const { port } = reservation.address();
  const released = once(reservation, 'close');
  reservation.close();
  await released;

  // Keep the server in the managed command's process group.
  const child = spawn(process.execPath, [require.resolve('next/dist/bin/next'),
    'start', '--hostname', '127.0.0.1', '--port', String(port)], { stdio: 'ignore' });
  const exited = new Promise(resolve => {
    child.once('error', () => resolve(false));
    child.once('exit', (code, signal) => resolve(code === 0 || code === 143 || signal === 'SIGTERM'));
  });
  try {
    const deadline = performance.now() + 10000;
    while (child.pid && child.exitCode === null && child.signalCode === null && performance.now() < deadline) {
      const response = await fetch(`http://127.0.0.1:${port}/`, {
        redirect: 'error', signal: AbortSignal.timeout(500), headers: { connection: 'close' },
      }).catch(() => null);
      const body = response?.ok ? await response.text().catch(() => '') : '';
      if (body.includes(marker)) return;
      await delay(100);
    }
    throw new Error('Next.js did not serve its fixture page');
  } finally {
    const signalled = child.kill('SIGTERM');
    const stopped = await Promise.race([exited, delay(3000, false, { ref: false })]);
    if (!stopped) child.kill('SIGKILL');
    if (!signalled || !stopped) throw new Error('Next.js did not stop cleanly');
  }
}

smoke(process.argv[2]).catch(() => { process.exitCode = 1; });
'''


class ProviderInvocationError(RuntimeError):
    pass


def invoke(command, *, timeout, success=True, env=None):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout, env=env)
    try:
        report = json.loads(result.stdout).get('result')
    except (json.JSONDecodeError, AttributeError):
        report = None
    if not isinstance(report, dict):
        report = {}
    reason = report.get('reason_code')
    if (result.returncode == 0) != success or reason == 'RUN_CONTROL_UNVERIFIED' or not report:
        detail = f'registered provider fixture returned exit {result.returncode}'
        if reason in ('INSUFFICIENT_SPACE', 'MEASUREMENT_INCOMPLETE'):
            detail += f' ({reason})'
        if reason in ('RUN_COMPLETED', 'RUN_FAILED', 'RUN_TIMED_OUT', 'RUN_INTERRUPTED',
                      'RUN_CONTROL_UNVERIFIED'):
            run = report.get('run')
            child_exit = run.get('exit_code') if isinstance(run, dict) else None
            released = report.get('lease_released')
            detail += (f' ({reason}; child exit_code={json.dumps(child_exit if type(child_exit) is int else None)}'
                       f'; lease_released={json.dumps(released if isinstance(released, bool) else None)})')
        raise ProviderInvocationError(detail)
    return report


def prove_next(base, state, executable, modules):
    for name, version in NEXT_VERSIONS.items():
        manifest = (modules / name / 'package.json').resolve(strict=True)
        if not manifest.is_relative_to(modules) or json.loads(manifest.read_text())['version'] != version:
            raise RuntimeError(f'Next.js fixture requires staged {name} {version}.')
    next_bin = (modules / '.bin' / 'next').resolve(strict=True)
    if not next_bin.is_relative_to(modules) or not next_bin.is_file():
        raise RuntimeError('Next.js fixture requires the staged next command.')
    node, pnpm = shutil.which('node'), shutil.which('pnpm')
    if not node or not pnpm:
        raise RuntimeError('Next.js fixture requires local node and pnpm.')

    root = base / 'nextjs'
    pages = root / 'pages'
    pages.mkdir(parents=True)
    ignored = shutil.ignore_patterns('.env', '.env.*', '*.env', '*.env.*', '.npmrc', 'credentials*.json',
                                     '*.pem', '*.key', '*.p8', '*.p12', '*.pfx')
    count = 0
    size = 0
    for directory, folders, files in os.walk(modules):
        skip = ignored(directory, folders + files)
        folders[:] = [name for name in folders if name not in skip]
        for name in folders + [name for name in files if name not in skip]:
            path = Path(directory) / name
            count += 1
            if path.is_symlink():
                assert not path.readlink().is_absolute(), 'staged dependency link must be relative'
                assert path.resolve(strict=True).is_relative_to(modules), 'staged dependency link escaped its root'
            else:
                size += path.stat().st_size
            assert count <= 15000 and size <= 512 * 1024 * 1024, 'staged dependencies exceed fixture budget'
    shutil.copytree(modules, root / 'node_modules', symlinks=True, ignore=ignored)
    marker = 'dsg-next-fixture-' + secrets.token_hex(8)
    (pages / 'index.js').write_text(f'export default function Home() {{ return <main>{marker}</main>; }}\n')
    (root / 'next.config.js').write_text('module.exports = { experimental: { cpus: 1 } };\n')
    (root / 'package.json').write_text(json.dumps({
        'name': 'dsg-next-fixture', 'private': True,
        'scripts': {'build': 'next build --webpack'}, 'dependencies': NEXT_VERSIONS,
    }))
    (root / 'pnpm-workspace.yaml').write_text('verifyDepsBeforeRun: false\noffline: true\n')
    smoke = root / 'smoke.cjs'
    smoke.write_text(NEXT_SMOKE)

    home = base / 'next-home'
    home.mkdir()
    config = home / 'npmrc'
    global_config = home / 'global-npmrc'
    config.touch()
    global_config.touch()
    policy = home / 'policy.json'
    policy.write_text('{"host_min_free_gb":0,"project_budget_gb":1}')
    env = {
        'PATH': os.pathsep.join((str(Path(node).parent), str(Path(pnpm).parent), os.defpath)),
        'HOME': str(home), 'TMPDIR': str(home),
        'NPM_CONFIG_USERCONFIG': str(config), 'NPM_CONFIG_GLOBALCONFIG': str(global_config),
        'COREPACK_ENABLE_NETWORK': '0', 'NEXT_TELEMETRY_DISABLED': '1',
        'NODE_OPTIONS': '--max-old-space-size=512',
    }
    cli = [str(executable), '--root', str(root), '--state-dir', str(state),
           '--global-policy', str(policy), '--json']
    project = invoke(cli + ['init', '--project-id', 'nextjs-fixture'], timeout=10, env=env)['project']
    build = invoke(cli + ['exec', '--provider', 'pnpm', '--timeout', '45', '--', 'build'], timeout=50, env=env)
    assert build['provider'] == 'pnpm' and build['task'] == 'build' and build['workers'] == 1
    assert build['run']['outcome'] == 'success' and build['lease_released']
    output = root / '.next'
    assert (output / 'BUILD_ID').read_text().strip()
    assert marker in (output / 'server' / 'pages' / 'index.html').read_text()

    started = invoke(cli + ['exec', '--timeout', '20', '--', node, str(smoke), marker], timeout=25, env=env)
    assert started['run']['outcome'] == 'success' and started['lease_released']

    database = state / 'registry.sqlite3'
    assert database.is_file() and not database.is_symlink()
    with closing(sqlite3.connect(database)) as ledger:
        ledger.execute('PRAGMA query_only=ON')
        runs = list(ledger.execute('''
            SELECT audit_event.reason_code, execution_lease.id
            FROM audit_event JOIN execution_lease ON execution_lease.id = audit_event.lease_id
            WHERE audit_event.project_id = ? AND execution_lease.project_id = audit_event.project_id
              AND audit_event.action = 'managed_execution' AND audit_event.outcome = 'success'
              AND execution_lease.released_at_ms IS NOT NULL
            ORDER BY audit_event.id
        ''', (project['id'],)))
    assert [reason for reason, _ in runs] == ['PROVIDER_PNPM_BUILD_SUCCESS', 'RUN_COMPLETED']
    assert len({lease_id for _, lease_id in runs}) == 2
    print('Next.js: pnpm production build, prerendered route, loopback response and two released leases audited.',
          flush=True)


def assert_browser_trace(path):
    with ZipFile(path) as trace:
        for member in trace.infolist():
            if not member.filename.endswith('-trace.trace'):
                continue
            assert member.file_size <= 1024 * 1024, 'Browser trace exceeded the fixture limit'
            with trace.open(member) as events:
                records = [json.loads(line) for line in events]
            clicks = {entry['callId'] for entry in records
                      if entry.get('type') == 'before' and entry.get('method') == 'click'}
            if any(entry.get('type') == 'after' and entry.get('callId') in clicks
                   and not entry.get('error') for entry in records):
                return
    raise AssertionError('Playwright browser trace lacks a completed click')


def write_playwright_fixture(test, browser, *, fail=False, expected_failure=False):
    expected = 3 if fail else 2
    mark_failure = 'test.fail();' if expected_failure else ''
    if browser:
        body = f'''async ({{ page }}) => {{
            let requests = 0;
            page.on('request', () => {{ requests += 1; }});
            await page.setContent(`<button onclick="document.querySelector('output').textContent = '2'">Run</button><output>0</output>`);
            const output = page.locator('output');
            expect(await output.textContent()).toBe('0');
            await page.getByRole('button', {{ name: 'Run' }}).click();
            const observed = await output.textContent();
            expect(observed).toBe('2');
            expect(requests).toBe(0);
            {mark_failure}
            expect(observed).toBe('{expected}');
        }}'''
    else:
        body = f"async () => {{ {mark_failure} await test.step('check', () => expect(2).toBe({expected})); }}"
    test.write_text("const {test, expect} = require('@playwright/test');\n"
                    f"test('synthetic', {body});\n", encoding='utf-8')


def prove_playwright(base, state, executable, modules, browser=None):
    root = base / 'playwright'
    root.mkdir()
    cli = [str(executable), '--root', str(root), '--state-dir', str(state), '--json']
    if browser:
        if shutil.disk_usage(base).free < 2 * (1 << 30):
            raise RuntimeError('Chromium verification requires at least 2 GiB of free fixture storage.')
        policy = base / 'policy.json'
        policy.write_text('{"host_min_free_gb":1,"project_budget_gb":1}', encoding='utf-8')
        cli.extend(['--global-policy', str(policy)])
    shutil.copytree(modules, root / 'node_modules')
    prior = root / 'test-results' / 'keep.txt'
    prior.parent.mkdir()
    prior.write_text('keep', encoding='utf-8')
    if browser:
        config = {
            'timeout': 12000,
            'use': {'browserName': 'chromium', 'headless': True, 'offline': True,
                    'launchOptions': {'executablePath': str(browser),
                                      'args': ['--disable-background-networking']}},
        }
        (root / 'playwright.config.cjs').write_text(f'module.exports = {json.dumps(config)};\n',
                                                  encoding='utf-8')
    test = root / 'fixture.spec.js'
    project = invoke(cli + ['init'], timeout=10)['project']
    command = cli + ['exec', '--provider', 'playwright', '--timeout', '20']
    outputs = set()

    def verify_trace(report, *, success):
        assert report['lease_released'] and report['workers'] == 1
        assert report['run']['outcome'] == ('success' if success else 'failure')
        output = Path(report['evidence_path'])
        assert not output.is_absolute() and len(output.parts) == 2 and output.parts[1] == 'output'
        resolved = (state / output).resolve(strict=True)
        assert resolved.is_relative_to(state.resolve()) and resolved not in outputs
        outputs.add(resolved)
        traces = list(resolved.glob('*/trace.zip'))
        assert len(traces) == 1 and traces[0].is_file() and traces[0].stat().st_size > 0
        if browser:
            assert_browser_trace(traces[0])
        assert prior.read_text(encoding='utf-8') == 'keep'
        return traces[0]

    retained = []
    for success in (True, False):
        write_playwright_fixture(test, browser, fail=not success)
        report = invoke(command + ['--', 'test'], timeout=25, success=success)
        retained.append(verify_trace(report, success=success))
    print('Playwright: success and failure traces retained in separate run folders.', flush=True)

    ttl = command + ['--trace-ttl-seconds', '1', '--', 'test']
    write_playwright_fixture(test, browser)
    passed = invoke(ttl, timeout=25)
    assert passed['trace_ttl_eligible'] and passed['trace_retention']['registered'] == 1
    generated = verify_trace(passed, success=True)
    time.sleep(1.1)
    write_playwright_fixture(test, browser, fail=True, expected_failure=True)
    expected_failure = invoke(ttl, timeout=25)
    assert expected_failure['trace_ttl_eligible'] is False
    assert expected_failure['trace_retention']['sweep']['removed'] == 1
    assert not generated.exists() and all(path.is_file() for path in retained)
    verify_trace(expected_failure, success=True)
    print('Playwright TTL: one expired success trace reclaimed; prior and expected-failure traces preserved.',
          flush=True)

    database = state / 'registry.sqlite3'
    assert database.is_file() and not database.is_symlink()
    with closing(sqlite3.connect(database)) as ledger:
        ledger.execute('PRAGMA query_only=ON')
        runs = list(ledger.execute('''
            SELECT audit_event.reason_code, execution_lease.id
            FROM audit_event JOIN execution_lease ON execution_lease.id = audit_event.lease_id
            WHERE audit_event.project_id = ? AND execution_lease.project_id = audit_event.project_id
              AND audit_event.action = 'managed_execution' AND execution_lease.released_at_ms IS NOT NULL
            ORDER BY audit_event.id
        ''', (project['id'],)))
    assert [reason for reason, _ in runs] == ['PROVIDER_PLAYWRIGHT_TEST_SUCCESS',
                                             'PROVIDER_PLAYWRIGHT_TEST_FAILURE',
                                             'PROVIDER_PLAYWRIGHT_TEST_SUCCESS',
                                             'PROVIDER_PLAYWRIGHT_TEST_SUCCESS']
    assert len({lease_id for _, lease_id in runs}) == 4
    if browser:
        print('Playwright Chromium: completed clicks verified in four browser traces; four released leases audited.',
              flush=True)


def prove(base, executable, playwright_modules=None, browser=None):
    config = base / 'npm-config'
    config.touch()
    os.environ.update({
        'CARGO_TARGET_DIR': str(base / 'cargo-target'),
        'GOCACHE': str(base / 'go-cache'),
        'GOENV': 'off',
        'GRADLE_USER_HOME': str(base / 'gradle-cache'),
        'NPM_CONFIG_USERCONFIG': str(config),
        'UV_CACHE_DIR': str(base / 'uv-cache'),
        'UV_NO_CONFIG': '1',
        'PYTHONDONTWRITEBYTECODE': '1',
    })
    state = base / 'state'
    for name, (task, files) in PROJECTS.items():
        if not shutil.which(name):
            raise RuntimeError(f'requested runtime unavailable: {name}')
        root = base / name
        for relative, content in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding='utf-8')
        cli = [str(executable), '--root', str(root), '--state-dir', str(state), '--json']
        invoke(cli + ['init'], timeout=10)
        command = cli + ['exec', '--provider', name, '--timeout', '45', '--', task]
        completed = invoke(command, timeout=50)
        assert completed['provider'] == name and completed['task'] == task
        assert completed['workers'] == 1 and completed['lease_released']
        assert completed['run']['outcome'] == 'success'
        print(f'Registered execution passed: {name} {task}', flush=True)
        if name == 'node':
            (root / 'fixture.test.js').write_text(
                "const test = require('node:test'); test('fixture', () => { throw Error('failure'); });\n"
            )
            failed = invoke(command, timeout=20, success=False)
            assert failed['run']['outcome'] == 'failure' and failed['lease_released']
        if name == 'pnpm':
            assert (root / 'built.txt').read_text(encoding='utf-8') == 'fixture'
    if playwright_modules:
        prove_playwright(base, state, executable, playwright_modules, browser)
    database = state / 'registry.sqlite3'
    assert database.is_file() and not database.is_symlink()
    # The private fixture uses WAL. The SQLite client may need to create its
    # shared-memory index after the last writer closes.
    with closing(sqlite3.connect(database)) as ledger:
        ledger.execute('PRAGMA query_only=ON')
        reasons = set(ledger.execute(
            "SELECT reason_code FROM audit_event WHERE action = 'managed_execution' AND lease_id IS NOT NULL"
        ))
    for name, (task, _) in PROJECTS.items():
        assert (f'PROVIDER_{name.upper()}_{task.upper()}_SUCCESS',) in reasons
    assert ('PROVIDER_NODE_TEST_FAILURE',) in reasons
    print('Provider execution: success and failure audited with lease references.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('executable', type=Path)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--next-only', action='store_true')
    mode.add_argument('--playwright-only', action='store_true')
    args = parser.parse_args()
    next_modules = os.environ.get('DSG_TEST_NEXT_MODULES')
    playwright_modules = None if args.next_only else os.environ.get('DSG_TEST_PLAYWRIGHT_MODULES')
    browser_path = None if args.next_only else os.environ.get('DSG_TEST_PLAYWRIGHT_EXECUTABLE')
    if args.next_only and not next_modules:
        parser.error('--next-only requires DSG_TEST_NEXT_MODULES')
    if (args.playwright_only or browser_path) and not playwright_modules:
        parser.error('Playwright verification requires DSG_TEST_PLAYWRIGHT_MODULES')
    executable = args.executable.resolve(strict=True)
    modules = Path(playwright_modules).resolve(strict=True) if playwright_modules else None
    browser = Path(browser_path).resolve(strict=True) if browser_path else None
    if browser and (not browser.is_file() or not os.access(browser, os.X_OK)):
        parser.error('DSG_TEST_PLAYWRIGHT_EXECUTABLE must be an executable file')
    if browser and not args.playwright_only:
        parser.error('Browser verification requires --playwright-only to isolate and retain evidence')
    if args.playwright_only:
        base = Path(mkdtemp(prefix='dsg-pw-')).resolve()
        try:
            scratch = base / 'tmp'
            scratch.mkdir(mode=0o700)
            os.environ.update(TMPDIR=str(scratch), XDG_CACHE_HOME=str(base / 'cache'),
                              XDG_CONFIG_HOME=str(base / 'config'))
            prove_playwright(base, base / 'state', executable, modules, browser)
        except BaseException as error:
            print(f'Playwright fixture preserved at {base}', file=sys.stderr, flush=True)
            if isinstance(error, ProviderInvocationError):
                raise SystemExit(str(error)) from None
            raise
        shutil.rmtree(base)
        raise SystemExit(0)
    with TemporaryDirectory(prefix='dsg-provider-test-') as fixture:
        base = Path(fixture).resolve()
        if not args.next_only:
            prove(base, executable, modules, browser)
        if next_modules:
            prove_next(base, base / 'state', executable, Path(next_modules).resolve(strict=True))
