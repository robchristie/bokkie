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
        self.c.complete('a', True, evidence={'observations': {'contexts': [{'thread_id': 'root'}, {'thread_id': 'child'}], 'uncertainties': []}})
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
            'contexts': [{'thread_id': 'root'}], 'uncertainties': []}})
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


if __name__ == '__main__':
    unittest.main()
