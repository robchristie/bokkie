import test from 'node:test';
import assert from 'node:assert/strict';
import { createPushWorker, mergeReceipt, taskURL, validatePayload } from './push-worker.js';

const payload = (changes = {}) => ({ version: 1, id: 'delivery-12345678-1234-4234-8234-123456789abc',
  task_id: 'task-12345678-1234-4234-8234-123456789abc', title: 'Review priorities', body: 'Choose one useful next action.',
  receipt_token: 'a'.repeat(64), expires_at: 2000, ...changes });
function memoryStore() {
  const rows = new Map();
  return { rows, get: async id => rows.get(id), save: async row => {
    const value = mergeReceipt(rows.get(row.id), row); rows.set(row.id, value); return value;
  }, pending: async () => [...rows.values()].filter(row => row.reported !== row.state),
  reported: async row => { rows.get(row.id).reported = row.state; } };
}
function fixture({ offline = false, storageFailure = false, reportFailure = false, showFailure = false, store = memoryStore(), clients = [] } = {}) {
  const state = { shows: [], fetches: [], opened: [], token: 'b'.repeat(64), offline, reportFailure };
  if (storageFailure) store = { get: async () => { throw Error('storage'); }, save: async () => { throw Error('storage'); }, pending: async () => { throw Error('storage'); }, reported: async () => { throw Error('storage'); } };
  const scope = { location: { origin: 'https://bokkie.example.test' },
    registration: { getNotifications: async ({ tag }) => state.shows.filter(item => item.options.tag === tag),
      showNotification: async (title, options) => { if (showFailure) throw Error('show failed'); state.shows.push({ title, options }); } },
    fetch: async (path, options) => {
      state.fetches.push({ path, options, showsAtFetch: state.shows.length });
      if (state.offline) throw Error('offline');
      if (path === '/bootstrap') return { ok: true, json: async () => ({ mutation_token: state.token }) };
      return { ok: !state.reportFailure };
    },
    clients: { matchAll: async () => clients, openWindow: async url => { state.opened.push(url); return {}; } },
  };
  return { state, store, worker: createPushWorker(scope, store, () => 1000), scope };
}

test('offline push is self-contained and displays before any Bokkie API request', async () => {
  const f = fixture({ offline: true }); await f.worker.push(payload());
  assert.equal(f.state.shows.length, 1);
  assert.equal(f.state.shows[0].title, 'Bokkie · Review priorities');
  assert.equal(f.state.shows[0].options.body, payload().body);
  assert.equal(f.state.shows[0].options.tag, payload().id);
  assert.equal(f.state.shows[0].options.renotify, false);
  assert.equal(f.state.fetches[0].showsAtFetch, 1);
  assert.equal(f.store.rows.get(payload().id).state, 'displayed');
  assert.equal(f.store.rows.get(payload().id).reported, null);
});

test('receipt retry survives a new worker and obtains a fresh process token', async () => {
  const first = fixture({ offline: true }); await first.worker.push(payload());
  const next = fixture({ store: first.store }); next.state.token = 'c'.repeat(64);
  await next.worker.flush();
  const receipt = next.state.fetches.find(item => item.path.endsWith('/receipts'));
  assert.equal(receipt.options.headers['X-Bokkie-Mutation-Token'], 'c'.repeat(64));
  assert.equal(JSON.parse(receipt.options.body).state, 'displayed');
  assert.equal(next.store.rows.get(payload().id).reported, 'displayed');
  assert.ok(!JSON.stringify([...next.store.rows.values()]).includes('c'.repeat(64)));
});

test('expiry never displays and is reported separately from display', async () => {
  const f = fixture(); await f.worker.push(payload({ expires_at: 1000 }));
  assert.equal(f.state.shows.length, 0);
  assert.equal(f.store.rows.get(payload().id).state, 'expired');
  assert.equal(JSON.parse(f.state.fetches[1].options.body).state, 'expired');
});

