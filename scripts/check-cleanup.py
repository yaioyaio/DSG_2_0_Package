"""Test approval, automatic run events, and Git closure only on owned fixtures."""

import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time


def execute(command, *, expected=0, stdin=None, cwd=None, timeout=12):
    process = subprocess.run(command, input=stdin, capture_output=True, text=True, timeout=timeout, check=False, cwd=cwd)
    if process.returncode != expected:
        code = None
        try:
            code = json.loads(process.stderr).get('error', {}).get('code')
        except (ValueError, AttributeError):
            pass
        raise RuntimeError(f'{Path(command[0]).name} exit {process.returncode}, expected {expected}, application code {code}.')
    return process.stdout


def rust_result(cli, *arguments, expected=0, stdin=None):
    return json.loads(execute(cli + list(arguments), expected=expected, stdin=stdin))['result']


def verify_manual(cli, root, state):
    protected = root / 'build' / 'data' / 'original.txt'
    protected.parent.mkdir(parents=True)
    protected.write_text('protected')
    execute(cli + ['gc-own', '--candidate', 'build', '--confirm-generated'], expected=2)
    dist = root / 'dist'
    dist.mkdir()
    generated = dist / 'artifact.txt'
    generated.write_text('owned generated fixture')
    age = time.time() - 9 * 86400
    os.utime(dist, (age, age))
    rust_result(cli, 'gc-own', '--candidate', 'dist', '--confirm-generated')

    def plan_approval():
        plan = rust_result(cli, 'gc-plan')
        assert [value['relative_path'] for value in plan['plan']['candidates']] == ['dist']
        number = str(plan['record']['plan_id'])
        approval = rust_result(cli, 'gc-approve', '--plan', number, '--candidate', 'dist', '--confirm')
        return number, approval['approval'] + '\n'

    stale_plan, stale_approval = plan_approval()
    generated.write_text('changed generated item!')
    preserved = rust_result(cli, 'gc-apply', '--plan', stale_plan, '--approval-stdin', stdin=stale_approval, expected=5)
    assert preserved['status'] == 'preserved' and generated.exists()
    approved_plan, approved = plan_approval()
    execute(cli + ['gc-apply', '--plan', approved_plan, '--approval-stdin'], stdin=('0' * 64) + '\n', expected=2)
    reclaimed = rust_result(cli, 'gc-apply', '--plan', approved_plan, '--approval-stdin', stdin=approved)
    assert reclaimed['status'] == 'applied' and reclaimed['reclaimed_bytes'] > 0
    execute(cli + ['gc-apply', '--plan', approved_plan, '--approval-stdin'], stdin=approved, expected=2)
    assert protected.read_text() == 'protected' and not dist.exists()
    with sqlite3.connect(state / 'registry.sqlite3') as db:
        events = db.execute("""
            SELECT apply.action, lease.action, execution_lease.released_at_ms IS NOT NULL
            FROM audit_event AS apply
            JOIN execution_lease ON execution_lease.id = apply.lease_id
            JOIN audit_event AS lease ON lease.lease_id = execution_lease.id
            WHERE apply.plan_id = ? AND apply.reason_code = 'GC_APPLIED'
              AND lease.reason_code = 'GC_LEASE_RELEASED'
        """, (approved_plan,)).fetchall()
        assert events == [('gc_apply', 'managed_execution', 1)]
    print(f'Approved GC: removed=1, bytes={reclaimed["reclaimed_bytes"]}, protected=1, changed=1, replay rejected.')


