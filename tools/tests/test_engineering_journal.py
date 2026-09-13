"""No-model storage qualification for segmented broker journals."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('journal_broker', Path(__file__).resolve().parents[1] / 'engineering-runtime' / 'broker.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


class JournalTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)

    def test_poll_during_initial_publication_remains_uncertain_until_manifest(self):
        publish = b.atomic
        def paused_publish(path, value):
            if path.name == 'journal.json':
                self.assertEqual((self.root / 'events-000000.jsonl').read_bytes(), b'')
                self.assertEqual(list(b.iter_events(self.root, tolerate_partial=True)), [])
                with self.assertRaises(ValueError):
                    list(b.iter_events(self.root))
                with self.assertRaises(FileExistsError):
                    b.Spool(self.root)
            publish(path, value)
        with patch.object(b, 'atomic', paused_publish):
            spool = b.Spool(self.root)
        spool.append('progress', {'published': True})
        self.assertEqual(list(b.iter_events(self.root)), spool.events)
        # A manifest lookup can precede publication while the segment read follows
        # the first append. The stale lookup must still yield an uncertain poll.
        actual = b.os.path.lexists
        with patch.object(b.os.path, 'lexists', lambda path: False if Path(path).name == 'journal.json' else actual(path)):
            self.assertEqual(list(b.iter_events(self.root, tolerate_partial=True)), [])
        self.assertEqual(list(b.iter_events(self.root)), spool.events)

    def test_active_read_uses_snapshot_length_during_append(self):
        path = self.root / 'active'
        path.write_bytes(b'first')
        actual = os.fstat
        def append_after_stat(fd):
            metadata = actual(fd)
            with path.open('ab') as stream:
                stream.write(b'next')
            return metadata
        with patch.object(b.os, 'fstat', append_after_stat):
            self.assertEqual(b.regular_bytes(path, 100), b'first')
        self.assertEqual(path.read_bytes(), b'firstnext')

    def test_read_only_ignores_unreferenced_blob_write_but_counts_its_bytes(self):
        spool = b.Spool(self.root)
        spool.append('progress', {})
        directory = self.root / 'journal-blobs'
        directory.mkdir()
        (directory / ('a' * 64)).write_bytes(b'partial write')
        self.assertEqual(len(list(b.iter_events(self.root))), 1)
        with self.assertRaises(ValueError):
            b.Spool(self.root)
        with self.assertRaises(ValueError):
            b.Spool(self.root, create=False, blob_limit=1)

    def test_rollover_exact_replay_and_sealed_integrity(self):
        spool = b.Spool(self.root, segment_limit=1024)
        for index in range(20):
            spool.append('progress', {'index': index, 'text': 'x' * 200})
        manifest = b.read(self.root / 'journal.json')
        self.assertGreater(len(manifest['segments']), 1)
        for segment in manifest['segments'][:-1]:
            raw = (self.root / segment['path']).read_bytes()
            self.assertEqual(segment['sha256'], hashlib.sha256(raw).hexdigest())
            self.assertEqual(segment['bytes'], len(raw))
        self.assertEqual(list(b.iter_events(self.root)), spool.events)
        self.assertEqual(b.Spool(self.root).events, spool.events)

    def test_source_and_output_dedup_preserves_exact_unicode_bytes(self):
        spool = b.Spool(self.root)
        source = {'files': {'é.txt': {'sha256': 'a' * 64, 'byte_length': 2}}, 'clean': True}
        output = 'é\r\n' * 12000
        original = {'item': {'type': 'commandExecution', 'id': 'cmd', 'aggregatedOutput': output}}
        for _ in range(5):
            spool.append('command_source', {'source': source})
            spool.append('item/completed', original)
        self.assertEqual(len(list((self.root / 'journal-blobs').iterdir())), 2)
        self.assertNotIn('source', spool.events[0]['value'])
        self.assertNotIn('aggregatedOutput', spool.events[1]['value']['item'])
        self.assertEqual(original['item']['aggregatedOutput'], output)
        resolved = list(b.iter_events(self.root))
        self.assertEqual(resolved[0]['value']['source'], source)
        self.assertEqual(resolved[1]['value']['item']['aggregatedOutput'].encode(), output.encode())
        self.assertLess(spool.size, len(output.encode()))

    def test_legacy_remains_inline_and_history_is_unchanged(self):
        path = self.root / 'events.jsonl'
        first = b.encoded({'sequence': 1, 'kind': 'launch_committed', 'value': {}}) + b'\n'
        path.write_bytes(first)
        spool = b.Spool(self.root)
        spool.append('command_source', {'source': {'files': {}}})
        self.assertTrue(path.read_bytes().startswith(first))
        self.assertIn('source', spool.events[-1]['value'])
        self.assertFalse((self.root / 'journal.json').exists())
        self.assertFalse((self.root / 'journal-blobs').exists())

    def test_read_only_empty_inspection(self):
        self.assertEqual(list(b.iter_events(self.root)), [])
        self.assertEqual(list(self.root.iterdir()), [])

    def test_total_and_count_budgets_preserve_terminal_receipts(self):
        spool = b.Spool(self.root, journal_limit=b.RESERVE + 200, event_limit=6)
        spool.append('progress', {})
        spool.append('progress', {})
        with self.assertRaises(ValueError):
            spool.append('progress', {})
        for kind in ('failure', 'stderr_diagnostic', 'boundary_reaped', 'final'):
            spool.append(kind, {}, terminal=True)
        with self.assertRaises(ValueError):
            spool.append('overflow', {}, terminal=True)
        self.assertTrue(b.Spool(self.root).has('boundary_reaped'))

    def test_byte_budget_preserves_terminal_receipt(self):
        spool = b.Spool(self.root, journal_limit=b.RESERVE + 200)
        with self.assertRaises(ValueError):
            spool.append('progress', {'text': 'x' * 200})
        spool.append('boundary_reaped', {}, terminal=True)

    def test_blob_budget_includes_orphans_and_failure_can_be_recorded(self):
        spool = b.Spool(self.root, blob_limit=10)
        spool._blob(b'1234567890', 'utf8')
        with self.assertRaises(ValueError):
            spool.append('command_source', {'source': {'files': {}}})
        spool.append('failure', {'message': 'blob exhausted'}, terminal=True)
        self.assertEqual(sum(b.Spool(self.root, blob_limit=10).blobs.values()), 10)
        with self.assertRaises(ValueError):
            b.Spool(self.root, blob_limit=9)

    def test_blob_count_bound(self):
        spool = b.Spool(self.root, blob_count_limit=1)
        spool._blob(b'a', 'utf8')
        spool._blob(b'a', 'utf8')
        with self.assertRaises(ValueError):
            spool._blob(b'b', 'utf8')

    def test_missing_corrupt_and_symlink_blobs_fail_closed(self):
        spool = b.Spool(self.root)
        event = spool.append('command_source', {'source': {'files': {}}})
        path = self.root / 'journal-blobs' / event['value']['source_ref']['sha256']
        original = path.read_bytes()
        path.unlink()
        with self.assertRaises(ValueError):
            b.Spool(self.root)
        path.write_bytes(b'corrupt')
        with self.assertRaises(ValueError):
            b.Spool(self.root)
        path.unlink()
        target = self.root / 'target'
        target.write_bytes(original)
        path.symlink_to(target)
        with self.assertRaises((OSError, ValueError)):
            b.Spool(self.root)

    def test_sealed_truncation_missing_file_and_hash_mismatch(self):
        spool = b.Spool(self.root, segment_limit=256)
        for _ in range(4):
            spool.append('progress', {'text': 'x' * 100})
        path = self.root / spool.manifest['segments'][0]['path']
        original = path.read_bytes()
        for replacement in (original[:-1], original.replace(b'progress', b'progresS')):
            path.write_bytes(replacement)
            with self.assertRaises(ValueError):
                b.Spool(self.root)
        path.unlink()
        with self.assertRaises(OSError):
            b.Spool(self.root)

    def test_active_partial_is_uncertain_and_never_resurrected(self):
        spool = b.Spool(self.root)
        spool.append('launch_committed', {})
        with spool.path.open('ab') as stream:
            stream.write(b'{"sequence":2')
        with self.assertRaises(ValueError):
            b.Spool(self.root)
        with self.assertRaises(ValueError):
            list(b.iter_events(self.root))
        self.assertEqual(list(b.iter_events(self.root, tolerate_partial=True)), spool.events)

    def test_rollover_crash_before_manifest_does_not_overwrite_orphan(self):
        spool = b.Spool(self.root, segment_limit=256)
        spool.append('launch_committed', {'text': 'x' * 100})
        with patch.object(b, 'atomic', side_effect=OSError('injected before manifest publication')):
            with self.assertRaises(OSError):
                spool.append('progress', {'text': 'x' * 100})
        orphan = self.root / 'events-000001.jsonl'
        self.assertEqual(orphan.read_bytes(), b'')
        replay = b.Spool(self.root, segment_limit=256)
        self.assertEqual(replay.events, spool.events)
        with self.assertRaises(FileExistsError):
            replay.append('progress', {'text': 'x' * 100})
        self.assertEqual(orphan.read_bytes(), b'')

    def test_rollover_crash_after_manifest_keeps_empty_active_segment(self):
        spool = b.Spool(self.root, segment_limit=256)
        spool.append('launch_committed', {'text': 'x' * 100})
        spool._rollover()  # Crash before the triggering event is appended.
        replay = b.Spool(self.root, segment_limit=256)
        self.assertTrue(replay.has('launch_committed'))
        replay.append('boundary_reaped', {}, terminal=True)
        self.assertEqual(len(list(b.iter_events(self.root))), 2)

    def test_ambiguous_manifest_and_mixed_generation_rejected(self):
        spool = b.Spool(self.root)
        good = json.loads(b.encoded(spool.manifest))
        for mutate in (
                lambda value: value.update(version=3),
                lambda value: value['segments'][0].update(bytes=0),
                lambda value: value['segments'][0].update(path='../escape'),
                lambda value: value['segments'][0].update(first_sequence=True)):
            bad = json.loads(b.encoded(good))
            mutate(bad)
            b.atomic(self.root / 'journal.json', bad)
            with self.assertRaises(ValueError):
                b.Spool(self.root)
        b.atomic(self.root / 'journal.json', good)
        (self.root / 'events.jsonl').touch()
        with self.assertRaises(ValueError):
            b.Spool(self.root)


if __name__ == '__main__':
    unittest.main()
