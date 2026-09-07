import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

import { createHash } from 'node:crypto';
import { captureSettled } from './ui-capture-settling.mjs';

const root = process.cwd();
const evidence = resolve(process.env.BOKKIE_UI_EVIDENCE_DIR ?? join(root, '.ui-qualification-runtime/appearance'));
await mkdir(evidence, { recursive: true });
const moduleName = process.env.BOKKIE_PLAYWRIGHT_MODULE ?? 'playwright';
const moduleSpecifier = moduleName.startsWith('/') ? pathToFileURL(moduleName).href : moduleName;
const { chromium } = await import(moduleSpecifier);

let fixture;
let fixtureRoot;
let browser;
let diagnosticPage;
let expectingDisconnect = false;
const unexpected = [];
const operatorRequests = [];
const observations = { browser: {}, journeys: [], states: {}, viewports: [], errors: unexpected };
const json = value => JSON.stringify(
  value,
  (_key, item) => typeof item === 'bigint' ? item.toString() : item,
  2,
);

async function startFixture(variant, port = null) {
  await stopFixture();
  fixture = spawn('target/debug/bokkie-ui-fixture', [
    '--ui-dir', 'apps/bokkie-attention-ui/web', '--variant', variant,
    ...(port == null ? [] : ['--port', String(port)]),
  ], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
  fixture.stderr.setEncoding('utf8');
  let diagnostics = '';
  fixture.stderr.on('data', chunk => { diagnostics += chunk; });
  fixture.stdout.setEncoding('utf8');
  const line = await new Promise((accept, reject) => {
    let buffered = '';
    const timeout = setTimeout(() => reject(new Error(`fixture startup timed out: ${diagnostics}`)), 20_000);
    fixture.stdout.on('data', chunk => {
      buffered += chunk;
      const newline = buffered.indexOf('\n');
      if (newline >= 0) {
        clearTimeout(timeout);
        accept(buffered.slice(0, newline));
      }
    });
    fixture.once('exit', code => reject(new Error(`fixture exited ${code}: ${diagnostics}`)));
  });
  const identity = JSON.parse(line);
  const reportedRoot = resolve(identity.root);
  if (dirname(reportedRoot) !== resolve(tmpdir())
      || !/^bokkie-ui-qualification-[0-9a-f-]{36}$/.test(basename(reportedRoot))) {
    throw new Error(`fixture reported unsafe temporary root ${identity.root}`);
  }
  fixtureRoot = reportedRoot;
  observations.states[variant] = { fixture: identity };
  return `http://${identity.address}/ui/`;
}

async function stopFixture() {
  const process = fixture;
  const ownedRoot = fixtureRoot;
  fixture = undefined;
  fixtureRoot = undefined;
  if (process) {
    if (process.exitCode == null) {
      process.kill('SIGINT');
      await Promise.race([once(process, 'exit'), new Promise(resolve => setTimeout(resolve, 3_000))]);
    }
    if (process.exitCode == null) {
      process.kill('SIGKILL');
      await once(process, 'exit');
    }
  }
  if (ownedRoot) await rm(ownedRoot, { recursive: true, force: true });
}

const snapshot = page => page.evaluate(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot());
const node = (state, id) => state.ui_snapshot.nodes.find(candidate => candidate.id === id);

async function waitCurrent(page) {
  await page.locator('#bokkie-attention-canvas[data-bokkie-ready="true"]').waitFor({ timeout: 30_000 });
  let last;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    last = await page.evaluate(() => {
      try {
        return { value: window.__BOKKIE_ATTENTION_HANDLE?.test_snapshot() };
      } catch (error) {
        return { error: String(error) };
      }
    });
    if (last.value?.interaction?.connection === 'current'
        && !last.value.ui_snapshot.semantic_audit.length) return;
    if (last.value?.interaction?.connection === 'loading' && !observations.states.loading) {
      observations.states.loading = {
        nodes: last.value.ui_snapshot.nodes.length,
        classification: 'direct Rust current-frame state before the first HTTP snapshot completed',
      };
    }
    await page.waitForTimeout(100);
  }
  throw new Error(`Bokkie did not reach an audited current frame: ${json({ last, unexpected, response: observations.last_operator_response })}`);
}

