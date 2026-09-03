import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const root = process.cwd();
const evidence = resolve(process.env.BOKKIE_UI_EVIDENCE_DIR ?? join(root, 'docs/ui-qualification-evidence'));
await mkdir(evidence, { recursive: true });
const moduleName = process.env.BOKKIE_PLAYWRIGHT_MODULE ?? 'playwright';
const moduleSpecifier = moduleName.startsWith('/') ? pathToFileURL(moduleName).href : moduleName;
const { chromium } = await import(moduleSpecifier);

let fixture;
let fixtureRoot;
let browser;
let expectingDisconnect = false;
const unexpected = [];
const observations = { browser: {}, journeys: [], states: {}, viewports: [], errors: unexpected };
const json = value => JSON.stringify(
  value,
  (_key, item) => typeof item === 'bigint' ? item.toString() : item,
  2,
);

async function startFixture(variant) {
  await stopFixture();
  fixture = spawn('target/debug/bokkie-ui-fixture', [
    '--ui-dir', 'apps/bokkie-attention-ui/web', '--variant', variant,
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

try {
  browser = await chromium.launch({
    headless: true,
    env: {
      ...process.env,
      LD_LIBRARY_PATH: `${resolve(process.env.BOKKIE_UI_SYSROOT ?? '/nvme/development/polyorama/.tools/sysroot', 'usr/lib')}:${process.env.LD_LIBRARY_PATH ?? ''}`,
    },
    args: [
      '--no-sandbox', '--enable-unsafe-webgpu', '--enable-features=Vulkan',
      '--use-angle=vulkan', '--disable-vulkan-surface',
    ],
  });
  observations.browser = {
    version: browser.version(),
    backend: 'browser WebGPU requested through eframe/wgpu',
    source_revision: process.env.BOKKIE_SOURCE_REVISION ?? 'working-tree',
  };
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.on('pageerror', error => unexpected.push(`pageerror: ${error.stack ?? error}`));
  page.on('console', message => {
    if (message.type() !== 'error') return;
    const failure = `console: ${message.text()}`;
    if (expectingDisconnect && failure.includes('ERR_CONNECTION_REFUSED')) {
      observations.states.disconnected_console = failure;
    } else {
      unexpected.push(failure);
    }
  });
  page.on('requestfailed', request => {
    const failure = `network: ${request.method()} ${request.url()} ${request.failure()?.errorText}`;
    if (expectingDisconnect && request.url().includes('/operator/snapshot')) {
      observations.states.disconnected_request = failure;
    } else {
      unexpected.push(failure);
    }
  });
  page.on('response', response => {
    if (response.url().includes('/operator/')) observations.last_operator_response = {
      url: response.url(), status: response.status(),
    };
  });

  let url = await startFixture('full');
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  let state = audit(await snapshot(page), 'full ready');
  const webgpu = await page.evaluate(async () => {
    if (!navigator.gpu) return { navigator_gpu: false, adapter_available: false };
    const adapter = await navigator.gpu.requestAdapter();
    const info = adapter?.info;
    return {
      navigator_gpu: true,
      adapter_available: adapter != null,
      adapter: info ? {
        vendor: info.vendor,
        architecture: info.architecture,
        device: info.device,
        description: info.description,
      } : null,
    };
  });
  if (!webgpu.adapter_available) throw new Error(`WebGPU adapter unavailable: ${json(webgpu)}`);
  observations.browser.webgpu = webgpu;
  observations.states.full.ready = {
    obligations: state.virtualisation.total_rows,
    nodes: state.ui_snapshot.nodes.length,
    text_coverage: state.ui_snapshot.text_audit_coverage,
    semantic_audit: state.ui_snapshot.semantic_audit,
    text_audit: state.ui_snapshot.text_audit,
  };

  await clickId(page, 'bokkie.inbox-row.gardener:implement:'
    + state.ui_snapshot.nodes.find(candidate => candidate.id.startsWith('bokkie.inbox-row.gardener:implement:'))
      .id.split('bokkie.inbox-row.gardener:implement:')[1]);
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
    .some(candidate => candidate.actions.includes('approve_exact_gardener_proposal') && candidate.enabled));
  await clickAction(page, 'approve_exact_gardener_proposal', 3);
  await page.waitForFunction(() => {
    const state = window.__BOKKIE_ATTENTION_HANDLE.test_snapshot();
    return state.interaction.confirmation_action === 'approve_exact_gardener_proposal'
      && state.ui_snapshot.nodes.some(candidate => candidate.id === 'bokkie.lifecycle-confirmation');
  });
  state = audit(await snapshot(page), 'gardener confirmation');
  const confirmationText = state.ui_snapshot.text.map(item => item.component_id);
  const confirmation = node(state, 'bokkie.lifecycle-confirmation');
  if (!confirmation
      || state.interaction.confirmation_obligation == null
      || !state.interaction.confirmation_prompt
      || !state.interaction.confirmation_fingerprint
      || state.interaction.confirmation_occurrence !== 1
      || !state.interaction.confirmation_consequence) {
    throw new Error('exact gardener confirmation is not physically visible');
  }
  await page.screenshot({ path: join(evidence, 'browser-gardener-confirmation.png') });
  await writeFile(
    join(evidence, 'browser-gardener-confirmation-semantic.json'),
    `${json(state)}\n`,
  );
  observations.journeys.push({
    name: 'exact gardener confirmation',
    classification: 'direct physical pointer plus Rust semantic state',
    obligation: state.interaction.confirmation_obligation,
    confirmation_bounds: confirmation.rect,
    observed_measured_components: confirmationText.length,
    consequence_action: state.interaction.confirmation_action,
    occurrence: state.interaction.confirmation_occurrence,
    consequence: state.interaction.confirmation_consequence,
    fingerprint: state.interaction.confirmation_fingerprint,
    prompt: state.interaction.confirmation_prompt,
    submitted: false,
  });
  await clickAction(page, 'dismiss_lifecycle_confirmation');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .interaction.confirmation_action == null
      && !window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
        .some(candidate => candidate.id === 'bokkie.lifecycle-confirmation'));

  await clickId(page, 'bokkie.inbox-row.approval-safe-cancel');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().interaction.selected_obligation
    === 'approval-safe-cancel');
  await clickAction(page, 'cancel_obligation', 3);
  await page.waitForFunction(() => {
    const state = window.__BOKKIE_ATTENTION_HANDLE.test_snapshot();
    return state.interaction.confirmation_action === 'cancel_obligation'
      && state.ui_snapshot.nodes.some(candidate => candidate.actions.includes('confirm_lifecycle_action'));
  });
  await clickAction(page, 'confirm_lifecycle_action');
  await page.waitForFunction(() => {
    const state = window.__BOKKIE_ATTENTION_HANDLE.test_snapshot();
    return state.interaction.confirmation_action == null
      && state.interaction.selected_obligation === 'approval-safe-cancel'
      && state.interaction.connection === 'current';
  }, null, { timeout: 15_000 });
  const durableAction = await page.evaluate(async () => {
    const current = await (await fetch('/operator/snapshot')).json();
    const topic = await (await fetch('/operator/obligations/approval-safe-cancel/topic')).json();
    return {
      state: current.obligations.find(item => item.id === 'approval-safe-cancel')?.state,
      last_event: topic.items.filter(item => item.source === 'audit_event').at(-1)?.event_type,
    };
  });
  if (durableAction.state !== 'cancelled' || durableAction.last_event !== 'cancelled') {
    throw new Error(`safe lifecycle did not retain its durable event: ${json(durableAction)}`);
  }
  state = audit(await snapshot(page), 'post action');
  observations.journeys.push({
    name: 'safe cancel lifecycle',
    classification: 'direct physical pointer, real HTTP/store path, refreshed durable projection',
    durable_result: durableAction,
  });

  await clickId(page, 'bokkie.inbox-row.attention-nonretryable');
  await clickAction(page, 'cancel_obligation', 3);
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
    .some(candidate => candidate.enabled && candidate.actions.includes('confirm_lifecycle_action')));
  await page.evaluate(async () => {
    const snapshot = await (await fetch('/operator/snapshot')).json();
    const obligation = snapshot.obligations.find(item => item.id === 'attention-nonretryable');
    await fetch('/operator/obligations/attention-nonretryable/cancel', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        precondition: obligation.capabilities.cancel.precondition,
        actor: '',
        note: null,
      }),
    });
  });
  await clickAction(page, 'confirm_lifecycle_action');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .interaction.confirmation_conflict != null, null, { timeout: 15_000 });
  state = audit(await snapshot(page), 'conflict');
  observations.journeys.push({
    name: 'stale confirmation conflict',
    classification: 'direct physical confirmation against externally advanced temporary store state',
    conflict: state.interaction.confirmation_conflict,
    connection: state.interaction.connection,
    submit_enabled: state.ui_snapshot.nodes.some(candidate => candidate.enabled
      && candidate.actions.includes('confirm_lifecycle_action')),
  });
  await clickAction(page, 'dismiss_lifecycle_confirmation');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .interaction.confirmation_action == null
      && !window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
        .some(candidate => candidate.id === 'bokkie.lifecycle-confirmation'));

  let focused = [];
  for (let index = 0; index < 12 && !focused.length; index += 1) {
    await page.keyboard.press('Tab');
    await page.waitForTimeout(50);
    state = audit(await snapshot(page), 'keyboard path');
    focused = state.ui_snapshot.nodes.filter(candidate => candidate.focused).map(candidate => candidate.id);
  }
  if (!focused.length) throw new Error('keyboard traversal did not reach an observed semantic control');
  observations.journeys.push({
    name: 'keyboard focus traversal',
    classification: 'direct keyboard input and Rust current-frame focus semantics',
    focused,
  });

  const obligationsPoint = await pointFor(page, 'pane.2');
  await page.mouse.move(obligationsPoint.x, obligationsPoint.y);
  await page.mouse.wheel(0, 1_400);
  await page.waitForTimeout(200);
  const scrolled = await snapshot(page);
  const scrolledStart = Number(scrolled.virtualisation.visible_rows[0]);
  if (scrolledStart === 0) throw new Error('physical obligations scroll did not move the visible range');
  await clickAction(page, 'refresh_operator_state');
  await page.waitForFunction(frame => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().frame_number > frame,
    scrolled.frame_number);
  state = audit(await snapshot(page), 'scroll across refresh');
  const refreshedStart = Number(state.virtualisation.visible_rows[0]);
  if (refreshedStart === 0) throw new Error('refresh discarded the obligations scroll position');
  observations.journeys.push({
    name: 'selection and scroll across refresh',
    classification: 'direct wheel, physical refresh action and Rust visible-range observation',
    selected: state.interaction.selected_obligation,
    visible_start_before: scrolledStart,
    visible_start_after: refreshedStart,
  });

  for (const viewport of [
    { width: 1440, height: 900, label: '1440x900' },
    { width: 1280, height: 720, label: '1280x720' },
    { width: 480, height: 720, label: 'narrow-480x720' },
  ]) {
    await page.setViewportSize(viewport);
    await page.waitForTimeout(300);
    state = audit(await snapshot(page), viewport.label);
    const layout = await page.evaluate(() => {
      const canvas = document.querySelector('#bokkie-attention-canvas').getBoundingClientRect();
      return {
        document_width: document.documentElement.clientWidth,
        document_scroll_width: document.documentElement.scrollWidth,
        document_height: document.documentElement.clientHeight,
        document_scroll_height: document.documentElement.scrollHeight,
        canvas: { width: canvas.width, height: canvas.height },
      };
    });
    if (layout.document_scroll_width > layout.document_width
        || layout.document_scroll_height > layout.document_height
        || layout.canvas.width !== viewport.width
        || layout.canvas.height !== viewport.height) {
      throw new Error(`${viewport.label} physical layout overflow: ${json(layout)}`);
    }
    const file = `browser-${viewport.label}.png`;
    await page.screenshot({ path: join(evidence, file) });
    observations.viewports.push({
      ...viewport,
      screenshot: file,
      node_count: state.ui_snapshot.nodes.length,
      selected: state.interaction.selected_obligation,
      active_pane: state.interaction.active_pane,
      layout,
    });
  }

  const beforeIdle = (await snapshot(page)).frame_number;
  await page.waitForTimeout(1_000);
  const afterIdle = (await snapshot(page)).frame_number;
  observations.states.full.warmed_idle = {
    before: beforeIdle,
    after: afterIdle,
    stable: beforeIdle === afterIdle,
    interval_ms: 1_000,
  };
  if (beforeIdle !== afterIdle) throw new Error('warmed idle produced an unsolicited frame');

  await page.setViewportSize({ width: 1440, height: 900 });
  url = await startFixture('empty');
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  state = audit(await snapshot(page), 'empty database');
  observations.states.empty.result = {
    total_rows: state.virtualisation.total_rows,
    selected: state.interaction.selected_obligation,
    classification: 'direct real-router empty temporary SQLite database',
  };

  url = await startFixture('empty-inbox');
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  state = audit(await snapshot(page), 'empty inbox');
  const inboxRows = state.ui_snapshot.nodes.filter(candidate => candidate.id.startsWith('bokkie.inbox-row.'));
  if (Number(state.virtualisation.total_rows) !== 1 || inboxRows.length !== 0) {
    throw new Error(`empty inbox fixture diverged: ${json({ virtualisation: state.virtualisation, inboxRows })}`);
  }
  observations.states.empty_inbox = {
    total_rows: state.virtualisation.total_rows,
    inbox_rows: inboxRows.length,
    classification: 'direct real-router terminal-only temporary SQLite database',
  };

  await page.setViewportSize({ width: 1440, height: 900 });
  url = await startFixture('large');
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  state = audit(await snapshot(page), 'large database');
  const materialised = Number(state.virtualisation.materialised_rows[1]
    - state.virtualisation.materialised_rows[0]);
  if (Number(state.virtualisation.total_rows) !== 5_000 || materialised > 16) {
    throw new Error(`large-list materialisation bound failed: ${json(state.virtualisation)}`);
  }
  observations.states.large.result = {
    ...state.virtualisation,
    materialised_count: materialised,
    classification: 'direct current-frame Rust virtualisation observation',
  };

  expectingDisconnect = true;
  await stopFixture();
  const disconnected = await page.evaluate(async () => {
    try {
      await fetch('/operator/snapshot', { cache: 'no-store' });
      return false;
    } catch {
      return true;
    }
  });
  observations.states.disconnected = {
    network_disconnected: disconnected,
    app_stale_surface: 'covered by conflict journey; post-shutdown repaint unavailable in headless harness',
    classification: 'direct failed same-origin fetch after fixture shutdown; app surface classification approximate',
  };

  // A deliberate 409 is part of the stale-confirmation journey.
  for (let index = unexpected.length - 1; index >= 0; index -= 1) {
    if (unexpected[index].includes('409 (Conflict)')) {
      unexpected.splice(index, 1);
    }
  }
  if (unexpected.length) throw new Error(`unexpected browser failures: ${unexpected.join('\n')}`);

  const finalState = await snapshot(page);
  await writeFile(join(evidence, 'browser-semantic.json'), `${json(finalState.ui_snapshot)}\n`);
  await writeFile(join(evidence, 'browser-text.json'), `${json({
    observations: finalState.ui_snapshot.text,
    audit: finalState.ui_snapshot.text_audit,
    coverage: finalState.ui_snapshot.text_audit_coverage,
    classification: 'direct for measured Polyorama text; native egui controls explicitly excluded by coverage',
  })}\n`);
  await writeFile(join(evidence, 'browser-interactions.json'), `${json(observations)}\n`);
  console.log('browser UI smoke passed');
} catch (error) {
  console.error(error.stack ?? error);
  process.exitCode = 1;
} finally {
  await stopFixture();
  await browser?.close();
}
