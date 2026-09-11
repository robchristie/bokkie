"""Durable qualification admission and accounting; obligation lifecycle stays in Store.

The runner must use its fixed repository registry, never a caller-selected ledger.
A stable scope binds one active campaign and its finite allowance; only a
reviewed, landed campaign with final acceptance may have an explicit successor. Reservations are
charged before any process starts and never refunded, including ambiguous crashes.
Historical h/j used 19/9 root executions; 24 plus authority turns is a conservative
complete-fixture envelope. The default reserves 48 root executions times five
contexts: a three-context execution cap plus two concurrently starting children
of polling slack. The runner must enforce the execution cap and bounded polling slack. Defaults
allow three complete attempts, two bounded live probes, and 750 contexts while
protecting 240 contexts for final qualification. Deterministic probes cost nothing.
Telemetry is cumulative *per thread*, not a sum of repeated journal snapshots or
parent totals that already include children. Missing telemetry remains unknown.
"""
from __future__ import annotations

from functools import wraps
import json
import re
import sqlite3
import time
from pathlib import Path

DEFAULT_LIMITS = {"complete_fixture": 3, "live_probe": 2,
                  "application_dogfood": 1, "contexts": 750, "final_headroom": 240, "diagnosis_threshold": 2}
STAGES = ("complete_fixture", "live_probe", "application_dogfood")


class AdmissionDenied(RuntimeError):
    """A persisted campaign gate refused another live launch."""


FAILURE_CLASSES = frozenset({
    'configuration', 'payload_size', 'source_binding', 'submission', 'child_review',
    'journal_decoding', 'deadline', 'qualification_acceptance', 'admission',
    'deterministic_compatibility', 'interrupted', 'unknown_failure',
})


def normalise_failure(value):
    value = re.sub(r"[^a-z0-9]+", "_", value.lower()).strip("_")
    if not value:
        raise ValueError("failure class must not be empty")
    aliases = {'tool_reply_overflow': 'payload_size', 'startup_config': 'configuration'}
    value = aliases.get(value, value)
    return value if value in FAILURE_CLASSES else 'unknown_failure'


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _fingerprint(value):
    if not isinstance(value, dict) or not value or any(not isinstance(k, str) or not isinstance(v, str) or not v for k, v in value.items()):
        raise ValueError("fingerprint requires named, non-empty string identities")
    return _json(value)


def _mutation(method):
    @wraps(method)
    def wrapped(self, *args, **kwargs):
        owner = not self.db.in_transaction
        if owner:
            self.db.execute('BEGIN IMMEDIATE')
        try:
            self._assert_active()
            result = method(self, *args, **kwargs)
            if owner:
                self.db.commit()
            return result
        except BaseException:
            if owner:
                self.db.rollback()
            raise
    return wrapped


