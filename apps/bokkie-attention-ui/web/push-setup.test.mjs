import test from 'node:test';
import assert from 'node:assert/strict';
import { createPushSetup } from './push-setup.js';
const key = Buffer.concat([Buffer.from([4]), Buffer.alloc(64, 1)]).toString('base64url');
function fixture({ active = false, ios = false, installed = true, configured = true, permission = 'default', failRegister = false, configurationRevision = 3 } = {}) {
  const calls = [], storage = new Map();
  let setup = { service: { session_id: 's', build: 'bokkie' }, configured, configuration_revision: configurationRevision, vapid_public_key: key,
    device: active ? { id: 'device-a', label: 'Other phone', active: true } : null, ttl_seconds: 3600 };
  const state = { failRegister, permission, requests: [], subscriptions: 0, unsubscribes: 0 };
  let commandNumber = 0;
  const current = { options: { applicationServerKey: Uint8Array.from(Buffer.from(key, 'base64url')).buffer },
    unsubscribe: async () => { state.unsubscribes++; },
    toJSON: () => ({ endpoint: 'https://push.example.test/id', keys: { p256dh: 'p256dh', auth: 'auth' } }) };
  const registration = { active: { postMessage: value => calls.push(['message', value]) }, pushManager: {
    getSubscription: async () => state.subscriptions ? current : null,
    subscribe: async options => { calls.push(['subscribe', options]); state.subscriptions++; return current; },
  } };
  const notification = { get permission() { return state.permission; }, requestPermission: () => {
    calls.push(['permission']); state.permission = 'granted'; return Promise.resolve('granted');
  } };
  const env = { storage: { getItem: k => storage.get(k), setItem: (k, v) => storage.set(k, v), removeItem: k => storage.delete(k) },
    navigator: { userAgent: ios ? 'iPhone' : 'Chromium', platform: 'Linux', serviceWorker: {
      register: async (path, options) => { calls.push(['register', path, options]); return registration; }, ready: Promise.resolve(registration),
    } }, Notification: notification, PushManager: {}, isSecureContext: true,
    matchMedia: () => ({ matches: installed }), randomUUID: () => `12345678-1234-4234-8234-${String(++commandNumber).padStart(12, '0')}`,
    location: { href: 'https://bokkie.example.test/ui/', origin: 'https://bokkie.example.test', search: '' },
    history: { replaceState: (_a, _b, url) => calls.push(['history', url]) },
    fetch: async (path, options) => {
      calls.push(['fetch', path, options]);
      if (path === '/bootstrap') return { ok: true, json: async () => ({ service: setup.service, mutation_token: 'a'.repeat(64) }) };
      if (path.endsWith('/register')) {
        state.requests.push(JSON.parse(options.body));
        if (state.failRegister) throw Error('lost registration response');
        if (state.requests.at(-1).configuration_revision !== setup.configuration_revision) return { ok: false, status: 409 };
        setup = { ...setup, configuration_revision: setup.configuration_revision + 1, device: { id: 'device-a', label: state.requests.at(-1).label, active: true } };
      }
      if (path.endsWith('/disable')) setup = { ...setup, configuration_revision: 5, device: { ...setup.device, active: false } };
      return { ok: true, json: async () => setup };
    } };
  return { state, calls, storage, env, controller: createPushSetup(env), registration,
    setSetup: changes => { setup = { ...setup, ...changes }; }, getSetup: () => setup };
}

test('explicit enable requests permission synchronously before any asynchronous work', async () => {
  const f = fixture(); await f.controller.refresh(); f.calls.length = 0;
  const work = f.controller.enable('My phone');
  assert.equal(f.calls[0][0], 'permission'); await work;
  assert.equal(f.state.subscriptions, 1);
  const options = f.calls.find(([name]) => name === 'subscribe')[1]; assert.equal(options.userVisibleOnly, true);
  assert.equal(options.applicationServerKey.length, 65);
  assert.equal(f.controller.snapshot().local_device, true);
});

test('unconfigured service and uninstalled iOS never prompt for permission', async () => {
  for (const options of [{ configured: false }, { ios: true, installed: false }, { permission: 'denied' }]) {
    const f = fixture(options); await f.controller.refresh(); await f.controller.enable('Phone');
    assert.equal(f.calls.filter(([name]) => name === 'permission').length, 0);
    assert.equal(f.controller.snapshot().can_enable, false);
  }
});

