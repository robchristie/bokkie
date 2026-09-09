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


if __name__ == '__main__':
    unittest.main()
