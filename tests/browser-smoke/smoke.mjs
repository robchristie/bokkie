// The browser smoke of Bokkie's web client. scripts/browser-smoke runs it;
// see CONTRIBUTING.md, "Browser smoke".
//
// It serves a production web package from the real gateway binary, with the
// pinned Codex app servers pointed at the gateway tests' mocks (through a
// wrapper that adds the mocks' `-c` overrides and environment, so the
// gateway itself has no test seam): the model, and a ChatGPT sign-in
// authority and backend that issued the household's synthetic sign-in. A
// synthetic admin in a temporary data directory has one Bot. Pinned
// Chromium, headful, then:
//
// - starts the page under the gateway's Content-Security-Policy;
// - signs in by clicking the fields and typing;
// - opens the Bot and sends from the composer; the reply streams in more
//   than one delta and completes, and its Copy, pressed through the
//   accessibility layer, puts it on the clipboard;
// - stops a second reply mid-stream, which shows as interrupted;
// - drops the connection mid-reply and refuses new ones while more than one
//   piece arrives, then lets the client reconnect: the reply completes with
//   nothing lost or repeated;
// - shows a reply in Markdown, streamed with its link split across
//   pieces, formatted, with the link a link that opens a new tab;
// - sends by Return typed in the composer and by a pointer's click on Send;
// - follows the page's width from a phone's to a desktop's;
// - follows the system's appearance, light and dark, at a phone's width
//   (390 by 844) and a desktop's (1280 by 800), with a Bot whose name and
//   label are long: the page never scrolls sideways, every control lies
//   within it, and a screenshot of each goes to the evidence;
// - switches notifications on for the device, through a stand-in push
//   service: the subscription reaches the gateway, and no observation
//   carries its endpoint;
// - makes a routine in the Bot's settings, in its sheet, which keeps the
//   panel behind it out of reach and closes without saving first, and runs
//   it now: the run
//   succeeds in its history; its push is recorded for the one subscription
//   (whose endpoint, on a loopback name, the gateway refuses to reach), the
//   Bot is unread and the badge counts it; beside the settings, the list's
//   row for the Bot reads it unread, with the run's reply as its preview,
//   from its node and its accessible button; Show brings its
//   marker and the mock's reply into view, says the Bot is viewed, and
//   marks it read;
// - opens the Bot's panel, grants notes with its switch and runs the
//   routine again, calling a tool: the panel shows the routine, the run
//   among its recent runs and the call in its activity; then deletes the
//   routine through its confirm, kept first and then confirmed;
// - opens the Bot a tapped notification names, through the service
//   worker's own route handling, then switches notifications off;
// - shows the subscription meter and the limit-reached banner from windows
//   the mock reports;
// - has the admin invite a member, with permissions, from the accounts
//   screen, and copy the code with its Copy, read back from the clipboard
//   and published nowhere; the member joins with it in a second browser
//   context, whose system appearance is light where the admin's is dark,
//   and sees the household's status only; the member makes a Bot;
//   the admin sets the member's limit and permissions, which reach the
//   member live; the member is refused beyond the limit; and oversight,
//   switched on while the member is signed in, shows its notice on the
//   member's page while the admin reads the conversation, read-only;
// - signs the household out of ChatGPT, refused until confirmed: asked,
//   kept signed in, then asked and confirmed;
// - checks the installable application as Stobie's pwa-smoke.mjs does, and
//   that the font notices are served as text;
// - fails on any CSP violation, page error or console error;
// - shows that no app server tried to reach any host but the mocks.
//
// It never calls a real model, never uses a real ChatGPT credential and
// never reaches a real push service.
//
// node smoke.mjs GATEWAY_BIN MOCK_BIN CODEX_BIN WEB_ROOT EVIDENCE_DIR
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile, chmod } from 'node:fs/promises';
import { createServer, connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { chromium } from 'playwright';

// The pinned Node, wherever this runs (directly, in CI or through a runner).
const pinned = readFileSync(new URL('.node-version', import.meta.url), 'utf8').trim();
assert.equal(process.version, `v${pinned}`, `the smoke needs Node ${pinned}`);

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
// Stobie's accessibility layer, the page's ARIA tree.
const LAYER = '[id^="stobie-a11y:layer"]';
const PASSWORD = `synthetic ${randomUUID()}`;
// The stand-in push service's subscription. Its endpoint names a loopback
// host, which the gateway refuses to send to; its keys are well-formed, as
// the gateway requires, and belong to no device.
const PUSH_ENDPOINT = 'https://localhost/push-stand-in/device-1';
const PUSH_KEYS = {
  p256dh: 'BEl62iUYgUivxIkv69yViEuiBIa-Ib9-SkvMeAtA3LFgDzkrxZJjSgSnfckjBJuBkr3qBUYIHBQFLXYp5Nksh8U',
  auth: 'c21va2UtYXV0aC0xMjM0NQ',
};
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
    // A field or control that the view presents afresh between discovery
    // and invocation, as when a reply arrives, is rejected as no longer
    // presented, or stale: discover it again once the view has settled.
    for (let attempt = 0; ; attempt++) {
      // An action presented on each row has one binding per row, carrying
      // the row's ID, besides its own: take the row's control for this
      // value if it is presented, as a person would, and the action's own
      // binding otherwise.
      const discovered = await this.request({ op: 'discover', selector, limit: 64 });
      const rows = discovered.capabilities.filter(binding => binding.argument !== undefined && binding.argument === value);
      const chosen = rows.length > 0 ? rows : discovered.capabilities.filter(binding => binding.argument === undefined);
      assert.equal(chosen.length, 1, `one binding for ${JSON.stringify(selector)} and ${JSON.stringify(value)}`);
      const operation = { op: 'invoke', selector, expected: chosen[0].target };
      if (value !== undefined) operation.arguments = { value };
      let receipt;
      try {
        ({ receipt } = await this.request(operation));
      } catch (error) {
        if (!/stale_target|is no longer presented/.test(error.message) || attempt >= 20) throw error;
        await wait(50);
        continue;
      }
      for (let tries = 0; receipt.state === 'queued' && tries < 400; tries++) {
        await wait(25);
        ({ receipt } = await this.request({ op: 'receipt', request_id: receipt.request_id }));
      }
      const represented = receipt.error?.code === 'stale_target'
        || /is no longer presented/.test(receipt.error?.message ?? '');
      if (receipt.state === 'rejected' && represented && attempt < 20) {
        await wait(50);
        continue;
      }
      assert.equal(receipt.state, 'completed', `${JSON.stringify(selector)}: ${JSON.stringify(receipt)}`);
      return;
    }
  }
  action(id, value) { return this.invoke({ capability: id }, value); }
  async nodes(selector) {
    const { nodes } = await this.request({ op: 'query', selector, limit: 64 });
    return nodes.map(({ node }) => node);
  }
  setText(id, text) { return this.invoke({ id, capability: 'stobie.set-text' }, text); }
  // Write in the composer, which Stobie names "Message", as a person types.
  async compose(text) {
    const [field, ...others] = await this.nodes({ role: 'multiline_text_input', name: 'Message' });
    assert(field && others.length === 0, 'one composer');
    await this.setText(field.id, text);
  }
  // Scroll down (or up) with the mouse wheel, as a person does, until a
  // control is in view and so presented.
  async scrollTo(selector, by = 200) {
    for (let attempt = 0; attempt < 40; attempt++) {
      const { capabilities } = await this.request({ op: 'discover', selector, limit: 64 });
      if (capabilities.length > 0) return;
      await this.page.mouse.move(215, 600);
      await this.page.mouse.wheel(0, by);
      await wait(100);
    }
    throw new Error(`${JSON.stringify(selector)} never came into view`);
  }
  // The content text in view: the excerpts the page publishes of its
  // headings, labels and reading text, in presentation order.
  async contents() {
    const { observation } = await this.request({ op: 'observe' });
    return (observation.snapshot?.text ?? [])
      .map(text => text.content)
      .filter(content => content?.kind === 'excerpt')
      .map(content => content.text);
  }
}