test('another active device requires separate disabling review and exact confirmation', async () => {
  const f = fixture({ active: true }); await f.controller.refresh(); await f.controller.enable('Phone');
  assert.equal(f.state.subscriptions, 0); assert.equal(f.state.requests.length, 0);
  f.controller.reviewDisable(); assert.equal(f.controller.snapshot().disable_review, 'Other phone');
  assert.equal(f.calls.filter(([name, path]) => name === 'fetch' && path.endsWith('/disable')).length, 0);
  await f.controller.disable();
  const mutation = f.calls.find(([name, path]) => name === 'fetch' && path.endsWith('/disable'))[2];
  assert.equal(JSON.parse(mutation.body).configuration_revision, 3);
  assert.equal(f.controller.snapshot().active, false);
});

test('lost response retains subscription and exact persisted request for retry after reload', async () => {
  const f = fixture({ failRegister: true }); await f.controller.refresh(); await f.controller.enable('My phone');
  assert.equal(f.controller.snapshot().pending, true); assert.equal(f.controller.snapshot().active, false);
  assert.equal(f.state.subscriptions, 1);
  f.state.failRegister = false;
  const reload = createPushSetup(f.env); await reload.refresh(); await reload.enable('A different edited label');
  assert.deepEqual(f.state.requests[0], f.state.requests[1]); assert.equal(f.state.subscriptions, 1);
  assert.equal(reload.snapshot().active, true);
  assert.ok(![...f.storage.values()].some(value => value.includes('a'.repeat(64))));
});

test('revoked browser permission is visible even while server device remains active', async () => {
  const f = fixture(); await f.controller.refresh(); await f.controller.enable('Phone');
  f.state.permission = 'denied'; assert.equal(f.controller.snapshot().active, true);
  assert.equal(f.controller.snapshot().permission, 'denied'); assert.equal(f.controller.snapshot().can_enable, false);
});

test('restart between reviewed setup and mutation fails closed without enrolment', async () => {
  const f = fixture(); await f.controller.refresh(); const fetch = f.env.fetch;
  f.env.fetch = async (path, options) => path === '/bootstrap'
    ? { ok: true, json: async () => ({ service: { session_id: 'new' }, mutation_token: 'a'.repeat(64) }) }
    : fetch(path, options);
  await f.controller.enable('Phone'); assert.equal(f.state.requests.length, 0);
  assert.match(f.controller.snapshot().error, /restarted/);
});

test('task links reject external paths and can queue exact task without clearing a draft', () => {
  const f = fixture(), id = 'task-12345678-1234-4234-8234-123456789abc';
  f.env.location.search = '?task=https://evil.example/'; assert.equal(f.controller.takeTaskLink(), null);
  f.controller.queueTaskLink(id); assert.equal(f.controller.takeTaskLink(), id);
  assert.equal(f.controller.takeTaskLink(), null);
  assert.equal(f.calls.at(-1)[1], 'https://bokkie.example.test/ui/?task=' + id);
});


test('trusted canvas gesture can stage permission without making a network request', async () => {
  const f = fixture(); await f.controller.refresh(); f.calls.length = 0;
  f.controller.beginPermissionGesture();
  assert.deepEqual(f.calls.map(call => call[0]), ['permission']);
  await f.controller.enable('Phone');
  assert.equal(f.calls.filter(call => call[0] === 'permission').length, 1);
  assert.equal(f.controller.snapshot().active, true);
});


test('published setup revision changes on enrolment and disable, and stays stable on reads', async () => {
  const f = fixture(); assert.equal(f.controller.snapshot().configuration_revision, null);
  await f.controller.refresh(); assert.equal(f.controller.snapshot().configuration_revision, 3);
  await f.controller.refresh(); assert.equal(f.controller.snapshot().configuration_revision, 3);
  await f.controller.enable('Phone'); assert.equal(f.controller.snapshot().configuration_revision, 4);
  await f.controller.refresh(); assert.equal(f.controller.snapshot().configuration_revision, 4);
  f.controller.reviewDisable(); await f.controller.disable();
  assert.equal(f.controller.snapshot().configuration_revision, 5);
});


