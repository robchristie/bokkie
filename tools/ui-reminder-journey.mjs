/** Finite synthetic reminder journey through the actual Polyorama interface. */
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID, createHash } from 'node:crypto';
import { chromium } from 'playwright';
import { captureSettled } from './ui-capture-settling.mjs';

const liveModel = process.argv.includes('--live-model');
const evidence = resolve(process.env.BOKKIE_REMINDER_EVIDENCE ?? `.ui-qualification-runtime/reminders${liveModel ? '-live' : ''}`);
await mkdir(evidence, { recursive: true });
const fixtureRoot = join(evidence, `state-${randomUUID()}`);
const profile = liveModel ? process.env.BOKKIE_REMINDER_PROFILE : join(evidence, 'synthetic-profile.json');
if (!profile) throw Error('Live interpretation requires an explicit private BOKKIE_REMINDER_PROFILE');
if (!liveModel) await writeFile(profile, JSON.stringify({
  broker: resolve('tests/fixtures/reminder_broker.py'), codex: '/usr/bin/true', bwrap: '/usr/bin/true',
  model: 'synthetic-reminder-peer', effort: 'medium', timezone: 'Australia/Adelaide',
  timeout_seconds: 15, max_context_bytes: 65536, max_output_bytes: 16384,
}));
const report = { source: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  tracked_diff_sha256: createHash('sha256').update(execFileSync('git', ['diff', '--binary'])).digest('hex'),
  started_at: new Date().toISOString(),
  mode: liveModel ? 'live-model; synthetic transport and recipients; no external mail' : 'synthetic-peer-and-transport; no live model or external mail',
  budget: { model_dispatches: liveModel ? 16 : 0, seconds: liveModel ? 900 : 300 }, checks: [], captures: [], errors: [], passed: false };