class Campaign:
    @classmethod
    def open(cls, registry_path, scope, campaign_id, limits=None):
        return cls(registry_path, scope, campaign_id, limits)

    def __init__(self, registry_path, scope, campaign_id, limits=None):
        if not scope or not campaign_id:
            raise ValueError("stable scope and campaign identifier required")
        self.scope, self.campaign_id = scope, campaign_id
        self.path = Path(registry_path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(self.path, timeout=30, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.executescript('''
        CREATE TABLE IF NOT EXISTS campaigns(scope TEXT PRIMARY KEY, campaign_id TEXT UNIQUE NOT NULL, limits TEXT NOT NULL, created REAL NOT NULL);
        CREATE TABLE IF NOT EXISTS attempts(scope TEXT NOT NULL, id TEXT NOT NULL, stage TEXT NOT NULL, fingerprint TEXT NOT NULL, envelope INTEGER NOT NULL, final INTEGER NOT NULL, state TEXT NOT NULL, created REAL NOT NULL, launched REAL, completed REAL, passed INTEGER, failure TEXT, relevant TEXT, evidence TEXT, pre_model_fault INTEGER DEFAULT 0, final_only_defect INTEGER DEFAULT 0, PRIMARY KEY(scope,id));
        CREATE TABLE IF NOT EXISTS repairs(scope TEXT NOT NULL, failure_id TEXT NOT NULL, fingerprint TEXT NOT NULL, evidence TEXT NOT NULL, created REAL NOT NULL, PRIMARY KEY(scope,failure_id,fingerprint));
        CREATE TABLE IF NOT EXISTS probes(scope TEXT NOT NULL, id TEXT NOT NULL, failure_id TEXT NOT NULL, fingerprint TEXT NOT NULL, passed INTEGER NOT NULL, evidence TEXT NOT NULL, created REAL NOT NULL, PRIMARY KEY(scope,id));
        CREATE TABLE IF NOT EXISTS telemetry(scope TEXT NOT NULL, attempt_id TEXT NOT NULL, thread_id TEXT NOT NULL, parent TEXT, input INTEGER, cached INTEGER, total INTEGER, output INTEGER, responses INTEGER, PRIMARY KEY(scope,thread_id));
        CREATE TABLE IF NOT EXISTS checks(scope TEXT NOT NULL, id TEXT NOT NULL, stage TEXT NOT NULL, fingerprint TEXT NOT NULL, passed INTEGER NOT NULL, evidence TEXT NOT NULL, failure TEXT, created REAL NOT NULL, PRIMARY KEY(scope,id));
        CREATE TABLE IF NOT EXISTS suppressions(scope TEXT NOT NULL, attempt_id TEXT NOT NULL, reason TEXT NOT NULL, created REAL NOT NULL);
        ''')
        columns = {row[1] for row in self.db.execute('PRAGMA table_info(campaigns)')}
        if 'terminal' not in columns:
            try:
                self.db.execute('ALTER TABLE campaigns ADD COLUMN terminal TEXT')
            except sqlite3.OperationalError:
                if 'terminal' not in {row[1] for row in self.db.execute('PRAGMA table_info(campaigns)')}:
                    raise
        self.db.execute('CREATE TABLE IF NOT EXISTS policy_events(scope TEXT NOT NULL, kind TEXT NOT NULL, evidence TEXT NOT NULL, created REAL NOT NULL)')
        requested = self._limits(limits)
        self.db.execute('BEGIN IMMEDIATE')
        try:
            archived = self.db.execute('SELECT * FROM campaigns WHERE campaign_id=?', (campaign_id,)).fetchone()
            if archived and archived['scope'] != scope and archived['scope'] == scope + '::' + campaign_id and archived['terminal']:
                self.scope = archived['scope']
                scope = self.scope
            row = self.db.execute('SELECT * FROM campaigns WHERE scope=?', (scope,)).fetchone()
            if row:
                if row['campaign_id'] != campaign_id or (limits is not None and row['limits'] != _json(requested)):
                    raise AdmissionDenied('scope already bound to an immutable campaign and allowance')
                self.limits = json.loads(row['limits'])
            else:
                self.db.execute('INSERT INTO campaigns(scope,campaign_id,limits,created) VALUES(?,?,?,?)', (scope, campaign_id, _json(requested), time.time()))
                self.limits = requested
            self.db.commit()
        except BaseException:
            self.db.rollback()
            raise

    @staticmethod
    def _limits(limits):
        requested = dict(DEFAULT_LIMITS, **(limits or {}))
        if set(requested) != set(DEFAULT_LIMITS) or any(type(v) is not int or v < 0 for v in requested.values()):
            raise ValueError("invalid finite allowance")
        if requested['diagnosis_threshold'] < 1:
            raise ValueError('diagnosis threshold must be positive')
        if requested['final_headroom'] > requested['contexts']:
            raise ValueError("headroom exceeds context allowance")
        return requested

    def _assert_active(self):
        row = self.db.execute('SELECT * FROM campaigns WHERE scope=? AND campaign_id=?', (self.scope, self.campaign_id)).fetchone()
        if row is None or row['terminal']:
            raise AdmissionDenied('campaign is terminal or has been superseded')
        self.limits = json.loads(row['limits'])

    @_mutation
    def configure(self, limits, evidence):
        """Set initial finite policy before spending; retain the decision evidence."""
        if not evidence or self.db.execute('SELECT 1 FROM attempts WHERE scope=?', (self.scope,)).fetchone():
            raise AdmissionDenied('policy can only be configured before any reservation with evidence')
        self.limits = self._limits(limits)
        self.db.execute('UPDATE campaigns SET limits=? WHERE scope=?', (_json(self.limits), self.scope))
        self.db.execute('INSERT INTO policy_events VALUES(?,?,?,?)', (self.scope, 'initial_policy', _json({'limits': self.limits, 'evidence': evidence}), time.time()))

    @_mutation
    def finish(self, evidence):
        """Close after final acceptance and recorded review/landing identities."""
        if not isinstance(evidence, dict) or not all(isinstance(evidence.get(k), str) and evidence[k].strip() for k in ('reviewed_revision', 'landed_reference')):
            raise ValueError('terminal evidence requires reviewed_revision and landed_reference')
        rows = self.db.execute('SELECT * FROM attempts WHERE scope=? ORDER BY created,id', (self.scope,)).fetchall()
        if not rows or any(r['state'] != 'completed' for r in rows) or not (rows[-1]['stage'] == 'complete_fixture' and rows[-1]['final'] and rows[-1]['passed']):
            raise AdmissionDenied('terminal campaign requires all attempts reconciled and latest final qualification passed')
        self.db.execute('UPDATE campaigns SET terminal=? WHERE scope=?', (_json(evidence), self.scope))

    def begin_successor(self, new_campaign_id, evidence, limits=None):
        """Explicitly archive a terminal campaign and start its next work package."""
        if not new_campaign_id or new_campaign_id == self.campaign_id or not evidence:
            raise ValueError('distinct successor identity and work-package evidence required')
        requested = self._limits(limits)
        scope = self.scope
        archive = scope + '::' + self.campaign_id
        self.db.execute('BEGIN IMMEDIATE')
        try:
            old = self.db.execute('SELECT * FROM campaigns WHERE scope=? AND campaign_id=?', (scope, self.campaign_id)).fetchone()
            if not old or not old['terminal'] or '::' in scope:
                raise AdmissionDenied('only the current terminal campaign can begin a successor')
            if self.db.execute('SELECT 1 FROM campaigns WHERE campaign_id=? OR scope=?', (new_campaign_id, archive)).fetchone():
                raise AdmissionDenied('successor or archive identity already exists')
            for table in ('campaigns', 'attempts', 'repairs', 'probes', 'telemetry', 'checks', 'suppressions', 'policy_events'):
                self.db.execute(f'UPDATE {table} SET scope=? WHERE scope=?', (archive, scope))
            self.db.execute('INSERT INTO campaigns(scope,campaign_id,limits,created) VALUES(?,?,?,?)', (scope, new_campaign_id, _json(requested), time.time()))
            self.db.execute('INSERT INTO policy_events VALUES(?,?,?,?)', (scope, 'successor', _json({'previous_campaign': self.campaign_id, 'evidence': evidence}), time.time()))
            self.db.commit()
        except BaseException:
            self.db.rollback()
            raise
        self.scope = archive
        return Campaign.open(self.path, scope, new_campaign_id)

    def close(self):
        self.db.close()

    def _attempt(self, attempt_id):
        row = self.db.execute('SELECT * FROM attempts WHERE scope=? AND id=?', (self.scope, attempt_id)).fetchone()
        if row is None:
            raise ValueError('unknown attempt')
        return dict(row)

    def reserve(self, attempt_id, stage, fingerprint, root_turns=24, authority_turns=24, subagents=2, final=False, contexts_per_execution=5):
        fp = _fingerprint(fingerprint)
        if not attempt_id or stage not in STAGES or any(type(v) is not int or v < 0 for v in (root_turns, authority_turns, subagents)) or root_turns < 1:
            raise ValueError('invalid attempt or finite execution envelope')
        if final and stage != 'complete_fixture':
            raise ValueError('final headroom belongs to complete qualification')
        multiplier = 1 + subagents if contexts_per_execution is None else contexts_per_execution
        if type(multiplier) is not int or multiplier < 1 + subagents:
            raise ValueError('context envelope must contain root and all concurrent children')
        envelope = (root_turns + authority_turns) * multiplier
        self.db.execute('BEGIN IMMEDIATE')
        try:
            self._assert_active()
            rows = [dict(r) for r in self.db.execute('SELECT * FROM attempts WHERE scope=? ORDER BY created,id', (self.scope,))]
            reason = None
            prior = next((r for r in rows if r['id'] == attempt_id), None)
            if prior:
                # Reopening an identity never grants a second launch.
                reason = 'attempt identity already reserved; reconcile or report it without relaunching'
            elif any(r['state'] != 'completed' for r in rows):
                reason = 'unresolved reservation or launch requires reconciliation'
            elif sum(r['stage'] == stage for r in rows) >= self.limits[stage]:
                reason = 'stage allowance exhausted'
            elif self._charged_contexts(rows) + envelope + (0 if final else self.limits['final_headroom']) > self.limits['contexts']:
                reason = 'context allowance or final headroom exhausted'
            if not reason:
                complete = rows
                failures = [r for r in complete if not r['passed']]
                if failures:
                    failed = failures[-1]
                    relevant = json.loads(failed['relevant'])
                    old = json.loads(failed['fingerprint'])
                    changed = all(k in fingerprint for k in relevant) and any(old.get(k) != fingerprint[k] for k in relevant)
                    repair = self.db.execute('SELECT * FROM repairs WHERE scope=? AND failure_id=? AND fingerprint=?', (self.scope, failed['id'], fp)).fetchone()
                    probe = self.db.execute('SELECT * FROM probes WHERE scope=? AND failure_id=? AND fingerprint=? ORDER BY created DESC,id DESC LIMIT 1', (self.scope, failed['id'], fp)).fetchone()
                    if not changed:
                        reason = 'unchanged failure class and relevant input fingerprint'
                    elif not repair or not probe or not probe['passed'] or probe['created'] < repair['created']:
                        reason = 'changed repair and subsequent relevant passing probe required'
                    elif self._consecutive_complete_failures(rows) >= self.limits['diagnosis_threshold'] and not json.loads(repair['evidence']).get('diagnosis'):
                        reason = 'two consecutive complete failures require recorded diagnosis'
            if reason:
                self.db.execute('INSERT INTO suppressions VALUES(?,?,?,?)', (self.scope, attempt_id, reason, time.time()))
                self.db.commit()
                raise AdmissionDenied(reason)
            self.db.execute('INSERT INTO attempts(scope,id,stage,fingerprint,envelope,final,state,created) VALUES(?,?,?,?,?,?,?,?)', (self.scope, attempt_id, stage, fp, envelope, int(final), 'reserved', time.time()))
            self.db.commit()
            return self._attempt(attempt_id)
        except BaseException:
            if self.db.in_transaction:
                self.db.rollback()
            raise

    @_mutation
    def mark_launched(self, attempt_id):
        # Call immediately before the external launch. A crash here is ambiguous
        # and must consume the reservation; this transition cannot be replayed.
        changed = self.db.execute("UPDATE attempts SET state='launched',launched=? WHERE scope=? AND id=? AND state='reserved'", (time.time(), self.scope, attempt_id)).rowcount
        if changed != 1:
            raise AdmissionDenied('launch already claimed or attempt unresolved')

    @_mutation
    def interrupt(self, attempt_id, evidence):
        self._attempt(attempt_id)
        self.db.execute("UPDATE attempts SET state='interrupted',evidence=? WHERE scope=? AND id=? AND state!='completed'", (_json(evidence), self.scope, attempt_id))

    @_mutation
    def complete(self, attempt_id, passed, failure_class=None, relevant_inputs=None, evidence=None, pre_model_fault=False, final_only_defect=False):
        row = self._attempt(attempt_id)
        if row['state'] == 'completed':
            raise AdmissionDenied('completion already recorded')
        if row['state'] == 'interrupted' and not ((evidence or {}).get('cessation_verified') is True or (evidence or {}).get('not_started') is True):
            raise ValueError('interrupted attempt requires reconciliation evidence')
        if row['state'] == 'reserved' and not pre_model_fault and not (evidence or {}).get('not_started') is True:
            raise ValueError('unlaunched reservation requires not-started evidence')
        if passed and row['state'] != 'launched':
            raise ValueError('passing completion requires a launch')
        relevant = sorted(set(relevant_inputs or json.loads(row['fingerprint'])))
        if any(k not in json.loads(row['fingerprint']) for k in relevant):
            raise ValueError('relevant input missing from attempt fingerprint')
        failure = None if passed else normalise_failure(failure_class or 'unknown_failure')
        changed = self.db.execute("UPDATE attempts SET state='completed',completed=?,passed=?,failure=?,relevant=?,evidence=?,pre_model_fault=?,final_only_defect=? WHERE scope=? AND id=? AND state=?", (time.time(), int(passed), failure, _json(relevant), _json(evidence or {}), int(pre_model_fault), int(final_only_defect), self.scope, attempt_id, row['state'])).rowcount
        if changed != 1:
            raise AdmissionDenied('completion already recorded')

    @_mutation
    def record_repair(self, failure_attempt_id, fingerprint, evidence):
        failed = self._attempt(failure_attempt_id)
        fp = _fingerprint(fingerprint)
        if failed['state'] != 'completed' or failed['passed'] or not evidence:
            raise ValueError('repair requires a completed failure and evidence')
        relevant = json.loads(failed['relevant'])
        old = json.loads(failed['fingerprint'])
        if not all(k in fingerprint for k in relevant) or not any(old[k] != fingerprint[k] for k in relevant):
            raise ValueError('repair must change relevant inputs')
        self.db.execute('INSERT OR IGNORE INTO repairs VALUES(?,?,?,?,?)', (self.scope, failure_attempt_id, fp, _json(evidence), time.time()))

    @_mutation
    def record_probe(self, probe_id, failure_attempt_id, fingerprint, passed, evidence):
        self._attempt(failure_attempt_id)
        if not evidence:
            raise ValueError('probe evidence required')
        values = (self.scope, probe_id, failure_attempt_id, _fingerprint(fingerprint), int(passed), _json(evidence))
        previous = self.db.execute('SELECT * FROM probes WHERE scope=? AND id=?', values[:2]).fetchone()
        if previous:
            if tuple(previous[k] for k in ('scope', 'id', 'failure_id', 'fingerprint', 'passed', 'evidence')) != values:
                raise ValueError('probe identity cannot be rewritten')
            return
        self.db.execute('INSERT INTO probes VALUES(?,?,?,?,?,?,?)', (*values, time.time()))

    def telemetry(self, attempt_id, thread_id, input_tokens=None, cached_input_tokens=None, total_tokens=None, parent_thread_id=None, output_tokens=None, model_responses=None):
        self._attempt(attempt_id)
        if not thread_id or thread_id == parent_thread_id or any(v is not None and (type(v) is not int or v < 0) for v in (input_tokens, cached_input_tokens, total_tokens, output_tokens, model_responses)):
            raise ValueError('invalid per-thread cumulative telemetry')
        if cached_input_tokens is not None and (input_tokens is None or cached_input_tokens > input_tokens):
            raise ValueError('cached tokens require a containing input count')
        self.db.execute('BEGIN IMMEDIATE')
        try:
            self._assert_active()
            old = self.db.execute('SELECT * FROM telemetry WHERE scope=? AND thread_id=?', (self.scope, thread_id)).fetchone()
            if old and (old['attempt_id'] != attempt_id or old['parent'] != parent_thread_id):
                raise ValueError('thread identity already bound')
            # A cumulative input snapshot carries its matching cache count.
            # Older snapshots must not independently increase cache totals.
            if old and old['input'] is not None and (input_tokens is None or input_tokens < old['input']):
                input_tokens, cached_input_tokens = old['input'], old['cached']
            elif old and input_tokens == old['input'] and cached_input_tokens is None:
                cached_input_tokens = old['cached']
            if old and old['total'] is not None:
                total_tokens = max(total_tokens or 0, old['total'])
            if old:
                output_tokens = max(output_tokens or 0, old['output']) if old['output'] is not None else output_tokens
                model_responses = max(model_responses or 0, old['responses']) if old['responses'] is not None else model_responses
            self.db.execute('INSERT OR REPLACE INTO telemetry VALUES(?,?,?,?,?,?,?,?,?)', (self.scope, attempt_id, thread_id, parent_thread_id, input_tokens, cached_input_tokens, total_tokens, output_tokens, model_responses))
            self.db.commit()
        except BaseException:
            self.db.rollback()
            raise

    @staticmethod
    def _consecutive_complete_failures(rows):
        count = 0
        for row in reversed(rows):
            if row['stage'] != 'complete_fixture':
                continue
            if row['passed']:
                break
            count += 1
        return count

    @_mutation
    def record_check(self, check_id, stage, fingerprint, passed, evidence, failure_class=None):
        """Record deterministic evidence without reserving live allowance."""
        if stage not in ('preflight', 'focused_probe') or not evidence:
            raise ValueError('deterministic check stage and evidence required')
        values = (self.scope, check_id, stage, _fingerprint(fingerprint), int(passed), _json(evidence), None if passed else normalise_failure(failure_class or 'unknown_failure'))
        old = self.db.execute('SELECT * FROM checks WHERE scope=? AND id=?', values[:2]).fetchone()
        if old:
            if tuple(old[k] for k in ('scope', 'id', 'stage', 'fingerprint', 'passed', 'evidence', 'failure')) != values:
                raise ValueError('check identity cannot be rewritten')
            return
        self.db.execute('INSERT INTO checks VALUES(?,?,?,?,?,?,?,?)', (*values, time.time()))

    @_mutation
    def reconcile(self, attempt_id, evidence, failure_class='interrupted', relevant_inputs=None):
        """Close an ambiguous launch only with explicit cessation evidence."""
        if not evidence or not (evidence.get('cessation_verified') is True or evidence.get('not_started') is True):
            raise ValueError('reconciliation requires verified cessation or not-started evidence')
        self.complete(attempt_id, False, failure_class, relevant_inputs, evidence)

    def _charged_contexts(self, rows):
        return sum(max(r['envelope'], self.db.execute('SELECT COUNT(*) FROM telemetry WHERE scope=? AND attempt_id=?', (self.scope, r['id'])).fetchone()[0]) for r in rows)

    def report(self):
        binding = self.db.execute('SELECT * FROM campaigns WHERE campaign_id=?', (self.campaign_id,)).fetchone()
        self.scope = binding['scope']
        self.limits = json.loads(binding['limits'])
        rows = [dict(r) for r in self.db.execute('SELECT * FROM attempts WHERE scope=? ORDER BY created,id', (self.scope,))]
        tokens = [dict(r) for r in self.db.execute('SELECT * FROM telemetry WHERE scope=?', (self.scope,))]
        known = [r for r in tokens if r['input'] is not None and r['cached'] is not None]
        uncached = sum(r['input'] - r['cached'] for r in known)
        accepted = sum(r['stage'] == 'complete_fixture' and r['passed'] == 1 for r in rows)
        missing = any(not any(t['attempt_id'] == r['id'] for t in tokens) for r in rows if r['launched']) or len(known) != len(tokens)
        return {'scope': self.scope, 'campaign_id': self.campaign_id, 'limits': self.limits,
                'attempts': rows, 'stages': {s: sum(r['stage'] == s for r in rows) for s in STAGES},
                'terminal': json.loads(binding['terminal']) if binding['terminal'] else None,
                'policy_events': [dict(r) for r in self.db.execute('SELECT * FROM policy_events WHERE scope=? ORDER BY created', (self.scope,))],
                'charged_contexts': self._charged_contexts(rows),
                'reserved_contexts': sum(r['envelope'] for r in rows), 'observed_contexts': len(tokens),
                'telemetry_complete': not missing, 'known_uncached_tokens': uncached,
                'uncached_tokens_per_accepted_qualification': uncached / accepted if accepted and not missing else None,
                'known_output_tokens': sum(r['output'] or 0 for r in tokens),
                'known_model_responses': sum(r['responses'] or 0 for r in tokens),
                'checks': [dict(r) for r in self.db.execute('SELECT * FROM checks WHERE scope=? ORDER BY created,id', (self.scope,))],
                'accepted_qualifications': accepted, 'suppressions': [dict(r) for r in self.db.execute('SELECT * FROM suppressions WHERE scope=?', (self.scope,))],
                'pre_model_faults': sum(r['pre_model_fault'] for r in rows),
                'final_only_defects': sum(r['final_only_defect'] for r in rows),
                'elapsed_seconds': sum((r['completed'] or time.time()) - r['created'] for r in rows),
                'interventions': sum(json.loads(r['evidence'] or '{}').get('interventions', 0) for r in rows),
                'historical_baseline': {'contexts': 91, 'approx_first_response_uncached_tokens': 3340000, 'approx_fixture_input_tokens': 4150000, 'savings_claim': None}}