test('stale revision-zero request needs reviewed local discard before a fresh revision-two enrolment', async () => {
  const f = fixture({ configurationRevision: 0, failRegister: true });
  await f.controller.refresh(); await f.controller.enable('Original browser');
  const original = f.state.requests[0], saved = f.storage.get('bokkie-push-pending-v1');
  assert.equal(original.configuration_revision, 0);
  assert.equal(f.controller.snapshot().pending, true);
  f.state.failRegister = false;
  f.setSetup({ configuration_revision: 2, device: { id: 'another-device', label: 'Other browser', active: false } });
  f.storage.set('bokkie-push-device-v1', JSON.stringify('previous-local-device'));
  const reload = createPushSetup(f.env); await reload.refresh();
  for (let n = 0; n < 2; n++) {
    await reload.enable('Edited name must not change the retained request');
    assert.match(reload.snapshot().error, /409/);
    await reload.refresh();
  }
  assert.deepEqual(f.state.requests, [original, original, original]);
  assert.equal(reload.snapshot().pending, true); assert.equal(reload.snapshot().can_disable, false);
  const serverBefore = structuredClone(f.getSetup());
  const postCount = () => f.calls.filter(([name, _path, options]) => name === 'fetch' && options?.method === 'POST').length;
  const postsBefore = postCount();
  await reload.discardPending(); // No review: no effect.
  assert.equal(f.storage.get('bokkie-push-pending-v1'), saved);
  reload.reviewDiscardPending();
  assert.equal(reload.snapshot().pending_discard_review, 'Original browser');
  assert.equal(reload.snapshot().can_enable, false);
  assert.equal(f.storage.get('bokkie-push-pending-v1'), saved);
  reload.cancelDiscardPending();
  assert.equal(reload.snapshot().pending, true);
  assert.equal(f.storage.get('bokkie-push-pending-v1'), saved);
  reload.reviewDiscardPending(); await reload.discardPending();
  assert.equal(reload.snapshot().pending, false);
  assert.equal(reload.snapshot().configuration_revision, 2);
  assert.equal(f.storage.has('bokkie-push-pending-v1'), false);
  assert.equal(f.storage.get('bokkie-push-device-v1'), JSON.stringify('previous-local-device'));
  assert.deepEqual(f.getSetup(), serverBefore);
  assert.equal(postCount(), postsBefore);
  assert.equal(f.state.subscriptions, 1); assert.equal(f.state.unsubscribes, 0);
  assert.equal(f.state.requests.length, 3); // Discard never auto-enrols.
  await reload.enable('Fresh chosen browser');
  const fresh = f.state.requests[3];
  assert.equal(fresh.configuration_revision, 2);
  assert.notEqual(fresh.command_id, original.command_id);
  assert.equal(fresh.label, 'Fresh chosen browser');
  assert.equal(fresh.endpoint, original.endpoint);
  assert.deepEqual(fresh.keys, original.keys);
  assert.equal(f.state.subscriptions, 1); assert.equal(f.state.unsubscribes, 0);
  assert.equal(reload.snapshot().active, true);
});

test('failed storage deletion retains the exact request and does not claim a discard', async () => {
  const f = fixture({ failRegister: true }); await f.controller.refresh(); await f.controller.enable('Browser');
  const saved = f.storage.get('bokkie-push-pending-v1');
  f.env.storage.removeItem = () => { throw Error('Storage deletion unavailable'); };
  f.controller.reviewDiscardPending(); await f.controller.discardPending();
  assert.equal(f.controller.snapshot().pending, true);
  assert.match(f.controller.snapshot().status, /retained/);
  assert.equal(f.storage.get('bokkie-push-pending-v1'), saved);
  assert.equal(f.state.unsubscribes, 0);
});


test('worker loading failure retains server settings and exposes an actionable retry', async () => {
  const f = fixture();
  f.env.navigator.serviceWorker.register = async () => { throw Error('Script HTTP 401'); };
  await f.controller.refresh();
  const state = f.controller.snapshot();
  assert.equal(state.configured, true);
  assert.equal(state.ready, false);
  assert.equal(state.can_enable, false);
  assert.match(state.status, /Reopen Bokkie, sign in if asked/);
  assert.equal(state.error, 'Script HTTP 401');
  assert.equal(f.calls.some(([name]) => name === 'permission'), false);
  f.env.navigator.serviceWorker.register = async () => f.registration;
  await f.controller.refresh();
  assert.equal(f.controller.snapshot().can_enable, true);
});