test('duplicate events and event races display one notification', async () => {
  const f = fixture(); await Promise.all([f.worker.push(payload()), f.worker.push(payload())]);
  assert.equal(f.state.shows.length, 1);
  assert.equal(f.state.fetches.filter(item => item.path.endsWith('/receipts')).length, 1);
});

test('storage failure preserves display and an immediate best-effort receipt', async () => {
  const f = fixture({ storageFailure: true }); await f.worker.push(payload());
  await f.worker.push(payload());
  assert.equal(f.state.shows.length, 1); // Browser notification tag provides the fallback deduplication.
  assert.ok(f.state.fetches.some(item => item.path.endsWith('/receipts')));
});

test('failed OS display never claims displayed', async () => {
  const f = fixture({ showFailure: true }); await assert.rejects(f.worker.push(payload()));
  assert.equal(f.store.rows.size, 0); assert.equal(f.state.fetches.length, 0);
});

test('failed authenticated receipt has finite attempts and retries on activity', async () => {
  const f = fixture({ reportFailure: true }); await f.worker.push(payload());
  assert.equal(f.state.fetches.length, 2);
  assert.equal(f.store.rows.get(payload().id).reported, null);
  f.state.reportFailure = false; await f.worker.flush();
  assert.equal(f.store.rows.get(payload().id).reported, 'displayed');
});

test('worker flush caps its receipt cohort', async () => {
  const f = fixture();
  for (let n = 0; n < 40; n++) await f.store.save({ ...payload({ id: `delivery-12345678-1234-4234-8234-${String(n).padStart(12, '0')}` }), state: 'displayed' });
  await f.worker.flush(); assert.equal(f.state.fetches.length, 17);
});

test('closed-page click opens only the exact same-origin task', async () => {
  const f = fixture(); await f.worker.push(payload()); await f.worker.click(payload());
  assert.deepEqual(f.state.opened, ['https://bokkie.example.test/ui/?task=' + payload().task_id]);
  assert.equal(f.store.rows.get(payload().id).state, 'opened');
  await f.worker.push(payload()); assert.equal(f.state.shows.length, 1);
});

test('existing page receives task context without navigation that would lose its composer', async () => {
  const messages = [], client = { url: 'https://bokkie.example.test/ui/', postMessage: value => messages.push(value), focus: async () => {} };
  const f = fixture({ clients: [client] }); await f.worker.click(payload());
  assert.deepEqual(messages, [{ type: 'bokkie-open-task', task_id: payload().task_id }]);
  assert.equal(f.state.opened.length, 0);
});

test('receipt states cannot regress from opened to displayed or expired', () => {
  const row = { ...payload(), state: 'opened', reported: 'opened' };
  assert.equal(mergeReceipt(row, { ...row, state: 'displayed' }).state, 'opened');
  assert.equal(mergeReceipt(row, { ...row, state: 'expired' }).state, 'opened');
  assert.throws(() => mergeReceipt(row, { ...row, receipt_token: 'c'.repeat(64) }));
});

test('hostile deep links, injected URLs, mismatched identities and oversized payloads are rejected', async () => {
  const f = fixture();
  for (const change of [{ task_id: 'https://evil.test/' }, { id: 'other' }, { url: 'https://evil.test/' },
    { title: 'a'.repeat(201) }, { body: 'a'.repeat(2001) }, { body: '💚'.repeat(900) }, { receipt_token: 'bad' }, { expires_at: Infinity }]) {
    assert.throws(() => validatePayload(payload(change))); await f.worker.push(payload(change));
  }
  assert.throws(() => taskURL('task-x/../../admin', f.scope.location.origin));
  assert.equal(f.state.shows.length, 0); assert.equal(f.state.fetches.length, 0);
});

test('valid Unicode title uses codepoints, and push identity cannot change under the same tag', async () => {
  const f = fixture(); await f.worker.push(payload({ title: '💚'.repeat(200) }));
  await f.worker.push(payload({ receipt_token: 'c'.repeat(64) }));
  assert.equal(f.state.shows.length, 1);
});
