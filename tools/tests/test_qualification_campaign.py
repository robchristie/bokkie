"""Admission invariants use a real temporary SQLite registry and no model calls."""
import concurrent.futures
import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('qualification_campaign', Path(__file__).parents[1] / 'qualification_campaign.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
Campaign, AdmissionDenied = MODULE.Campaign, MODULE.AdmissionDenied


class CampaignTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'registry.sqlite'
        self.c = Campaign.open(self.path, 'work-package', 'campaign')
        self.addCleanup(self.c.close)
        self.fp = {'source': 'one', 'adapter': 'one'}

    def reserve(self, identity='a', **kwargs):
        return self.c.reserve(identity, 'complete_fixture', kwargs.pop('fingerprint', self.fp), root_turns=1, authority_turns=0, subagents=0, contexts_per_execution=None, **kwargs)

    def failed(self, identity='a', fingerprint=None):
        self.reserve(identity, fingerprint=fingerprint or self.fp)
        self.c.mark_launched(identity)
        self.c.complete(identity, False, ' Tool / Reply Overflow ', ['adapter'], {'log': identity})

    def repaired(self, failure='a', adapter='two', diagnosis=False):
        fp = dict(self.fp, adapter=adapter)
        self.c.record_repair(failure, fp, {'patch': adapter, 'diagnosis': diagnosis})
        self.c.record_probe('probe-' + adapter, failure, fp, True, {'test': 'bounded reply regression'})
        return fp

    def test_default_conservative_envelope_and_final_headroom(self):
        result = self.c.reserve('a', 'complete_fixture', self.fp)
        self.assertEqual(result['envelope'], 240)
        self.assertEqual(self.c.report()['limits']['final_headroom'], 240)

    def test_scope_and_allowance_cannot_be_rebound(self):
        with self.assertRaises(AdmissionDenied):
            Campaign.open(self.path, 'work-package', 'fresh-id')
        with self.assertRaises(AdmissionDenied):
            Campaign.open(self.path, 'work-package', 'campaign', {'contexts': 999})
        reopened = Campaign.open(self.path, 'work-package', 'campaign')
        self.addCleanup(reopened.close)
        self.assertEqual(reopened.limits, self.c.limits)

    def test_crash_before_launch_and_resume_cannot_double_spend(self):
        self.reserve()
        reopened = Campaign.open(self.path, 'work-package', 'campaign')
        self.addCleanup(reopened.close)
        for identity in ('a', 'b'):
            with self.assertRaises(AdmissionDenied):
                reopened.reserve(identity, 'live_probe', self.fp)
        self.assertEqual(reopened.report()['reserved_contexts'], 1)
        reopened.mark_launched('a')
        with self.assertRaises(AdmissionDenied):
            self.c.mark_launched('a')
        self.c.interrupt('a', {'process': 'unknown'})
        with self.assertRaises(AdmissionDenied):
            self.reserve('b')
        self.c.complete('a', False, 'interrupted', evidence={'cessation_verified': True})
        self.assertEqual(self.c.report()['reserved_contexts'], 1)

    def test_concurrent_admission_serialises_before_launch(self):
        def admit(identity):
            campaign = Campaign.open(self.path, 'work-package', 'campaign')
            try:
                campaign.reserve(identity, 'live_probe', self.fp, root_turns=1, authority_turns=0, subagents=0, contexts_per_execution=None)
                return True
            except AdmissionDenied:
                return False
            finally:
                campaign.close()
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            result = list(pool.map(admit, ['a', 'b']))
        self.assertEqual(sorted(result), [False, True])
        self.assertEqual(self.c.report()['reserved_contexts'], 1)

    def test_unchanged_relevant_fingerprint_suppressed(self):
        self.failed()
        self.assertEqual(self.c.report()['attempts'][0]['failure'], 'payload_size')
        with self.assertRaises(AdmissionDenied):
            self.reserve('b', fingerprint=dict(self.fp, source='unrelated'))
        with self.assertRaises(ValueError):
            self.c.record_repair('a', dict(self.fp, source='unrelated'), {'patch': 'unrelated'})
        self.assertEqual(len(self.c.report()['suppressions']), 1)

    def test_repair_and_passing_relevant_probe_both_required(self):
        self.failed()
        fp = dict(self.fp, adapter='two')
        self.c.record_repair('a', fp, {'patch': 'fix'})
        with self.assertRaises(AdmissionDenied):
            self.reserve('b', fingerprint=fp)
        self.c.record_probe('p1', 'a', fp, False, {'test': 'fails'})
        with self.assertRaises(AdmissionDenied):
            self.reserve('b', fingerprint=fp)
        self.c.record_probe('p2', 'a', fp, True, {'test': 'passes'})
        self.reserve('b', fingerprint=fp)
        self.assertEqual(self.c.report()['reserved_contexts'], 2)

    def test_probe_replay_and_changed_fingerprint_do_not_waive_gate(self):
        self.failed()
        fp = self.repaired()
        self.c.record_probe('probe-two', 'a', fp, True, {'test': 'bounded reply regression'})
        with self.assertRaises(ValueError):
            self.c.record_probe('probe-two', 'a', fp, False, {'test': 'different'})
        with self.assertRaises(AdmissionDenied):
            self.reserve('b', fingerprint=dict(fp, adapter='three'))
        self.assertEqual(self.c.report()['reserved_contexts'], 1)

    def test_two_consecutive_failures_require_diagnosis(self):
        self.failed()
        fp = self.repaired()
        self.failed('b', fp)
        fp3 = self.repaired('b', 'three')
        with self.assertRaisesRegex(AdmissionDenied, 'diagnosis'):
            self.reserve('c', fingerprint=fp3)
        fp4 = self.repaired('b', 'four', diagnosis='root cause and regression identified')
        self.reserve('c', fingerprint=fp4)

    def test_exhaustion_and_final_reservation(self):
        other = Campaign.open(self.path, 'other-package', 'other', {'contexts': 3, 'final_headroom': 2})
        self.addCleanup(other.close)
        other.reserve('a', 'live_probe', self.fp, 1, 0, 0, contexts_per_execution=None)
        other.mark_launched('a')
        other.complete('a', True)
        with self.assertRaises(AdmissionDenied):
            other.reserve('b', 'complete_fixture', self.fp, 1, 0, 0, contexts_per_execution=None)
        other.reserve('b', 'complete_fixture', self.fp, 2, 0, 0, final=True, contexts_per_execution=None)
        other.mark_launched('b')
        other.complete('b', True)
        with self.assertRaises(AdmissionDenied):
            other.reserve('c', 'complete_fixture', self.fp, 1, 0, 0, final=True, contexts_per_execution=None)

    def test_missing_and_cumulative_per_thread_telemetry(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.complete('a', True, evidence={'observations': {'contexts': [{'thread_id': 'root'}, {'thread_id': 'child'}], 'uncertainties': [], 'context_inventory_complete': True}})
        self.assertIsNone(self.c.report()['uncached_tokens_per_accepted_qualification'])
        for _ in range(2):
            self.c.telemetry('a', 'root', 100, 40, 120, output_tokens=20)
            self.c.telemetry('a', 'child', 80, 20, 95, 'root', output_tokens=15)
        self.c.telemetry('a', 'root', 50, 45, 60)  # Old cache snapshot ignored.
        report = self.c.report()
        self.assertEqual(report['observed_contexts'], 2)
        self.assertEqual(report['known_uncached_tokens'], 120)
        self.assertEqual(report['uncached_tokens_per_accepted_qualification'], 120)
        self.c.telemetry('a', 'unknown-child', parent_thread_id='root')
        self.assertFalse(self.c.report()['telemetry_complete'])
        self.assertIsNone(self.c.report()['uncached_tokens_per_accepted_qualification'])
        with self.assertRaises(ValueError):
            self.c.telemetry('a', 'child', 90, 20, 100)

    def test_collector_uncertainties_keep_known_usage_but_suppress_ratios(self):
        for reason in ('torn_journal', 'journal_bound', 'event_gap', 'missing_thread', 'source_provenance_missing'):
            with self.subTest(reason=reason):
                campaign = Campaign.open(self.path, reason, reason)
                self.addCleanup(campaign.close)
                campaign.reserve('a', 'complete_fixture', self.fp, 1, 0, 0, contexts_per_execution=None)
                campaign.mark_launched('a')
                campaign.telemetry('a', 'root', 100, 40, 120, output_tokens=20)
                campaign.complete('a', True, evidence={'observations': {
                    'contexts': [{'thread_id': 'root'}],
                    'uncertainties': [{'reason': reason}]}})
                report = campaign.report()
                self.assertFalse(report['telemetry_complete'])
                self.assertFalse(report['contexts_complete'])
                self.assertEqual(report['known_input_tokens'], 100)
                self.assertEqual(report['known_cached_input_tokens'], 40)
                self.assertEqual(report['known_output_tokens'], 20)
                self.assertIsNone(report['uncached_tokens_per_accepted_qualification'])
                self.assertIsNone(report['fresh_contexts_per_accepted_qualification'])

    def test_missing_output_is_unknown_while_context_count_can_be_complete(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.telemetry('a', 'root', 100, 40, 120)
        self.c.complete('a', True, evidence={'observations': {
            'contexts': [{'thread_id': 'root'}], 'uncertainties': [], 'context_inventory_complete': True}})
        report = self.c.report()
        self.assertTrue(report['input_tokens_complete'])
        self.assertTrue(report['cached_input_tokens_complete'])
        self.assertFalse(report['output_tokens_complete'])
        self.assertFalse(report['telemetry_complete'])
        self.assertIsNone(report['uncached_tokens_per_accepted_qualification'])
        self.assertEqual(report['fresh_contexts_per_accepted_qualification'], 1)

    def test_active_or_missing_inventory_never_claims_complete_telemetry(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.telemetry('a', 'root', 100, 40, 120, output_tokens=20)
        self.assertFalse(self.c.report()['telemetry_complete'])
        self.c.complete('a', True)
        self.assertFalse(self.c.report()['telemetry_complete'])
        self.assertIsNone(self.c.report()['fresh_contexts_per_accepted_qualification'])

    def test_legacy_completeness_claim_is_not_coverage_evidence(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.telemetry('a', 'root', 100, 40, output_tokens=20)
        self.c.complete('a', True, evidence={'observations': {
            'contexts': [{'thread_id': 'root'}], 'uncertainties': [],
            'contexts_complete': True, 'telemetry_complete': True}})
        report = self.c.report()
        self.assertFalse(report['context_inventory_complete'])
        self.assertTrue(report['observed_telemetry_complete'])
        self.assertFalse(report['input_tokens_complete'])
        self.assertIsNone(report['uncached_tokens_per_accepted_qualification'])
        self.assertEqual(report['known_uncached_tokens'], 60)

    def test_pre_model_zero_usage_and_failed_checks_are_counted(self):
        self.c.record_check('bad-startup', 'preflight', self.fp, False,
                            {'error': 'configuration rejected'}, 'configuration')
        self.c.record_check('negative-regression', 'focused_probe', self.fp, True,
                            {'result': 'invalid configuration rejected as expected'})
        self.reserve()
        self.c.complete('a', False, 'configuration', pre_model_fault=True)
        report = self.c.report()
        self.assertTrue(report['telemetry_complete'])
        self.assertEqual(report['known_input_tokens'], 0)
        self.assertEqual(report['pre_model_faults'], 2)

    def test_outer_and_human_interventions_are_separate(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.complete('a', True, evidence={'human_interventions': 1,
                                            'outer_agent_interventions': 2,
                                            'planned_fixture_injections': 3})
        report = self.c.report()
        self.assertEqual(report['human_interventions'], 1)
        self.assertEqual(report['outer_agent_interventions'], 2)
        self.assertTrue(report['intervention_counts_complete'])

    def test_policy_can_change_only_before_reservation(self):
        self.c.configure({'contexts': 900}, {'decision': 'measured slack'})
        self.assertEqual(self.c.report()['limits']['contexts'], 900)
        self.reserve()
        with self.assertRaises(AdmissionDenied):
            self.c.configure({'contexts': 1000}, {'decision': 'try to reset'})

    def test_active_or_exhausted_campaign_cannot_start_successor(self):
        with self.assertRaises(AdmissionDenied):
            self.c.begin_successor('next', {'package': 'next change'})
        self.failed()
        with self.assertRaises(AdmissionDenied):
            self.c.finish({'reviewed_revision': 'abc', 'landed_reference': 'pull/1'})
        with self.assertRaises(AdmissionDenied):
            self.c.begin_successor('next', {'package': 'next change'})

    def test_terminal_successor_preserves_history_and_blocks_stale_writes(self):
        self.reserve(final=True)
        self.c.mark_launched('a')
        self.c.complete('a', True)
        with self.assertRaises(ValueError):
            self.c.finish({'acceptance': 'yes'})
        self.c.finish({'reviewed_revision': 'abc', 'landed_reference': 'pull/1'})
        stale = Campaign.open(self.path, 'work-package', 'campaign')
        self.addCleanup(stale.close)
        successor = self.c.begin_successor('next', {'package': 'next change'})
        self.addCleanup(successor.close)
        self.assertEqual(successor.report()['reserved_contexts'], 0)
        self.assertEqual(self.c.report()['reserved_contexts'], 1)
        archived = Campaign.open(self.path, 'work-package', 'campaign')
        self.addCleanup(archived.close)
        self.assertEqual(archived.report()['accepted_qualifications'], 1)
        for previous in (self.c, stale, archived):
            with self.assertRaises(AdmissionDenied):
                previous.reserve('b', 'complete_fixture', self.fp)
            with self.assertRaises(AdmissionDenied):
                previous.record_check('x', 'preflight', self.fp, True, {'probe': 'x'})
        self.assertEqual(successor.report()['checks'], [])

    def test_observed_overrun_consumes_future_allowance(self):
        small = Campaign.open(self.path, 'small', 'small', {'contexts': 3, 'final_headroom': 0})
        self.addCleanup(small.close)
        small.reserve('a', 'live_probe', self.fp, 1, 0, 0, contexts_per_execution=None)
        small.mark_launched('a')
        small.complete('a', True)
        for thread in ('a', 'b', 'c'):
            small.telemetry('a', thread)
        self.assertEqual(small.report()['charged_contexts'], 3)
        with self.assertRaises(AdmissionDenied):
            small.reserve('b', 'complete_fixture', self.fp, 1, 0, 0, contexts_per_execution=None)

    def test_stage_switch_does_not_bypass_failure_gate(self):
        self.failed()
        with self.assertRaises(AdmissionDenied):
            self.c.reserve('p', 'live_probe', self.fp, 1, 0, 0, contexts_per_execution=None)
        with self.assertRaises(AdmissionDenied):
            self.c.reserve('dog', 'application_dogfood', self.fp, 1, 0, 0, contexts_per_execution=None)

    def test_deterministic_checks_cost_nothing_and_replay_once(self):
        for _ in range(2):
            self.c.record_check('startup', 'preflight', self.fp, True, {'result': 'ready'})
        report = self.c.report()
        self.assertEqual(len(report['checks']), 1)
        self.assertEqual(report['reserved_contexts'], 0)
        with self.assertRaises(ValueError):
            self.c.record_check('startup', 'preflight', self.fp, False, {'result': 'broken'})

    def test_reconciliation_requires_cessation_evidence(self):
        self.reserve()
        self.c.mark_launched('a')
        self.c.interrupt('a', {'state': 'unknown'})
        with self.assertRaises(ValueError):
            self.c.complete('a', False)
        with self.assertRaises(ValueError):
            self.c.reconcile('a', {'state': 'unknown'})
        self.c.reconcile('a', {'cessation_verified': True})
        self.assertEqual(self.c.report()['reserved_contexts'], 1)

    def test_output_telemetry_is_cumulative(self):
        self.reserve()
        self.c.telemetry('a', 'root', 100, 40, 120, output_tokens=20, model_responses=2)
        self.c.telemetry('a', 'root', 50, 10, 60, output_tokens=10, model_responses=1)
        self.assertEqual(self.c.report()['known_output_tokens'], 20)
        self.assertEqual(self.c.report()['known_model_responses'], 2)

    def test_pre_model_fault_does_not_refund_reservation(self):
        self.reserve()
        self.c.complete('a', False, 'startup config', pre_model_fault=True)
        self.assertEqual(self.c.report()['pre_model_faults'], 1)
        self.assertEqual(self.c.report()['reserved_contexts'], 1)
        with self.assertRaises(AdmissionDenied):
            self.c.complete('a', True)




class ApplicationClosureTests(unittest.TestCase):
    def setUp(self):
        import hashlib
        import json
        import sqlite3
        import sys
        sys.path.insert(0, str(Path(__file__).parents[1]))
        self.addCleanup(lambda: sys.path.remove(str(Path(__file__).parents[1])))
        self.json = json
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.c = Campaign.open(self.root / 'registry.sqlite', 'application', 'app', purpose='application_delivery')
        self.addCleanup(self.c.close)
        self.brokers = self.root / 'brokers'
        (self.brokers / 'blobs').mkdir(parents=True)
        (self.brokers / 'reviews').mkdir()
        (self.brokers / 'controller.lock').touch()
        for identity in ('worker', 'supervisor'):
            (self.brokers / identity).mkdir()
            (self.brokers / identity / 'owner.lock').touch()
        def save(path, value):
            path.write_text(json.dumps(value))
        self.save = save
        def blob(value):
            data = json.dumps(value).encode()
            digest = hashlib.sha256(data).hexdigest()
            (self.brokers / 'blobs' / digest).write_bytes(data)
            return digest
        self.blob = blob
        def ci(head):
            run = {'id': 1, 'url': 'https://example.test/run/1', 'attempt': 1, 'head': head,
                   'status': 'completed', 'conclusion': 'success'}
            return {'state': 'success', 'head': head, 'run': run, 'runs': [run], 'jobs': [
                {'id': 2, 'url': 'https://example.test/job/2', 'run_id': 1, 'attempt': 1, 'head': head,
                 'name': 'Fresh-checkout verification', 'status': 'completed', 'conclusion': 'success'}]}
        self.delivery = {'merged': True, 'post_merge_verified': True, 'head': 'head',
                         'merge_commit': 'merge-head', 'pr': 1, 'repo': 'test/repository',
                         'base': 'main', 'branch': 'codex/task', 'head_tree': 'tree', 'merge_tree': 'tree',
                         'tree_equal': True, 'pre_merge_ci': ci('head'), 'post_merge_ci': ci('merge-head')}
        artefact = {'kind': 'git', 'repository': '/synthetic', 'commit': 'head', 'tree': 'tree'}
        self.review = {'reviewer_identity': 'codex-thread:child:turn:review', 'artefacts': [artefact],
                       'evidence_digest': blob({'verdict': 'pass', 'artefacts': [artefact], 'findings': []})}
        save(self.brokers / 'reviews' / (self.review['evidence_digest'] + '.json'), self.review)
        acceptance = {'contract_revision': 2, 'assessor_execution_id': 'supervisor',
                      'assessment_ids': ['assessment'], 'review': self.review}
        self.state = {'id': 'outcome', 'state_revision': 8, 'contract_revision': 2,
            'contracts': [{'revision': 2, 'contract': {'criteria': [{'id': 'works'}]}}],
            'executions': [{'id': i, 'role': i, 'cessation_verified': True} for i in ('worker', 'supervisor')],
            'acceptance': acceptance,
            'submissions': [{'id': 'submission', 'execution_id': 'worker', 'contract_revision': 2,
                'digest': 'result-digest', 'input': {'artefacts': [artefact], 'evidence': [
                    {'criterion_id': 'works', 'artefact': artefact, 'exit_code': 0,
                     'command_digest': blob('command'), 'output_digest': blob('output')} ]}}],
            'assessments': [{'id': 'assessment', 'contract_revision': 2, 'input': {
                'submission_id': 'submission', 'submission_digest': 'result-digest',
                'verdict': 'accept', 'unmet_criteria': [], 'review': self.review}}],
            'delivery_operations': [{'id': 'merge', 'operation': 'merge', 'contract_revision': 2,
                'post_merge_verified': True, 'arguments_json': json.dumps({'arguments': {'head': 'head', 'pr': 1}, 'review': self.review}),
                'evidence_digest': blob(self.delivery)}]}
        self.db = sqlite3.connect(self.root / 'engineering.sqlite')
        self.addCleanup(self.db.close)
        self.db.executescript('''CREATE TABLE engineering_writers(workspace TEXT);
            CREATE TABLE obligations(id TEXT, state TEXT, lease_token TEXT);
            CREATE TABLE engineering_outcomes(id TEXT, state_revision INT, root_obligation_id TEXT);
            CREATE TABLE engineering_versions(outcome_id TEXT, revision INT, snapshot_json TEXT);
            INSERT INTO obligations VALUES('root', 'completed', NULL);
            INSERT INTO engineering_outcomes VALUES('outcome', 8, 'root');''')
        self.persist()
        save(self.root / 'profile.json', {'broker_root': str(self.brokers), 'workspace': '/synthetic',
            'github_delivery': {'repo': 'test/repository', 'base': 'main', 'branch': 'codex/task', 'authority': 'synthetic authority'}})
        save(self.root / 'campaign-binding.json', {'campaign': 'app', 'registry': str(self.c.path),
                                                   'root': str(self.root), 'attempt_id': 'a'})
        self.c.reserve('a', 'application_dogfood', {'source': 'one'}, root_turns=1, authority_turns=0)
        self.c.mark_launched('a')
        self.c.complete('a', True, evidence={'root': str(self.root), 'outcome_id': 'outcome', 'acceptance': acceptance})
        self.selectors = {'attempts': [{'attempt_id': 'a', 'outcome_id': 'outcome',
                                       'contract_revision': 2, 'state_revision': 8}]}

    def persist(self):
        self.db.execute('DELETE FROM engineering_versions')
        self.db.execute('INSERT INTO engineering_versions VALUES(?,?,?)', ('outcome', 8, self.json.dumps(self.state)))
        self.db.commit()

    def test_supported_cli_verifies_and_closes_application(self):
        import contextlib
        import io
        from unittest.mock import patch
        import sys
        spec = importlib.util.spec_from_file_location('application_cli', Path(__file__).parents[1] / 'qualify-engineering.py')
        cli = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cli)
        selectors = self.root / 'closure.json'
        self.save(selectors, self.selectors)
        argv = ['qualify-engineering.py', 'finish-application', '--campaign', 'app', '--evidence', str(selectors)]
        with patch('qualification_campaign.Campaign.open', return_value=self.c), contextlib.redirect_stdout(io.StringIO()):
            with patch.object(sys, 'argv', argv + ['--dry-run']):
                cli.main()
            self.assertIsNone(self.c.report()['terminal'])
            with patch.object(sys, 'argv', argv):
                cli.main()
        self.assertEqual(self.c.report()['terminal']['policy'], 'store_application_acceptance_v1')

    def test_application_closure_preserves_allowance_and_successor_history(self):
        before = self.c.report()
        receipt = self.c.finish_application(self.selectors, dry_run=True)
        self.assertEqual(receipt['qualification_claim'], 'application_delivery_only')
        self.assertIsNone(self.c.report()['terminal'])
        self.c.finish_application(self.selectors)
        after = self.c.report()
        for key in ('attempts', 'limits', 'reserved_contexts', 'charged_contexts', 'stages', 'suppressions'):
            self.assertEqual(before[key], after[key])
        successor = self.c.begin_successor('next', {'package': 'new runtime change'})
        self.addCleanup(successor.close)
        self.assertEqual(successor.report()['purpose'], 'runtime_qualification')
        self.assertEqual(self.c.report()['attempts'][0]['scope'], 'application::app')
        with self.assertRaises(AdmissionDenied):
            self.c.finish_application(self.selectors)

    def test_purpose_cannot_change_and_fixture_finish_cannot_close_application(self):
        with self.assertRaises(AdmissionDenied):
            Campaign.open(self.c.path, 'application', 'app', purpose='runtime_qualification')
        with self.assertRaises(AdmissionDenied):
            self.c.finish({'reviewed_revision': 'claimed', 'landed_reference': 'claimed'})
        with self.assertRaises(AdmissionDenied):
            self.c.reserve('fixture', 'complete_fixture', {'source': 'two'}, final=True)
        runtime = Campaign.open(self.root / 'runtime.sqlite', 'runtime', 'runtime')
        self.addCleanup(runtime.close)
        with self.assertRaises(AdmissionDenied):
            runtime.finish_application(self.selectors, classify_legacy=True)

    def test_legacy_classification_is_atomic_and_cannot_follow_failure(self):
        self.c.db.execute('UPDATE campaigns SET purpose=NULL,terminal_policy=NULL')
        with self.assertRaises(AdmissionDenied):
            self.c.finish_application(self.selectors)
        self.selectors['attempts'][0]['contract_revision'] = 1
        with self.assertRaises(ValueError):
            self.c.finish_application(self.selectors, classify_legacy=True)
        self.assertEqual(self.c.report()['purpose'], 'legacy_unclassified')
        self.selectors['attempts'][0]['contract_revision'] = 2
        self.c.db.execute('UPDATE attempts SET passed=0')
        with self.assertRaises(AdmissionDenied):
            self.c.finish_application(self.selectors, classify_legacy=True)
        self.c.db.execute('UPDATE attempts SET passed=1')
        self.c.finish_application(self.selectors, classify_legacy=True)
        self.assertEqual(self.c.report()['purpose'], 'application_delivery')
        self.assertEqual(self.c.report()['policy_events'][-1]['kind'], 'legacy_application_classification')

    def test_forged_selectors_cannot_replace_store_or_registered_evidence(self):
        for mutate in (
            lambda: self.state.update(acceptance=None),
            lambda: self.state['acceptance'].update(contract_revision=1),
            lambda: self.state['acceptance']['review'].update(reviewer_identity='invented'),
            lambda: self.state['contracts'][0]['contract']['criteria'].append({'id': 'unproved'}),
            lambda: self.state['submissions'][0].update(digest='changed'),
            lambda: self.state['delivery_operations'][0].update(evidence_digest=None),
            lambda: self.state['delivery_operations'][0].update(post_merge_verified=False),
            lambda: self.state['executions'][0].update(cessation_verified=False),
        ):
            original = self.json.loads(self.json.dumps(self.state))
            with self.subTest(mutation=mutate):
                mutate()
                self.persist()
                with self.assertRaises((ValueError, TypeError)):
                    self.c.finish_application(self.selectors)
                self.assertIsNone(self.c.report()['terminal'])
            self.state = original
        self.persist()
        (self.brokers / 'reviews' / (self.review['evidence_digest'] + '.json')).unlink()
        with self.assertRaises(FileNotFoundError):
            self.c.finish_application(self.selectors)

    def test_settled_failed_merge_then_successful_merge_closes_with_full_history(self):
        failed_result = {'error': 'exact-head CI was not ready', 'uncertain': False}
        failed = dict(self.state['delivery_operations'][0], id='failed-merge',
                      post_merge_verified=False, evidence_digest=self.blob(failed_result))
        self.state['delivery_operations'].insert(0, failed)
        self.persist()
        attempts = self.c.report()['attempts']
        receipt = self.c.finish_application(self.selectors)
        history = receipt['attempts'][0]['delivery_history']
        self.assertEqual([item['operation_id'] for item in history], ['failed-merge', 'merge'])
        self.assertEqual(history[0]['disposition'], 'settled_failure')
        self.assertEqual(history[0]['result'], failed_result)
        self.assertEqual(history[0]['evidence_digest'], failed['evidence_digest'])
        self.assertEqual([item['operation_id'] for item in receipt['attempts'][0]['deliveries']], ['merge'])
        self.assertEqual(self.c.report()['attempts'], attempts)

    def test_successful_merge_cannot_hide_unresolved_or_uncertain_delivery(self):
        for result in (None, {'error': 'uncertain launch', 'uncertain': True},
                       {'error': 'missing cessation evidence'},
                       {'uncertain': True, 'post_merge_verified': True},
                       {'merged': True, 'post_merge_verified': False}):
            with self.subTest(result=result):
                earlier = dict(self.state['delivery_operations'][-1], id='earlier-merge',
                    post_merge_verified=False, evidence_digest=self.blob(result) if result is not None else None)
                self.state['delivery_operations'] = [earlier, self.state['delivery_operations'][-1]]
                self.persist()
                with self.assertRaises(ValueError):
                    self.c.finish_application(self.selectors)
                self.assertIsNone(self.c.report()['terminal'])
        # A settled failure alone cannot supply final delivery acceptance.
        self.state['delivery_operations'] = [dict(earlier, evidence_digest=self.blob(
            {'error': 'merge refused', 'uncertain': False}))]
        self.persist()
        with self.assertRaisesRegex(ValueError, 'no verified merge'):
            self.c.finish_application(self.selectors)

    def test_delivery_requires_exact_trees_and_ci_not_boolean_claims(self):
        import copy
        from unittest.mock import patch
        for mutate in (
            lambda d: d.update(merge_tree='other-tree'),
            lambda d: d.update(head_tree='other-tree', merge_tree='other-tree'),
            lambda d: d.update(base='other-base'),
            lambda d: d['post_merge_ci']['run'].update(attempt=2),
            lambda d: d['post_merge_ci'].update(head='other-head'),
            lambda d: d['pre_merge_ci'].update(jobs=[]),
        ):
            altered = copy.deepcopy(self.delivery)
            mutate(altered)
            self.state['delivery_operations'][0]['evidence_digest'] = self.blob(altered)
            self.persist()
            with self.assertRaises(ValueError):
                self.c.finish_application(self.selectors)
        legacy = {k: v for k, v in self.delivery.items() if k not in
                  ('head_tree', 'merge_tree', 'tree_equal', 'pre_merge_ci', 'post_merge_ci')}
        self.state['delivery_operations'][0]['evidence_digest'] = self.blob(legacy)
        self.persist()
        with patch('qualification_application.observe_delivery', return_value={
                'adapter_sha256': 'synthetic', 'result': self.delivery}) as observe:
            receipt = self.c.finish_application(self.selectors, dry_run=True)
        observe.assert_called_once()
        self.assertEqual(receipt['attempts'][0]['deliveries'][0]['supplemental_observation']['result'], self.delivery)
        mismatched = dict(self.delivery, head='unrelated')
        with patch('qualification_application.observe_delivery', return_value={'result': mismatched}):
            with self.assertRaises(ValueError):
                self.c.finish_application(self.selectors)
        self.assertIsNone(self.c.report()['terminal'])

    def test_controller_broker_writer_and_unaccounted_attempt_block_closure(self):
        import fcntl
        for path in (self.brokers / 'controller.lock', self.brokers / 'worker/owner.lock'):
            with path.open('rb') as held:
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                with self.assertRaises(ValueError):
                    self.c.finish_application(self.selectors)
        self.db.execute("INSERT INTO engineering_writers VALUES('workspace')")
        self.db.commit()
        with self.assertRaises(ValueError):
            self.c.finish_application(self.selectors)
        self.db.execute('DELETE FROM engineering_writers')
        self.db.execute("UPDATE obligations SET state='running'")
        self.db.commit()
        with self.assertRaises(ValueError):
            self.c.finish_application(self.selectors)
        self.db.execute("UPDATE obligations SET state='completed'")
        self.db.commit()
        with self.assertRaises(ValueError):
            self.c.finish_application({'attempts': self.selectors['attempts'] * 2})


if __name__ == '__main__':
    unittest.main()
