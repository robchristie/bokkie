"""Compact observations from retained qualification journals; never prompt copies."""
import hashlib
import importlib.util
import json
import sqlite3
from pathlib import Path


_spec = importlib.util.spec_from_file_location(
    'qualification_journal', Path(__file__).parent / 'engineering-runtime/broker.py')
_journal = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_journal)


def journal_roots(root):
    """Discover both formats once, without opening a mutable broker spool."""
    return sorted({path.parent for name in ('events.jsonl', 'journal.json', 'events-*.jsonl')
                   for path in (Path(root) / 'brokers').glob('*/' + name)})


def file_identity(path):
    path = Path(path)
    return hashlib.sha256(path.read_bytes()).hexdigest()


def collect(root):
    """Re-reading is deliberate: stable thread/item keys and maxima make it idempotent.

    Token totals are per-thread cumulative observations, not sums of events.
    Missing telemetry remains None. Parent token totals are not augmented with
    the same child's tokens: each actual thread contributes only its own total.
    """
    contexts = {}
    inputs = {}
    uncertain = []
    def context(identity, role=None):
        value = contexts.setdefault(identity, {'thread_id': identity, 'role': role,
            'input_tokens': None, 'cached_input_tokens': None, 'output_tokens': None,
            'responses': set(), 'usage_updates': set()})
        if role is not None and (value['role'] is None or role != 'child'):
            value['role'] = role
        return value
    paths = journal_roots(root)
    database = Path(root) / 'fixture.sqlite'
    if database.exists():
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
            snapshots = connection.execute('SELECT v.snapshot_json FROM engineering_outcomes o JOIN engineering_versions v ON v.outcome_id=o.id AND v.revision=o.state_revision').fetchall()
        expected = {e['id'] for (raw,) in snapshots for e in json.loads(raw)['executions']}
        for missing in sorted(expected - {path.name for path in paths}):
            uncertain.append({'execution': missing, 'reason': 'missing_execution_journal'})
    for path in paths:
        execution = path.name
        root_thread = None
        role = None
        manifest = path / 'dispatch.json'
        if manifest.exists():
            role = json.loads(manifest.read_text()).get('role')
        coverage_reported = False
        def observed_events():
            try:
                yield from _journal.iter_events(path)
            except (OSError, ValueError) as error:
                diagnostic = str(error).lower()
                reason = ('journal_sequence_gap' if 'sequence' in diagnostic else
                          'journal_bound' if 'bound' in diagnostic or 'oversized' in diagnostic else
                          'torn_journal')
                uncertain.append({'execution': execution, 'reason': reason})
        for event in observed_events():
            kind, value = event.get('kind'), event.get('value', {})
            if kind == 'context_limit':
                coverage_reported = True
                uncertain.append({'execution': execution,
                    'reason': 'context_coverage_unknown',
                    'enforcement': value.get('enforcement', 'unknown'),
                    'unreported_children': value.get('unreported_children', 'unknown')})
            if kind == 'context_observed':
                context(value['thread_id'], role if value.get('source') == 'root' else 'child')
            if kind == 'thread_identity':
                root_thread = value['thread_id']
                context(root_thread, role)
            if kind == 'guidance_identities':
                inputs.setdefault(execution, {})['guidance_files_bytes'] = sum(
                    item.get('byte_length', 0) for item in value)
                continue
            if kind == 'context_input_bytes':
                inputs.setdefault(execution, {}).update(value)
            thread = value.get('threadId') or value.get('thread_id')
            if kind in ('turn/started', 'turn/completed') and thread:
                context(thread, role if thread == root_thread else 'child')
            if kind == 'item/completed' and thread:
                item = value.get('item', {})
                if item.get('type') == 'agentMessage':
                    context(thread, role if thread == root_thread else 'child')['responses'].add(
                        (value.get('turnId'), item.get('id')))
            if kind in ('thread/tokenUsage/updated', 'token_usage') and thread:
                item = context(thread, role if thread == root_thread else 'child')
                usage = value.get('tokenUsage', {}).get('total', {})
                if isinstance(usage.get('inputTokens'), int):
                    item['usage_updates'].add(tuple(usage.get(k) for k in
                        ('inputTokens', 'cachedInputTokens', 'outputTokens')))
                count = usage.get('inputTokens')
                if type(count) is int and count >= 0:
                    cached = usage.get('cachedInputTokens')
                    valid_cache = type(cached) is int and 0 <= cached <= count
                    if item['input_tokens'] is None or count > item['input_tokens']:
                        item['input_tokens'] = count
                        item['cached_input_tokens'] = cached if valid_cache else None
                    elif count == item['input_tokens'] and valid_cache:
                        item['cached_input_tokens'] = cached
                output = usage.get('outputTokens')
                if type(output) is int and output >= 0:
                    item['output_tokens'] = max(item['output_tokens'] or 0, output)
        if not coverage_reported:
            uncertain.append({'execution': execution, 'reason': 'context_coverage_evidence_missing'})
    if not paths:
        uncertain.append({'reason': 'context_coverage_evidence_missing'})
    uncertain = [json.loads(value) for value in sorted({json.dumps(item, sort_keys=True)
                                                      for item in uncertain})]
    for value in contexts.values():
        value['model_responses'] = max(len(value.pop('responses')), len(value.pop('usage_updates')))
        value['response_measure'] = 'distinct cumulative usage updates or observed agent messages; lower bound'
        value['uncached_input_tokens'] = (None if value['input_tokens'] is None or
            value['cached_input_tokens'] is None else
            max(0, value['input_tokens'] - value['cached_input_tokens']))
    return {'contexts': list(contexts.values()), 'context_input_bytes': inputs,
            # The current broker observes events; it cannot certify all children.
            'context_inventory_complete': False,
            'uncertainties': uncertain, 'measure': 'observed per-thread counters; absent is unknown'}


def failure_category(root, error):
    """Stable categories; changing IDs and raw diagnostic strings are not keys."""
    text = type(error).__name__.lower() + ' ' + str(error).lower()
    for path in journal_roots(root):
        try:
            for event in _journal.iter_events(path):
                if event.get('kind') == 'failure':
                    text += ' ' + json.dumps(event.get('value', {})).lower()
        except (OSError, ValueError):
            return 'journal_decoding'
    for category, words in [
        ('configuration', ('config', 'capabilit', 'mcp', 'sandbox')),
        ('payload_size', ('exceeds bound', 'spool exhausted', 'reply limit', 'journal exhaust')),
        ('source_binding', ('source capture', 'source file', 'validation source')),
        ('submission', ('submission', 'criterion')),
        ('child_review', ('reviewer', 'child review')),
        ('deadline', ('timeout', 'deadline')),
        ('admission', ('allowance', 'campaign', 'context limit')),
    ]:
        if any(word in text for word in words):
            return category
    return 'qualification_acceptance'
