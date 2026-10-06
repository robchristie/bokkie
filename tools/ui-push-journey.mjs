/** Finite synthetic Web Push source journey; no provider enrolment or external push send. */
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash, createECDH, randomUUID } from 'node:crypto';
import { chromium } from 'playwright';
import { captureSettled } from './ui-capture-settling.mjs';

const evidence = resolve(process.env.BOKKIE_PUSH_EVIDENCE ?? '.ui-qualification-runtime/push');
await mkdir(evidence, { recursive: true });
const root = join(evidence, `state-${randomUUID()}`), profile = join(evidence, 'synthetic-profile.json');
const syntheticBroker = join(evidence, 'synthetic-push-broker.py');
await writeFile(syntheticBroker, (await readFile('tests/fixtures/reminder_broker.py', 'utf8')).replace("'name': 'Review today’s priorities'", "'name': 'Bokkie TEST: Review today’s priorities'"));
await writeFile(profile, JSON.stringify({ broker: syntheticBroker,
  codex: '/usr/bin/true', bwrap: '/usr/bin/true', model: 'synthetic-reminder-peer', effort: 'medium',
  timezone: 'Australia/Adelaide', timeout_seconds: 15, max_context_bytes: 65536, max_output_bytes: 16384 }));
const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT ?? 'http://127.0.0.1:9341';
const report = { source: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  tracked_diff_sha256: createHash('sha256').update(execFileSync('git', ['diff', '--binary'])).digest('hex'),
  started_at: new Date().toISOString(), mode: 'synthetic subscription + transport, actual service worker + CDP push injection; no provider send or model',
  budget: { seconds: 300, model_dispatches: 0 }, checks: [], captures: [], errors: [], passed: false,
  limitations: ['Browser permission is granted by the fixture, with a synthetic subscription. This is not operator device enrolment.',
    'CDP injects decrypted JSON into the actual worker. Provider encryption and delivery require separate transport evidence.',
    'Notification click routing is invoked synthetically using actual notification data; no native OS tap or physical phone delivery is claimed.'] };
for (const file of ['target/debug/bokkie-conversation-fixture', 'apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm',
  'apps/bokkie-attention-ui/web/service-worker.js', 'apps/bokkie-attention-ui/web/push-worker.js', 'apps/bokkie-attention-ui/web/push-setup.js']) {
  report[file] = createHash('sha256').update(await readFile(file)).digest('hex');
}
let fixture, browser, displayServer, context, page, observer, cdp, origin, buffer = '', queued = [], pending = [], current;
const line = () => queued.length ? Promise.resolve(queued.shift()) : new Promise((ok, fail) => {
  const timer = setTimeout(() => fail(Error('Fixture response exceeded 20 seconds')), 20000);
  pending.push(value => { clearTimeout(timer); ok(value); });
});
async function control(command = {}) {
  fixture.stdin.write(JSON.stringify(command) + '\n'); const result = await line();
  if (result.error) throw Error(result.error); return result;
}
async function start(resume = false) {
  fixture = spawn('target/debug/bokkie-conversation-fixture', ['--root', root, '--ui-dir', resolve('apps/bokkie-attention-ui/web'),
    '--profile', profile, '--synthetic-push-reminders', ...(resume ? ['--resume','--port',new URL(origin).port] : [])], { stdio: ['pipe', 'pipe', 'pipe'] });
  fixture.stderr.on('data', bytes => report.errors.push(String(bytes)));
  fixture.stdout.on('data', bytes => {
    buffer += bytes;
    while (buffer.includes('\n')) {
      const at = buffer.indexOf('\n'), value = JSON.parse(buffer.slice(0, at)); buffer = buffer.slice(at + 1);
      if (pending.length) pending.shift()(value); else queued.push(value);
    }
  });
  const initial = await line(); origin = `http://${initial.address}`; report.initial ??= initial;
  if (!resume) await control({ now: Math.floor(Date.now() / 1000) });
}
async function stop() {
  if (fixture && fixture.exitCode == null) { const done = once(fixture, 'exit'); fixture.stdin.end('{"stop":true}\n'); await done; }
}
const get = async path => (await fetch(origin + path)).json();
async function fixtureMutation(path, body) {
 const bootstrap=await get('/bootstrap');const response=await fetch(origin+path,{method:'POST',headers:{'Content-Type':'application/json','Origin':origin,'X-Bokkie-Mutation-Token':bootstrap.mutation_token},body:JSON.stringify(body)});
 if(!response.ok)throw Error('Synthetic setup mutation failed: '+response.status); return response.json();
}

