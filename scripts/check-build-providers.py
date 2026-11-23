"""Verify registered providers with isolated, dependency-free projects."""

import json
from contextlib import closing
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
from tempfile import TemporaryDirectory
import time


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


def invoke(command, *, timeout, success=True):
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if (result.returncode == 0) != success:
        raise RuntimeError(f'registered provider fixture returned exit {result.returncode}')
    return json.loads(result.stdout)['result']


def prove_playwright(base, state, executable, modules):
    root = base / 'playwright'
    root.mkdir()
    shutil.copytree(modules, root / 'node_modules')
    prior = root / 'test-results' / 'keep.txt'
    prior.parent.mkdir()
    prior.write_text('keep', encoding='utf-8')
    test = root / 'fixture.spec.js'
    cli = [str(executable), '--root', str(root), '--state-dir', str(state), '--json']
    invoke(cli + ['init'], timeout=10)
    command = cli + ['exec', '--provider', 'playwright', '--timeout', '20']
    outputs = set()
    retained = []
    for expected in (2, 3):
        test.write_text(
            "const {test, expect} = require('@playwright/test');\n"
            f"test('synthetic', async () => {{ await test.step('check', () => expect(2).toBe({expected})); }});\n",
            encoding='utf-8',
        )
        report = invoke(command + ['--', 'test'], timeout=25, success=expected == 2)
        assert report['lease_released'] and report['workers'] == 1
        output = Path(report['evidence_path'])
        assert not output.is_absolute() and len(output.parts) == 2 and output.parts[1] == 'output'
        resolved = (state / output).resolve(strict=True)
        assert resolved.is_relative_to(state.resolve()) and resolved not in outputs
        outputs.add(resolved)
        traces = list(resolved.glob('*/trace.zip'))
        assert len(traces) == 1 and traces[0].is_file() and traces[0].stat().st_size > 0
        retained.extend(traces)
        assert prior.read_text(encoding='utf-8') == 'keep'
    print('Playwright: native success and failure traces retained in separate run folders.', flush=True)

    ttl = command + ['--trace-ttl-seconds', '1', '--', 'test']
    test.write_text("const {test} = require('@playwright/test'); test('passed', () => {});\n")
    passed = invoke(ttl, timeout=25)
    assert passed['trace_ttl_eligible'] and passed['trace_retention']['registered'] == 1
    generated = list((state / passed['evidence_path']).glob('*/trace.zip'))
    assert len(generated) == 1 and generated[0].is_file()
    time.sleep(1.1)
    test.write_text("const {test, expect} = require('@playwright/test');\n"
                    "test('expected failure', () => {test.fail(); expect(2).toBe(3);});\n")
    expected_failure = invoke(ttl, timeout=25)
    assert expected_failure['trace_ttl_eligible'] is False
    assert expected_failure['trace_retention']['sweep']['removed'] == 1
    assert not generated[0].exists() and all(path.is_file() for path in retained)
    assert len(list((state / expected_failure['evidence_path']).glob('*/trace.zip'))) == 1
    assert prior.read_text(encoding='utf-8') == 'keep'
    print('Playwright TTL: one expired success trace reclaimed; prior and expected-failure traces preserved.',
          flush=True)


def prove(base, executable):
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
    playwright_modules = os.environ.get('DSG_TEST_PLAYWRIGHT_MODULES')
    if playwright_modules:
        prove_playwright(base, state, executable, Path(playwright_modules).resolve(strict=True))
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
    if playwright_modules:
        assert {('PROVIDER_PLAYWRIGHT_TEST_SUCCESS',), ('PROVIDER_PLAYWRIGHT_TEST_FAILURE',)} <= reasons
    print('Provider execution: success and failure audited with lease references.', flush=True)


if __name__ == '__main__':
    with TemporaryDirectory(prefix='dsg-provider-test-') as fixture:
        prove(Path(fixture).resolve(), Path(sys.argv[1]).resolve(strict=True))