async function pointFor(page, id) {
  const state = await snapshot(page);
  const rootNode = node(state, state.ui_snapshot.root);
  const target = node(state, id);
  const canvas = await page.locator('#bokkie-attention-canvas').boundingBox();
  if (!rootNode || !target || !canvas) throw new Error(`missing current Rust geometry for ${id}`);
  const rootRect = rootNode.rect;
  const rect = target.rect;
  return {
    x: canvas.x + ((rect.min_x + rect.max_x) / 2 - rootRect.min_x) * canvas.width / (rootRect.max_x - rootRect.min_x),
    y: canvas.y + ((rect.min_y + rect.max_y) / 2 - rootRect.min_y) * canvas.height / (rootRect.max_y - rootRect.min_y),
  };
}

async function clickId(page, id) {
  let point = await pointFor(page, id);
  await page.mouse.move(point.x, point.y);
  await page.waitForTimeout(50);
  point = await pointFor(page, id);
  await page.mouse.click(point.x, point.y);
}

async function clickAction(page, action, pane = null) {
  await page.waitForFunction(({ action, pane }) => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(candidate => candidate.enabled
      && candidate.actions.includes(action) && (pane == null || candidate.pane === pane)),
  { action, pane }, { timeout: 10_000 });
  const state = await snapshot(page);
  const target = state.ui_snapshot.nodes.find(candidate => candidate.enabled
    && candidate.actions.includes(action) && (pane == null || candidate.pane === pane));
  if (!target) throw new Error(`enabled action ${action} is absent`);
  await clickId(page, target.id);
}

async function selectCollection(page, collection) {
  await page.waitForFunction(id => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(candidate => candidate.id === id), `bokkie.collection.${collection}`);
  await clickId(page, `bokkie.collection.${collection}`);
  const pane = collection === 'attention' ? 'pane.1' : 'pane.2';
  await page.waitForFunction(id => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(candidate => candidate.id === id), pane);
}

function audit(state, label) {
  if (!state.ui_snapshot.nodes.length
      || state.ui_snapshot.nodes.length > 300
      || state.ui_snapshot.semantic_audit.length
      || state.ui_snapshot.text_audit.length) {
    throw new Error(`${label} audit failed: ${json({
      nodes: state.ui_snapshot.nodes.length,
      semantic: state.ui_snapshot.semantic_audit,
      text: state.ui_snapshot.text_audit,
    })}`);
  }
  return state;
}

