/** Push receipt state lives here; no private API responses or mutation tokens are cached. */
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const DELIVERY = /^delivery-[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const TOKEN = /^[a-f0-9]{64}$/;
const MAX_RECEIPTS_PER_EVENT = 16;

export function validatePayload(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
      || Object.keys(value).some(key => !['version', 'id', 'task_id', 'title', 'body', 'receipt_token', 'expires_at'].includes(key))
      || value.version !== 1 || typeof value.id !== 'string' || !DELIVERY.test(value.id)
      || typeof value.task_id !== 'string' || !UUID.test(value.task_id.replace(/^task-/, '')) || !value.task_id.startsWith('task-')
      || typeof value.title !== 'string' || !value.title.trim() || Array.from(value.title).length > 200
      || typeof value.body !== 'string' || Array.from(value.body).length > 2000
      || typeof value.receipt_token !== 'string' || !TOKEN.test(value.receipt_token)
      || new TextEncoder().encode(JSON.stringify(value)).length > 3200
      || !Number.isSafeInteger(value.expires_at) || value.expires_at < 1) {
    throw Error('Invalid Bokkie push payload');
  }
  return { version: 1, id: value.id, task_id: value.task_id, title: value.title,
    body: value.body, receipt_token: value.receipt_token, expires_at: value.expires_at };
}

export function taskURL(taskId, origin) {
  if (typeof taskId !== 'string' || !taskId.startsWith('task-') || !UUID.test(taskId.slice(5))) throw Error('Invalid task identity');
  const url = new URL(`/ui/?task=${encodeURIComponent(taskId)}`, origin);
  if (url.origin !== origin) throw Error('Invalid Bokkie origin');
  return url.href;
}

// Expiry describes an unshown notification; it cannot erase evidence of display or opening.
export function mergeReceipt(previous, next) {
  if (!previous) return { ...next, reported: null };
  if (previous.receipt_token !== next.receipt_token || previous.task_id !== next.task_id
      || previous.expires_at !== next.expires_at) throw Error('Push identity changed');
  if (previous.state === 'opened' || previous.state === next.state
      || (next.state === 'expired' && previous.state === 'displayed')) return previous;
  if (previous.state === 'expired') return previous;
  return { ...previous, state: next.state };
}

export function createReceiptStore(indexedDB) {
  let opening;
  const database = () => opening ??= new Promise((resolve, reject) => {
    const request = indexedDB.open('bokkie-push-receipts-v1', 1);
    request.onupgradeneeded = () => request.result.createObjectStore('receipts', { keyPath: 'id' });
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? Error('Push receipt storage unavailable'));
    request.onblocked = () => reject(Error('Push receipt storage blocked'));
  });
  const transaction = async (mode, operation) => {
    const db = await database();
    return new Promise((resolve, reject) => {
      const tx = db.transaction('receipts', mode), store = tx.objectStore('receipts');
      let result;
      operation(store, value => { result = value; });
      tx.oncomplete = () => resolve(result);
      tx.onabort = tx.onerror = () => reject(tx.error ?? Error('Push receipt transaction failed'));
    });
  };
  return {
    get: id => transaction('readonly', (store, done) => {
      const request = store.get(id); request.onsuccess = () => done(request.result);
    }),
    save: row => transaction('readwrite', (store, done) => {
      const request = store.get(row.id);
      request.onsuccess = () => { const value = mergeReceipt(request.result, row); store.put(value); done(value); };
    }),
    pending: () => transaction('readwrite', (store, done) => {
      const rows = [], request = store.openCursor(), now = Math.floor(Date.now() / 1000);
      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) return done(rows);
        const row = cursor.value;
        // Keep a bounded deduplication window, including failed receipt reports.
        if (row.expires_at + 86400 < now) cursor.delete();
        else if (row.reported !== row.state && rows.length < MAX_RECEIPTS_PER_EVENT) rows.push(row);
        cursor.continue();
      };
    }),
    reported: row => transaction('readwrite', store => {
      const request = store.get(row.id);
      request.onsuccess = () => {
        const current = request.result;
        if (current && current.receipt_token === row.receipt_token) {
          current.reported = row.state; store.put(current);
        }
      };
    }),
  };
}

