import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualification_observations import collect, failure_category
from qualification_campaign import Campaign


class ObservationTests(unittest.TestCase):
    def test_replay_cumulative_children_and_missing_usage(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            spool = root / 'brokers/execution'
            spool.mkdir(parents=True)
            (spool / 'dispatch.json').write_text('{"role":"supervisor"}')
            events = [{'kind': 'thread_identity', 'value': {'thread_id': 'root'}},
                      {'kind': 'turn/started', 'value': {'threadId': 'child', 'turn': {'id': 'c'}}}]
            for count in [100, 200, 100, 200]:
                events.append({'kind': 'token_usage', 'value': {'threadId': 'root',
                    'tokenUsage': {'total': {'inputTokens': count, 'cachedInputTokens': 50,
                                            'outputTokens': 10}}}})
            response = {'kind': 'item/completed', 'value': {'threadId': 'child',
                'turnId': 'c', 'item': {'id': 'answer', 'type': 'agentMessage'}}}
            events += [response, response]
            (spool / 'events.jsonl').write_text('\n'.join(map(json.dumps, events)))
            result = collect(root)
            self.assertEqual(result, collect(root))
            self.assertEqual(len(result['contexts']), 2)
            self.assertEqual(result['contexts'][0]['uncached_input_tokens'], 150)
            self.assertIsNone(result['contexts'][1]['input_tokens'])
            self.assertEqual(result['contexts'][1]['model_responses'], 1)

    def test_collector_to_durable_report_coverage_metrics_and_replay(self):
        for coverage in ('unknown', 'missing'):
            for missing_metric in (None, 'inputTokens', 'cachedInputTokens', 'outputTokens'):
                with self.subTest(coverage=coverage, missing_metric=missing_metric), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    spool = root / 'brokers/execution'
                    spool.mkdir(parents=True)
                    usage = {'inputTokens': 100, 'cachedInputTokens': 40, 'outputTokens': 20}
                    if missing_metric:
                        del usage[missing_metric]
                    events = [{'kind': 'thread_identity', 'value': {'thread_id': 'root'}},
                              {'kind': 'token_usage', 'value': {'threadId': 'root',
                               'tokenUsage': {'total': usage}}}]
                    if coverage == 'unknown':
                        events.append({'kind': 'context_limit', 'value': {
                            'enforcement': 'observed_events', 'unreported_children': 'unknown',
                            'limit': 5, 'observed_count': 1, 'exceeded': False}})
                    # Replay with fresh sequence numbers mirrors retained event replay.
                    events *= 2
                    for sequence, event in enumerate(events, 1):
                        events[sequence - 1] = dict(event, sequence=sequence)
                    (spool / 'events.jsonl').write_text('\n'.join(map(json.dumps, events)))
                    observation = collect(root)
                    self.assertEqual(observation, collect(root))
                    self.assertFalse(observation['context_inventory_complete'])
                    self.assertEqual(len(observation['uncertainties']), 1)
                    if coverage == 'unknown':
                        self.assertEqual(observation['uncertainties'][0]['unreported_children'], 'unknown')
                    registry = root / 'registry.sqlite'
                    campaign = Campaign.open(registry, 'scope', 'campaign')
                    campaign.reserve('a', 'complete_fixture', {'source': 'synthetic'})
                    campaign.mark_launched('a')
                    for _ in range(2):
                        for item in collect(root)['contexts']:
                            campaign.telemetry('a', item['thread_id'],
                                input_tokens=item['input_tokens'], cached_input_tokens=item['cached_input_tokens'],
                                output_tokens=item['output_tokens'], model_responses=item['model_responses'])
                    campaign.complete('a', True, evidence={'observations': observation})
                    campaign.close()
                    campaign = Campaign.open(registry, 'scope', 'campaign')
                    self.addCleanup(campaign.close)
                    report = campaign.report()
                    self.assertEqual(report, campaign.report())
                    self.assertEqual(report['observed_contexts'], 1)
                    self.assertEqual(report['reserved_contexts'], 240)
                    self.assertEqual(report['charged_contexts'], 240)
                    self.assertFalse(report['contexts_complete'])
                    self.assertFalse(report['context_inventory_complete'])
                    self.assertFalse(report['telemetry_complete'])
                    self.assertIsNone(report['fresh_contexts_per_accepted_qualification'])
                    self.assertIsNone(report['uncached_tokens_per_accepted_qualification'])
                    self.assertEqual(report['observed_telemetry_complete'], missing_metric is None)
                    self.assertEqual(report['observed_token_metrics_complete']['input'], missing_metric != 'inputTokens')
                    self.assertEqual(report['observed_token_metrics_complete']['output'], missing_metric != 'outputTokens')
                    self.assertEqual(report['known_input_tokens'], 0 if missing_metric == 'inputTokens' else 100)
                    self.assertEqual(report['known_cached_input_tokens'], 0 if missing_metric in ('inputTokens', 'cachedInputTokens') else 40)
                    self.assertEqual(report['known_uncached_tokens'], 0 if missing_metric in ('inputTokens', 'cachedInputTokens') else 60)
                    self.assertEqual(report['known_output_tokens'], 0 if missing_metric == 'outputTokens' else 20)
                    self.assertEqual(report['known_model_responses'], 0 if missing_metric == 'inputTokens' else 1)

    def test_collected_empty_journals_preserve_verified_pre_model_zero(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            observation = collect(root)
            self.assertFalse(observation['context_inventory_complete'])
            campaign = Campaign.open(root / 'registry.sqlite', 'scope', 'campaign')
            self.addCleanup(campaign.close)
            campaign.reserve('a', 'complete_fixture', {'source': 'synthetic'})
            campaign.complete('a', False, 'configuration', pre_model_fault=True,
                evidence={'not_started': True, 'observations': observation})
            report = campaign.report()
            self.assertTrue(report['context_inventory_complete'])
            self.assertTrue(report['telemetry_complete'])
            self.assertEqual(report['observed_contexts'], 0)
            for metric in ('input_tokens', 'cached_input_tokens', 'uncached_tokens', 'output_tokens'):
                self.assertEqual(report['known_' + metric], 0)
            self.assertEqual(report['reserved_contexts'], 240)

    def test_torn_tail_is_unknown_and_dynamic_error_ids_not_fingerprint(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            spool = root / 'brokers/execution'
            spool.mkdir(parents=True)
            (spool / 'events.jsonl').write_text('{')
            self.assertIn('torn_journal', {item['reason'] for item in collect(root)['uncertainties']})
            self.assertEqual(failure_category(root, RuntimeError('id-123')), 'journal_decoding')
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(failure_category(temporary, RuntimeError('MCP id123 failed at 1')), 'configuration')
            self.assertEqual(failure_category(temporary, RuntimeError('MCP id999 failed at 9')), 'configuration')


if __name__ == '__main__':
    unittest.main()