for (const file of ['target/debug/bokkie-conversation-fixture', 'apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm']) {
  report[file] = createHash('sha256').update(await readFile(file)).digest('hex');
}
const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT ?? 'http://127.0.0.1:9339';
let fixture, browser, page, origin, current, buffer = '', queued = [], pending = [];
const line = () => queued.length ? Promise.resolve(queued.shift()) : new Promise((ok, fail) => {
  const timer = setTimeout(() => fail(Error('Fixture response exceeded 20 seconds')), 20000);
  pending.push(value => { clearTimeout(timer); ok(value); });
});
async function start(resume = false) {
  fixture = spawn('target/debug/bokkie-conversation-fixture', ['--root', fixtureRoot, '--ui-dir', resolve('apps/bokkie-attention-ui/web'), '--profile', profile, '--synthetic-reminders', ...(resume ? ['--resume'] : [])], { stdio: ['pipe', 'pipe', 'pipe'] });
  fixture.stderr.on('data', bytes => report.errors.push(String(bytes)));
  fixture.stdout.on('data', bytes => {
    buffer += bytes;
    while (buffer.includes('\n')) {
      const at = buffer.indexOf('\n'), value = JSON.parse(buffer.slice(0, at)); buffer = buffer.slice(at + 1);
      if (pending.length) pending.shift()(value); else queued.push(value);
    }
  });
  const initial = await line(); origin = `http://${initial.address}`; report.initial ??= initial;
}
async function stop() {
  if (fixture && fixture.exitCode == null) { const done = once(fixture, 'exit'); fixture.stdin.end('{"stop":true}\n'); await done; }
  fixture = undefined;
}
async function control(command = {}) {
  fixture.stdin.write(JSON.stringify(command) + '\n'); const result = await line();
  if (result.error) throw Error(result.error); return result;
}
const get = async path => (await fetch(origin + path)).json();
const snapshot = () => page.evaluate(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
function check(condition, text) { if (!condition) throw Error(text); report.checks.push(text); }
async function ready() { await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot().interaction.connection === 'current', null, { timeout: 30000 }); }
async function reveal(id) {
  for (let n = 0; n < 45; n++) {
    const state = await snapshot(), node = state.ui_snapshot.nodes.find(n => n.id === id), size = page.viewportSize();
    if (node && node.rect.min_y >= 0 && node.rect.max_y <= size.height - 8) return node;
    const x = node ? (node.rect.min_x + node.rect.max_x) / 2 : size.width / 2;
    await page.mouse.move(x, Math.min(size.height - 185, Math.max(170, size.height / 2)));
    await page.mouse.wheel(0, node && node.rect.min_y < 0 ? -350 : 350); await page.waitForTimeout(70);
  }
  throw Error(`Control is not reachable: ${id}`);
}
async function click(id) {
  const node = await reveal(id); check(node.enabled, `Enabled action ${id}`);
  await page.mouse.click((node.rect.min_x + node.rect.max_x) / 2, (node.rect.min_y + node.rect.max_y) / 2);
  await page.waitForTimeout(120);
}
async function clickAction(action) {
  // Polyorama presentation identities are separate from an action's stable key.
  await page.mouse.move(page.viewportSize().width * 0.7, 350);
  await page.mouse.wheel(0, -10000); await page.waitForTimeout(150);
  const state = await snapshot();
  const target = state.ui_snapshot.nodes.find(n => n.actions?.includes(action));
  if (!target) throw Error(`Action is not reachable: ${action}`);
  await click(target.id);
}
async function send(text) {
  if (liveModel && (await control()).model_calls + 2 > report.budget.model_dispatches) throw Error('Finite model budget exhausted');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes.find(n => n.id === 'bokkie.conversation.text')?.enabled, null, { timeout: 20000 });
  await click('bokkie.conversation.text');
  const entered = await page.evaluate(text => {
    const input = document.activeElement; if (!(input instanceof HTMLInputElement)) return false;
    input.value = text; input.dispatchEvent(new InputEvent('input', { bubbles: true, data: text, inputType: 'insertText' })); return true;
  }, text);
  check(entered, 'Physical composer has its browser text input'); await page.waitForTimeout(80);
  const posted = page.waitForRequest(r => r.method() === 'POST' && r.url().endsWith('/conversations/turn'));
  posted.catch(() => {});
  await click('bokkie.conversation.send'); const request = (await posted).postDataJSON();
  const started = Date.now();
  while (Date.now() - started < (liveModel ? 210000 : 10000)) {
    const view = await get(`/conversations/${request.conversation_id}`);
    if (!view.busy && view.messages.some(m => m.request_id === request.command_id && m.role === 'assistant')) {
      if (view.request_error) throw Error(view.request_error); current = view;
      await page.waitForTimeout(1200);
      return view;
    }
    await page.waitForTimeout(50);
  }
  throw Error('Synthetic conversation exceeded its finite bound');
}
async function confirm() {
  const before = current.receipt?.command_id; await click('bokkie.conversation.confirm');
  for (let n = 0; n < 50; n++) {
    const view = await get(`/conversations/${current.id}`);
    if (view.receipt && view.receipt.command_id !== before) { current = view; await page.waitForTimeout(300); return view; }
    await page.waitForTimeout(80);
  }
  throw Error('Confirmation receipt missing');
}
async function capture(name, focus) {
  if (focus) await reveal(focus);
  const settled = await captureSettled(page, async () => snapshot());
  await writeFile(join(evidence, name + '.png'), settled.png);
  await writeFile(join(evidence, name + '.json'), JSON.stringify(settled.state, (_k, v) => typeof v === 'bigint' ? v.toString() : v, 2));
  const targets = JSON.parse(execFileSync('lantern', ['targets', '--endpoint', endpoint, '--json'], { encoding: 'utf8' }));
  const pages = (targets.targets ?? targets.result?.targets ?? []).filter(t => t.type === 'page'); check(pages.length === 1, 'Owned inspection target is unambiguous');
  const layout = JSON.parse(execFileSync('lantern', ['layout', '--container-selector', 'body', '--endpoint', endpoint, '--target-id', pages[0].id, '--json'], { encoding: 'utf8' }));
  check(layout.ok, 'Lantern layout collection completed');
  await writeFile(join(evidence, name + '-layout.json'), JSON.stringify(layout, null, 2));
  report.captures.push({ name, viewport: page.viewportSize(), settling: settled.settling });
}
async function widths(name, focus) {
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport); await page.waitForTimeout(200);
    await capture(name + '-' + viewport.width, focus);
    const state = await snapshot(), composer = state.ui_snapshot.nodes.find(n => n.id === 'bokkie.conversation.text');
    if (composer) check(composer.rect.max_y < viewport.height && composer.rect.min_y >= 0, `Visible composer at ${viewport.width}`);
  }
  await page.setViewportSize({ width: 1440, height: 900 }); await page.waitForTimeout(200);
}
const deadline = setTimeout(() => { fixture?.kill('SIGTERM'); browser?.close(); }, report.budget.seconds * 1000);
try {
  await start();
  browser = await chromium.launch({ headless: true,
    env: { ...process.env, LD_LIBRARY_PATH: `${resolve(process.env.BOKKIE_UI_SYSROOT ?? '/nvme/development/polyorama/.tools/sysroot', 'usr/lib')}:${process.env.LD_LIBRARY_PATH ?? ''}` },
    args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan', '--use-angle=vulkan', '--disable-vulkan-surface', `--remote-debugging-port=${new URL(endpoint).port}`],
  });
  report.browser = browser.version(); page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.on('pageerror', error => report.errors.push(String(error)));
  await page.goto(origin + '/ui/'); await ready();
  await widths('home', 'bokkie.conversation.text');
  let view = await send('Every weekday at 9, remind me to review today’s priorities.');
  report.clarification = view;
  check(!view.task && !view.review, 'Ambiguous time is clarified without saving a schedule');
  view = await send('9 am, please.');
  report.draft = view;
  check(view.task.status === 'draft' && view.review.preview.occurrences.length === 5, 'Inactive draft previews five concrete occurrences');
  check(view.review.preview.definition.destination === 'fixture-recipient@example.invalid', 'Review contains the single configured synthetic destination');
  check(view.review.preview.definition.trigger.timezone === 'Australia/Adelaide', 'Review retains the default named zone');
  const taskId = view.task.id, firstDue = view.review.preview.occurrences[0];
  const originalTrigger = JSON.stringify(view.review.preview.definition.trigger);
  await widths('review', 'bokkie.conversation.confirm');
  await send('Yes, go ahead.');
  check((await get(`/tasks/managed/${taskId}`)).status === 'draft', 'Conversational assent does not activate the task');
  await confirm(); check(current.task.status === 'active', 'Exact explicit confirmation activates the reminder');
  await click('bokkie.schedule.today'); await widths('today', 'bokkie.conversation.catalogue.' + taskId);
  await click('bokkie.conversation.catalogue.' + taskId);
  const before = await control(); await page.close(); page = undefined;
  const due = await control({ now: firstDue, reminder_tick: true, delivery: 'retryable_rejection' });
  check(due.reminder_ran && due.details[0].runs.filter(r => r.result).length === 1, 'Reminder executes with browser closed and saves one result');
  const delivery = due.details[0].runs.find(r => r.result).delivery;
  check(delivery.status === 'retry_scheduled', 'Proved rejection schedules a separate notification retry');
  check(due.model_calls === before.model_calls, 'Execution and delivery make no model dispatches');
  await stop(); await start(true);
  const retry = await control({ now: delivery.next_retry_at, delivery: 'accepted' });
  check(retry.details[0].runs.find(r => r.result).delivery.id === delivery.id, 'Restart retry keeps the stable delivery identity');
  check(retry.details[0].runs.find(r => r.result).delivery.status === 'accepted_by_relay', 'Relay acceptance is distinct from occurrence completion');
  await control({ reminder_tick: true, delivery: 'accepted' });
  check((await control()).details[0].runs.filter(r => r.result).length === 1, 'Repeated ticks and acceptance do not duplicate occurrence or delivery');
  page = await browser.newPage({ viewport: { width: 1440, height: 900 } }); page.on('pageerror', error => report.errors.push(String(error)));
  await page.goto(origin + '/ui/'); await ready(); await click('bokkie.home.tasks');
  await click('bokkie.conversation.catalogue.' + taskId); await widths('result', 'bokkie.conversation.text');
  await send('Change it to 10 am on weekdays.');
  check(JSON.stringify(current.task.active.definition.trigger) === originalTrigger, 'Editing leaves the confirmed schedule unchanged until confirmation');
  await confirm(); await send('Pause this reminder.'); await confirm();
  check(current.task.status === 'paused', 'Pause keeps the same task identity');
  await send('Resume this reminder.'); await confirm();
  check(current.task.status === 'active' && current.task.id === taskId, 'Resume keeps the same task with a future occurrence');
  const resumed = await control(), nextDue = resumed.details[0].next_wake_at;
  await control({ now: nextDue, reminder_tick: true, delivery: 'crash_after_dispatch' });
  await stop(); await start(true); const uncertain = await control({ now: nextDue + 31 });
  const uncertainDelivery = uncertain.details[0].runs.find(r => r.delivery?.status === 'uncertain').delivery;
  const again = await control({ delivery: 'accepted' });
  check(again.details[0].runs.find(r => r.delivery?.id === uncertainDelivery.id).delivery.status === 'uncertain', 'Restart after possible dispatch blocks automatic resend');
  check(again.catalogue.items.length === 1, 'The complete journey retains exactly one managed task');
  await page.goto(origin + '/ui/'); await ready(); await click('bokkie.collection.attention');
  await click('bokkie.inbox-row.' + uncertainDelivery.id);
  await widths('delivery-attention');
  await clickAction('reconcile_notification'); await widths('recovery-confirmation');
  await clickAction('confirm_lifecycle_action'); await page.waitForTimeout(500);
  const resolved = await get('/operator/snapshot');
  check(resolved.obligations.find(o => o.id === uncertainDelivery.id).task.notification.status === 'reconciled', 'Explicit recovery records acknowledgement without resending');
  check((await control()).model_calls === again.model_calls, 'Attention recovery and unchanged polling make no model dispatches');
  await click('bokkie.home.tasks'); await click('bokkie.conversation.catalogue.' + taskId);
  check((await snapshot()).ui_snapshot.nodes.some(n => n.name?.includes('Review today')), 'Global Tasks returns from attention to the same reminder catalogue and saved context');
  report.broker_dispatches = (await control()).model_calls;
  report.model_dispatches = liveModel ? report.broker_dispatches : 0;
  report.passed = report.errors.length === 0;
  if (!report.passed) throw Error('Unexpected runtime errors retained');
} catch (error) {
  report.error = String(error); process.exitCode = 1;
  if (page) await page.screenshot({ path: join(evidence, 'failure.png') }).catch(() => {});
} finally {
  clearTimeout(deadline); if (browser) await browser.close(); await stop();
  report.finished_at = new Date().toISOString();
  await writeFile(join(evidence, 'qualification.json'), JSON.stringify(report, (_k, v) => typeof v === 'bigint' ? v.toString() : v, 2));
  console.log(JSON.stringify({ passed: report.passed, error: report.error, checks: report.checks.length, evidence }));
}