export function createPushWorker(scope, store, now = () => Math.floor(Date.now() / 1000)) {
  // Serialise this worker's events so a duplicate does not race another display.
  let work = Promise.resolve();
  const serial = job => { const result = work.then(job, job); work = result.catch(() => {}); return result; };
  const request = (path, options = {}) => scope.fetch(path, { credentials: 'same-origin', cache: 'no-store',
    redirect: 'error', ...options, signal: AbortSignal.timeout(6000) });
  async function flush(fallback) {
    let rows;
    try { rows = await store.pending(); } catch { rows = []; }
    if (fallback && fallback.reported !== fallback.state && !rows.some(row => row.id === fallback.id)) rows.push(fallback);
    if (!rows.length) return;
    const deadline = Date.now() + 8000;
    // A fresh process token is kept only on this stack, even after an earlier failed report.
    let bootstrap;
    try {
      const response = await request('/bootstrap');
      if (!response.ok) return;
      bootstrap = await response.json();
      if (!/^[a-f0-9]{64}$/i.test(bootstrap.mutation_token)) return;
    } catch { return; }
    for (const row of rows.slice(0, MAX_RECEIPTS_PER_EVENT)) {
      if (Date.now() >= deadline) break;
      try {
        const response = await request('/notifications/push/receipts', { method: 'POST',
          headers: { 'Content-Type': 'application/json', 'X-Bokkie-Mutation-Token': bootstrap.mutation_token },
          body: JSON.stringify({ id: row.id, receipt_token: row.receipt_token, state: row.state }) });
        if (!response.ok) break;
        await store.reported(row);
      } catch { break; /* Retry on the next app or worker event; never poll indefinitely. */ }
    }
  }
  async function retain(payload, state) {
    const row = { id: payload.id, task_id: payload.task_id, expires_at: payload.expires_at,
      receipt_token: payload.receipt_token, state };
    try { return await store.save(row); } catch { return row; }
  }
  return {
    push: value => serial(async () => {
      let payload;
      try { payload = validatePayload(value); } catch { return; }
      let previous;
      try { previous = await store.get(payload.id); } catch { /* Display remains available without storage. */ }
      if (previous && (previous.receipt_token !== payload.receipt_token || previous.task_id !== payload.task_id
          || previous.expires_at !== payload.expires_at)) return;
      if (previous?.state === 'displayed' || previous?.state === 'opened' || previous?.state === 'expired') {
        await flush(); return;
      }
      if (payload.expires_at <= now()) {
        await flush(await retain(payload, 'expired')); return;
      }
      const notifications = await scope.registration.getNotifications({ tag: payload.id }).catch(() => []);
      if (!notifications.length) {
        await scope.registration.showNotification(`Bokkie · ${payload.title}`, { body: payload.body,
          icon: '/ui/icons/bokkie-192.png', badge: '/ui/icons/bokkie-badge.png', tag: payload.id,
          renotify: false, data: payload });
      }
      // Successful showNotification resolves before any API call or receipt write.
      await flush(await retain(payload, 'displayed'));
    }),
    click: value => serial(async () => {
      let payload;
      try { payload = validatePayload(value); } catch { return; }
      const url = taskURL(payload.task_id, scope.location.origin);
      const clients = await scope.clients.matchAll({ type: 'window', includeUncontrolled: true });
      const client = clients.find(candidate => {
        const candidateURL = new URL(candidate.url);
        return candidateURL.origin === scope.location.origin && candidateURL.pathname.startsWith('/ui/');
      });
      if (client) { client.postMessage({ type: 'bokkie-open-task', task_id: payload.task_id }); await client.focus(); }
      else if (!await scope.clients.openWindow(url)) return;
      await flush(await retain(payload, 'opened'));
    }),
    flush: () => serial(() => flush()),
  };
}
