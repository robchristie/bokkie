"""Continuation must preserve the dogfood outcome and its original inputs."""
import importlib.util
import json
from pathlib import Path
import sqlite3
import tempfile
import time
import unittest

SPEC = importlib.util.spec_from_file_location(
    'knowledge_driver', Path(__file__).resolve().parents[1] / 'supervise-knowledge.py')
DRIVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DRIVER)


class RetainedRunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / 'app'
        self.workspace.mkdir()
        knowledge = self.root / 'knowledge'
        knowledge.mkdir()
        (knowledge / 'Home.md').write_text('# Original source\n')
        DRIVER.dump(self.root / 'source-before.json', DRIVER.hashes(knowledge))
        DRIVER.dump(self.root / 'submitted-intent.json', {'intent': 'Saved intent'})
        self.profile = {'workspace': str(self.workspace), 'broker_root': str(self.root / 'brokers')}
        DRIVER.dump(self.root / 'profile.json', self.profile)
        import hashlib
        self.history = [{'kind': 'intent_saved', 'receipt': {'outcome_id': 'outcome'},
                         'profile_sha256': hashlib.sha256((self.root / 'profile.json').read_bytes()).hexdigest()},
                        {'kind': 'qualification_interrupted', 'error': 'infrastructure'}]
        DRIVER.dump(self.root / 'journey.json', self.history)
        self.state = {'contracts': [{'contract': {'intent': 'Saved intent',
                        'budget': {'deadline': int(time.time()) + 600}}}]}
        self.connection = sqlite3.connect(self.root / 'supervision.sqlite')
        self.addCleanup(self.connection.close)
        self.connection.executescript('''
            CREATE TABLE engineering_outcomes(id TEXT, state_revision INTEGER, root_obligation_id TEXT);
            CREATE TABLE engineering_versions(outcome_id TEXT, revision INTEGER, snapshot_json TEXT);
            CREATE TABLE obligations(id TEXT, state TEXT);
            INSERT INTO engineering_outcomes VALUES ('outcome', 1, 'root');
            INSERT INTO obligations VALUES ('root', 'pending');
        ''')
        self.connection.execute('INSERT INTO engineering_versions VALUES (?, ?, ?)',
                                ('outcome', 1, json.dumps(self.state)))
        self.connection.commit()

    def test_resume_reads_original_identity_without_rewriting_any_input(self):
        before = DRIVER.hashes(self.root)
        profile, history, sources, outcome = DRIVER.retained_run(self.root, self.workspace)
        self.assertEqual(profile, self.profile)
        self.assertEqual(history, self.history)
        self.assertEqual(outcome, 'outcome')
        self.assertEqual(sources, DRIVER.hashes(self.root / 'knowledge'))
        self.assertEqual(DRIVER.hashes(self.root), before)

    def test_resume_rejects_changed_profile_source_and_workspace(self):
        with self.assertRaisesRegex(ValueError, 'workspace or profile'):
            DRIVER.retained_run(self.root, self.root / 'different')
        DRIVER.dump(self.root / 'profile.json', {**self.profile, 'max_turns': 999})
        with self.assertRaisesRegex(ValueError, 'workspace or profile'):
            DRIVER.retained_run(self.root, self.workspace)
        DRIVER.dump(self.root / 'profile.json', self.profile)
        (self.root / 'knowledge/Home.md').write_text('changed')
        with self.assertRaisesRegex(ValueError, 'knowledge changed'):
            DRIVER.retained_run(self.root, self.workspace)

    def test_resume_cannot_reset_deadline_or_reopen_terminal_outcome(self):
        self.connection.execute("UPDATE obligations SET state='cancelled'")
        self.connection.commit()
        with self.assertRaisesRegex(ValueError, 'non-terminal'):
            DRIVER.retained_run(self.root, self.workspace)
        self.connection.execute("UPDATE obligations SET state='pending'")
        self.state['contracts'][0]['contract']['budget']['deadline'] = 1
        self.connection.execute('UPDATE engineering_versions SET snapshot_json=?', (json.dumps(self.state),))
        self.connection.commit()
        with self.assertRaisesRegex(ValueError, 'deadline exhausted'):
            DRIVER.retained_run(self.root, self.workspace)

    def test_resume_rejects_ambiguous_intake_and_changed_original_intent(self):
        DRIVER.dump(self.root / 'journey.json', self.history + [self.history[0]])
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            DRIVER.retained_run(self.root, self.workspace)
        DRIVER.dump(self.root / 'journey.json', self.history)
        DRIVER.dump(self.root / 'submitted-intent.json', {'intent': 'Different intent'})
        with self.assertRaisesRegex(ValueError, 'original intent'):
            DRIVER.retained_run(self.root, self.workspace)


