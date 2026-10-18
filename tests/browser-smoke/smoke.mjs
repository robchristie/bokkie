// The browser smoke of Bokkie's web client. scripts/browser-smoke runs it;
// see CONTRIBUTING.md, "Browser smoke".
//
// It serves a production web package from the real gateway binary, with the
// pinned Codex app server pointed at the gateway tests' mock model (through
// a wrapper that adds the mock's `-c` overrides, so the gateway itself has
// no test seam), a synthetic admin in a temporary data directory and one
// Bot. Pinned Chromium, headful, then:
//
// - starts the page under the gateway's Content-Security-Policy;
// - signs in by clicking the fields and typing;
// - opens the Bot and sends; the reply streams in more than one delta and
//   completes;
// - stops a second reply mid-stream, which shows as interrupted;
// - drops the connection mid-reply and refuses new ones while more than one
//   piece arrives, then lets the client reconnect: the reply completes with
//   nothing lost or repeated;
// - shows the subscription meter and the limit-reached banner from windows
//   the mock reports;
// - checks the installable application as Stobie's pwa-smoke.mjs does, and
//   that the font notices are served as text;
// - fails on any CSP violation, page error or console error.
//
// It never calls a real model and never uses a ChatGPT credential.
//
// node smoke.mjs GATEWAY_BIN MOCK_BIN CODEX_BIN WEB_ROOT EVIDENCE_DIR
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, rm, writeFile, chmod } from 'node:fs/promises';
import { createServer, connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { chromium } from 'playwright';

const [gatewayBin, mockBin, codexBin, webRootArgument, evidenceArgument] = process.argv.slice(2);
assert(gatewayBin && mockBin && codexBin && webRootArgument && evidenceArgument,
  'usage: GATEWAY_BIN MOCK_BIN CODEX_BIN WEB_ROOT EVIDENCE_DIR');
const webRoot = resolve(webRootArgument);
const evidence = resolve(evidenceArgument);
await mkdir(evidence, { recursive: true });
const started = Date.now();
const wait = ms => new Promise(fulfil => setTimeout(fulfil, ms));
const report = { steps: [] };
const step = (name, details = {}) => {
  const entry = { name, at_ms: Date.now() - started, ...details };
  report.steps.push(entry);
  console.log(`smoke: ${name}${Object.keys(details).length ? ` ${JSON.stringify(details)}` : ''}`);
};

// The reply each turn streams: 24 pieces, emoji included, under the 200
// characters a fact carries, so the whole reply can be compared.
const PIECES = ['Kia ora! ', 'Here ', 'is ', 'a ', 'plan: ', 'Monday, ', 'a ', 'walk 🥾; ', 'Tuesday, ',
  'pasta; ', 'Wednesday, ', 'rest; ', 'Thursday, ', 'tacos 🌮; ', 'Friday, ', 'a ', 'film; ',
  'Saturday, ', 'the ', 'market; ', 'Sunday, ', 'a ', 'roast. ', 'Enjoy!'];
const REPLY = PIECES.join('');
const PIECE_MS = 200;
const PASSWORD = `synthetic ${randomUUID()}`;
const USERNAME = 'smoke';

// Processes and servers to stop at the end.
const children = [];
const closers = [];
function start(name, command, args, options = {}) {
  const child = spawn(command, args, { stdio: ['pipe', 'pipe', 'pipe'], ...options });
  const log = [];
  child.stdout.on('data', chunk => log.push(String(chunk)));
  child.stderr.on('data', chunk => log.push(String(chunk)));
  const exited = new Promise(fulfil => child.once('exit', fulfil));
  children.push({ name, child, log, exited });
  return { child, log };
}
async function freePort() {
  const server = createServer();
  await new Promise(fulfil => server.listen(0, '127.0.0.1', fulfil));
  const { port } = server.address();
  await new Promise(fulfil => server.close(fulfil));
  return port;
}

// A TCP relay in front of the gateway: `drop()` cuts every connection, as
// a network blip does, and `refusing` turns new connections away, as an
// unreachable server does.
function relay(listenPort, targetPort) {
  const pairs = new Set();
  const state = { refusing: false, dropped: 0 };
  const handle = client => {
    if (state.refusing) { client.destroy(); return; }
    const upstream = connect(targetPort, '127.0.0.1');
    const pair = [client, upstream];
    pairs.add(pair);
    const end = () => { pairs.delete(pair); client.destroy(); upstream.destroy(); };
    client.on('error', end); upstream.on('error', end);
    client.on('close', end); upstream.on('close', end);
    client.pipe(upstream); upstream.pipe(client);
  };
  const servers = [createServer(handle), createServer(handle)];
  state.drop = () => {
    state.dropped += pairs.size;
    for (const [client, upstream] of pairs) { client.destroy(); upstream.destroy(); }
    pairs.clear();
  };
  state.listen = async () => {
    // Browsers may try either loopback address for `localhost`.
    await new Promise((fulfil, reject) => { servers[0].once('error', reject); servers[0].listen(listenPort, '127.0.0.1', fulfil); });
    await new Promise(fulfil => { servers[1].once('error', () => fulfil()); servers[1].listen(listenPort, '::1', fulfil); });
  };
  state.close = () => { state.drop(); for (const server of servers) server.close(); };
  return state;
}

// Stobie's automation protocol, through the page (`?automation=1`).
class Automation {
  constructor(page) { this.page = page; this.instance = null; }
  async request(operation) {
    const request = { version: 1, request_id: randomUUID(), operation };
    if (this.instance) request.instance = this.instance;
    const raw = await this.page.evaluate(json => window.__STOBIE_AUTOMATION.request(json), JSON.stringify(request));
    const response = JSON.parse(raw);
    if (response.error) throw new Error(`${operation.op}: ${response.error.code}: ${response.error.message}`);
    this.instance ??= response.instance;
    return response.result;
  }
  async hello() {
    await this.page.waitForFunction(() => typeof window.__STOBIE_AUTOMATION?.request === 'function', null, { timeout: 60000 });
    return this.request({ op: 'hello' });
  }
  async facts() {
    const { observation } = await this.request({ op: 'observe' });
    const facts = {};
    for (const [key, value] of Object.entries(observation.snapshot?.facts ?? observation.facts ?? {})) {
      facts[key] = value && typeof value === 'object' && 'value' in value ? value.value : value;
    }
    return facts;
  }
  async until(description, predicate, timeoutMs = 30000) {
    const deadline = Date.now() + timeoutMs;
    let facts;
    while (Date.now() < deadline) {
      facts = await this.facts();
      if (predicate(facts)) return facts;
      await wait(25);
    }
    throw new Error(`timed out waiting for ${description}; last facts ${JSON.stringify(facts)}`);
  }
  async invoke(selector, value) {
    const discovered = await this.request({ op: 'discover', selector, limit: 2 });
    assert.equal(discovered.capabilities.length, 1, `one capability for ${JSON.stringify(selector)}`);
    const operation = { op: 'invoke', selector, expected: discovered.capabilities[0].target };
    if (value !== undefined) operation.arguments = { value };
    let { receipt } = await this.request(operation);
    for (let tries = 0; receipt.state === 'queued' && tries < 400; tries++) {
      await wait(25);
      ({ receipt } = await this.request({ op: 'receipt', request_id: receipt.request_id }));
    }
    assert.equal(receipt.state, 'completed', `${JSON.stringify(selector)}: ${JSON.stringify(receipt)}`);
  }
  action(id, value) { return this.invoke({ capability: id }, value); }
  setText(id, text) { return this.invoke({ id, capability: 'stobie.set-text' }, text); }
}

const scratch = await mkdtemp(join(tmpdir(), 'bokkie-smoke-'));
let context;
let failed = true;
try {
  // The mock model, and the pinned Codex pointed at it.
  const mock = start('mock-model', mockBin, ['127.0.0.1:0']);
  const [line] = await Promise.race([
    once(mock.child.stdout, 'data'),
    wait(30000).then(() => { throw new Error('the mock model did not start'); }),
  ]);
  const { overrides, control } = JSON.parse(String(line).split('\n')[0]);
  const controlRequest = async (path, body) => {
    const response = await fetch(`${control}/${path}`, {
      method: body === undefined ? 'GET' : 'PUT',
      headers: { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    assert.equal(response.status, 200, `mock ${path}`);
    return response.json();
  };
  await controlRequest('reply', { pieces: PIECES, piece_delay_ms: PIECE_MS });
  // The gateway starts app servers with a cleared environment, so the
  // wrapper carries the overrides itself.
  const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
  const wrapper = join(scratch, 'codex');
  await writeFile(wrapper, [
    '#!/bin/sh',
    '# The pinned Codex with the mock model\'s overrides before app-server.',
    'if [ "$1" = app-server ]; then',
    `  exec ${quote(codexBin)} ${overrides.map(o => `-c ${quote(o)}`).join(' ')} "$@"`,
    'fi',
    `exec ${quote(codexBin)} "$@"`,
    '',
  ].join('\n'));
  await chmod(wrapper, 0o755);

  // The real gateway, behind the relay, with a synthetic admin.
  const [port, gatewayPort] = [await freePort(), await freePort()];
  const origin = `http://localhost:${port}`;
  const data = join(scratch, 'data');
  await mkdir(data);
  const env = {
    PATH: process.env.PATH, HOME: scratch, BOKKIE_DATA: data, BOKKIE_ORIGIN: origin,
    BOKKIE_LISTEN: `127.0.0.1:${gatewayPort}`, BOKKIE_WEB_ROOT: webRoot, BOKKIE_CODEX_BIN: wrapper,
    BOKKIE_LOG: 'info',
  };
  const bootstrap = start('bootstrap-admin', gatewayBin, ['bootstrap-admin', USERNAME, 'Smoke Admin'], { env });
  bootstrap.child.stdin.end(`${PASSWORD}\n`);
  const [code] = await once(bootstrap.child, 'exit');
  assert.equal(code, 0, `bootstrap-admin: ${bootstrap.log.join('')}`);
  const gateway = start('gateway', gatewayBin, ['serve'], { env });
  const link = relay(port, gatewayPort);
  await link.listen();
  closers.push(() => link.close());
  for (let attempt = 0; ; attempt++) {
    assert(attempt < 300 && gateway.child.exitCode === null, `the gateway did not start: ${gateway.log.join('')}`);
    try { if ((await fetch(`${origin}/healthz`)).ok) break; } catch { /* not yet */ }
    await wait(100);
  }
  step('gateway serving the package', { origin, web_root: webRoot });

  // One Bot, created over the API as the admin.
  const api = async (method, path, body, cookie) => {
    const response = await fetch(`${origin}/api/v1${path}`, {
      method,
      headers: { origin, 'content-type': 'application/json', ...(cookie ? { cookie } : {}) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { response, body: await response.json().catch(() => null) };
  };
  const session = await api('POST', '/session', { username: USERNAME, password: PASSWORD });
  assert.equal(session.response.status, 200, 'API sign-in');
  const cookie = session.response.headers.get('set-cookie').split(';')[0];
  const bot = (await api('POST', '/bots', { name: 'Planner', label: 'Plans the week', description: 'Suggest a plan.' }, cookie)).body;
  assert(Number.isInteger(bot?.id), 'the Bot was created');
  const storedReply = async () => {
    const { body } = await api('GET', `/bots/${bot.id}/messages`, undefined, cookie);
    return body.messages.at(-1);
  };

  // Pinned Chromium, headful (egui's text fields take no typing headless),
  // with a persistent profile, since Chromium never installs from an
  // incognito one.
  const profile = join(scratch, 'profile');
  context = await chromium.launchPersistentContext(profile, {
    headless: false,
    args: ['--ozone-platform=x11', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
    viewport: { width: 430, height: 900 },
  });
  report.browser = context.browser()?.version() ?? 'persistent context';
  const page = context.pages()[0] ?? await context.newPage();
  const problems = [];
  page.on('pageerror', error => problems.push(`page error: ${error}`));
  // Failed requests are expected: the session check before signing in
  // (401) and the dropped and refused connections. They are recorded, not
  // failed on; every other console error fails the smoke.
  report.network_errors = [];
  page.on('console', message => {
    const text = message.text();
    if (/Content.Security.Policy/i.test(text)) problems.push(`console ${message.type()}: ${text}`);
    else if (message.type() !== 'error') return;
    else if (/^Failed to load resource|^WebSocket connection to .* failed/.test(text)) report.network_errors.push(text);
    else problems.push(`console error: ${text}`);
  });
  await page.addInitScript(() => {
    window.__cspViolations = [];
    document.addEventListener('securitypolicyviolation', event => window.__cspViolations.push(
      `${event.effectiveDirective} blocked ${event.blockedURI || 'inline'} (${event.sourceFile}:${event.lineNumber})`));
  });
  // Every event frame the page receives, and which API responses the
  // service worker answered.
  const frames = [];
  page.on('websocket', socket => socket.on('framereceived', frame => frames.push({ at: Date.now(), payload: String(frame.payload) })));
  const apiFromWorker = [];
  page.on('response', response => {
    if (new URL(response.url()).pathname.startsWith('/api/') && response.fromServiceWorker()) apiFromWorker.push(response.url());
  });

  const pageResponse = await page.goto(`${origin}/?automation=1`);
  const policy = pageResponse.headers()['content-security-policy'];
  assert.match(policy ?? '', /frame-ancestors 'none'/, 'the page carries the gateway\'s CSP');
  await page.waitForFunction(() => window.__STOBIE_STARTUP?.status === 'ready', null, { timeout: 60000 });
  const app = new Automation(page);
  report.hello = (await app.hello()).application;
  await app.until('the sign-in screen', facts => facts.screen === 'signed_out');
  step('page started', { csp: policy });

  // Sign in by clicking each field and typing, as a person does. Under a
  // bare X server no window manager focuses the window, so focus is
  // emulated, as Playwright's own launch does for new pages.
  const focus = await context.newCDPSession(page);
  await focus.send('Emulation.setFocusEmulationEnabled', { enabled: true });
  await page.bringToFront();
  const typeInto = async (name, text) => {
    const field = page.getByRole('textbox', { name, exact: true });
    await field.click();
    await page.waitForFunction(() => document.activeElement?.tagName === 'INPUT', null, { timeout: 5000 });
    report.focus ??= [];
    report.focus.push(await page.evaluate(() => document.activeElement?.outerHTML?.slice(0, 300)));
    await page.keyboard.type(text);
  };
  await typeInto('Username', USERNAME);
  await typeInto('Password', PASSWORD);
  await app.until('the username typed', facts => facts.username === undefined || facts.screen === 'signed_out', 5000);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await app.until('signed in and live', facts => facts.bots === 'loaded' && facts.connection === 'live' && facts.user === USERNAME);
  step('signed in by typing');

  // The installable application, after Stobie's pwa-smoke.mjs.
  const cdp = await context.newCDPSession(page);
  const manifest = await cdp.send('Page.getAppManifest');
  assert.deepEqual(manifest.errors, [], 'manifest errors');
  const parsed = JSON.parse(manifest.data);
  assert.equal(parsed.name, 'Bokkie');
  assert.equal(parsed.short_name, 'Bokkie');
  assert.equal(parsed.theme_color, '#295b50');
  assert.equal(parsed.background_color, '#f5f1e9');
  assert.equal(parsed.display, 'standalone');
  for (const icon of parsed.icons) {
    const response = await page.request.get(new URL(icon.src, manifest.url).href);
    assert.equal(response.status(), 200, icon.src);
    assert.equal(response.headers()['content-type'], 'image/png', icon.src);
  }
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null, null, { timeout: 30000 });
  const worker = await page.evaluate(() => window.__STOBIE_STARTUP.serviceWorker);
  assert.equal(worker.state, 'registered');
  assert.equal(worker.scope, `${origin}/`);
  const installability = await cdp.send('Page.getInstallabilityErrors');
  assert.deepEqual(installability.installabilityErrors, [], 'installability errors');
  // The fonts' notices display as text.
  const licence = await page.evaluate(() => document.querySelector('link[rel=license]').href);
  const notices = await page.request.get(licence);
  assert.equal(notices.status(), 200);
  assert.equal(notices.headers()['content-type'], 'text/plain; charset=utf-8');
  assert.match(await notices.text(), /^Third-party notices for Bokkie's web client/);
  step('installable', { manifest: manifest.url, service_worker: worker, icons: parsed.icons.length, notices: licence });

  // Open the Bot and send: the reply streams in several deltas.
  await app.action('bokkie.open-bot', bot.id);
  await app.until('the conversation', facts => facts.conversation === 'loaded' && facts.bot === bot.id);
  const deltas = () => frames.filter(frame => frame.payload.includes('"message_delta"')).length;
  const send = async text => {
    await app.setText('bokkie.draft', text);
    await app.until('the draft entered', facts => facts.draft === text, 5000);
    await app.action('bokkie.send');
    await app.until('delivered', facts => facts.delivery === 'new_turn' && facts.draft === '');
  };
  let before = deltas();
  await send('Plan my week');
  const seen = new Set();
  const complete = await app.until('the first reply to complete', facts => {
    if (facts.reply_state === 'streaming') seen.add(facts.reply_chars);
    return facts.reply_state === 'complete' && facts.bot_status === 'idle' && facts.message_count === 2;
  }, 60000);
  const firstDeltas = deltas() - before;
  assert(firstDeltas > 1, `the reply arrived in ${firstDeltas} delta frames`);
  assert(seen.size > 1, `the page showed the reply growing (${[...seen].join(', ')})`);
  assert.equal(complete.reply, REPLY, 'the page holds the whole reply');
  assert.equal((await storedReply()).text, REPLY);
  step('reply streamed and completed', { delta_frames: firstDeltas, lengths_seen: [...seen].sort((a, b) => a - b) });

  // Stop a second reply mid-stream.
  await send('And next week?');
  await app.until('the second reply streaming', facts => facts.reply_state === 'streaming' && facts.reply_chars > 0, 60000);
  await app.action('bokkie.stop');
  const stopped = await app.until('the reply interrupted', facts => facts.reply_state === 'interrupted' && facts.bot_status === 'idle', 30000);
  const storedStop = await storedReply();
  assert.equal(storedStop.state, 'interrupted');
  assert.equal(stopped.reply, storedStop.text, 'the page shows what was stored');
  assert(stopped.reply_chars < [...REPLY].length, 'the reply was cut short');
  step('reply stopped', { chars: stopped.reply_chars });

  // Drop the connection mid-reply and keep the server unreachable while
  // more than one piece arrives, then let the client back.
  await send('One more plan, please');
  await app.until('the third reply streaming', facts => facts.reply_state === 'streaming' && facts.reply_chars >= 10, 60000);
  link.refusing = true;
  link.drop();
  const framesAtDrop = frames.length;
  const down = await app.until('reconnecting', facts => facts.connection === 'reconnecting', 10000);
  // Hold the server unreachable while several more pieces stream.
  await wait(PIECE_MS * 6);
  const whileDown = await app.facts();
  assert.equal(whileDown.reply_chars, down.reply_chars, 'nothing arrives while the connection is down');
  assert.equal(whileDown.reply_state, 'streaming', 'the reply was still running when the connection went');
  link.refusing = false;
  const back = await app.until('live again', facts => facts.connection === 'live', 20000);
  const done = await app.until('the third reply to complete', facts => facts.reply_state === 'complete' && facts.bot_status === 'idle', 60000);
  assert.equal(done.reply, REPLY, 'nothing lost or repeated after the drop');
  assert.equal((await storedReply()).text, REPLY);
  // Text the page missed while down came back at once, as replayed events:
  // more than one piece's boundary lies between the two lengths.
  const boundaries = PIECES.map((_, n) => [...PIECES.slice(0, n + 1).join('')].length);
  const caughtUp = boundaries.filter(length => length > whileDown.reply_chars && length <= back.reply_chars).length;
  assert(caughtUp > 1, `the client caught up ${caughtUp} pieces on reconnecting (${whileDown.reply_chars} to ${back.reply_chars})`);
  const replayed = frames.slice(framesAtDrop).filter(frame => frame.payload.includes('"message_delta"')).length;
  step('dropped connection caught up', {
    chars_at_drop: down.reply_chars, chars_when_live: back.reply_chars, pieces_caught_up: caughtUp,
    delta_frames_after_drop: replayed, connections_dropped: link.dropped,
  });

  // The subscription meter, then the limit-reached banner, from windows the
  // mock reports in its response headers.
  const resetsAt = Math.floor(Date.now() / 1000) + 3 * 3600;
  await controlRequest('rate-limits', {
    primary: { used_percent: 42, window_minutes: 300, resets_at: resetsAt },
    secondary: { used_percent: 7, window_minutes: 10080, resets_at: resetsAt + 86400 },
  });
  await controlRequest('reply', { pieces: ['Short ', 'reply.'], piece_delay_ms: 50 });
  await send('How much is used?');
  const meter = await app.until('the meter at 42%', facts => facts.subscription_used === 42 && facts.reply_state === 'complete', 60000);
  assert.equal(meter.limit, '', 'no banner below the limit');
  await controlRequest('rate-limits', {
    primary: { used_percent: 100, window_minutes: 300, resets_at: resetsAt },
    secondary: null,
  });
  await send('And now?');
  const reached = await app.until('the limit-reached banner', facts => facts.subscription_used === 100 && facts.limit !== '', 60000);
  step('subscription meter and banner', { used: meter.subscription_used, banner: reached.limit });

  // Nothing from the API reached the service worker's cache.
  const cached = await page.evaluate(async () => (await Promise.all((await caches.keys()).map(async name =>
    (await (await caches.open(name)).keys()).map(request => request.url)))).flat());
  assert(cached.length > 0, 'the service worker cached the package');
  assert(!cached.some(url => new URL(url).pathname.startsWith('/api/')), `no API response is cached: ${cached.join(', ')}`);
  assert.deepEqual(apiFromWorker, [], 'no API response came from the service worker');
  step('nothing private cached', { cached: cached.length });

  const violations = await page.evaluate(() => window.__cspViolations);
  assert.deepEqual(violations, [], 'CSP violations');
  assert.deepEqual(problems, [], 'page errors');
  step('no CSP violation, page error or console error');
  failed = false;
} finally {
  report.passed = !failed;
  if (failed) await context?.pages()[0]?.screenshot({ path: join(evidence, 'failure.png') }).catch(() => {});
  report.duration_ms = Date.now() - started;
  await context?.close().catch(() => {});
  for (const close of closers) close();
  // SIGINT, which the gateway and the mock take as a request to shut down:
  // the gateway then stops its app servers before it exits.
  for (const { child } of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGINT');
  }
  await Promise.all(children.map(({ exited }) => exited));
  for (const { name, log } of children) await writeFile(join(evidence, `${name}.log`), log.join(''));
  await writeFile(join(evidence, 'smoke.json'), `${JSON.stringify(report, null, 2)}\n`);
  await rm(scratch, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}
console.log(`smoke: passed in ${((Date.now() - started) / 1000).toFixed(1)} s; evidence ${evidence}`);
