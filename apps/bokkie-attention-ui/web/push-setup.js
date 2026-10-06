import { taskURL } from './push-worker.js';

function readSaved(storage, key) {
  try { return JSON.parse(storage.getItem(key) ?? 'null'); } catch { return null; }
}
function save(storage, key, value) {
  try { if (value === null) storage.removeItem(key); else storage.setItem(key, JSON.stringify(value)); }
  catch { /* Permission and subscriptions remain useful when browser storage is unavailable. */ }
}
function applicationKey(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]+$/.test(value)) throw Error('Invalid Bokkie push key');
  const bytes = Uint8Array.from(atob(value.replace(/-/g, '+').replace(/_/g, '/')), char => char.charCodeAt(0));
  if (bytes.length !== 65 || bytes[0] !== 4) throw Error('Invalid Bokkie push key');
  return bytes;
}
function sameService(a, b) { return JSON.stringify(a) === JSON.stringify(b); }

/** The Polyorama button calls enable directly, before crossing an asynchronous boundary. */
export function createPushSetup(env, repaint = () => {}) {
  let setup = null, registration = null, pending = readSaved(env.storage, 'bokkie-push-pending-v1');
  let localDevice = readSaved(env.storage, 'bokkie-push-device-v1');
  let taskLinkRead = false, queuedTask = null, gesturePermission = null;
  let busy = false, error = '', status = 'Loading notification settings…', disableReview = null, pendingDiscardReview = null;
  const installed = () => env.matchMedia('(display-mode: standalone)').matches || env.navigator.standalone === true;
  const ios = () => /iPad|iPhone|iPod/.test(env.navigator.userAgent)
    || (env.navigator.platform === 'MacIntel' && env.navigator.maxTouchPoints > 1);
  const supported = () => env.isSecureContext && !!env.navigator.serviceWorker && !!env.PushManager && !!env.Notification;
  const update = (message, failure = '') => { status = message; error = failure; repaint(); };
  const request = async (path, options = {}) => {
    const response = await env.fetch(path, { credentials: 'same-origin', cache: 'no-store', redirect: 'error',
      ...options, signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw Error(`Bokkie notification request failed (${response.status}). Refresh settings before another change.`);
    return response.json();
  };
  const mutation = async (path, body, expectedService) => {
    const bootstrap = await request('/bootstrap');
    if (!sameService(bootstrap.service, expectedService) || !/^[a-f0-9]{64}$/i.test(bootstrap.mutation_token)) {
      throw Error('Bokkie restarted or its identity changed. Refresh and review notification settings again.');
    }
    const result = await request(path, { method: 'POST', headers: {
      'Content-Type': 'application/json', 'X-Bokkie-Mutation-Token': bootstrap.mutation_token,
    }, body: JSON.stringify(body) });
    if (!sameService(result.service, bootstrap.service)) throw Error('Bokkie notification response changed service identity.');
    return result;
  };
  const subscription = async () => registration ? registration.pushManager.getSubscription() : null;
  const retryReceipts = () => (registration?.active ?? env.navigator.serviceWorker?.controller)
    ?.postMessage({ type: 'bokkie-retry-push-receipts' });
  async function refresh() {
    if (busy) return;
    busy = true; disableReview = null; pendingDiscardReview = null; update('Loading notification settings…');
    try {
      setup = await request('/notifications/push');
      if (!setup || typeof setup.configured !== 'boolean' || !Number.isSafeInteger(setup.configuration_revision)) {
        throw Error('Bokkie returned invalid notification settings.');
      }
      if (supported()) {
        registration = await env.navigator.serviceWorker.register('/ui/service-worker.js', { scope: '/ui/', type: 'module' });
        registration = await env.navigator.serviceWorker.ready;
        retryReceipts();
      }
      update(setup.device?.active ? `Bokkie reminders are assigned to ${setup.device.label}.`
        : 'Choose this browser or installed Bokkie to receive reminders.');
    } catch (failure) { setup = null; update('Notification settings are unavailable.', String(failure.message ?? failure)); }
    finally { busy = false; repaint(); }
  }
  function snapshot() {
    const permission = env.Notification?.permission ?? 'unavailable';
    const active = setup?.device?.active === true;
    return { busy, error, status, supported: supported(), installed: installed(), ios: ios(), permission,
      configured: setup?.configured === true, configuration_revision: setup?.configuration_revision ?? null, ready: !!registration,
      active, device_label: setup?.device?.label ?? '', local_device: active && localDevice === setup.device.id,
      can_enable: !busy && !pendingDiscardReview && !!registration && setup?.configured === true && supported()
        && (!ios() || installed()) && permission !== 'denied' && (!active || !!pending),
      can_disable: !busy && !pendingDiscardReview && active, pending: !!pending, disable_review: disableReview?.label ?? null,
      can_discard_pending: !busy && !!pending, pending_discard_review: pendingDiscardReview?.label ?? null };
  }
  async function finishEnable(permissionPromise, label) {
    busy = true; update('Enabling notifications on this device…');
    try {
      if (await permissionPromise !== 'granted') throw Error('Notification permission was not granted. Change browser or device notification settings, then refresh.');
      if (!pending) {
        const key = applicationKey(setup.vapid_public_key);
        let current = await subscription();
        if (current?.options?.applicationServerKey
            && (current.options.applicationServerKey.byteLength !== key.byteLength || !Array.from(new Uint8Array(current.options.applicationServerKey)).every((byte, index) => byte === key[index]))) {
          throw Error('This browser has a subscription for an older Bokkie key. Disable the old device first, then remove its browser notification subscription before enabling again.');
        }
        current ??= await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
        const data = current.toJSON();
        pending = { body: { command_id: env.randomUUID(), configuration_revision: setup.configuration_revision,
          label, endpoint: data.endpoint, keys: { p256dh: data.keys.p256dh, auth: data.keys.auth } },
          service: setup.service };
        save(env.storage, 'bokkie-push-pending-v1', pending);
      }
      const result = await mutation('/notifications/push/register', pending.body, setup.service);
      if (!result.device?.active) throw Error('Bokkie did not confirm an active notification device.');
      setup = result; localDevice = result.device.id;
      save(env.storage, 'bokkie-push-device-v1', localDevice);
      pending = null; save(env.storage, 'bokkie-push-pending-v1', null);
      update(`Notifications are enabled for ${setup.device.label}. Push-service acceptance and device reports appear separately in reminder history.`);
    } catch (failure) {
      update('Notifications are not confirmed enabled. The browser subscription is retained for a safe retry.', String(failure.message ?? failure));
    } finally { busy = false; repaint(); }
  }
  function enable(label) {
    if (!snapshot().can_enable) return;
    label = label.trim();
    if (!label || Array.from(label).length > 80) { update(status, 'Give this device a name of 1 to 80 characters.'); return; }
    // This call must stay directly inside the operator's click handler. No fetch or await precedes it.
    let permission;
    try {
      permission = env.Notification.permission === 'granted'
        ? Promise.resolve('granted') : (gesturePermission ?? env.Notification.requestPermission());
    } catch (failure) { update(status, `Notification permission could not be requested: ${failure.message ?? failure}`); return; }
    gesturePermission = null;
    return finishEnable(permission, label);
  }
  function reviewDiscardPending() {
    if (busy || !pending) return;
    disableReview = null;
    pendingDiscardReview = { request: pending, label: pending.body?.label ?? 'this device' };
    repaint();
  }
  async function discardPending() {
    if (busy || !pendingDiscardReview || pending !== pendingDiscardReview.request) return;
    // Confirmed local discard is separate from every server/device operation.
    // Keep the request when storage refuses deletion; never claim it was forgotten.
    try {
      env.storage.removeItem('bokkie-push-pending-v1');
      if (env.storage.getItem('bokkie-push-pending-v1') != null) throw Error('Saved request remains in storage');
    } catch (failure) {
      update('The pending request was retained because browser storage could not discard it.', String(failure.message ?? failure));
      return;
    }
    pending = null; pendingDiscardReview = null; gesturePermission = null;
    setup = null; // Enrolment stays unavailable until the current server setup is read.
    await refresh();
    update(`The local enrolment request was discarded. It may already have been accepted; no Bokkie device or history was changed, and the browser subscription is retained. ${status} Enabling again requires a fresh choice.`, error);
  }
  function reviewDisable() {
    if (!snapshot().can_disable) return;
    disableReview = { label: setup.device.label, revision: setup.configuration_revision, service: setup.service,
      command_id: env.randomUUID() };
    repaint();
  }
  async function disable() {
    if (!disableReview || busy) return;
    const review = disableReview; busy = true; update('Disabling the reviewed device…');
    try {
      setup = await mutation('/notifications/push/disable', {
        command_id: review.command_id, configuration_revision: review.revision,
      }, review.service);
      disableReview = null; pending = null; save(env.storage, 'bokkie-push-pending-v1', null);
      // Retain the browser subscription: already admitted reminders remain pinned to it.
      update('This device is disabled for future reminder setup. Already admitted reminders and their history remain assigned to their original device.');
    } catch (failure) { update('Disabling the device was not confirmed. Refresh settings before reviewing another change.', String(failure.message ?? failure)); }
    finally { busy = false; repaint(); }
  }
  return { refresh, snapshot, enable, beginPermissionGesture: () => {
    if (snapshot().can_enable && !gesturePermission && env.Notification.permission === 'default') {
      try { gesturePermission = env.Notification.requestPermission(); }
      catch (failure) { update(status, `Notification permission could not be requested: ${failure.message ?? failure}`); }
    }
  }, reviewDiscardPending, discardPending, cancelDiscardPending: () => { pendingDiscardReview = null; repaint(); },
    reviewDisable, disable, cancelDisable: () => { disableReview = null; repaint(); },
    retryReceipts, snapshotJSON: () => JSON.stringify(snapshot()),
    queueTaskLink: task => { try { taskURL(task, env.location.origin); queuedTask = task; repaint(); } catch {} },
    takeTaskLink: () => {
      const task = queuedTask ?? (!taskLinkRead ? new URLSearchParams(env.location.search).get('task') : null);
      taskLinkRead = true; queuedTask = null;
      if (!task) return null;
      try {
        taskURL(task, env.location.origin);
        env.history.replaceState(null, '', taskURL(task, env.location.origin));
        return task;
      } catch { return null; }
    } };
}

export function browserEnvironment(window) {
  // Reading localStorage can itself fail in restricted browser contexts.
  let storage;
  try { storage = window.localStorage; } catch { storage = { getItem: () => null, setItem: () => {}, removeItem: () => {} }; }
  return { navigator: window.navigator, Notification: window.Notification, PushManager: window.PushManager,
    isSecureContext: window.isSecureContext, matchMedia: value => window.matchMedia(value),
    fetch: (path, options) => window.fetch(path, options), randomUUID: () => window.crypto.randomUUID(),
    storage, location: window.location, history: window.history };
}
