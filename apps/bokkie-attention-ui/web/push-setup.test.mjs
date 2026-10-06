import test from 'node:test';
import assert from 'node:assert/strict';
import { createPushSetup } from './push-setup.js';
const key = Buffer.concat([Buffer.from([4]), Buffer.alloc(64, 1)]).toString('base64url');
function fixture({ active = false, ios = false, installed = true, configured = true, permission = 'default', failRegister = false } = {}) {
  const calls = [], storage = new Map();
  let setup = { service: { session_id: 's', build: 'bokkie' }, configured, configuration_revision: 3, vapid_public_key: key,
    device: active ? { id: 'device-a', label: 'Other phone', active: true } : null, ttl_seconds: 3600 };
  const state = { failRegister, permission, requests: [], subscriptions: 0 };
  const current = { options: { applicationServerKey: Uint8Array.from(Buffer.from(key, 'base64url')).buffer },
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
      register: async () => registration, ready: Promise.resolve(registration),
    } }, Notification: notification, PushManager: {}, isSecureContext: true,
    matchMedia: () => ({ matches: installed }), randomUUID: () => '12345678-1234-4234-8234-123456789abc',
    location: { href: 'https://bokkie.example.test/ui/', origin: 'https://bokkie.example.test', search: '' },
    history: { replaceState: (_a, _b, url) => calls.push(['history', url]) },
    fetch: async (path, options) => {
      calls.push(['fetch', path, options]);
      if (path === '/bootstrap') return { ok: true, json: async () => ({ service: setup.service, mutation_token: 'a'.repeat(64) }) };
      if (path.endsWith('/register')) {
        state.requests.push(JSON.parse(options.body));
        if (state.failRegister) throw Error('lost registration response');
        setup = { ...setup, configuration_revision: 4, device: { id: 'device-a', label: state.requests.at(-1).label, active: true } };
      }
      if (path.endsWith('/disable')) setup = { ...setup, configuration_revision: 5, device: { ...setup.device, active: false } };
      return { ok: true, json: async () => setup };
    } };
  return { state, calls, storage, env, controller: createPushSetup(env), registration };
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
