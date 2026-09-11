"""No-model probes of qualification-driver acceptance and ownership boundaries."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('fixture_runner', Path(__file__).resolve().parents[1] / 'qualify-engineering.py')
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


class RunnerTests(unittest.TestCase):
    def test_offline_marker_requires_actual_worker_command(self):
        worker = {'role': 'worker'}
        fake = {'kind': 'item/started', 'value': {'item': {'type': 'dynamicToolCall', 'command': 'bokkie-offline-window'}}}
        self.assertFalse(r.offline_command_seen([(worker, fake)]))
        fake['value']['item']['type'] = 'commandExecution'
        self.assertTrue(r.offline_command_seen([(worker, fake)]))
        self.assertFalse(r.offline_command_seen([({'role': 'supervisor'}, fake)]))

    def test_acceptance_requires_every_fixture_observation(self):
        state = {'repairs': ['repair'], 'acceptance': {'id': 'accept'}, 'questions': [{'resolution': 'answer'}]}
        r.validate_acceptance_observations(state, True, True)
        for key in state:
            with self.assertRaises(AssertionError):
                r.validate_acceptance_observations({**state, key: []}, True, True)
        for flags in [(False, True), (True, False)]:
            with self.assertRaises(AssertionError):
                r.validate_acceptance_observations(state, *flags)

    def test_reconciliation_requires_terminal_outcome_and_stopped_controller(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with sqlite3.connect(root / 'fixture.sqlite') as connection:
                connection.execute('CREATE TABLE engineering_outcomes(id TEXT)')
                connection.execute("INSERT INTO engineering_outcomes VALUES('outcome')")
            for executions in [[], [{'cessation_verified': True}]]:
                with patch.object(r, 'snapshot', return_value={'observed_root_state': 'pending', 'executions': executions}):
                    self.assertFalse(r.reconciled(root))
            with patch.object(r, 'snapshot', return_value={'observed_root_state': 'cancelled', 'executions': [{'cessation_verified': True}]}):
                self.assertTrue(r.reconciled(root))
                r.dump(root / 'controller-identity.json', r.process_identity(os.getpid()))
                self.assertFalse(r.reconciled(root))

    def test_driver_death_stops_controller_before_dispatch_can_resume(self):
        with tempfile.TemporaryDirectory() as temporary:
            receipt = Path(temporary) / 'child.json'
            script = '''import importlib.util,json,os,pathlib,subprocess,sys,time
spec=importlib.util.spec_from_file_location('r',sys.argv[1]);r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)
parent=os.getpid()
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'],start_new_session=True,preexec_fn=lambda:r.stop_with_parent(parent))
r.dump(pathlib.Path(sys.argv[2]),r.process_identity(child.pid))
time.sleep(30)
'''
            parent = subprocess.Popen([sys.executable, '-c', script, str(Path(r.__file__)), str(receipt)])
            try:
                until = time.monotonic() + 5
                while not receipt.exists() and time.monotonic() < until:
                    time.sleep(.01)
                self.assertTrue(receipt.exists())
                child = json.loads(receipt.read_text())
                parent.kill(); parent.wait(timeout=5)
                until = time.monotonic() + 5
                while r.process_identity(child['pid']) == child and time.monotonic() < until:
                    time.sleep(.01)
                self.assertNotEqual(r.process_identity(child['pid']), child)
            finally:
                if parent.poll() is None:
                    parent.kill(); parent.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