const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const source = {
  revision: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  polyorama_revision: process.env.BOKKIE_POLYORAMA_REVISION ?? null,
  polyorama_tree: process.env.BOKKIE_POLYORAMA_TREE ?? null,
  diff_sha256: digest(execFileSync('git', ['diff', '--binary', 'HEAD'])),
  wasm_sha256: digest(await readFile('apps/bokkie-attention-ui/web/pkg/bokkie_attention_ui_bg.wasm')),
  appearance_source_sha256: digest(await readFile('apps/bokkie-attention-ui/src/appearance.rs')),
  fixture_binary_sha256: digest(await readFile('target/debug/bokkie-ui-fixture')),
  inter_regular_sha256: digest(await readFile('apps/bokkie-attention-ui/assets/fonts/Inter-Regular.ttf')),
  inter_semibold_sha256: digest(await readFile('apps/bokkie-attention-ui/assets/fonts/Inter-SemiBold.ttf')),
};
const cases = process.env.BOKKIE_APPEARANCE_CASES ? JSON.parse(process.env.BOKKIE_APPEARANCE_CASES) : [
  { name: 'graphite-source-sans', identity: 'graphite', typeface: 'source-sans' },
  { name: 'restrained-blue-source-sans', identity: 'restrained-blue', typeface: 'source-sans' },
  { name: 'warm-light-source-sans', identity: 'warm-light', typeface: 'source-sans' },
  { name: 'graphite-inter', identity: 'graphite', typeface: 'inter' },
];
await writeFile(join(evidence, 'tracked-source.patch'), execFileSync('git', ['diff', '--binary', 'HEAD']));
await writeFile(join(evidence, 'appearance-source.rs.txt'), await readFile('apps/bokkie-attention-ui/src/appearance.rs'));
observations.source = source;
observations.captures = [];
try {
  browser = await chromium.launch({
    headless: true,
    env: { ...process.env, LD_LIBRARY_PATH: `${resolve(process.env.BOKKIE_UI_SYSROOT ?? '/nvme/development/polyorama/.tools/sysroot', 'usr/lib')}:${process.env.LD_LIBRARY_PATH ?? ''}` },
    args: ['--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan', '--use-angle=vulkan', '--disable-vulkan-surface'],
  });
  observations.browser = { version: browser.version(), fontconfig_file: process.env.FONTCONFIG_FILE ?? 'system default' };
  const url = await startFixture('full');
  for (const candidate of cases) {
    const { name, width = 1440, height = 900, selection = 'proposal', keyboard_focus = false, confirmation = false, ...appearance } = candidate;
    const page = await browser.newPage({ viewport: { width, height } });
    diagnosticPage = page;
    page.on('pageerror', error => unexpected.push(String(error)));
    await page.goto(`${url}?appearance=${encodeURIComponent(JSON.stringify(appearance))}`, { waitUntil: 'domcontentloaded' });
    await waitCurrent(page);
    let state = audit(await snapshot(page), `${name} ready`);
    const selected = selection === 'failure' ? 'bokkie.inbox-row.attention-nonretryable'
      : state.ui_snapshot.nodes.find(item => item.id.startsWith('bokkie.inbox-row.gardener:implement:'))?.id;
    if (!selected) throw new Error('missing selected proposal');
    await clickId(page, selected);
    await page.waitForFunction(() => !window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().interaction.topic_busy);
    // Move the pointer away: selection colour must not be an incidental hover.
    await page.mouse.move(2, 2);
    await page.waitForTimeout(200);
    if (keyboard_focus) {
      await page.keyboard.press('Tab');
      await page.waitForTimeout(150);
    }
    if (confirmation) {
      await clickAction(page, 'approve_exact_gardener_proposal', 3);
      await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().interaction.confirmation_action === 'approve_exact_gardener_proposal');
    }
    const capture = await captureSettled(page, snapshot);
    state = audit(capture.state, name);
    const focused = state.ui_snapshot.nodes.filter(item => item.focused).map(item => item.id);
    if (keyboard_focus && !focused.length) throw new Error(`${name}: keyboard focus was not observed`);
    await writeFile(join(evidence, `${name}.png`), capture.png);
    await writeFile(join(evidence, `${name}.json`), `${json(state)}\n`);
    observations.captures.push({ name, appearance, viewport: { width, height }, selected: state.interaction.selected_obligation,
      focused, keyboard_focus, confirmation, settling: capture.settling, screenshot: `${name}.png`, semantic: `${name}.json`, text_audit: state.ui_snapshot.text_audit,
      semantic_audit: state.ui_snapshot.semantic_audit, classification: 'real Rust egui/wgpu application; candidate, not an approved replacement baseline' });
    await page.close();
  }
  if (unexpected.length) throw new Error(unexpected.join('\n'));
  await writeFile(join(evidence, 'appearance-observations.json'), `${json(observations)}\n`);
} catch (error) {
  if (diagnosticPage && !diagnosticPage.isClosed()) {
    await diagnosticPage.screenshot({ path: join(evidence, 'capture-failure.png') }).catch(() => {});
    await writeFile(join(evidence, 'capture-failure.json'), json(await snapshot(diagnosticPage).catch(() => null)));
  }
  throw error;
} finally {
  await browser?.close();
  await stopFixture();
}
