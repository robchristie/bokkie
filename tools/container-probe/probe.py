"""Bounded, offline container observations using synthetic data only."""
import importlib.util
import errno
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def emit(value):
    print(json.dumps(value, sort_keys=True), flush=True)


def run(argv, **kwargs):
    return subprocess.run(argv, text=True, capture_output=True, timeout=30, **kwargs)


def namespaces():
    return {name: os.readlink('/proc/self/ns/' + name) for name in ('user', 'mnt', 'pid')}


def inner():
    try:
        Path('/data/boundary-canary').write_text('unexpected write')
        read_only = False
    except OSError as error:
        read_only = error.errno == errno.EROFS
    Path('/tmp/private-write').write_text('private')
    emit({'namespaces': namespaces(), 'root_read_only': read_only,
          'account_hidden': not Path('/home/probe/.codex/canary').exists(),
          'outer_tmp_hidden': not Path('/tmp/outer-canary').exists()})


def boundary():
    if os.getuid() == 0:
        raise RuntimeError('probe requires a non-root user')
    spec = importlib.util.spec_from_file_location('broker', '/opt/conversation/broker.py')
    broker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(broker)
    Path('/home/probe/.codex/canary').write_text('synthetic account canary')
    Path('/tmp/outer-canary').write_text('outer temporary directory')
    # This mount is writable outside Bubblewrap, so EROFS proves the inner remount.
    Path('/data/boundary-canary').write_text('outer writable state')
    profile = {'bwrap': '/usr/bin/bwrap', 'codex': '/usr/local/bin/codex'}
    command = broker.command(profile, {})
    command = command[:command.index('--') + 1] + ['/usr/bin/python3', '/opt/probe.py', 'inner']
    result = run(command)
    report = {'uid': os.getuid(), 'outer_namespaces': namespaces(), 'command': command,
              'exit_code': result.returncode, 'stderr': result.stderr,
              'model_calls': 0, 'credentials_mounted': False}
    if result.returncode:
        report['result'] = 'blocked'
        emit(report)
        return 20
    observed = json.loads(result.stdout)
    report['inner'] = observed
    report['result'] = 'passed' if (
        all(observed[key] for key in ('root_read_only', 'account_hidden', 'outer_tmp_hidden'))
        and all(observed['namespaces'][name] != report['outer_namespaces'][name]
                for name in ('mnt', 'pid'))
        and not Path('/tmp/private-write').exists()
    ) else 'failed'
    emit(report)
    return 0 if report['result'] == 'passed' else 1


def preflight():
    profile = {'broker': '/opt/conversation/broker.py', 'codex': '/usr/local/bin/codex',
               'bwrap': '/usr/bin/bwrap', 'model': 'gpt-5.6-terra', 'effort': 'medium',
               'timezone': 'Australia/Adelaide', 'timeout_seconds': 15,
               'max_context_bytes': 4096, 'max_output_bytes': 4096}
    # A zero-model broker preflight never sends turn/start, and uses no credentials.
    result = run(['/usr/bin/python3', profile['broker']],
                 input=json.dumps({'profile': profile, 'preflight': True}))
    emit({'exit_code': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr})
    return result.returncode


def fixture(resume, controls):
    argv = ['/usr/local/bin/bokkie-conversation-fixture', '--root', '/data/fixture']
    if resume:
        argv.append('--resume')
    result = run(argv, input=''.join(json.dumps(x) + '\n' for x in controls + [{'stop': True}]))
    if result.returncode:
        raise RuntimeError(result.stderr)
    values = [json.loads(line) for line in result.stdout.splitlines()]
    if any('error' in value for value in values):
        raise RuntimeError(str(values))
    return values


def persistence(stage):
    database = '/data/fixture/fixture.sqlite'
    if stage == 'seed':
        fixture(False, [{'seed_calibration': True}])
        created = run(['bokkie', '--database', database, 'create', '--id', 'container-probe-future',
                       '--description', 'Synthetic future container probe',
                       '--scheduled-at', '4102444800', '--recurrence-cron', '0 30 8 * * MON',
                       '--recurrence-timezone', 'Australia/Adelaide'])
        if created.returncode:
            raise RuntimeError(created.stderr)
        values = fixture(True, [{}])
        expected = {'fixture': values[-1], 'obligation': json.loads(created.stdout)}
        Path('/data/expected.json').write_text(json.dumps(expected))
        emit({'result': 'seeded', **expected})
    elif stage == 'verify':
        expected = json.loads(Path('/data/expected.json').read_text())
        values = fixture(True, [{}])
        shown = run(['bokkie', '--database', database, 'show', 'container-probe-future'])
        doctor = run(['bokkie', '--database', database, 'doctor'])
        if shown.returncode or doctor.returncode:
            raise RuntimeError(shown.stderr + doctor.stderr)
        health = json.loads(doctor.stdout)
        if not health['summary']['healthy'] or health['summary']['failed'] != 0:
            raise RuntimeError('persisted database failed doctor: ' + doctor.stdout)
        if values[-1] != expected['fixture'] or json.loads(shown.stdout) != expected['obligation']:
            raise RuntimeError('persisted domain identity or state changed')
        emit({'result': 'passed', 'fixture': values[-1], 'obligation': json.loads(shown.stdout),
              'doctor': health, 'model_calls': 0})
    else:
        raise ValueError('invalid persistence stage')
    return 0


def hold():
    stopped = False

    def stop(*_):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    emit({'ready': True, 'uid': os.getuid()})
    while not stopped:
        time.sleep(0.1)
    emit({'stopped': True})


if __name__ == '__main__':
    command = sys.argv[1]
    if command == 'inner':
        inner()
    elif command == 'hold':
        hold()
    elif command == 'boundary':
        sys.exit(boundary())
    elif command == 'preflight':
        sys.exit(preflight())
    elif command == 'persistence':
        sys.exit(persistence(sys.argv[2]))
    else:
        raise ValueError('unknown command')
