/**
 * Fresh-browser authenticated manifest/classic-worker regression through the
 * actual deploy/manage.py nginx renderer. Requires Python, Docker, OpenSSL and
 * Playwright; BOKKIE_AUTH_SSH optionally selects a disposable remote Docker host.
 * Set BOKKIE_AUTH_NGINX_IMAGE and BOKKIE_AUTH_RUNTIME_IMAGE to immutable local
 * image IDs (the latter must contain Node). No image pull, production mount,
 * provider enrolment, push event, notification permission or model call occurs.
 * BOKKIE_AUTH_EVIDENCE selects a retained report/fixture directory.
 *
 * The renderer receives a literal loopback authority instead of a production
 * hostname. This calls render(), not load()/run(): the production hostname and
 * host-policy qualification are separate. All generated nginx routes/authentication
 * settings are used unchanged. A synthetic static/API upstream replaces Bokkie;
 * API mutation-token/Origin enforcement requires separate real-kernel evidence.
 */
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { cp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { request as httpRequest } from 'node:http';
import { createServer } from 'node:net';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = process.cwd(), runId = randomUUID();
const evidence = resolve(process.env.BOKKIE_AUTH_EVIDENCE ?? `/tmp/bokkie-authenticated-ingress-${runId}`);
const fixture = join(evidence, 'fixture'), assets = join(fixture, 'ui');
const remote = process.env.BOKKIE_AUTH_SSH;
const nginxImage = process.env.BOKKIE_AUTH_NGINX_IMAGE;
const runtimeImage = process.env.BOKKIE_AUTH_RUNTIME_IMAGE;
for (const [name, value] of Object.entries({ BOKKIE_AUTH_NGINX_IMAGE: nginxImage, BOKKIE_AUTH_RUNTIME_IMAGE: runtimeImage })) {
  if (!/^sha256:[a-f0-9]{64}$/.test(value ?? '')) throw Error(`${name} must be an existing immutable Docker image ID`);
}
if (remote && !/^[A-Za-z0-9_.@-]+$/.test(remote)) throw Error('BOKKIE_AUTH_SSH must be one SSH host alias');
await mkdir(assets, { recursive: true, mode: 0o755 });
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const report = {
  source: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8', cwd: root }).trim(),
  tracked_diff_sha256: hash(execFileSync('git', ['diff', '--binary'], { cwd: root })),
  started_at: new Date().toISOString(), run_id: runId, docker_host: remote ?? 'local',
  images: { nginx: nginxImage, runtime: runtimeImage }, inputs: {}, checks: [], errors: [], passed: false,
  model_calls: 0, provider_attempts: 0, push_events: 0, notifications: 0,
  limitations: [
    'Literal HTTP loopback is a browser secure context; production HTTPS, DNS and device behaviour need separate qualification.',
    'Actual generated nginx Basic-auth routes are used, with a synthetic static/API upstream and a fixture bootstrap in place of the graphical UI.',
    'Only the imported core gets a fixture marker; the service-worker entry script and setup registration code retain their candidate bytes.',
    'Renderer load/run validation, the production launcher and real Bokkie mutation boundaries are not exercised.',
  ], cleanup: {},
};
const check = (condition, label) => { if (!condition) throw Error(label); report.checks.push(label); };
function command(executable, args, input = null, timeout = 60000) {
  return new Promise((accept, reject) => {
    const child = spawn(executable, args, { cwd: root, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    const timer = setTimeout(() => child.kill('SIGKILL'), timeout);
    child.stdout.on('data', data => { stdout += data; });
    child.stderr.on('data', data => { stderr += data; });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('close', code => {
      clearTimeout(timer);
      if (code === 0) accept(stdout.trim());
      else reject(Error(`${executable} exited ${code}: ${stderr.trim() || stdout.trim()}`));
    });
    child.stdin.end(input);
  });
}
const quote = value => `'${String(value).replaceAll("'", "'\\''")}'`;
const hostCommand = (args, input = null) => remote
  ? command('ssh', ['-o', 'BatchMode=yes', remote, args.map(quote).join(' ')], input)
  : command(args[0], args.slice(1), input);
const docker = (...args) => hostCommand(['docker', ...args]);
const upstream = `import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
const types = { html: 'text/html', js: 'text/javascript', mjs: 'text/javascript',
  css: 'text/css', webmanifest: 'application/manifest+json', png: 'image/png', svg: 'image/svg+xml' };
createServer(async (request, response) => {
  const path = new URL(request.url, 'http://127.0.0.1').pathname;
  const row = { path, method: request.method, destination: request.headers['sec-fetch-dest'] ?? null,
    worker: request.headers['service-worker'] ?? null, cache_control: request.headers['cache-control'] ?? null,
    authorization_present: !!request.headers.authorization, time: new Date().toISOString() };
  let body, status = 200, type;
  if (request.method === 'GET' && path === '/notifications/push') {
    body = JSON.stringify({ configured: false, configuration_revision: 0, device: { active: false } });
    type = 'application/json';
  } else if (request.method === 'GET' && path.startsWith('/ui/') && !path.includes('..')) {
    try { const name = path === '/ui/' ? '/ui/index.html' : path;
      body = await readFile('/fixture' + name); type = types[name.split('.').at(-1)] ?? 'application/octet-stream';
    } catch { status = 404; body = 'Fixture file missing'; type = 'text/plain'; }
  } else { status = 404; body = 'Synthetic peer has no mutation or model/provider path'; type = 'text/plain'; }
  row.status = status; console.log(JSON.stringify(row));
  response.writeHead(status, { 'Content-Type': type,
    'Cache-Control': path.endsWith('.js') ? 'public, max-age=3600' : 'no-store' });
  response.end(body);
}).listen(7744, '127.0.0.1', () => console.log(JSON.stringify({ ready: true })));
`;
const bootstrap = `import { createPushSetup, browserEnvironment } from './push-setup.js';
window.fixtureSetup = createPushSetup(browserEnvironment(window));
window.fixtureReady = window.fixtureSetup.refresh();
window.fixtureRegistration = () => navigator.serviceWorker.getRegistration('/ui/');
window.fixtureMarker = async () => {
  const registration = await window.fixtureRegistration();
  return new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    const timeout = setTimeout(() => reject(Error('Worker marker did not execute')), 5000);
    channel.port1.onmessage = event => { clearTimeout(timeout); channel.port1.close(); resolve(event.data); };
    registration.active.postMessage({ type: 'bokkie-auth-fixture-marker' }, [channel.port2]);
  });
};
`;
const marker = version => `\n// Disposable ingress-regression marker; candidate core above is unchanged.\n
if (typeof ServiceWorkerGlobalScope !== 'undefined' && self instanceof ServiceWorkerGlobalScope) {
  self.addEventListener('message', event => {
    if (event.data?.type === 'bokkie-auth-fixture-marker') event.ports[0]?.postMessage({
      version: ${JSON.stringify(version)}, frozen: Object.isFrozen(self.BokkiePushWorker),
      task_url: self.BokkiePushWorker.taskURL('task-11111111-1111-4111-8111-111111111111', self.location.origin),
    });
  });
}\n`;
let hostRoot, runtimeId, edgeId, tunnel, browser, context;
const name = `bokkie-auth-regression-${runId}`;
let runtimeLog = '';
try {
  const portServer = createServer();
  portServer.listen(0, '127.0.0.1'); await once(portServer, 'listening');
  const localPort = portServer.address().port;
  await new Promise(accept => portServer.close(accept));
  report.origin = `http://127.0.0.1:${localPort}`;
  const sourceAssets = join(root, 'apps/bokkie-attention-ui/web');
  for (const file of ['index.html', 'manifest.webmanifest', 'service-worker.js', 'push-worker-core.js', 'push-worker.js', 'push-setup.js', 'styles.css']) {
    const bytes = await readFile(join(sourceAssets, file));
    report.inputs[file] = hash(bytes); await writeFile(join(assets, file), bytes);
  }
  await cp(join(sourceAssets, 'icons'), join(assets, 'icons'), { recursive: true });
  const core = await readFile(join(assets, 'push-worker-core.js'), 'utf8');
  await writeFile(join(assets, 'push-worker-core.js'), core + marker('initial'));
  report.fixture_core_initial_sha256 = hash(core + marker('initial'));
  await writeFile(join(assets, 'bootstrap.js'), bootstrap);
  await writeFile(join(assets, 'module-probe.js'), "import './push-worker-core.js';\n");
  await writeFile(join(fixture, 'upstream.mjs'), upstream);
  const password = randomBytes(24).toString('hex');
  const passwordHash = await command('openssl', ['passwd', '-apr1', '-stdin'], password + '\n');
  // Only generated disposable credentials, readable by capability-free nginx.
  await writeFile(join(fixture, 'web-auth'), `bokkie-fixture:${passwordHash}\n`, { mode: 0o644 });
  const config = { name: 'bokkie-authfixture', source: report.source, image: runtimeImage, edge_image: nginxImage,
    hostname: `127.0.0.1:${localPort}`, uid: 10001, gid: 10001, data: fixture,
    web_auth: join(fixture, 'web-auth'), codex_auth: null, conversation_profile: null,
    notification_config: null, push_config: null };
  await writeFile(join(fixture, 'release.json'), JSON.stringify(config, null, 2));
  const renderer = join(root, 'deploy/manage.py');
  report.inputs['deploy/manage.py'] = hash(await readFile(renderer));
  report.inputs['tools/ui-authenticated-push.mjs'] = hash(await readFile(new URL(import.meta.url)));
  await command('python3', ['-c',
    'import importlib.util,json,sys; from pathlib import Path; spec=importlib.util.spec_from_file_location("bokkie_fixture_renderer",sys.argv[1]); module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); root=Path(sys.argv[2]); module.render(json.loads((root/"release.json").read_text()),root)',
    renderer, fixture]);
  report.nginx_config_sha256 = hash(await readFile(join(fixture, 'nginx.conf')));
  if (remote) {
    const allocatedRoot = await hostCommand(['mktemp', '-d', `/tmp/${name}.XXXXXX`]);
    check(new RegExp(`^/tmp/${name}\\.[A-Za-z0-9]+$`).test(allocatedRoot), 'Owned remote fixture directory has the expected unique path');
    hostRoot = allocatedRoot;
    await command('scp', ['-q', '-r', fixture, `${remote}:${hostRoot}/fixture`]);
  } else hostRoot = evidence;
  const hostFixture = join(hostRoot, 'fixture');
  // Namespace owner publishes only a dynamic host-loopback ingress port.
  runtimeId = await docker('run', '-d', '--pull=never', '--name', `${name}-upstream`,
    '--label', `bokkie.auth-regression=${runId}`, '--read-only', '--cap-drop=ALL',
    '--security-opt=no-new-privileges', '--memory=128m', '--cpus=1', '--pids-limit=64',
    '--tmpfs', '/tmp:rw,noexec,nosuid,size=16m', '--publish', `127.0.0.1:${remote ? '' : localPort}:8080`,
    '--mount', `type=bind,src=${hostFixture},dst=/fixture,readonly`,
    '--entrypoint', 'node', runtimeImage, '/fixture/upstream.mjs');
  edgeId = await docker('run', '-d', '--pull=never', '--name', `${name}-edge`,
    '--label', `bokkie.auth-regression=${runId}`, '--network', `container:${runtimeId}`,
    '--read-only', '--cap-drop=ALL', '--security-opt=no-new-privileges', '--memory=64m', '--cpus=1', '--pids-limit=32',
    '--tmpfs', '/tmp:rw,noexec,nosuid,size=16m',
    '--mount', `type=bind,src=${join(hostFixture, 'nginx.conf')},dst=/etc/nginx/nginx.conf,readonly`,
    '--mount', `type=bind,src=${join(hostFixture, 'web-auth')},dst=/run/bokkie-web-auth,readonly`,
    '--entrypoint', 'nginx', nginxImage, '-c', '/etc/nginx/nginx.conf', '-g', 'daemon off;');
  report.containers = { runtime: runtimeId, edge: edgeId };
  const published = await docker('port', runtimeId, '8080/tcp');
  check(/^127\.0\.0\.1:\d+$/.test(published), 'Disposable ingress is published on host loopback only');
  report.published = published;
  if (remote) {
    tunnel = spawn('ssh', ['-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes', '-N',
      '-L', `127.0.0.1:${localPort}:${published}`, remote], { stdio: ['ignore', 'ignore', 'pipe'] });
    tunnel.stderr.on('data', data => { report.tunnel_diagnostics = (report.tunnel_diagnostics ?? '') + data; });
  }
  async function until(observe, predicate, label, milliseconds = 15000) {
    const end = Date.now() + milliseconds; let value;
    while (Date.now() < end) {
      value = await observe(); if (predicate(value)) return value;
      await new Promise(accept => setTimeout(accept, 100));
    }
    throw Error(`Finite wait failed: ${label}; final observation ${JSON.stringify(value)}`);
  }
  await until(async () => { try { return (await fetch(report.origin + '/ui/', { signal: AbortSignal.timeout(1000) })).status; } catch { return null; } }, status => status === 401, 'generated ingress readiness');
  report.anonymous = [];
  const protectedPaths = ['/ui/', '/ui/manifest.webmanifest', '/ui/service-worker.js', '/ui/push-worker-core.js',
    '/ui/push-worker.js', '/ui/push-setup.js', '/bootstrap', '/notifications/push', '/tasks', '/conversations'];
  for (const path of protectedPaths) {
    const response = await fetch(report.origin + path);
    report.anonymous.push({ path, method: 'GET', status: response.status });
    check(response.status === 401 && response.headers.get('www-authenticate') === 'Basic realm="Bokkie"', `Anonymous GET ${path} receives the nginx Basic-auth challenge`);
  }
  for (const path of ['/notifications/push/register', '/notifications/push/disable', '/notifications/push/receipts', '/conversations/turn']) {
    const response = await fetch(report.origin + path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
    report.anonymous.push({ path, method: 'POST', status: response.status });
    check(response.status === 401, `Anonymous POST ${path} stays protected`);
  }
  report.wrong_host_status = await new Promise((accept, reject) => {
    const request = httpRequest(report.origin + '/ui/', { headers: { Host: 'wrong.invalid' } }, response => {
      response.resume(); accept(response.statusCode);
    });
    request.on('error', reject); request.setTimeout(5000, () => request.destroy(Error('Wrong-Host probe timed out'))); request.end();
  });
  check(report.wrong_host_status === 421, 'Generated ingress rejects a different Host before authentication');
  const moduleName = process.env.BOKKIE_PLAYWRIGHT_MODULE ?? 'playwright';
  const { chromium } = await import(moduleName.startsWith('/') ? pathToFileURL(moduleName).href : moduleName);
  browser = await chromium.launch({ headless: true, channel: 'chromium', args: ['--no-sandbox', '--disable-background-networking'] });
  report.browser = browser.version(); report.playwright_module = moduleName;
  context = await browser.newContext({ serviceWorkers: 'allow' });
  check(context.serviceWorkers().length === 0 && (await context.cookies()).length === 0, 'New browser context starts without a worker, cookies or reused profile');
  const page = await context.newPage(), cdp = await context.newCDPSession(page);
  const challenges = [], authChallenges = [], versions = new Map(), registrations = new Map(), activatedVersions = new Set();
  cdp.on('Network.responseReceivedExtraInfo', event => {
    if (event.statusCode === 401) challenges.push({ request_id: event.requestId, status: event.statusCode,
      challenge: event.headers['WWW-Authenticate'] ?? event.headers['www-authenticate'] });
  });
  cdp.on('ServiceWorker.workerVersionUpdated', event => {
    for (const version of event.versions) {
      versions.set(version.versionId, version);
      if (version.status === 'activated') activatedVersions.add(version.versionId);
    }
    report.worker_versions = [...versions.values()];
    report.activated_version_ids = [...activatedVersions];
  });
  cdp.on('ServiceWorker.workerRegistrationUpdated', event => {
    for (const registration of event.registrations) registrations.set(registration.registrationId, registration);
    report.worker_registrations = [...registrations.values()];
  });
  cdp.on('Fetch.requestPaused', event => {
    void cdp.send('Fetch.continueRequest', { requestId: event.requestId }).catch(error => report.errors.push(`Request continuation: ${error}`));
  });
  cdp.on('Fetch.authRequired', event => {
    authChallenges.push(event.authChallenge);
    void cdp.send('Fetch.continueWithAuth', { requestId: event.requestId,
      authChallengeResponse: { response: 'ProvideCredentials', username: 'bokkie-fixture', password } })
      .catch(error => report.errors.push(`Authentication continuation: ${error}`));
  });
  await cdp.send('Network.enable'); await cdp.send('ServiceWorker.enable');
  // Answer the actual HTTP challenge, without preemptive headers or saved login.
  await cdp.send('Fetch.enable', { handleAuthRequests: true });
  await page.goto(report.origin + '/ui/', { waitUntil: 'load' });
  await page.evaluate(() => window.fixtureReady);
  await page.waitForFunction(() => window.fixtureSetup.snapshot().ready && navigator.serviceWorker.controller?.state === 'activated');
  check(authChallenges.some(value => value.source === 'Server' && value.scheme === 'basic' && value.realm === 'Bokkie'),
    'Fresh browser navigation negotiates a normal HTTP Basic-auth challenge');
  report.browser_challenges = authChallenges; report.network_401_responses = challenges;
  const manifest = await cdp.send('Page.getAppManifest'); report.manifest = manifest;
  check(!!manifest.data && JSON.parse(manifest.data).name === 'Bokkie' && manifest.manifest?.name === 'Bokkie'
    && manifest.manifest.display === 'kStandalone' && manifest.manifest.startUrl === report.origin + '/ui/'
    && !manifest.errors.some(error => error.critical), 'Chromium recognises the authenticated manifest as the Bokkie standalone app');
  report.initial = await page.evaluate(async () => {
    const registration = await window.fixtureRegistration(); window.fixtureOriginalWorker = registration.active;
    window.fixtureUpdates = 0; registration.addEventListener('updatefound', () => window.fixtureUpdates++);
    return { setup: window.fixtureSetup.snapshot(), marker: await window.fixtureMarker(), state: registration.active.state,
      scope: registration.scope, script: registration.active.scriptURL, update_via_cache: registration.updateViaCache };
  });
  check(report.initial.marker.version === 'initial' && report.initial.marker.frozen
    && report.initial.marker.task_url === report.origin + '/ui/?task=task-11111111-1111-4111-8111-111111111111'
    && report.initial.update_via_cache === 'none', 'Actual setup activates the classic worker and executes its imported core with cache bypass');
  report.module_probe = await page.evaluate(async () => {
    const response = await fetch('/ui/module-probe.js');
    try { await navigator.serviceWorker.register('/ui/module-probe.js', { type: 'module', scope: '/ui/module-probe/', updateViaCache: 'none' });
      return { page_fetch_status: response.status, registered: true }; }
    catch (error) { return { page_fetch_status: response.status, registered: false, error: String(error) }; }
  });
  check(report.module_probe.page_fetch_status === 200 && !report.module_probe.registered && report.module_probe.error.includes('401'),
    'Authenticated page fetch succeeds while Chromium module-worker registration reproduces the 401 cause');
  report.unchanged = await page.evaluate(async () => {
    const registration = await window.fixtureRegistration(); await registration.update();
    return { same_worker: registration.active === window.fixtureOriginalWorker, updates: window.fixtureUpdates,
      installing: !!registration.installing, waiting: !!registration.waiting, marker: await window.fixtureMarker() };
  });
  check(report.unchanged.same_worker && report.unchanged.updates === 0 && !report.unchanged.installing && !report.unchanged.waiting,
    'Explicit unchanged update retains the active worker without creating an installation');
  const updatedCore = core + marker('dependency-only-update');
  await writeFile(join(assets, 'push-worker-core.js'), updatedCore);
  if (remote) await command('scp', ['-q', join(assets, 'push-worker-core.js'), `${remote}:${join(hostFixture, 'ui/push-worker-core.js')}`]);
  report.fixture_core_updated_sha256 = hash(updatedCore);
  check(hash(await readFile(join(assets, 'service-worker.js'))) === report.inputs['service-worker.js'], 'Dependency update leaves the actual worker entry bytes unchanged');
  await page.evaluate(async () => { await (await window.fixtureRegistration()).update(); });
  report.updated = await until(() => page.evaluate(async () => {
    const registration = await window.fixtureRegistration();
    return { same_worker: registration.active === window.fixtureOriginalWorker, state: registration.active.state,
      updates: window.fixtureUpdates, controller_is_active: navigator.serviceWorker.controller === registration.active };
  }), value => !value.same_worker && value.state === 'activated' && value.controller_is_active, 'imported dependency activates');
  report.updated.marker = await page.evaluate(() => window.fixtureMarker());
  check(report.updated.marker.version === 'dependency-only-update', 'The newly activated worker executes the changed imported dependency');
  check(!report.updated.same_worker && report.updated.updates === 1, 'Changing only the imported dependency creates one new activated worker that executes the new marker');
  await page.reload({ waitUntil: 'load' }); await page.evaluate(() => window.fixtureReady);
  report.refreshed = await page.evaluate(async () => {
    const registration = await window.fixtureRegistration();
    return { ready: window.fixtureSetup.snapshot().ready, marker: await window.fixtureMarker(), scope: registration.scope,
      script: registration.active.scriptURL, notifications: (await registration.getNotifications()).length,
      subscription: !!await registration.pushManager.getSubscription() };
  });
  check(report.refreshed.ready && report.refreshed.marker.version === 'dependency-only-update'
    && report.refreshed.scope === report.initial.scope && report.refreshed.script === report.initial.script,
    'Refresh retains the updated registration and actual notification setup readiness');
  check(report.refreshed.notifications === 0 && !report.refreshed.subscription, 'Regression finishes with zero notifications and no provider subscription');
  report.worker_versions = [...versions.values()]; report.worker_registrations = [...registrations.values()];
  const appRegistration = [...registrations.values()].find(value => value.scopeURL === report.origin + '/ui/');
  check(!!appRegistration && [...versions.values()].filter(value => value.registrationId === appRegistration.registrationId && activatedVersions.has(value.versionId)).length === 2,
    'Chromium independently reports the initial and dependency-updated activated versions in one app registration');
  runtimeLog = await docker('logs', runtimeId);
  report.upstream_requests = runtimeLog.split('\n').filter(Boolean).map(line => JSON.parse(line)).filter(row => !row.ready);
  check(report.upstream_requests.every(row => !row.authorization_present), 'Generated nginx removes Basic credentials before forwarding to the upstream');
  check(report.upstream_requests.every(row => row.method === 'GET'), 'No mutation, push receipt, model or provider path reaches the synthetic upstream');
  for (const path of ['/ui/service-worker.js', '/ui/push-worker-core.js']) {
    check(report.upstream_requests.some(row => row.path === path && row.status === 200 && (path.endsWith('core.js') || row.worker === 'script')),
      `Authenticated worker loading reaches the generated ingress upstream for ${path}`);
  }
  check(report.upstream_requests.filter(row => row.path === '/ui/push-worker-core.js' && row.cache_control === 'max-age=0' && row.status === 200).length >= 3,
    'Initial load, unchanged update and dependency update revalidate the imported core through nginx despite its one-hour cache lifetime');
  report.passed = true;
} catch (error) { report.errors.push(String(error.stack ?? error)); }
finally {
  if (browser) await browser.close().catch(error => report.errors.push(`Browser cleanup: ${error}`));
  if (tunnel) {
    if (tunnel.exitCode === null && tunnel.signalCode === null) {
      const closed = once(tunnel, 'close'), timer = setTimeout(() => tunnel.kill('SIGKILL'), 5000);
      tunnel.kill('SIGTERM'); await closed; clearTimeout(timer);
    }
    report.cleanup.tunnel_stopped = tunnel.exitCode !== null || tunnel.signalCode !== null;
  }
  for (const [kind, id] of [['edge', edgeId], ['runtime', runtimeId]]) {
    if (!id) continue;
    try {
      await writeFile(join(evidence, `${kind}.log`), await docker('logs', id));
      const owner = await docker('inspect', '--format', '{{index .Config.Labels "bokkie.auth-regression"}}', id);
      if (owner !== runId) throw Error('Disposable container ownership changed');
      await docker('rm', '-f', id);
      const remaining = await docker('ps', '-a', '--filter', `id=${id}`, '--format', '{{.ID}}');
      check(remaining === '', `Owned ${kind} container was removed`); report.cleanup[kind] = 'removed';
    } catch (error) { report.errors.push(`Container cleanup: ${error}`); }
  }
  if (remote && hostRoot) {
    try { await hostCommand(['rm', '-rf', '--', hostRoot]); report.cleanup.remote_fixture = 'removed'; }
    catch (error) { report.errors.push(`Remote fixture cleanup: ${error}`); }
  }
  report.finished_at = new Date().toISOString();
  if (report.errors.length) report.passed = false;
  await writeFile(join(evidence, 'report.json'), JSON.stringify(report, null, 2) + '\n');
}
console.log(JSON.stringify({ passed: report.passed, checks: report.checks.length, report: join(evidence, 'report.json'),
  errors: report.errors, cleanup: report.cleanup }, null, 2));
if (!report.passed) process.exitCode = 1;