// Visible text the page's accessibility layer carries: aria-labels and leaf
// text. Facts never carry content text, and content text carries no
// control's label, so controls and notices are read here; Bot text is read
// as content text, and an invitation code only from the clipboard.
const pageTexts = page => page.evaluate(() => {
  const texts = new Set();
  for (const element of document.querySelectorAll('body *:not(style):not(script)')) {
    const label = element.getAttribute('aria-label');
    if (label) texts.add(label.trim());
    for (const node of element.childNodes) {
      if (node.nodeType === Node.TEXT_NODE && node.textContent.trim()) texts.add(node.textContent.trim());
    }
  }
  return [...texts];
});

// The clipboard's text once `holds` accepts it, within five seconds. It is
// read from here, since a page function's promise is not awaited by
// waitForFunction.
async function clipboardText(page, holds) {
  let text = '';
  for (let tries = 0; tries < 100; tries++) {
    text = await page.evaluate(() => navigator.clipboard.readText());
    if (holds(text)) return text;
    await wait(50);
  }
  throw new Error(`the clipboard holds ${text.length} characters, not what was copied`);
}
const clipboardHolds = (page, expected) => clipboardText(page, text => text === expected);

// The page's stand-ins for a push service and the app badge, as Stobie's
// web-push smoke has them: subscribing records the call and keeps the
// subscription across reloads, and badge calls are recorded. No push
// service is asked for anything.
function pushStandIn({ endpoint, keys }) {
  const stored = () => JSON.parse(localStorage.getItem('push-stand-in') ?? 'null');
  const subscription = value => value && Object.assign(Object.create(null), {
    endpoint: value.endpoint, expirationTime: null,
    toJSON: () => value,
    unsubscribe: async () => { localStorage.removeItem('push-stand-in'); return true; },
  });
  window.__pushStandIn = { calls: [], badges: [] };
  PushManager.prototype.subscribe = function (options) {
    window.__pushStandIn.calls.push({
      userVisibleOnly: options?.userVisibleOnly,
      key_bytes: new Uint8Array(options?.applicationServerKey ?? []).length,
    });
    const value = { endpoint, expirationTime: null, keys };
    localStorage.setItem('push-stand-in', JSON.stringify(value));
    return Promise.resolve(subscription(value));
  };
  PushManager.prototype.getSubscription = function () { return Promise.resolve(subscription(stored())); };
  Navigator.prototype.setAppBadge = function (count) { window.__pushStandIn.badges.push(count); return Promise.resolve(); };
  Navigator.prototype.clearAppBadge = function () { window.__pushStandIn.badges.push(0); return Promise.resolve(); };
}