def verify_cargo_queue(cli, root):
    cargo = root / 'cargo'
    cargo.write_text('#!/bin/sh\nset -e\nmkdir "$CARGO_TARGET_DIR/debug"\nprintf fixture > "$CARGO_TARGET_DIR/debug/artifact.txt"\ncase "$2" in\nfail) exit 17;;\nwait) : > fixture-ready; exec sleep 4;;\nesac\n')
    cargo.chmod(0o700)
    args = cli + ['exec', '--timeout', '8', '--auto-clean-cargo', '--', str(cargo), 'check']
    success = json.loads(execute(args))['result']['cleanup']
    assert success['removed'] == 1 and success['reclaimed_bytes'] == len('fixture'), success
    failure = json.loads(execute(args + ['fail'], expected=17))['result']['cleanup']
    assert failure['removed'] == 0 and failure['preserved'] == 1
    with subprocess.Popen(args + ['wait'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as child:
        deadline = time.monotonic() + 3
        while not (root / 'fixture-ready').exists() and time.monotonic() < deadline and child.poll() is None:
            time.sleep(0.03)
        assert child.poll() is None and (root / 'fixture-ready').exists()
        child.send_signal(signal.SIGINT)
        stdout, _ = child.communicate(timeout=8)
        assert child.returncode == 130
    interrupted = json.loads(stdout)['result']['cleanup']
    assert interrupted['removed'] + interrupted['preserved'] == 1
    print(f'Automatic Cargo events: success removed=1/{success["reclaimed_bytes"]} B, failure preserved=1, interrupted removed={interrupted["removed"]}, preserved={interrupted["preserved"]}.')


def verify_git(executable, base, completion):
    remote = base / 'remote.git'
    project = base / 'git-fixture'
    auxiliary = base / 'worktree-fixture'
    state = base / 'git-state'
    for name, value in {'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                        'GIT_AUTHOR_NAME': 'DSG Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                        'GIT_COMMITTER_NAME': 'DSG Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid',
                        'DSG_CLI': executable}.items():
        os.environ[name] = value
    execute(['git', 'init', '--bare', '--initial-branch=main', str(remote)])
    execute(['git', 'init', '--initial-branch=main', str(project)])
    def git(*arguments):
        return execute(['git', '-C', str(project), *arguments]).strip()

    path = project / 'fixture.txt'
    path.write_text('registered Git fixture')
    git('add', 'fixture.txt')
    git('commit', '-m', 'fixture: verified initial tree')
    git('branch', 'develop')
    feature = time.strftime('feature/%Y%m%d-cleanup-fixture')
    git('branch', feature)
    git('remote', 'add', 'origin', str(remote))
    git('push', 'origin', 'main', 'develop', feature)
    git('switch', 'develop')
    dsg = [executable, '--root', str(project), '--state-dir', str(state), '--json']
    rust_result(dsg, 'worktree-create', '--branch', feature, '--destination', str(auxiliary))
    assert auxiliary.is_dir()
    report = json.loads(execute(['sh', str(completion), feature, str(state)], cwd=project))['result']
    assert report['removed'] == 1 and not auxiliary.exists(), report
    assert git('branch', '--show-current') == 'develop' and git('rev-parse', feature)
    assert git('rev-parse', f'origin/{feature}') and 'registered' in path.read_text()
    print(f'Git completion: registered and removed=1, reserved={report["preserved"]}, reported bytes={report["reclaimed_bytes"]}, refs and main workspace preserved.')


if __name__ == '__main__' and sys.platform.startswith(('darwin', 'linux')):
    binary = str(Path(sys.argv[1]).resolve())
    git_completion = Path(__file__).resolve().with_name('git-completion.sh')
    with tempfile.TemporaryDirectory(prefix='dsg-cleanup-test-') as fixture:
        base = Path(fixture)
        state = base / 'state'
        settings = base / 'policy.json'
        settings.write_text('{"host_min_free_gb":0,"project_budget_gb":1}')

        def fixture_project(name):
            root = base / name
            root.mkdir()
            cli = [binary, '--root', str(root), '--state-dir', str(state), '--global-policy', str(settings), '--json']
            rust_result(cli, 'init', '--project-id', name)
            return cli, root

        cli, project = fixture_project('cleanup-fixture')
        verify_manual(cli, project, state)
        denied = rust_result(cli, 'exec', '--timeout', '1', '--', 'true', expected=4)
        assert denied['reason_code'] == 'MEASUREMENT_INCOMPLETE'
        cargo_cli, cargo_project = fixture_project('cargo-cleanup-fixture')
        verify_cargo_queue(cargo_cli, cargo_project)
        verify_git(binary, base, git_completion)
