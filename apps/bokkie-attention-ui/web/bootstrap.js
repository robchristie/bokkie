import initialise, { WebHandle } from "./pkg/bokkie_attention_ui.js";

import { createPushSetup, browserEnvironment } from "./push-setup.js";

import { createHandoffBridge, showSelectableBrief } from "./handoff.js";
let handoffStorage;
try { handoffStorage = window.localStorage; }
catch { handoffStorage = { getItem: () => null, setItem: () => { throw Error("Local draft storage unavailable"); } }; }
window.__BOKKIE_HANDOFF = createHandoffBridge({
  location: window.location, history: window.history, storage: handoffStorage,
  clipboard: navigator.clipboard, showFallback: text => showSelectableBrief(document, text),
}, () => window.__BOKKIE_ATTENTION_HANDLE?.request_repaint());
window.addEventListener("popstate", () => {
  try { window.__BOKKIE_HANDOFF.queueLink(location.href); } catch {}
});
document.addEventListener("click", event => {
  const anchor = event.target.closest?.("a[href]");
  if (!anchor || event.defaultPrevented || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
  try { window.__BOKKIE_HANDOFF.queueLink(anchor.href); event.preventDefault(); } catch {}
});

window.__BOKKIE_PUSH = createPushSetup(browserEnvironment(window), () => {
  window.__BOKKIE_ATTENTION_HANDLE?.request_repaint();
});
void window.__BOKKIE_PUSH.refresh();
window.addEventListener("focus", () => void window.__BOKKIE_PUSH.refresh());
window.addEventListener("online", () => window.__BOKKIE_PUSH.retryReceipts());
navigator.serviceWorker?.addEventListener("message", event => {
  if (event.data?.type === "bokkie-open-task") window.__BOKKIE_PUSH.queueTaskLink(event.data.task_id);
});

await initialise();
const canvas = document.getElementById("bokkie-attention-canvas");
const handle = new WebHandle();
const appearance = new URLSearchParams(location.search).get("appearance") ?? "{}";
await handle.start_with_appearance(canvas, appearance);
document.getElementById("loading").remove();
canvas.dataset.bokkieReady = "true";
window.__BOKKIE_ATTENTION_HANDLE = handle;

// egui processes input on its next frame. Start browser permission in the actual
// trusted gesture, after checking that both pointer ends chose the enabled action.
let notificationPointer = null;
const notificationHit = event => {
  const snapshot = window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot();
  const node = snapshot?.ui_snapshot.nodes.find(node => node.id === "bokkie.notifications.enable" && node.enabled);
  return node && event.clientX >= node.rect.min_x && event.clientX <= node.rect.max_x
    && event.clientY >= node.rect.min_y && event.clientY <= node.rect.max_y;
};
canvas.addEventListener("pointerdown", event => {
  notificationPointer = event.isTrusted && notificationHit(event) ? event.pointerId : null;
}, true);
canvas.addEventListener("pointerup", event => {
  if (event.isTrusted && notificationPointer === event.pointerId && notificationHit(event)) {
    window.__BOKKIE_PUSH.beginPermissionGesture();
  }
  notificationPointer = null;
}, true);
canvas.addEventListener("pointercancel", () => { notificationPointer = null; }, true);

// Clipboard permission is requested in the same trusted pointer gesture as the
// enabled Copy action. The Rust view records only the browser's actual outcome.
let handoffPointer = null;
const handoffHit = event => {
  const node = window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot()?.ui_snapshot.nodes.find(node => node.id === "bokkie.handoff.copy" && node.enabled);
  return node && event.clientX >= node.rect.min_x && event.clientX <= node.rect.max_x && event.clientY >= node.rect.min_y && event.clientY <= node.rect.max_y;
};
canvas.addEventListener("pointerdown", event => { handoffPointer = event.isTrusted && handoffHit(event) ? event.pointerId : null; }, true);
canvas.addEventListener("pointerup", event => {
  if (event.isTrusted && handoffPointer === event.pointerId && handoffHit(event)) window.__BOKKIE_HANDOFF.beginCopyGesture();
  handoffPointer = null;
}, true);
canvas.addEventListener("pointercancel", () => { handoffPointer = null; }, true);
