// Chromium module-worker script requests omit HTTP authentication credentials.
// Classic script and importScripts requests retain same-origin authentication.
importScripts('./push-worker-core.js');
const { createPushWorker, createReceiptStore } = self.BokkiePushWorker;

const worker = createPushWorker(self, createReceiptStore(self.indexedDB));
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim().then(() => worker.flush())));
self.addEventListener('push', event => {
  let payload;
  try { payload = event.data?.json(); } catch { return; }
  event.waitUntil(worker.push(payload));
});
self.addEventListener('notificationclick', event => {
  event.notification.close();
  event.waitUntil(worker.click(event.notification.data));
});
self.addEventListener('message', event => {
  if (event.data?.type === 'bokkie-retry-push-receipts'
      && event.source?.url && new URL(event.source.url).origin === self.location.origin) {
    event.waitUntil(worker.flush());
  }
});
// There is deliberately no fetch handler: private APIs and transcripts always use the origin.
