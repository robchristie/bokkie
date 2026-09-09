import { execFile } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { captureSettled } from './ui-capture-settling.mjs';

const execute = promisify(execFile);

/**
 * Exercise the plain-intent engineering surface against the production HTTP
 * router and a fixture-owned Store. No engineering runtime or Codex process is
 * enabled: the UI proves only durable intake, follow-up and cancellation.
 */
export async function qualifyEngineeringJourney({
  page, evidence, startFixture, waitCurrent, snapshot, node, clickId,
  setFocusedTextAgentValue, audit, observations, json,
}) {
  const url = await startFixture('engineering');
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);

  async function capture(name) {
    const settled = await captureSettled(page, snapshot);
    audit(settled.state, name);
    await writeFile(join(evidence, `${name}.png`), settled.png);
    await writeFile(join(evidence, `${name}.json`), `${json(settled.state)}\n`);
  }

  async function lantern(name) {
    const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT;
    if (!endpoint) return;
    const targets = JSON.parse((await execute('lantern', ['targets', '--endpoint', endpoint, '--json'])).stdout);
    const pages = (targets.targets ?? targets.result?.targets ?? []).filter(item => item.type === 'page');
    if (pages.length !== 1) throw new Error('Lantern could not uniquely identify the engineering fixture page');
    const shared = ['--endpoint', endpoint, '--target-id', pages[0].id ?? pages[0].target_id, '--json'];
    for (const [suffix, args] of [
      ['layout', ['layout', '--container-selector', 'body']],
      ['screenshot', ['screenshot', '--output', join(evidence, `lantern-engineering-${name}.png`), '--overwrite']],
    ]) {
      const { stdout } = await execute('lantern', [...args, ...shared]);
      if (!JSON.parse(stdout).ok) throw new Error(`Lantern engineering ${name} ${suffix} did not complete`);
      await writeFile(join(evidence, `lantern-engineering-${name}-${suffix}.json`), stdout);
    }
  }

  await clickId(page, 'bokkie.engineering.new');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
    .some(item => item.id === 'bokkie.engineering.text'));
  await clickId(page, 'bokkie.engineering.text');
  const intent = 'Build a local engineering reading workspace with durable operator follow-ups.';
  await setFocusedTextAgentValue(page, intent);
  await capture('browser-engineering-compose-desktop');
  await lantern('compose-desktop');
  await page.setViewportSize({ width: 480, height: 720 });
  await page.waitForTimeout(250);
  await capture('browser-engineering-compose-narrow');
  await lantern('compose-narrow');
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(250);
  await clickId(page, 'bokkie.engineering.save');
  await page.waitForFunction(() => {
    const state = window.__BOKKIE_ATTENTION_HANDLE.test_snapshot();
    return state.interaction.selected_obligation != null
      && state.ui_snapshot.nodes.some(item => item.id === 'bokkie.engineering.follow-up')
      && state.ui_snapshot.nodes.some(item => item.id === 'bokkie.engineering.dismiss-saved');
  });
  const root = (await snapshot(page)).interaction.selected_obligation;
  if (!root) throw new Error('durable engineering intake did not select its root obligation');
  await capture('browser-engineering-detail-desktop');
  await lantern('detail-desktop');
  await page.setViewportSize({ width: 480, height: 720 });
  await page.waitForTimeout(250);
  await capture('browser-engineering-detail-narrow');
  await lantern('detail-narrow');
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(250);

  await clickId(page, 'bokkie.engineering.follow-up');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
    .some(item => item.id === 'bokkie.engineering.text'));
  await clickId(page, 'bokkie.engineering.text');
  const followUp = 'Keep the reader local-only and show the next responsible action.';
  await setFocusedTextAgentValue(page, followUp);
  await clickId(page, 'bokkie.engineering.save');
  await page.waitForFunction(id => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().interaction.selected_obligation === id, root);
  await page.waitForFunction(async ({ id, text }) => {
    const projection = await (await fetch(`/operator/obligations/${encodeURIComponent(id)}`)).json();
    return JSON.stringify(projection).includes(text);
  }, { id: root, text: followUp });

  await clickId(page, 'bokkie.engineering.cancel');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot().ui_snapshot.nodes
    .some(item => item.id === 'bokkie.engineering.compose'));
  await capture('browser-engineering-cancel-confirmation');
  await clickId(page, 'bokkie.engineering.save');
  await page.waitForFunction(async id => {
    const projection = await (await fetch(`/operator/obligations/${encodeURIComponent(id)}`)).json();
    return JSON.stringify(projection).includes('cancellation');
  }, root);
  // The save response deliberately triggers a bounded UI rebuild. Let that
  // same-session read settle before this fixture is replaced by another journey.
  await waitCurrent(page);
  const durable = await page.evaluate(async id => (await fetch(`/operator/obligations/${encodeURIComponent(id)}`)).json(), root);
  observations.journeys.push({
    name: 'engineering plain-intent intake, follow-up and cancellation',
    classification: 'physical pointer focus and browser IME text-input against fixture-owned production HTTP and Store; no engineering runtime or Codex process',
    root_obligation: root,
    intent,
    follow_up: followUp,
    durable_projection: durable.obligation,
    responsive_widths: [1440, 480],
  });
}
