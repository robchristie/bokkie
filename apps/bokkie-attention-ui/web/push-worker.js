// Keep the page/module API while classic service workers load the same implementation.
import './push-worker-core.js';
export const { validatePayload, taskURL, mergeReceipt, createReceiptStore, createPushWorker } = globalThis.BokkiePushWorker;