// The gateway's log without its colours.
const plain = log => log.join('').replace(/\x1b\[[0-9;]*m/g, '');

const scratch = await mkdtemp(join(tmpdir(), 'bokkie-smoke-'));
let context;
let memberContext;
let failed = true;
try {
  // The mock model, and the pinned Codex pointed at it.
  const mock = start('mock-model', mockBin, ['127.0.0.1:0', scratch]);
  // The mock prints one JSON line once it serves, which may arrive in
  // more than one piece.
  const line = await new Promise((fulfil, reject) => {
    let text = '';
    const timer = setTimeout(() => reject(new Error(`the mock did not start: ${text}`)), 30000);
    const read = chunk => {
      text += chunk;
      if (!text.includes('\n')) return;
      clearTimeout(timer);
      mock.child.stdout.off('data', read);
      fulfil(text.slice(0, text.indexOf('\n')));
    };
    mock.child.stdout.on('data', read);
  });
  const { overrides, env: mockEnv, household_auth: householdAuth, control } = JSON.parse(line);
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
  // wrapper carries the overrides and environment itself.
  const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
  const wrapper = join(scratch, 'codex');
  await writeFile(wrapper, [
    '#!/bin/sh',
    '# The pinned Codex with the mocks\' overrides before app-server.',
    ...Object.entries(mockEnv).map(([name, value]) => `export ${name}=${quote(value)}`),
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
  // The household's synthetic sign-in, as an admin's device-code sign-in
  // would leave it.
  await mkdir(join(data, 'household', 'codex'), { recursive: true, mode: 0o700 });
  await writeFile(join(data, 'household', 'codex', 'auth.json'), householdAuth, { mode: 0o600 });
  const env = {
    PATH: process.env.PATH, HOME: scratch, BOKKIE_DATA: data, BOKKIE_ORIGIN: origin,
    BOKKIE_LISTEN: `127.0.0.1:${gatewayPort}`, BOKKIE_WEB_ROOT: webRoot, BOKKIE_CODEX_BIN: wrapper,
    BOKKIE_LOG: 'info',
    // The gateway sends pushes, to the stand-in's loopback endpoint, which
    // it refuses to reach.
    BOKKIE_PUSH_CONTACT: 'mailto:smoke@example.invalid',
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
    // Times read in a fixed locale's 12-hour clock, and the system's
    // appearance is dark until the smoke changes it.
    locale: 'en-US',
    colorScheme: 'dark',
  });
  report.browser = context.browser()?.version() ?? 'persistent context';
  const page = context.pages()[0] ?? await context.newPage();
  // Notifications are allowed, as a person allows them when the switch
  // asks, and the push service is the stand-in.
  // So is the clipboard, which Copy writes and the smoke reads back.
  await context.grantPermissions(['notifications', 'clipboard-read', 'clipboard-write'], { origin });
  await page.addInitScript(pushStandIn, { endpoint: PUSH_ENDPOINT, keys: PUSH_KEYS });
  const problems = [];
  // Failed requests are expected: the session check before signing in
  // (401), the dropped and refused connections, and a send refused beyond
  // a limit (429). They are recorded, not failed on; every other console
  // error fails the smoke.
  report.network_errors = [];
  const watch = async page => {
    page.on('pageerror', error => problems.push(`page error: ${error}`));
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
  };
  await watch(page);
  // Every event frame the page receives, and which API responses the
  // service worker answered.
  const frames = [];
  const sentFrames = [];
  page.on('websocket', socket => {
    socket.on('framereceived', frame => frames.push({ at: Date.now(), payload: String(frame.payload) }));
    socket.on('framesent', frame => sentFrames.push(String(frame.payload)));
  });
  const viewingSent = () => sentFrames.map(payload => JSON.parse(payload)).filter(frame => frame.type === 'viewing')
    .map(frame => frame.bot_id ?? null);
  // What the page's own session holds at the gateway.
  const pageApi = path => page.evaluate(async path => (await fetch(`/api/v1${path}`)).json(), path);
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
  const typeInto = async (name, text, into = page) => {
    const field = into.getByRole('textbox', { name, exact: true });
    await field.click();
    await into.waitForFunction(() => document.activeElement?.tagName === 'INPUT', null, { timeout: 5000 });
    report.focus ??= [];
    report.focus.push(await into.evaluate(() => document.activeElement?.outerHTML?.slice(0, 300)));
    await into.keyboard.type(text);
  };
  await typeInto('Username', USERNAME);
  await typeInto('Password', PASSWORD);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  const signedIn = await app.until('signed in and live', facts => facts.bots === 'loaded' && facts.connection === 'live' && facts.user === USERNAME);
  assert.equal(signedIn.theme, 'dark', 'the page follows the system\'s dark appearance');
  step('signed in by typing', { theme: signedIn.theme });

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

  // Notifications on for this device: the switch subscribes with the
  // gateway's key, and the subscription reaches the gateway once.
  await app.until('the device checked', facts => facts.push === 'off' && facts.push_sending === true);
  await app.scrollTo({ capability: 'bokkie.notifications' });
  await app.action('bokkie.notifications');
  const switchedOn = await app.until('notifications on', facts => facts.push === 'on' && facts.push_saved === true
    && facts.push_error === '');
  const pushCalls = await page.evaluate(() => window.__pushStandIn.calls);
  assert.deepEqual(pushCalls, [{ userVisibleOnly: true, key_bytes: 65 }], 'one subscribe, with the gateway\'s key');
  assert.deepEqual(await pageApi('/push/subscription'), { subscribed: true }, 'the gateway holds the subscription');
  assert(!JSON.stringify(await app.request({ op: 'observe' })).includes('push-stand-in'), 'no observation carries the endpoint');
  await app.scrollTo({ capability: 'bokkie.open-bot' }, -200);
  step('notifications switched on', { push: switchedOn.push, saved: switchedOn.push_saved });

  // Open the Bot and send: the reply streams in several deltas.
  await app.action('bokkie.open-bot', bot.id);
  await app.until('the conversation', facts => facts.conversation === 'loaded' && facts.bot === bot.id);
  const deltas = () => frames.filter(frame => frame.payload.includes('"message_delta"')).length;
  // Each message goes to an idle Bot, so it starts a turn of its own rather
  // than steering the last one.
  const send = async text => {
    await app.until('the Bot idle', facts => facts.bot_status === 'idle' && facts.reply_state !== 'streaming', 60000);
    await app.compose(text);
    await app.until('the draft entered', facts => facts.draft === text, 5000);
    await app.action('bokkie.send', text);
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
  const growing = [...seen].filter(length => length > 0);
  assert(growing.length > 1, `the page showed the reply growing (${[...seen].join(', ')})`);
  assert.equal(complete.reply, REPLY, 'the page holds the whole reply');
  assert.equal((await storedReply()).text, REPLY);
  step('reply streamed and completed', { delta_frames: firstDeltas, lengths_seen: [...seen].sort((a, b) => a - b) });

  // Copy the reply, as VoiceOver's double tap does: a click on its control
  // in the accessibility layer. The clipboard holds the reply, and the
  // control says so.
  await page.evaluate(() => navigator.clipboard.writeText(''));
  const copies = page.locator(`${LAYER} [role="button"][aria-label="Copy message"]`);
  assert.equal(await copies.count(), 2, 'a Copy for each message');
  await copies.last().evaluate(target => target.click());
  await page.locator(`${LAYER} [role="status"]`, { hasText: 'Copied' }).first().waitFor({ state: 'attached', timeout: 5000 });
  await clipboardHolds(page, REPLY);
  step('reply copied', { copies: 2 });

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

  // A reply in Markdown, its link split across pieces, reads formatted:
  // its link is a link, named by its text, that opens a new tab.
  const MARKDOWN = ['Read **the ', 'notes** in [the ', 'guide](https://example.org/', 'guide).'];
  await controlRequest('reply', { pieces: MARKDOWN, piece_delay_ms: 100 });
  await send('Where are the notes?');
  const formatted = await app.until('the Markdown reply', facts => facts.reply_state === 'complete'
    && facts.bot_status === 'idle' && facts.reply === MARKDOWN.join(''), 60000);
  const [guide, ...others] = await app.nodes({ role: 'link', name: 'the guide' });
  assert(guide && others.length === 0, 'one link named by its text');
  assert.equal(guide.value, 'https://example.org/guide', 'the link\'s URL');
  assert.equal(guide.description, 'Opens in a new tab');
  const ariaLink = page.getByRole('link', { name: 'the guide', exact: true });
  assert.equal(await ariaLink.count(), 1, 'the accessibility layer presents the link');
  assert.equal(await ariaLink.getAttribute('aria-description'), 'Opens in a new tab');
  const readTexts = await app.contents();
  assert(readTexts.includes('Read the notes in the guide.'), `the reply reads formatted: ${JSON.stringify(readTexts)}`);
  assert(!readTexts.some(text => text.includes('**') || text.includes('](')), 'no Markdown source on the page');
  step('Markdown reply with a link', { reply: formatted.reply, link: guide.value });

  // Return typed in the composer sends, and so does a pointer's click on
  // Send, as a person's keyboard and mouse reach them. A control's point
  // on the page is its rectangle's centre, in points from the canvas's
  // corner.
  await controlRequest('reply', { pieces: ['Noted.'], piece_delay_ms: 0 });
  const centre = async selector => {
    const [node, ...more] = await app.nodes(selector);
    assert(node && more.length === 0, `one ${JSON.stringify(selector)}`);
    const canvas = await page.locator('#app-canvas').boundingBox();
    return { x: canvas.x + (node.rect.min_x + node.rect.max_x) / 2, y: canvas.y + (node.rect.min_y + node.rect.max_y) / 2 };
  };
  const answered = async (description, count) => app.until(description, facts => facts.message_count === count
    && facts.reply === 'Noted.' && facts.reply_state === 'complete' && facts.bot_status === 'idle' && facts.draft === '', 60000);
  await app.until('the Planner idle', facts => facts.bot_status === 'idle' && facts.reply_state !== 'streaming', 60000);
  const typedFrom = (await app.facts()).message_count;
  const field = await centre({ role: 'multiline_text_input', name: 'Message' });
  await page.mouse.click(field.x, field.y);
  await page.keyboard.type('Thanks');
  await app.until('the draft typed', facts => facts.draft === 'Thanks', 5000);
  await page.keyboard.press('Enter');
  await answered('the reply to Return', typedFrom + 2);
  await app.compose('Thanks again');
  await app.until('the draft entered', facts => facts.draft === 'Thanks again', 5000);
  const sendAt = await centre({ capability: 'bokkie.send' });
  await page.mouse.click(sendAt.x, sendAt.y);
  await answered('the reply to Send', typedFrom + 4);
  step('sent by Return and by a click on Send', { messages: typedFrom + 4 });

  // The navigation follows the page's width: one pane at a time on a
  // phone, the list beside the conversation from 720 points, and the
  // settings beside both from 1040. Nothing is lost or loaded again on the
  // way, and each pane is a region named by its title.
  const kept = await app.facts();
  const resized = [];
  for (const [width, layout, panes, regions] of [
    [800, 'split', 'list,content', ['Bokkie', 'Planner']],
    [1280, 'columns', 'list,content', ['Bokkie', 'Planner']],
    [430, 'stack', 'content', ['Planner']],
  ]) {
    await page.setViewportSize({ width, height: 900 });
    const laid = await app.until(`the ${layout} layout`, facts => facts.layout === layout && facts.panes === panes);
    assert.equal(laid.bot, bot.id, `the Planner open at ${width}`);
    assert.equal(laid.content, 'conversation');
    assert.equal(laid.conversation, 'loaded');
    assert.equal(laid.message_count, kept.message_count, `the conversation kept at ${width}`);
    assert.deepEqual((await app.nodes({ role: 'pane' })).map(node => node.name), regions);
    for (const name of regions) {
      assert.equal(await page.getByRole('region', { name, exact: true }).count(), 1, `the ${name} region at ${width}`);
    }
    // Back is the phone's alone: beside the list there is nowhere to go back.
    assert.equal((await app.nodes({ capability: 'bokkie.back' })).length, layout === 'stack' ? 1 : 0, `Back at ${width}`);
    if (layout === 'columns') {
      // The settings open beside the conversation, which stays in view.
      await app.action('bokkie.bot-settings');
      const detail = await app.until('the settings beside the conversation', facts => facts.detail === 'settings'
        && facts.panes === 'list,content,detail' && facts.routines === 'loaded');
      assert.equal(detail.viewing, bot.id, 'the conversation is still viewed');
      assert.deepEqual((await app.nodes({ role: 'pane' })).map(node => node.name), ['Bokkie', 'Planner', 'Planner settings']);
      await app.action('bokkie.bot-settings');
      await app.until('the settings hidden', facts => facts.detail === '' && facts.panes === panes);
    }
    resized.push({ width, layout, panes });
  }
  step('navigation followed the page width', { layouts: resized });

  // Light and dark, as the system's appearance says, at a phone's width
  // and a desktop's, beside a Bot whose name and label are long and which
  // is pinned, so the phone's list shows the strip of avatars too. The
  // page never scrolls sideways and every control lies within it; each
  // screen's screenshot goes to the evidence, to compare with the visual
  // target.
  // As long as a name and a label may be: 40 characters and 60.
  const LONG_NAME = 'The household\'s meal planner and lists';
  const long = (await api('POST', '/bots', { name: LONG_NAME, label: 'Plans the week\'s dinners around what is in season and cheap', description: 'Plan meals.' }, cookie)).body;
  assert(Number.isInteger(long?.id), 'the long-named Bot was created');
  assert.equal((await api('PATCH', `/bots/${long.id}`, { pinned: true }, cookie)).response.status, 200, 'the long-named Bot pinned');
  const fits = async (at, width) => {
    const page_ = await page.evaluate(() => ({
      scroll: document.scrollingElement.scrollWidth, inner: window.innerWidth,
      canvas: document.getElementById('app-canvas').getBoundingClientRect().right,
    }));
    assert(page_.scroll <= page_.inner && page_.canvas <= page_.inner, `no sideways scroll ${at}: ${JSON.stringify(page_)}`);
    const controls = await app.nodes({ role: 'button' });
    assert(controls.length > 0, `controls ${at}`);
    for (const node of controls) {
      assert(node.rect.min_x >= -0.5 && node.rect.max_x <= width + 0.5, `${node.name} within the page ${at}: ${JSON.stringify(node.rect)}`);
    }
    return controls.length;
  };
  const looks = [];
  const shoot = async (scheme, width, screen) => {
    // Let the page paint the frame the facts describe.
    await wait(300);
    const file = `${scheme}-${width}-${screen}.png`;
    await page.screenshot({ path: join(evidence, file) });
    looks.push({ scheme, width, screen, file, controls: await fits(`${screen}, ${scheme}, ${width}`, width) });
  };
  for (const scheme of ['light', 'dark']) {
    await page.emulateMedia({ colorScheme: scheme });
    for (const [width, height, layout, panes] of [[390, 844, 'stack', 'content'], [1280, 800, 'columns', 'list,content']]) {
      await page.setViewportSize({ width, height });
      await app.until(`the conversation, ${scheme}, at ${width}`, facts => facts.theme === scheme && facts.layout === layout
        && facts.panes === panes && facts.conversation === 'loaded' && facts.bot === bot.id && facts.bot_count === 2);
      await shoot(scheme, width, layout === 'stack' ? 'conversation' : 'list-and-conversation');
      if (layout === 'stack') {
        await app.action('bokkie.back');
        await app.until(`the list, ${scheme}, at ${width}`, facts => facts.panes === 'list' && facts.strip.split(',').includes(String(long.id)));
        await shoot(scheme, width, 'list');
        await app.action('bokkie.open-bot', bot.id);
        await app.until('the Planner again', facts => facts.conversation === 'loaded' && facts.bot === bot.id && facts.panes === 'content');
      }
    }
  }
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.setViewportSize({ width: 430, height: 900 });
  await app.until('dark at a phone\'s width again', facts => facts.theme === 'dark' && facts.layout === 'stack' && facts.panes === 'content');
  step('both appearances at a phone\'s and a desktop\'s width', { looks });

  // A routine, made in the Bot's settings and run now: its run shows in its
  // history as it goes, and Show brings its marker and reply into view.
  const ROUTINE = 'Smoke check';
  await controlRequest('reply', { pieces: ['Routine ', 'done.'], piece_delay_ms: 50 });
  await app.until('the Planner idle', facts => facts.bot_status === 'idle' && facts.reply_state !== 'streaming', 60000);
  await app.action('bokkie.bot-settings');
  await app.until('the settings', facts => facts.screen === 'bot_settings' && facts.routines === 'loaded'
    && facts.routine_count === 0);
  // The sheet opens over the panel, which is out of reach behind it, and
  // Close without saving closes it.
  await app.action('bokkie.new-routine');
  await app.until('the routine sheet', facts => facts.routine_form === 'new');
  const [sheet, ...otherSheets] = await app.nodes({ role: 'dialog', name: 'New routine' });
  assert(sheet && otherSheets.length === 0, 'one routine sheet');
  await assert.rejects(app.action('bokkie.refresh'), /unavailable/, 'the panel is behind the sheet');
  // egui fades a new layer in; let it settle before the picture.
  await wait(500);
  await page.screenshot({ path: join(evidence, 'routine-sheet-dark-430.png') });
  await app.action('bokkie.cancel-routine');
  await app.until('the sheet closed', facts => facts.routine_form === '');
  assert.equal((await app.nodes({ role: 'dialog' })).length, 0, 'no sheet');
  await app.action('bokkie.new-routine');
  await app.until('the routine sheet again', facts => facts.routine_form === 'new');
  await app.setText('bokkie.routine-name', ROUTINE);
  await app.setText('bokkie.routine-prompt', 'Check the smoke.');
  await app.scrollTo({ id: 'bokkie.routine-at', capability: 'stobie.set-text' });
  await app.setText('bokkie.routine-at', '06:45');
  await app.action('bokkie.save-routine');
  await app.until('the routine saved', facts => facts.routine_form === '' && facts.routine_count === 1);
  const { body: { routines: [routine] } } = await api('GET', `/bots/${bot.id}/routines`, undefined, cookie);
  assert.equal(routine.name, ROUTINE);
  assert.equal(routine.prompt, 'Check the smoke.');
  assert.deepEqual(routine.schedule, { repeat: 'daily', at: '06:45' });
  const scheduled = await app.contents();
  assert(scheduled.some(text => text.startsWith('Every day at 06:45 · Next run ')), `the routine's schedule shows: ${JSON.stringify(scheduled)}`);
  await app.action('bokkie.run-routine', routine.id);
  const ran = await app.until('the run to succeed', facts => facts.history === routine.id
    && facts.run_state === 'succeeded' && facts.run_messages >= 2, 60000);
  assert.equal(ran.runs, 1, 'one run in the history');
  // The run's push was recorded and sent to the one subscription; the
  // stand-in's endpoint is a loopback name, so the gateway refused to reach
  // it and recorded the push as failed.
  for (const deadline = Date.now() + 30000; !/sent a push .*cause="?run"? delivered=0 devices=1/.test(plain(gateway.log)); await wait(50)) {
    assert(Date.now() < deadline, 'the run\'s push was sent');
  }
  assert.match(plain(gateway.log), /a push service refused a push .*service=localhost .*is not a public address/);
  assert(!plain(gateway.log).includes('push-stand-in'), 'the gateway\'s log carries no endpoint');
  const database = new DatabaseSync(join(data, 'bokkie.sqlite3'), { readOnly: true });
  const pushes = database.prepare('SELECT bot_id, cause, routine_run_id, state FROM pushes').all();
  const subscriptions = database.prepare('SELECT endpoint FROM push_subscriptions').all();
  database.close();
  assert.deepEqual(pushes.map(row => ({ ...row })), [{ bot_id: bot.id, cause: 'run', routine_run_id: ran.run, state: 'failed' }]);
  assert.deepEqual(subscriptions.map(row => row.endpoint), [PUSH_ENDPOINT]);
  // The Planner is unread, in the list and on the badge, while its
  // settings show.
  const unread = await app.until('the Planner unread', facts => facts.unread === String(bot.id) && facts.badge === 1);
  assert.equal((await api('GET', `/bots/${bot.id}`, undefined, cookie)).body.unread, true);
  assert.equal((await page.evaluate(() => window.__pushStandIn.badges)).at(-1), 1, 'the app badge says one');
  assert.equal(viewingSent().at(-1), null, 'settings view no conversation');
  step('routine push recorded and the Bot unread', { pushes, unread: unread.unread, badge: unread.badge });
  // Beside the settings, the list's row for the Planner is a navigation
  // item named by the Bot, whose description reads its unread state (which
  // paints the unread dot), the run's reply as its preview, and the time
  // of day; the accessibility layer presents it as a button with that
  // description. The settings stay open, so nothing is viewed or read.
  await page.setViewportSize({ width: 800, height: 900 });
  await app.until('the list beside the settings', facts => facts.layout === 'split' && facts.panes === 'list,detail');
  let plannerRow;
  for (const deadline = Date.now() + 10000; ; await wait(50)) {
    [plannerRow] = await app.nodes({ role: 'navigation_item', name: 'Planner' });
    if (plannerRow?.description?.startsWith('Unread; Routine done.; ')) break;
    assert(Date.now() < deadline, `the Planner's row: ${JSON.stringify(plannerRow)}`);
  }
  // The time of day in en-US's 12-hour clock, or Yesterday for a run that
  // midnight overtook.
  assert.match(plannerRow.description, /^Unread; Routine done\.; (\d{1,2}:\d{2} [AP]M|Yesterday); Open the conversation of the Bot with this ID$/);
  assert.equal((await app.nodes({ role: 'navigation_item', name: 'Planner' })).length, 1, 'one row for the Planner');
  const ariaRow = page.getByRole('button', { name: 'Planner', exact: true });
  assert.equal(await ariaRow.count(), 1, 'the accessibility layer presents the row');
  assert.equal(await ariaRow.getAttribute('aria-description'), plannerRow.description);
  assert.equal((await app.facts()).unread, String(bot.id), 'still unread beside the list');
  await page.setViewportSize({ width: 430, height: 900 });
  await app.until('the settings alone', facts => facts.layout === 'stack' && facts.panes === 'detail');
  step('the list\'s row reads the Bot unread', { row: plannerRow.name, says: plannerRow.description });
  await app.action('bokkie.show-run', ran.run);
  const shown = await app.until('the run shown', facts => facts.screen === 'conversation'
    && facts.bot_status === 'idle' && facts.reply_state === 'complete', 30000);
  assert(shown.marker.startsWith(`${ROUTINE} · `) && shown.marker.endsWith(' · test run'), `the marker: ${shown.marker}`);
  assert.equal(shown.reply, 'Routine done.');
  assert.equal(shown.showing_error, '');
  // The view scrolls there smoothly, so wait for the marker and the reply
  // below it to be in view.
  let inView = [];
  for (const deadline = Date.now() + 10000; ; await wait(50)) {
    inView = await app.contents();
    if (inView.some(text => text.startsWith(`Routine run: ${ROUTINE} · `)) && inView.includes('Routine done.')) break;
    assert(Date.now() < deadline, `the run's marker and reply in view: ${JSON.stringify(inView)}`);
  }
  const { body: { runs } } = await api('GET', `/bots/${bot.id}/routines/${routine.id}/runs`, undefined, cookie);
  assert.equal(runs.length, 1);
  assert.equal(runs[0].state, 'succeeded');
  step('routine made and run now', { routine: routine.id, run: ran.run, marker: shown.marker, run_messages: ran.run_messages });
  // The conversation in view, with the page in the foreground, is viewed
  // and read.
  const read = await app.until('the Planner read', facts => facts.unread === '' && facts.badge === 0
    && facts.bot_unread === false && facts.viewing === bot.id);
  assert.equal((await api('GET', `/bots/${bot.id}`, undefined, cookie)).body.unread, false);
  assert.equal((await page.evaluate(() => window.__pushStandIn.badges)).at(-1), 0, 'the app badge cleared');
  assert.equal(viewingSent().at(-1), bot.id, 'the page says it views the Planner');
  step('viewed and read', { read_up_to: read.read_up_to, viewing: viewingSent() });

  // The Bot's panel, from the conversation's header: notes granted with
  // their switch, then the routine run again, calling notes_read before it
  // replies. The panel shows the routine, the run among its recent runs and
  // the call in its activity, as the turn's end reloads it.
  await controlRequest('reply', { pieces: ['Read the notes.'], piece_delay_ms: 0 });
  await app.action('bokkie.bot-settings');
  const opened = await app.until('the panel', facts => facts.screen === 'bot_settings' && facts.routines === 'loaded'
    && facts.routine_count === 1 && facts.recent_runs_state === 'loaded' && facts.tool_calls_state === 'loaded');
  assert.equal(opened.recent_run, ran.run, 'the first run is the newest recent run');
  assert.equal(opened.tool_calls, 0, 'no tool calls yet');
  await app.scrollTo({ capability: 'bokkie.grant-notes' });
  const [notesSwitch, ...otherSwitches] = await app.nodes({ role: 'switch', name: 'Grant notes' });
  assert(notesSwitch && otherSwitches.length === 0, 'one switch for the notes grant');
  await app.action('bokkie.grant-notes');
  await app.until('notes granted', facts => facts.grants === 'notes' && facts.updating === false);
  // The accessibility layer presents the switch, on.
  const ariaSwitch = page.getByRole('switch', { name: 'Grant notes', exact: true });
  for (const deadline = Date.now() + 10000; await ariaSwitch.getAttribute('aria-checked').catch(() => null) !== 'true'; await wait(50)) {
    assert(Date.now() < deadline, 'the notes switch reads on');
  }
  await controlRequest('tool-call', { name: 'notes_read', arguments: {} });
  await app.scrollTo({ capability: 'bokkie.run-routine' }, -200);
  await app.action('bokkie.run-routine', routine.id);
  const again = await app.until('the run and its call in the panel', facts => facts.recent_run > ran.run
    && facts.recent_run_state === 'succeeded' && facts.recent_runs === 2 && facts.tool_calls === 1
    && facts.tool_calls_state === 'loaded' && facts.bot_status === 'idle', 60000);
  const { body: { calls } } = await api('GET', `/bots/${bot.id}/tool-calls?limit=10`, undefined, cookie);
  assert.deepEqual(calls.map(call => [call.tool, call.outcome, call.routine_run_id]), [['notes_read', 'ok', again.recent_run]]);
  // Down the panel, as a person scrolls it: the routine, the run and the
  // call, each in view in turn.
  const inPanel = { routine: false, run: false, call: false };
  await app.scrollTo({ capability: 'bokkie.run-routine' }, -200);
  for (let attempt = 0; attempt < 40 && !(inPanel.routine && inPanel.run && inPanel.call); attempt++) {
    const texts = await app.contents();
    inPanel.routine ||= texts.includes(ROUTINE) && texts.some(text => text.startsWith('Every day at 06:45 · Next run '));
    inPanel.run ||= texts.some(text => text.startsWith('Succeeded · run now · '));
    inPanel.call ||= texts.some(text => /^notes_read · done · \d+ ms · .* · routine run$/.test(text));
    await page.mouse.move(215, 600);
    await page.mouse.wheel(0, 200);
    await wait(100);
  }
  assert.deepEqual(inPanel, { routine: true, run: true, call: true }, 'the panel showed the routine, its run and its call');
  await page.screenshot({ path: join(evidence, 'panel-dark-430.png') });
  step('the Bot\'s panel', { recent_run: again.recent_run, recent_runs: again.recent_runs, tool_calls: again.tool_calls, seen: inPanel });
  // Deleting the routine asks first, in a confirm that names it, with
  // nothing behind it in reach: kept, then deleted.
  await app.scrollTo({ capability: 'bokkie.delete-routine' }, -200);
  await app.action('bokkie.delete-routine', routine.id);
  await app.until('asked before deleting', facts => facts.routine_delete === routine.id);
  const [deleting, ...otherConfirms] = await app.nodes({ role: 'alert_dialog', name: 'Delete this routine?' });
  assert(deleting && otherConfirms.length === 0, 'one confirm');
  assert(deleting.description?.includes(`"${ROUTINE}"`), `the confirm names the routine: ${deleting.description}`);
  await assert.rejects(app.action('bokkie.new-routine'), /unavailable/, 'the panel is behind the confirm');
  await app.action('bokkie.keep-routine');
  await app.until('the routine kept', facts => facts.routine_delete === 0 && facts.routine_count === 1);
  assert.equal((await api('GET', `/bots/${bot.id}/routines`, undefined, cookie)).body.routines.length, 1, 'kept');
  await app.scrollTo({ capability: 'bokkie.delete-routine' }, -200);
  await app.action('bokkie.delete-routine', routine.id);
  await app.until('asked again', facts => facts.routine_delete === routine.id);
  await app.action('bokkie.confirm-delete-routine');
  await app.until('the routine deleted', facts => facts.routine_delete === 0 && facts.routine_count === 0, 30000);
  assert.equal((await api('GET', `/bots/${bot.id}/routines`, undefined, cookie)).body.routines.length, 0, 'deleted');
  step('the routine deleted after its confirm', { kept: true, deleted: routine.id });
  // Closed, the conversation is in view again and read.
  await app.action('bokkie.bot-settings');
  await app.until('the Planner read again', facts => facts.screen === 'conversation' && facts.unread === ''
    && facts.badge === 0 && facts.viewing === bot.id);

  // A tapped notification opens its Bot: Chromium cannot click a system
  // notification under automation, so the worker's own openRoute, which its
  // notificationclick handler calls, posts the route to the open page.
  await app.action('bokkie.back');
  await app.until('the list', facts => facts.screen === 'signed_in');
  const serviceWorker = context.serviceWorkers().find(candidate => candidate.url() === `${origin}/sw.js`);
  assert(serviceWorker, 'the service worker is running');
  await serviceWorker.evaluate(route => openRoute(route), `${origin}/?bot=${bot.id}`);
  await app.until('the routed Bot', facts => facts.screen === 'conversation' && facts.bot === bot.id
    && facts.conversation === 'loaded');
  step('tapped notification opened its Bot', { route: `/?bot=${bot.id}` });

  // Notifications off: the device unsubscribes and the gateway forgets it.
  await app.action('bokkie.back');
  await app.until('the list', facts => facts.screen === 'signed_in');
  await app.scrollTo({ capability: 'bokkie.notifications' });
  await app.action('bokkie.notifications');
  await app.until('notifications off', facts => facts.push === 'off' && facts.push_saved === false);
  assert.deepEqual(await pageApi('/push/subscription'), { subscribed: false }, 'the gateway forgot the subscription');
  assert.equal(await page.evaluate(() => localStorage.getItem('push-stand-in')), null, 'the device unsubscribed');
  await app.scrollTo({ capability: 'bokkie.open-bot' }, -200);
  await app.action('bokkie.open-bot', bot.id);
  await app.until('the Planner once more', facts => facts.conversation === 'loaded' && facts.bot === bot.id);
  step('notifications switched off');

  // Load older: a conversation longer than the newest page of 50 messages.
  // Its first 26 exchanges are sent over the API, each with a short reply.
  await controlRequest('reply', { pieces: ['Noted.'], piece_delay_ms: 0 });
  const diary = (await api('POST', '/bots', { name: 'Diary', label: 'Keeps notes', description: 'Note what is said.' }, cookie)).body;
  assert(Number.isInteger(diary?.id), 'the Diary was created');
  const settled = async () => {
    for (let attempt = 0; ; attempt++) {
      assert(attempt < 1200, 'the Diary settled');
      const [{ body: state }, { body: latest }] = await Promise.all([
        api('GET', `/bots/${diary.id}`, undefined, cookie),
        api('GET', `/bots/${diary.id}/messages?limit=1`, undefined, cookie),
      ]);
      const last = latest.messages.at(-1);
      if (state.status === 'idle' && last?.author === 'bot' && last.state === 'complete') return;
      await wait(25);
    }
  };
  const SEEDED = 26;
  for (let n = 1; n <= SEEDED; n++) {
    const { body: sent } = await api('POST', `/bots/${diary.id}/messages`, { text: `Note ${n}` }, cookie);
    assert.equal(sent?.delivery, 'new_turn', `note ${n} started a turn`);
    await settled();
  }
  await app.action('bokkie.back');
  await app.until('the list with the Diary', facts => facts.screen === 'signed_in' && facts.bot_count === 3);
  await app.action('bokkie.open-bot', diary.id);
  const newest = await app.until('the Diary\'s newest page', facts => facts.conversation === 'loaded' && facts.bot === diary.id);
  assert.equal(newest.message_count, 50, 'the newest page of 50');
  assert.equal(newest.older, 'more', 'older messages to load');
  // A reply streams while the older page loads, and both come out whole.
  await controlRequest('reply', { pieces: PIECES, piece_delay_ms: PIECE_MS });
  await send('One last note');
  await app.until('the Diary replying', facts => facts.reply_state === 'streaming' && facts.reply_chars > 0, 60000);
  await app.action('bokkie.load-older');
  const loaded = await app.until('the older page', facts => facts.older === '' && facts.message_count === SEEDED * 2 + 2, 30000);
  assert.equal(loaded.reply_state, 'streaming', 'the older page arrived during the reply');
  const finished = await app.until('the Diary\'s reply to complete', facts => facts.reply_state === 'complete' && facts.bot_status === 'idle', 60000);
  assert.equal(finished.reply, REPLY, 'the reply lost or repeated nothing');
  const { body: stored } = await api('GET', `/bots/${diary.id}/messages?limit=200`, undefined, cookie);
  assert.equal(stored.older, null);
  assert.equal(finished.message_count, stored.messages.length, 'the page holds the whole stored conversation');
  step('older messages loaded during a reply', {
    newest_page: newest.message_count, after_load: loaded.message_count, chars_when_loaded: loaded.reply_chars,
    stored: stored.messages.length,
  });
  await app.action('bokkie.back');
  await app.until('the list again', facts => facts.screen === 'signed_in');
  await app.action('bokkie.open-bot', bot.id);
  await app.until('the Planner again', facts => facts.conversation === 'loaded' && facts.bot === bot.id);

  // The subscription meter, then the limit-reached banner, from windows the
  // mock reports in its response headers.
  const resetsAt = Math.floor(Date.now() / 1000) + 3 * 3600;
  await controlRequest('rate-limits', {
    primary: { used_percent: 42, window_minutes: 300, resets_at: resetsAt },
    secondary: { used_percent: 7, window_minutes: 10080, resets_at: resetsAt + 86400 },
  });
  await controlRequest('reply', { pieces: ['Short ', 'reply.'], piece_delay_ms: 50 });
  const countBefore = (await app.facts()).message_count;
  await send('How much is used?');
  // Codex reports the windows as the response starts, before its reply
  // exists, so wait for this turn's own reply.
  const meter = await app.until('the meter at 42% after the reply', facts => facts.subscription_used === 42
    && facts.message_count === countBefore + 2 && facts.reply === 'Short reply.' && facts.reply_state === 'complete'
    && facts.bot_status === 'idle', 60000);
  assert.equal(meter.limit, '', 'no banner below the limit');
  await controlRequest('rate-limits', {
    primary: { used_percent: 100, window_minutes: 300, resets_at: resetsAt },
    secondary: null,
  });
  await send('And now?');
  const reached = await app.until('the limit-reached banner', facts => facts.subscription_used === 100 && facts.limit !== '', 60000);
  assert.match(reached.limit, /^The shared ChatGPT subscription has reached its limit\. Bots can reply again in /);
  step('subscription meter and banner', { used: meter.subscription_used, banner: reached.limit });

  // The admin invites a member, with permissions, from the accounts screen.
  await controlRequest('rate-limits', {
    primary: { used_percent: 42, window_minutes: 300, resets_at: resetsAt },
    secondary: null,
  });
  await controlRequest('reply', { pieces: ['Hello ', 'there.'], piece_delay_ms: 50 });
  await app.action('bokkie.back');
  await app.until('the list', facts => facts.screen === 'signed_in');
  // The accounts are at the bottom of the list.
  await app.scrollTo({ capability: 'bokkie.admin' });
  await app.action('bokkie.admin');
  await app.until('the accounts', facts => facts.screen === 'admin' && facts.accounts === 1);
  // A member who may create Bots but not edit their profiles.
  await app.action('bokkie.invite-may-edit-profiles');
  await app.setText('bokkie.invite-name', 'Smoke Member');
  await app.action('bokkie.create-invitation');
  await app.until('the code shown', facts => facts.invitation_shown === true && facts.invitations === 1);
  // The code is on the page, which a person reads off it, and never a fact.
  // The code reaches the admin only by Copy: an agent's copy, as a person's
  // press, puts it on the clipboard, read back here.
  await page.evaluate(() => navigator.clipboard.writeText(''));
  const [copyCode, ...otherCopies] = await app.nodes({ role: 'button', name: 'Copy invitation code' });
  assert(copyCode && otherCopies.length === 0, 'one Copy for the code');
  await app.invoke({ id: copyCode.id, capability: 'stobie.copy-text' });
  const invitationCode = await clipboardText(page, text => /^[A-Za-z0-9_-]{43}$/.test(text));
  assert(!JSON.stringify(await app.request({ op: 'observe' })).includes(invitationCode), 'the code is in no observation');
  step('admin invited a member', { invitations: 1 });

  // The member joins in a second browser context, with its own cookies.
  memberContext = await chromium.launchPersistentContext(join(scratch, 'member-profile'), {
    headless: false,
    args: ['--ozone-platform=x11', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
    viewport: { width: 430, height: 900 },
    locale: 'en-US',
    colorScheme: 'light',
  });
  const memberPage = memberContext.pages()[0] ?? await memberContext.newPage();
  await watch(memberPage);
  await (await memberContext.newCDPSession(memberPage)).send('Emulation.setFocusEmulationEnabled', { enabled: true });
  await memberPage.goto(`${origin}/?automation=1`);
  await memberPage.waitForFunction(() => window.__STOBIE_STARTUP?.status === 'ready', null, { timeout: 60000 });
  const member = new Automation(memberPage);
  await member.hello();
  await member.until('the member\'s sign-in screen', facts => facts.screen === 'signed_out');
  await member.action('bokkie.join');
  await member.until('the join form', facts => facts.joining === true);
  await member.setText('bokkie.invitation-code', invitationCode);
  await member.setText('bokkie.username', 'member');
  await member.setText('bokkie.display-name', 'Smoke Member');
  const memberPassword = `member ${randomUUID()}`;
  await memberPage.bringToFront();
  // Enter in the password field joins, after every key typed before it, so
  // the account gets the whole password.
  await typeInto('Password', memberPassword, memberPage);
  await memberPage.keyboard.press('Enter');
  const joined = await member.until('the member signed in and live', facts => facts.user === 'member'
    && facts.connection === 'live' && facts.bots === 'loaded' && facts.chatgpt === 'signed_in', 30000);
  assert.equal(joined.role, 'member');
  assert.equal(joined.may_create_bots, true);
  assert.equal(joined.may_edit_profiles, false, 'the invitation\'s permissions');
  assert.equal(joined.oversight, false);
  assert.equal(joined.theme, 'light', 'the member\'s page follows its system\'s light appearance');
  // Household status only: signed in, never as whom.
  const memberTexts = await pageTexts(memberPage);
  assert(memberTexts.includes('ChatGPT: signed in for the household'), 'the household status');
  assert(!memberTexts.some(text => text.includes('@')), 'no email on the member\'s page');
  await memberPage.screenshot({ path: join(evidence, 'member-light-430.png') });
  step('member joined in a second context', { role: joined.role, chatgpt: joined.chatgpt, theme: joined.theme });

  // The member makes a Bot, as the invitation allows.
  await member.setText('bokkie.bot-name', 'Helper');
  await member.until('the name entered', facts => facts.new_bot_name === 'Helper', 5000);
  await member.action('bokkie.create-bot');
  await member.until('the member\'s Bot', facts => facts.bot_count === 1 && facts.creating === false, 30000);
  assert((await pageTexts(memberPage)).includes('Create Bot'), 'New Bot is on the member\'s page');

  // The admin sets the member's limit and permissions, which reach the
  // member's page live.
  const { body: listed } = await api('GET', '/admin/users', undefined, cookie);
  const memberId = listed.users.find(entry => entry.user.username === 'member').user.id;
  await page.bringToFront();
  // The list shows the new account once loaded again.
  await app.action('bokkie.refresh');
  await app.until('the new account listed', facts => facts.accounts === 2 && facts.invitations === 0);
  await app.action('bokkie.open-account', memberId);
  await app.until('the member\'s account', facts => facts.screen === 'account' && facts.account === memberId);
  await app.setText('bokkie.daily-turns', '1');
  await app.action('bokkie.save-limits');
  await app.until('one turn a day', facts => facts.account_daily_turns === 1 && facts.account_saving === false);
  await app.action('bokkie.may-create-bots');
  await app.until('may not create Bots', facts => facts.account_may_create_bots === false && facts.account_saving === false);
  const limited = await member.until('the permission taken back live', facts => facts.may_create_bots === false, 30000);
  assert(!(await pageTexts(memberPage)).includes('Create Bot'), 'New Bot is gone from the member\'s page');
  step('admin set limits and permissions', { daily_turns: 1, may_create_bots: limited.may_create_bots });

  // The member's one turn runs; the next is refused beyond the limit.
  const memberSession = await api('POST', '/session', { username: 'member', password: memberPassword });
  assert.equal(memberSession.response.status, 200, 'the member\'s API sign-in');
  const memberCookie = memberSession.response.headers.get('set-cookie').split(';')[0];
  const helper = (await api('GET', '/bots', undefined, memberCookie)).body.bots[0];
  await member.action('bokkie.open-bot', helper.id);
  await member.until('the Helper', facts => facts.conversation === 'loaded' && facts.bot === helper.id);
  await member.compose('Hello');
  await member.until('the draft entered', facts => facts.draft === 'Hello', 5000);
  await member.action('bokkie.send', 'Hello');
  await member.until('the member\'s reply', facts => facts.reply === 'Hello there.' && facts.reply_state === 'complete'
    && facts.bot_status === 'idle', 60000);
  await member.compose('And again');
  await member.until('the second draft entered', facts => facts.draft === 'And again', 5000);
  await member.action('bokkie.send', 'And again');
  const refused = await member.until('the send refused', facts => facts.sending === false && facts.send_error !== '', 30000);
  assert.equal(refused.send_error, 'You have used your allowance for now. It refills over the next day or week');
  assert.equal(refused.draft, 'And again', 'the draft stays');
  assert.equal(refused.message_count, 2, 'nothing more was stored');
  step('member refused beyond the limit', { send_error: refused.send_error });

  // Oversight, switched on while the member is signed in, shows at once.
  await app.action('bokkie.oversight');
  await app.until('oversight on', facts => facts.account_oversight === true && facts.account_saving === false);
  const overseen = await member.until('the oversight notice', facts => facts.oversight === true, 30000);
  const noticeTexts = await pageTexts(memberPage);
  const notice = noticeTexts.find(text => text.startsWith('Oversight is on: an admin can read all your Bots and conversations'));
  assert(notice, `the oversight notice is on the member's page: ${JSON.stringify(noticeTexts)}`);
  // The admin reads the conversation, read-only.
  await app.until('every Bot under oversight', facts => facts.member_bots === 1 && facts.account_saving === false);
  await app.action('bokkie.read-conversation', helper.id);
  const reading = await app.until('the conversation read', facts => facts.screen === 'overseen' && facts.reading === helper.id
    && facts.reading_count === 2, 30000);
  step('oversight notice and read-only view', { notice, oversight: overseen.oversight, read: reading.reading_count });

  // Signing the household out of ChatGPT signs out every account's Bots, so
  // it is refused until confirmed: asked and kept, then asked and confirmed.
  await app.action('bokkie.household');
  await app.until('the household screen', facts => facts.screen === 'household' && facts.chatgpt === 'signed_in');
  await assert.rejects(app.action('bokkie.confirm-sign-out-chatgpt'), /unavailable/, 'refused before asking');
  await app.action('bokkie.sign-out-chatgpt');
  await app.until('asked before signing out', facts => facts.chatgpt_sign_out === 'confirming');
  const [signingOut, ...otherAsks] = await app.nodes({ role: 'alert_dialog', name: 'Sign out everyone?' });
  assert(signingOut && otherAsks.length === 0, 'one confirm');
  assert.match(signingOut.description ?? '', /signs out every account's Bots/);
  await assert.rejects(app.action('bokkie.refresh-bots'), /unavailable/, 'the screen is behind the confirm');
  await app.action('bokkie.keep-chatgpt');
  await app.until('kept signed in', facts => facts.chatgpt_sign_out === '' && facts.chatgpt === 'signed_in');
  assert.equal((await api('GET', '/codex/account', undefined, cookie)).body.signed_in, true, 'still signed in');
  await app.action('bokkie.sign-out-chatgpt');
  await app.until('asked again', facts => facts.chatgpt_sign_out === 'confirming');
  await app.action('bokkie.confirm-sign-out-chatgpt');
  await app.until('the household signed out', facts => facts.chatgpt === 'signed_out' && facts.chatgpt_sign_out === '', 30000);
  assert.equal((await api('GET', '/codex/account', undefined, cookie)).body.signed_in, false, 'signed out');
  step('the household signed out after its confirm', { kept: true, signed_out: true });
  for (const each of [page, memberPage]) {
    assert.deepEqual(await each.evaluate(() => window.__cspViolations), [], 'CSP violations');
  }

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

  // Every app server ran on the synthetic household sign-in, which needed
  // no refresh, and asked no host but the mocks.
  const mocked = await controlRequest('requests');
  assert.deepEqual(mocked.refused, [], 'hosts the app servers tried to reach');
  assert.equal(mocked.refreshes, 0, 'refreshes');
  step('nothing left the machine', { model_requests: mocked.count });
  failed = false;
} finally {
  report.passed = !failed;
  if (failed) await context?.pages()[0]?.screenshot({ path: join(evidence, 'failure.png') }).catch(() => {});
  report.duration_ms = Date.now() - started;
  await memberContext?.close().catch(() => {});
  await context?.close().catch(() => {});
  for (const close of closers) close();
  // SIGINT, which the gateway and the mock take as a request to shut down:
  // the gateway then stops its app servers before it exits.
  for (const { child } of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGINT');
  }
  // A process that ignores SIGINT is killed after 20 s.
  const stubborn = setTimeout(() => {
    for (const { child } of children) if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  }, 20000);
  await Promise.all(children.map(({ exited }) => exited));
  clearTimeout(stubborn);
  for (const { name, log } of children) await writeFile(join(evidence, `${name}.log`), log.join(''));
  await writeFile(join(evidence, 'smoke.json'), `${JSON.stringify(report, null, 2)}\n`);
  await rm(scratch, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
}
console.log(`smoke: passed in ${((Date.now() - started) / 1000).toFixed(1)} s; evidence ${evidence}`);