class ContinuationBudgetTests(unittest.TestCase):
    def state(self):
        return {'observed_root_state':'cancelled', 'acceptance':None,
                'contracts':[{'contract':{'budget':{'max_turns':24,
                'max_packages':8,'max_repairs':3,'max_recoveries':3,
                'max_questions':64,'max_checkpoints':512,
                'deadline':int(time.time())+600}}}],
                'turns_used':11,'recoveries_used':1,'packages':[1,2,3],
                'repairs':[], 'questions':[1],
                'executions':[{'cessation_verified':True,'checkpoints':[1,2]}]}

    def test_all_consumable_budgets_and_absolute_deadline_are_conserved(self):
        state = self.state()
        profile = DRIVER.continuation_profile({'workspace':'unchanged'},state)
        self.assertEqual({k:profile[k] for k in ('max_turns','max_packages',
                         'max_repairs','max_recoveries','max_questions','max_checkpoints')},
                         dict(max_turns=13,max_packages=5,max_repairs=3,
                              max_recoveries=2,max_questions=63,max_checkpoints=510))
        self.assertEqual(profile['deadline_at'],state['contracts'][0]['contract']['budget']['deadline'])
        self.assertEqual(profile['workspace'],'unchanged')

    def test_replacement_requires_proven_cessation_and_remaining_budget(self):
        state = self.state(); state['executions'][0]['cessation_verified']=False
        with self.assertRaisesRegex(ValueError,'not reconciled'):
            DRIVER.continuation_profile({},state)
        state=self.state();state['turns_used']=24
        with self.assertRaisesRegex(ValueError,'budget exhausted'):
            DRIVER.continuation_profile({},state)
        state=self.state();state['observed_root_state']='pending'
        with self.assertRaisesRegex(ValueError,'cancelled'):
            DRIVER.continuation_profile({},state)

    def test_lost_intake_acknowledgement_reuses_exact_pending_capsule(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            DRIVER.dump(root/'submitted-intent.json',{'intent':'Original milestone'})
            intervention=root/'repair.json';DRIVER.dump(intervention,{'repair':'adapter'})
            state=self.state()
            first=DRIVER.prepare_continuation(root,{'workspace':'app'},state,'old',intervention)
            # Simulate the server saving intake and the HTTP response being lost:
            # history was not advanced, and retry happens thirty seconds later.
            saved_request=(first[3]['command_id'],first[2].read_bytes(),first[1].read_bytes())
            before=DRIVER.hashes(root)
            with patch.object(DRIVER.time,'time',return_value=time.time()+30):
                replay=DRIVER.prepare_continuation(root,{'workspace':'app'},state,'old',intervention)
            self.assertEqual((replay[3]['command_id'],replay[2].read_bytes(),replay[1].read_bytes()),saved_request)
            self.assertEqual(first,replay)
            self.assertEqual(DRIVER.hashes(root),before)
            DRIVER.dump(replay[1],{'workspace':'tampered'})
            with self.assertRaisesRegex(ValueError,'identity changed'):
                DRIVER.prepare_continuation(root,{},state,'old',intervention)

    def test_continuation_chain_rejects_forks_and_loops(self):
        start={'kind':'intent_saved','receipt':{'outcome_id':'a'}}
        next_event={'kind':'continuation_intake_saved','previous_outcome_id':'a',
                    'receipt':{'outcome_id':'b'}}
        self.assertEqual(DRIVER.retained_intake([start,next_event]),next_event)
        with self.assertRaisesRegex(ValueError,'one chain'):
            DRIVER.retained_intake([start,next_event,next_event])
        with self.assertRaisesRegex(ValueError,'duplicate'):
            DRIVER.retained_intake([start,{**next_event,'receipt':{'outcome_id':'a'}}])


if __name__ == '__main__':
    unittest.main()