const snapshot = () => page.evaluate(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
const check = (condition, text) => { if (!condition) throw Error(text); report.checks.push(text); };
async function until(observation, condition, label, seconds = 15) {
  const deadline = Date.now() + seconds * 1000;
  let value;
  while (Date.now() < deadline) {
    value = await observation(); if (condition(value)) return value;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw Error(`Finite wait failed: ${label}`);
}
async function ready() {
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection === 'current', null, { timeout: 30000 });
}
async function reveal(id) {
  const node = await until(async () => {
    const nodes = (await snapshot()).ui_snapshot.nodes;
    const value = nodes.find(node => node.id === id);
    const composer = nodes.find(node => node.id === 'bokkie.conversation.text');
    const back = nodes.find(node => node.id === 'bokkie.conversation.panel-back');
    const notificationPanel = id.startsWith('bokkie.notifications.');
    const bottom = notificationPanel && composer ? composer.rect.min_y - 24 : page.viewportSize().height;
    const top = notificationPanel && back ? back.rect.max_y + 6 : 0;
    if (value && value.rect.max_y <= bottom && value.rect.min_y >= top) return value;
    await page.mouse.move(page.viewportSize().width * 0.8, 350); await page.mouse.wheel(0, value && value.rect.min_y < top ? -250 : 250); return null;
  }, node => !!node, id);
  return node;
}
async function click(id) {
  const node = await reveal(id);
  check(node.enabled, `Enabled ${id}`);
  await page.mouse.click((node.rect.min_x + node.rect.max_x) / 2, (node.rect.min_y + node.rect.max_y) / 2);
  await page.waitForTimeout(150);
}
async function clickAction(action) {
  const node=(await snapshot()).ui_snapshot.nodes.find(node=>node.actions?.includes(action));
  if(!node)throw Error('Missing action: '+action); await click(node.id);
}
async function send(text) {
  await click('bokkie.conversation.text');
  check(await page.evaluate(text => {
    const input = document.activeElement;
    if (!(input instanceof HTMLInputElement)) return false;
    input.value = text; input.dispatchEvent(new InputEvent('input', { bubbles: true, data: text, inputType: 'insertText' })); return true;
  }, text), 'Composer receives text through its focused browser input');
  await page.waitForTimeout(100);
  const posted = page.waitForRequest(request => request.method() === 'POST' && request.url().endsWith('/conversations/turn'));
  await click('bokkie.conversation.send'); const request = (await posted).postDataJSON();
  current = await until(() => get(`/conversations/${request.conversation_id}`), view => !view.busy
    && view.messages.some(message => message.request_id === request.command_id && message.role === 'assistant'), 'synthetic turn');
  await page.waitForTimeout(1000); return current;
}
async function confirm() {
  const previous = current?.receipt?.command_id;
  await click('bokkie.conversation.confirm');
  current = await until(() => get(`/conversations/${current.id}`), view => !!view.receipt && view.receipt.command_id !== previous, 'saved configuration receipt');
  await page.waitForTimeout(300); return current;
}
async function capture(name) {
  await page.bringToFront();
  const settled = await captureSettled(page, snapshot);
  await writeFile(join(evidence, name + '.png'), settled.png);
  const luminance = Number(execFileSync('identify', ['-format', '%[fx:mean]', join(evidence, name + '.png')], {encoding:'utf8'}));
  check(Number.isFinite(luminance) && luminance > 0.005, 'Application framebuffer contains visible content');
  await writeFile(join(evidence, name + '.json'), JSON.stringify(settled.state, (_k, value) => typeof value === 'bigint' ? value.toString() : value, 2));
  const targets = JSON.parse(execFileSync('lantern', ['targets', '--endpoint', endpoint, '--json'], { encoding: 'utf8' }));
  const pageSession = await context.newCDPSession(page);
  const ownId = (await pageSession.send('Target.getTargetInfo')).targetInfo.targetId;
  await pageSession.detach();
  const target = (targets.targets ?? targets.result?.targets ?? []).find(target => target.type === 'page' && target.id === ownId);
  check(!!target, 'Lantern identifies the application page separately from the CDP observation tab');
  const layout = JSON.parse(execFileSync('lantern', ['layout', '--container-selector', 'body', '--endpoint', endpoint, '--target-id', target.id, '--json'], { encoding: 'utf8' }));
  check(layout.ok, 'Lantern layout collection completed');
  await writeFile(join(evidence, name + '-layout.json'), JSON.stringify(layout, null, 2));
  report.captures.push({ name, viewport: page.viewportSize(), settling: settled.settling,
    expectations: 'Readable current state and notification actions, bounded detail scrolling, visible composer, no overlap or clipped controls. Pixels require the owner to open and judge.' });
}
async function widths(name) {
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport); await page.waitForTimeout(250);
    if(name==='enrolment-recovery') await reveal('bokkie.notifications.pending-discard-confirm');
    await capture(name + '-' + viewport.width);
    const composer = (await snapshot()).ui_snapshot.nodes.find(node => node.id === 'bokkie.conversation.text');
    if (!['delivery-attention','recovery-confirmation'].includes(name)) check(composer && composer.rect.min_y >= 0 && composer.rect.max_y < viewport.height, `Composer remains visible on Home at ${viewport.width}`);
    else check((await snapshot()).ui_snapshot.nodes.some(node=>node.enabled && node.actions?.some(action=>['reconcile_notification','confirm_lifecycle_action'].includes(action))), `Delivery recovery action is reachable at ${viewport.width}`);
  }
  await page.setViewportSize({ width: 1440, height: 900 }); await page.waitForTimeout(250);
}
async function worker() {
  return until(async () => context.serviceWorkers().find(worker => worker.url() === origin + '/ui/service-worker.js'), value => !!value, 'actual service worker');
}
async function notificationState() {
  const currentWorker = await worker();
  return currentWorker.evaluate(async () => {
    const notifications = await self.registration.getNotifications();
    const records = await new Promise((resolve, reject) => {
      const open = indexedDB.open('bokkie-push-receipts-v1', 1);
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result, request = db.transaction('receipts').objectStore('receipts').getAll();
        request.onsuccess = () => { resolve(request.result.map(({ receipt_token, ...row }) => row)); db.close(); };
        request.onerror = () => reject(request.error);
      };
    });
    return { notifications: notifications.map(notification => ({ title: notification.title, body: notification.body,
      tag: notification.tag, renotify: notification.renotify, data: { id: notification.data.id, task_id: notification.data.task_id } })), records };
  });
}
const deadline = setTimeout(() => { fixture?.kill('SIGTERM'); void browser?.close(); }, report.budget.seconds * 1000);
try {
  await start();
  // Full Chromium supports notification permission; the headless shell denies it.
  const sysroot = resolve(process.env.BOKKIE_UI_SYSROOT ?? '/nvme/development/polyorama/.tools/sysroot');
  const graphicsEnv = { ...process.env, LD_LIBRARY_PATH: `${join(sysroot, 'usr/lib')}:${process.env.LD_LIBRARY_PATH ?? ''}` };
  displayServer = spawn('bwrap', ['--die-with-parent', '--ro-bind', '/', '/', '--bind', '/tmp', '/tmp', '--ro-bind', '/usr/bin', '/opt', '--ro-bind', join(sysroot, 'usr/bin'), '/usr/bin', '--dev-bind', '/dev', '/dev', '--proc', '/proc', join(sysroot, 'usr/bin/Xvfb'), '-displayfd', '1', '-screen', '0', '1440x900x24', '-nolisten', 'tcp'], { env: graphicsEnv, stdio: ['ignore','pipe','pipe'] });
  displayServer.stderr.on('data', bytes => {report.display_errors = (report.display_errors ?? '') + String(bytes);});
  const displayNumber = await new Promise((ok, fail) => {const timer=setTimeout(()=>fail(Error('Owned display readiness exceeded 5 seconds')),5000);displayServer.stdout.once('data',data=>{clearTimeout(timer);ok(String(data).trim())});displayServer.once('error',fail);});
  check(/^\d+$/.test(displayNumber), 'Fixture-owned Xvfb display is ready without TCP exposure');
  graphicsEnv.DISPLAY = ':' + displayNumber; report.graphics = 'headed full Chromium on fixture-owned Xvfb and Vulkan';
  browser = await chromium.launch({ headless: false, channel: 'chromium', env: graphicsEnv,
    args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan', '--use-angle=vulkan', '--disable-vulkan-surface', `--remote-debugging-port=${new URL(endpoint).port}`] });
  report.browser = browser.version(); report.browser_channel = 'chromium'; context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await context.grantPermissions(['notifications'], { origin });
  const ecdh = createECDH('prime256v1'); ecdh.setPrivateKey(Buffer.alloc(32, 9));
  await context.addInitScript(({ endpoint, p256dh, auth }) => {
    let subscription;
    window.__BOKKIE_SYNTHETIC_SUBSCRIPTIONS = 0;
    PushManager.prototype.getSubscription = async () => subscription ?? null;
    PushManager.prototype.subscribe = async function(options) {
      window.__BOKKIE_SYNTHETIC_SUBSCRIPTIONS++;
      subscription = { options, toJSON: () => ({ endpoint, keys: { p256dh, auth } }) }; return subscription;
    };
  }, { endpoint: 'https://fcm.googleapis.com/fcm/send/bokkie-fixture-only',
    p256dh: ecdh.getPublicKey().toString('base64url'), auth: Buffer.alloc(16, 9).toString('base64url') });
  observer = await context.newPage(); await observer.goto('about:blank'); cdp = await context.newCDPSession(observer);
  const registrations = new Map();
  cdp.on('ServiceWorker.workerRegistrationUpdated', ({ registrations: values }) => {
    for (const value of values) registrations.set(value.registrationId, value);
  });
  await cdp.send('ServiceWorker.enable');
  page = await context.newPage(); page.on('pageerror', error => report.errors.push(String(error)));
  page.on('console', message => {if(message.type()==='error') {if(report.injecting_registration_failure && message.location().url===origin+'/notifications/push/register' && /503|409/.test(message.text())) (report.expected_registration_errors??=[]).push(message.text());else report.errors.push(message.text());}});
  await page.bringToFront();
  await page.goto(origin + '/ui/'); await ready();
  await click('bokkie.home.notifications'); await widths('notification-setup');
  // Reproduce the reviewed stale-pending case without touching a provider.
  report.injecting_registration_failure = true;
  await page.route('**/notifications/push/register', route => route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{message:'Synthetic registration failed before reaching Bokkie'}})}), {times:1});
  await click('bokkie.notifications.enable');
  await until(()=>page.evaluate(()=>JSON.parse(window.__BOKKIE_PUSH.snapshotJSON())),state=>state.pending && !state.busy,'retained failed enrolment');
  report.injecting_registration_failure = false;
  const initialSetup=await get('/notifications/push');
  const other=await fixtureMutation('/notifications/push/register',{command_id:randomUUID(),configuration_revision:initialSetup.configuration_revision,label:'Synthetic other browser',endpoint:'https://fcm.googleapis.com/fcm/send/bokkie-other-fixture-only',keys:{p256dh:ecdh.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,9).toString('base64url')}});
  await fixtureMutation('/notifications/push/disable',{command_id:randomUUID(),configuration_revision:other.configuration_revision});
  await page.reload();await ready();await click('bokkie.home.notifications');
  await until(()=>page.evaluate(()=>JSON.parse(window.__BOKKIE_PUSH.snapshotJSON())),state=>state.configuration_revision===2 && state.pending,'reloaded obsolete pending enrolment');
  report.injecting_registration_failure = true; await click('bokkie.notifications.enable');
  await until(()=>page.evaluate(()=>JSON.parse(window.__BOKKIE_PUSH.snapshotJSON())),state=>state.pending && !state.busy && !!state.error,'exact obsolete enrolment rejection');
  report.injecting_registration_failure = false;
  await widths('stale-enrolment'); await click('bokkie.notifications.pending-discard-review'); await widths('enrolment-recovery');
  await click('bokkie.notifications.pending-discard-cancel');
  check(await page.evaluate(()=>JSON.parse(window.__BOKKIE_PUSH.snapshotJSON()).pending),'Cancelling recovery preserves the exact pending request');
  await click('bokkie.notifications.pending-discard-review'); await click('bokkie.notifications.pending-discard-confirm');
  await until(()=>page.evaluate(()=>JSON.parse(window.__BOKKIE_PUSH.snapshotJSON())),state=>!state.pending && state.ready && state.configuration_revision===2,'explicit local discard and fresh settings');
  const unchangedSetup=await get('/notifications/push');check(unchangedSetup.configuration_revision===2 && !unchangedSetup.device.active,'Discard changes no Bokkie device or history');
  await click('bokkie.notifications.enable');
  const setup = await until(() => get('/notifications/push'), value => value.device?.active, 'explicit synthetic device enrolment');
  check(await page.evaluate(() => window.__BOKKIE_SYNTHETIC_SUBSCRIPTIONS) === 1, 'One physical enrolment action creates one synthetic subscription without contacting a provider');
  report.device = setup.device; await widths('notification-enabled'); await click('bokkie.conversation.panel-back');
  await send('Every weekday at 9, remind me to review today’s priorities.');
  const draft = await send('9 am, please.');
  check(draft.review.preview.occurrences.length === 5, 'Review shows five concrete reminder dates');
  check(draft.review.preview.definition.capability === 'reminder', 'Review retains the reminder capability');
  const taskId = draft.task.id, due = draft.review.preview.occurrences[0];
  await send('Yes, go ahead.'); check(!current.task.active, 'Conversational assent leaves the exact draft inactive');
  await widths('reminder-review'); await confirm();
  const before = await control(); await page.close(); page = null;
  check(context.pages().every(page => !page.url().startsWith(origin + '/ui/')), 'The Bokkie page is closed; only a blank observation tab remains');
  const completed = await control({ now: due, reminder_tick: true, delivery: 'accepted', push_payload: true });
  const payload = completed.push_payload;
  check(payload?.task_id === taskId, 'Push payload pins the exact saved task');
  check(completed.model_calls === before.model_calls, 'Due execution and push admission make no model dispatches');
  check(completed.details[0].runs.filter(run => run.result).length === 1, 'One due occurrence saves one result');
  check(completed.details[0].runs.find(run => run.delivery).delivery.status === 'accepted_by_push_service', 'History records push-service acceptance separately from device evidence');
  const registration = await until(async () => [...registrations.values()].find(value => value.scopeURL === origin + '/ui/' && !value.isDeleted), value => !!value, 'scope-bound service worker registration');
  await context.setOffline(true);
  await cdp.send('ServiceWorker.deliverPushMessage', { origin, registrationId: registration.registrationId, data: JSON.stringify(payload) });
  const offline = await until(notificationState, value => value.notifications.some(notification => notification.tag === payload.id), 'offline closed-page notification');
  report.offline_worker = offline;
  check(offline.notifications.find(notification => notification.tag === payload.id).body === payload.body, 'The offline worker displays the self-contained reminder text');
  check(offline.records.find(row => row.id === payload.id).state === 'displayed', 'Successful display is retained in actual IndexedDB');
  check(offline.records.find(row => row.id === payload.id).reported !== 'displayed', 'Offline display leaves an unreported durable receipt');
  await cdp.send('ServiceWorker.deliverPushMessage', { origin, registrationId: registration.registrationId, data: JSON.stringify(payload) });
  check((await notificationState()).notifications.filter(notification => notification.tag === payload.id).length === 1, 'Duplicate CDP push keeps one notification tag');
  await context.setOffline(false);
  page = await context.newPage(); page.on('pageerror', error => report.errors.push(String(error)));
  page.on('console', message => {if(message.type()==='error') {if(report.injecting_registration_failure && message.location().url===origin+'/notifications/push/register' && /503|409/.test(message.text())) (report.expected_registration_errors??=[]).push(message.text());else report.errors.push(message.text());}});
  await page.bringToFront();
  await page.goto(origin + '/ui/'); await ready();
  const reported = await until(() => get(`/tasks/managed/${taskId}`), value => value.runs.some(run => run.delivery?.push?.device_status === 'displayed'), 'queued device receipt after app activity');
  check(reported.runs.find(run => run.delivery).delivery.status === 'accepted_by_push_service', 'A device report does not replace push-service acceptance');
  await capture('reopened-home');
  const selected = page.waitForRequest(request => request.method() === 'POST' && request.url().endsWith('/conversations/select')).then(request => ({request}), error => ({error}));
  // Chromium exposes no native notification tap through CDP. Invoke the same
  // production click function with real notification data and a labelled
  // WindowClient adapter; app message handling, receipt storage and HTTP are real.
  await page.evaluate(async () => {
    const registration = await navigator.serviceWorker.ready;
    const notification = (await registration.getNotifications())[0];
    const { createPushWorker, createReceiptStore } = await import('/ui/push-worker.js');
    const scope = { registration, location, fetch: (...args) => fetch(...args), clients: {
      matchAll: async () => [{ url: location.href,
        postMessage: data => navigator.serviceWorker.dispatchEvent(new MessageEvent('message', { data })),
        focus: async () => {} }], openWindow: async () => { throw Error('Unexpected synthetic navigation'); }
    } };
    notification.close(); await createPushWorker(scope, createReceiptStore(indexedDB)).click(notification.data);
  });
  const selectedResult = await selected; if (selectedResult.error) throw selectedResult.error;
  const select = selectedResult.request.postDataJSON();
  check(select.task_id === taskId, 'Production click function with a synthetic WindowClient reaches the exact existing task-selection API');
  await until(() => get(`/conversations/${select.conversation_id}`), value => value.task?.id === taskId, 'exact task context');
  await page.waitForTimeout(600); await widths('task-result-and-device-report');
  check((await get(`/tasks/managed/${taskId}`)).runs.some(run => run.delivery?.push?.device_status === 'opened'), 'History distinguishes opening from display');
  check((await control()).model_calls === completed.model_calls, 'Receipt replay and notification opening do not run a model');
  const original = JSON.stringify((await get(`/tasks/managed/${taskId}`)).active.definition.trigger);
  await send('Change it to 10 am on weekdays.');
  check(JSON.stringify(current.task.active.definition.trigger) === original, 'Schedule change stays inactive until its exact confirmation');
  await confirm(); await send('Pause this reminder.'); await confirm(); check(current.task.status==='paused','Pause preserves task and conversation identity');
  await send('Resume this reminder.'); await confirm(); check(current.task.status==='active' && current.task.id===taskId,'Resume preserves the same task with future timing');
  const resumed=await control(), nextDue=resumed.details.find(task=>task.id===taskId).next_wake_at;
  const rejected=await control({now:nextDue,reminder_tick:true,delivery:'retryable_rejection'});
  const retry=rejected.details.find(task=>task.id===taskId).runs.find(run=>run.delivery?.status==='retry_scheduled').delivery;
  const nextRetry=retry.next_retry_at; const oldOrigin=origin; await stop(); await start(true);
  check(origin===oldOrigin,'Restart preserves the exact browser origin and device registration');
  const accepted=await control({now:nextRetry,delivery:'accepted'});
  check(accepted.details.find(task=>task.id===taskId).runs.some(run=>run.delivery?.id===retry.id && run.delivery.status==='accepted_by_push_service'),'Restart safely retries proved nonacceptance under the same delivery identity');
  const dueAgain=accepted.details.find(task=>task.id===taskId).next_wake_at;
  await control({now:dueAgain,reminder_tick:true,delivery:'crash_after_dispatch'}); await stop(); await start(true);
  const uncertain=await control({now:dueAgain+31});
  const uncertainDelivery=uncertain.details.find(task=>task.id===taskId).runs.find(run=>run.delivery?.status==='uncertain').delivery;
  await control({delivery:'accepted'});
  check((await get(`/tasks/managed/${taskId}`)).runs.some(run=>run.delivery?.id===uncertainDelivery.id && run.delivery.status==='uncertain'),'Restart after possible dispatch never automatically resends');
  await page.goto(origin+'/ui/'); await ready(); await click('bokkie.collection.attention'); await click('bokkie.inbox-row.'+uncertainDelivery.id);
  await widths('delivery-attention'); await clickAction('reconcile_notification'); await widths('recovery-confirmation'); await clickAction('confirm_lifecycle_action');
  await until(()=>get(`/tasks/managed/${taskId}`),value=>value.runs.some(run=>run.delivery?.id===uncertainDelivery.id && run.delivery.status==='reconciled'),'explicit resolve without resending');
  const final=await control(); check(final.catalogue.items.filter(task=>task.kind==='managed').length===1,'The complete journey retains one task and no duplicate schedules');
  check(final.model_calls===uncertain.model_calls,'Delivery recovery and polling make no model dispatches');
  report.broker_dispatches = final.model_calls; report.model_dispatches = 0; report.synthetic_push_events=2; report.external_push_sends=0;
  report.passed = report.errors.length === 0;
  if (!report.passed) throw Error('Unexpected runtime errors retained');
} catch (error) {
  report.error = String(error); report.failure_stack = error.stack; console.error(report.failure_stack); process.exitCode = 1;
  if (page) await page.screenshot({ path: join(evidence, 'failure.png') }).catch(() => {});
} finally {
  clearTimeout(deadline); if (browser) await browser.close(); await stop();
  if (displayServer && displayServer.exitCode == null) {const done=once(displayServer,'exit'); displayServer.kill('SIGTERM'); await done;}
  report.finished_at = new Date().toISOString();
  await writeFile(join(evidence, 'qualification.json'), JSON.stringify(report, (_key, value) => typeof value === 'bigint' ? value.toString() : value, 2));
  console.log(JSON.stringify({ passed: report.passed, error: report.error, checks: report.checks.length, evidence }));
}
