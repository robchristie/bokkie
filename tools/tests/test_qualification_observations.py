import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qualification_observations import collect, failure_category


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

    def test_torn_tail_is_unknown_and_dynamic_error_ids_not_fingerprint(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            spool = root / 'brokers/execution'
            spool.mkdir(parents=True)
            (spool / 'events.jsonl').write_text('{')
            self.assertEqual(collect(root)['uncertainties'][0]['reason'], 'torn_journal')
            self.assertEqual(failure_category(root, RuntimeError('id-123')), 'journal_decoding')
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(failure_category(temporary, RuntimeError('MCP id123 failed at 1')), 'configuration')
            self.assertEqual(failure_category(temporary, RuntimeError('MCP id999 failed at 9')), 'configuration')


if __name__ == '__main__':
    unittest.main()
