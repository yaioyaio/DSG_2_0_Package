"""Check CLI parity, policy validation and logical sizes on synthetic projects."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def observe(arguments, cwd):
    result = subprocess.run(arguments, cwd=cwd, capture_output=True, text=True, timeout=8)
    if result.returncode:
        raise SystemExit(f'Fixture CLI exited with {result.returncode}.')
    return json.loads(result.stdout)


def main(executable):
    poc = Path(__file__).resolve().parent.parent / 'poc'
    with tempfile.TemporaryDirectory(prefix='dsg-foundation-parity-') as fixture:
        base = Path(fixture)
        project = base / 'project'
        inputs = {
            'dist/output.txt': 'safe artifact',
            'build/nested/Data/fixture.txt': 'protected artifact',
            'target/nested/pinned/fixture.txt': 'project protection',
        }
        for relative, contents in inputs.items():
            path = project / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding='utf8')
        (project / '.dsg.json').write_text(json.dumps({'protected_names': ['pinned']}), encoding='utf8')
        aged = time.time() - 9 * 86400
        for directory in ('dist', 'build', 'target'):
            os.utime(project / directory, (aged, aged))

        python_cli = [sys.executable, '-m', 'dsg', 'gc-plan', '--root', str(project)]
        rust_cli = [str(executable), '--root', str(project), '--state-dir', str(base / 'rust-state'), '--json']
        os.environ['DSG_STATE_HOME'] = str(base / 'python-state')
        python = observe(python_cli, poc)['candidates']
        rust = observe(rust_cli + ['gc-plan'], base)['result']['plan']
        expected = [('dist', len(inputs['dist/output.txt'].encode()))]
        for candidates in (python, rust['candidates']):
            assert [(item['relative_path'], item['bytes']) for item in candidates] == expected
        assert rust['dry_run'] is True
        for relative, contents in inputs.items():
            assert (project / relative).read_text(encoding='utf8') == contents
        print('Python/Rust GC parity: protected trees preserved, expected candidate and byte count match.')

        for invalid in (
            {'project_budget_gb': 'not-a-number'},
            {'unknown_fixture_field': True},
        ):
            (project / '.dsg.json').write_text(json.dumps(invalid), encoding='utf8')
            failed = subprocess.run(rust_cli + ['doctor'], cwd=base, capture_output=True, text=True, timeout=8)
            assert failed.returncode == 2 and failed.stdout == ''
            assert json.loads(failed.stderr)['error']['code'] == 'CONFIG_INVALID'

        sparse_project = base / 'sparse-project'
        build = sparse_project / 'build'
        build.mkdir(parents=True)
        sparse = build / 'fixture.bin'
        logical_bytes = 5 * (1 << 30)
        with sparse.open('xb') as output:
            output.truncate(logical_bytes)
        assert sparse.stat().st_blocks * 512 < (1 << 20)
        report = observe([str(executable), '--root', str(sparse_project), '--json', 'scan'], base)
        scan = report['result']['scan']
        assert scan['complete'] and scan['total_bytes'] == logical_bytes
        print('Rust Foundation: invalid policies rejected and sparse logical size measured.')


if __name__ == '__main__':
    main(Path(sys.argv[1]).resolve())
