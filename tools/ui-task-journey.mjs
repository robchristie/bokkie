import { execFile } from 'node:child_process';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { captureSettled } from './ui-capture-settling.mjs';

const execute = promisify(execFile);

/** Physical task navigation and configuration against a fixture-owned database. */
export async function qualifyTaskJourney({
  page, evidence, startFixture, waitCurrent, snapshot, node, clickId,
  clickAction, selectCollection, pointFor, audit, observations, json,
}) {
  const parent = 'gardener:inspect:robchristie/bokkie';
  const url = await startFixture('tasks');
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  const before = await page.evaluate(async () => (await fetch('/operator/snapshot')).json());
  const configured = before.obligations.find(item => item.id === parent);
  const proposal = before.obligations.find(item => item.state === 'awaiting_approval');
  const completed = before.obligations.find(item => item.state === 'completed');
  if (configured?.task?.kind !== 'gardener_inspection'
      || proposal?.task?.parent_task_id !== parent
      || completed?.task?.parent_task_id !== parent) {
    throw new Error('task fixture does not expose authoritative parent and child identities');
  }

  async function selected(id) {
    await page.waitForFunction(expected => {
      const state = window.__BOKKIE_ATTENTION_HANDLE.test_snapshot();
      return state.interaction.selected_obligation === expected
        && !state.interaction.topic_busy && state.interaction.connection === 'current';
    }, id);
  }

  async function reveal(id) {
    for (let attempt = 0; attempt < 30; attempt += 1) {
      if (node(await snapshot(page), id)) return;
      const point = await pointFor(page, 'pane.3');
      await page.mouse.move(point.x, point.y);
      await page.mouse.wheel(0, 260);
      await page.waitForTimeout(100);
    }
    throw new Error(`task control did not become visible: ${id}`);
  }

  async function capture(name) {
    const settled = await captureSettled(page, snapshot);
    audit(settled.state, name);
    await writeFile(join(evidence, `${name}.png`), settled.png);
    await writeFile(join(evidence, `${name}.json`), `${json(settled.state)}\n`);
  }

  await selectCollection(page, 'all');
  const rows = (await snapshot(page)).ui_snapshot.nodes
    .filter(item => item.id.startsWith('bokkie.obligation-row.'));
  if (rows.length !== 1 || rows[0].id !== `bokkie.obligation-row.${parent}`) {
    throw new Error('Tasks must show the configured task without duplicating its generated work');
  }
  await clickId(page, rows[0].id);
  await selected(parent);
  await capture('browser-task-desktop');

  const endpoint = process.env.BOKKIE_UI_LANTERN_ENDPOINT;
  if (endpoint) {
    const targets = JSON.parse((await execute('lantern', ['targets', '--endpoint', endpoint, '--json'])).stdout);
    const targetList = targets.targets ?? targets.result?.targets ?? [];
    // This qualification owns one browser page; Lantern reports redacted URL shapes.
    const pages = targetList.filter(item => item.type === 'page');
    const target = pages.length === 1 ? pages[0] : null;
    if (!target) throw new Error('Lantern could not uniquely identify the owned fixture page');
    const shared = ['--endpoint', endpoint, '--target-id', target.id ?? target.target_id, '--json'];
    for (const [name, args] of [
      ['page', ['page']],
      ['layout', ['layout', '--container-selector', 'body']],
      ['flow', ['flow', '--timeout-ms', '10000', '--quiet-ms', '300']],
      ['screenshot', ['screenshot', '--output', join(evidence, 'lantern-task-desktop.png')]],
    ]) {
      const { stdout } = await execute('lantern', [...args, ...shared]);
      const result = JSON.parse(stdout);
      if (!result.ok) throw new Error(`Lantern ${name} did not complete`);
      await writeFile(join(evidence, `lantern-task-${name}.json`), stdout);
    }
  }

  await page.setViewportSize({ width: 480, height: 720 });
  await page.waitForTimeout(250);
  await capture('browser-task-narrow');
  await clickId(page, 'bokkie.back-to-list');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(item => item.id === 'pane.2'));
  await clickId(page, `bokkie.obligation-row.${parent}`);
  await selected(parent);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.waitForTimeout(250);

  await reveal('bokkie.task.settings.edit');
  await clickId(page, 'bokkie.task.settings.edit');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(item => item.id === 'bokkie.task.settings.instructions'));
  await clickId(page, 'bokkie.task.settings.extend');
  const instructions = 'Prioritise deterministic scheduler recovery tests.';
  for (const [field, value] of [['instructions', instructions], ['actor', 'task-qualification']]) {
    await clickId(page, `bokkie.task.settings.${field}`);
    await page.keyboard.press('ControlOrMeta+A');
    await page.keyboard.type(value, { delay: 15 });
  }
  await clickId(page, 'bokkie.task.settings.review');
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(item => item.id === 'bokkie.task.settings.save' && item.enabled));
  await capture('browser-task-settings-review');
  await clickId(page, 'bokkie.task.settings.save');
  await page.waitForFunction(async ({ parent, instructions }) => {
    const response = await fetch(`/operator/obligations/${encodeURIComponent(parent)}`);
    const body = await response.json();
    return body.obligation?.task?.configuration?.instructions === instructions;
  }, { parent, instructions });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await waitCurrent(page);
  const after = await page.evaluate(async () => (await fetch('/operator/snapshot')).json());
  const configuration = after.obligations.find(item => item.id === parent).task.configuration;
  const unchanged = after.obligations.find(item => item.id === proposal.id);
  if (configuration.revision <= configured.task.configuration.revision
      || configuration.instruction_mode !== 'extend'
      || !configuration.effective_instructions.includes(instructions)
      || JSON.stringify(unchanged.capabilities) !== JSON.stringify(proposal.capabilities)) {
    throw new Error('settings save did not persist or changed an existing proposal decision');
  }

  await selectCollection(page, 'all');
  await clickId(page, `bokkie.obligation-row.${parent}`);
  await selected(parent);
  await reveal(`bokkie.task.open.${proposal.id}`);
  await clickId(page, `bokkie.task.open.${proposal.id}`);
  await selected(proposal.id);
  await clickAction(page, 'approve_exact_gardener_proposal', 3);
  await page.waitForFunction(() => window.__BOKKIE_ATTENTION_HANDLE.test_snapshot()
    .ui_snapshot.nodes.some(item => item.id === 'bokkie.lifecycle-confirmation'));
  await capture('browser-task-proposal-confirmation');
  await clickAction(page, 'confirm_lifecycle_action');
  await page.waitForFunction(async id => {
    const projection = await (await fetch(`/operator/obligations/${encodeURIComponent(id)}`)).json();
    return projection.obligation.state === 'pending';
  }, proposal.id);
  await waitCurrent(page);
  await reveal(`bokkie.task.parent.${parent}`);
  await clickId(page, `bokkie.task.parent.${parent}`);
  await selected(parent);
  await reveal(`bokkie.task.open.${completed.id}`);
  await clickId(page, `bokkie.task.open.${completed.id}`);
  await selected(completed.id);
  await capture('browser-task-verified-result');

  observations.journeys.push({
    name: 'configured task, settings, proposal approval and verified follow-on work',
    classification: 'physical pointer/keyboard actions against synthetic Store-owned fixture state; no runner or publication',
    parent, pending_proposal: proposal.id, completed_work: completed.id,
    old_configuration_revision: configured.task.configuration.revision,
    saved_configuration: configuration,
    approval_unchanged_by_settings: true,
    approval_submitted: true,
    responsive_widths: [1440, 480],
  });
}
