/** Bounded API calibration. This evidence never substitutes for live UI qualification. */
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile, appendFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

const preflight = process.argv.includes('--preflight');
if (process.argv.slice(2).some(arg => arg !== '--preflight')) throw Error('Only --preflight is supported');
const profilePath = process.env.BOKKIE_CONVERSATION_PROFILE;
if (!profilePath) throw Error('An explicit private BOKKIE_CONVERSATION_PROFILE is required');
const evidence = resolve(process.env.BOKKIE_CONVERSATION_CALIBRATION_EVIDENCE ?? '.ui-qualification-runtime/conversation-calibration');
const binary = resolve('target/debug/bokkie-conversation-fixture');
const started = Date.now();
const maxCalls = 8;
const maxSeconds = 600;
const report = {
  schema_version: 1, mode: preflight ? 'no-model-preflight' : 'live-api-calibration',
  purpose: 'API calibration precedes and does not replace live UI qualification',
  started_at: new Date(started).toISOString(), budget: {model_calls: maxCalls, seconds: maxSeconds, reserved_per_interaction: 2, automatic_retries: 0},
  calls: 0, fixtures: [], interactions: [], checks: [], errors: [], passed: false,
};
const hash = value => createHash('sha256').update(value).digest('hex');
const canonical = value => JSON.stringify(value, (_key, item) => item && !Array.isArray(item) && typeof item === 'object'
  ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item);
await mkdir(evidence, {recursive: true, mode: 0o700});
// A fresh journal avoids treating a previous successful campaign as this attempt.
await writeFile(join(evidence, 'progress.jsonl'), '', {flag: 'wx', mode: 0o600});
async function persist() {
  report.elapsed_seconds = (Date.now() - started) / 1000;
  await writeFile(join(evidence, 'calibration.json'), JSON.stringify(report, null, 2), {mode: 0o600});
}
async function progress(stage, fields = {}) {
  const event = {stage, at: new Date().toISOString(), elapsed_seconds: (Date.now() - started) / 1000, calls: report.calls, ...fields};
  console.log(JSON.stringify(event));
  await appendFile(join(evidence, 'progress.jsonl'), JSON.stringify(event) + '\n');
  await persist();
}
function check(condition, criterion) {
  report.checks.push({criterion, passed: Boolean(condition)});
  if (!condition) throw Error(criterion);
}
function remaining() {
  const milliseconds = maxSeconds * 1000 - (Date.now() - started);
  if (milliseconds <= 0) throw Error('Aggregate calibration time budget exhausted');
  return milliseconds;
}
let active;
function terminate(child) {
  if (child && child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, 'SIGKILL'); } catch { child.kill('SIGKILL'); }
  }
}
const deadline = setTimeout(() => { report.errors.push('Aggregate calibration deadline reached'); terminate(active?.child); }, maxSeconds * 1000);

async function startFixture(name) {
  remaining();
  const root = join('/tmp', `bokkie-calibration-${name}-${randomUUID()}`);
  const record = {name, root, model_calls: 0};
  report.fixtures.push(record);
  const child = spawn(binary, ['--root', root, ...(!preflight ? ['--profile', profilePath] : [])], {stdio: ['pipe', 'pipe', 'pipe'], detached: true});
  const state = {child, record, queue: [], waiter: null, buffer: '', failure: null};
  active = state;
  const fail = error => { state.failure = error; state.waiter?.reject(error); state.waiter = null; };
  child.on('error', fail);
  child.on('exit', (code, signal) => fail(Error(`Fixture exited (${code ?? signal})`)));
  child.stderr.on('data', bytes => { record.stderr = ((record.stderr ?? '') + bytes).slice(-8192); });
  child.stdout.on('data', bytes => {
    state.buffer += bytes;
    if (state.buffer.length > 2 * 1024 * 1024) return fail(Error('Fixture output exceeded bound'));
    while (state.buffer.includes('\n')) {
      const offset = state.buffer.indexOf('\n');
      const raw = state.buffer.slice(0, offset);
      state.buffer = state.buffer.slice(offset + 1);
      try {
        const value = JSON.parse(raw);
        if (state.waiter) { state.waiter.resolve(value); state.waiter = null; }
        else state.queue.push(value);
      } catch { fail(Error('Fixture emitted invalid control JSON')); }
    }
  });
  const initial = await line();
  check(initial.database_kind === 'synthetic_fixture' && initial.scheduler === 'manual_stdin_only' && initial.root === root, 'Fixture is isolated, marked synthetic and manually scheduled');
  check(await readFile(join(root, 'SYNTHETIC_FIXTURE'), 'utf8') === 'bokkie-conversation-synthetic-v1\n', 'Synthetic marker matches the fixture contract');
  record.initial = initial;
  state.origin = `http://${initial.address}`;
  check(new URL(state.origin).hostname === '127.0.0.1', 'Fixture is loopback only');
  const bootstrap = await request('/bootstrap');
  state.token = bootstrap.mutation_token;
  record.service = bootstrap.service;
  const stats = await control();
  check(stats.model_calls === 0 && stats.catalogue.items.length === 0, `${name} starts with an empty catalogue and zero model calls`);
  await progress('fixture_ready', {fixture: name});
}
async function line() {
  const state = active;
  if (state.queue.length) return state.queue.shift();
  if (state.failure) throw state.failure;
  return new Promise((resolveLine, rejectLine) => {
    const timer = setTimeout(() => { state.waiter = null; rejectLine(Error('Fixture control timed out')); }, Math.min(20000, remaining()));
    state.waiter = {resolve: value => {clearTimeout(timer); resolveLine(value);}, reject: error => {clearTimeout(timer); rejectLine(error);}};
  });
}
async function control(input = {}) {
  active.child.stdin.write(JSON.stringify(input) + '\n');
  const result = await line();
  if (result.error) throw Error(result.error);
  if (Number.isSafeInteger(result.model_calls)) {
    active.record.model_calls = result.model_calls;
    report.calls = report.fixtures.reduce((total, fixture) => total + fixture.model_calls, 0);
  }
  return result;
}
async function request(path, body) {
  const response = await fetch(active.origin + path, {
    method: body ? 'POST' : 'GET',
    headers: body ? {'content-type': 'application/json', origin: active.origin, 'x-bokkie-mutation-token': active.token} : {},
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(Math.min(20000, remaining())),
  });
  const result = await response.json();
  if (!response.ok) throw Error(`HTTP ${response.status} ${path}: ${JSON.stringify(result)}`);
  return result;
}
async function stopFixture() {
  if (!active) return;
  const state = active;
  try { state.record.final = await control(); } catch (error) { state.record.accounting_error = String(error); }
  if (state.child.exitCode === null && state.child.signalCode === null) {
    const exited = new Promise(resolveExit => state.child.once('exit', resolveExit));
    state.child.stdin.end('{"stop":true}\n');
    const kill = setTimeout(() => terminate(state.child), 5000);
    await exited;
    clearTimeout(kill);
  }
  active = undefined;
  await persist();
}
async function send(name, text, view) {
  remaining();
  const before = await control();
  check(report.calls + 2 <= maxCalls, 'Two dispatches remain reserved before each model interaction');
  const body = {command_id: randomUUID(), conversation_id: view?.id ?? randomUUID(), expected_revision: view?.revision ?? 0, text};
  report.first_dispatch_at ??= new Date().toISOString();
  const interaction = {name, fixture: active.record.name, started_at: new Date().toISOString(), input: text, input_sha256: hash(text), request: body, canonical_request_sha256: hash(canonical(body)), calls_before: report.calls};
  report.interactions.push(interaction);
  await progress('interaction_started', {name, command_id: body.command_id});
  await request('/conversations/turn', body);
  const turnStarted = Date.now();
  for (;;) {
    const current = await request(`/conversations/${body.conversation_id}`);
    if (!current.busy && current.messages.some(message => message.role === 'user' && message.request_id === body.command_id)) {
      const after = await control();
      interaction.dispatches = after.model_calls - before.model_calls;
      interaction.view = current;
      interaction.state = after;
      interaction.finished_at = new Date().toISOString();
      await progress('interaction_completed', {name, dispatches: interaction.dispatches});
      check(interaction.dispatches >= 1 && interaction.dispatches <= 2 && report.calls <= maxCalls, `${name} respects the durable dispatch budget`);
      check(!current.request_error, `${name} completed without runtime error: ${current.request_error ?? 'none'}`);
      return current;
    }
    if (Date.now() - turnStarted > Math.min(390000, maxSeconds * 1000)) throw Error(`${name} exceeded its interaction deadline`);
    await delay(Math.min(400, remaining()));
  }
}
function localTime(epoch) {
  return Object.fromEntries(new Intl.DateTimeFormat('en-AU', {timeZone: 'Australia/Adelaide', weekday: 'short', hour: '2-digit', minute: '2-digit', hourCycle: 'h23'}).formatToParts(new Date(epoch * 1000)).map(part => [part.type, part.value]));
}
function validateReminder(view, name) {
  check(view.task?.status === 'draft' && !view.task.active && view.task.runs.length === 0 && view.task.next_wake_at === null, `${name} creates only an inactive draft without work`);
  const definition = view.task.candidate?.definition;
  check(definition?.capability === 'local_note' && definition.trigger.kind === 'recurring' && definition.trigger.timezone === 'Australia/Adelaide', `${name} uses local reminder capability and the Adelaide timezone`);
  check(/research/i.test(definition.instructions) && /queue/i.test(definition.instructions) && /review/i.test(definition.instructions), `${name} retains the requested research queue review text`);
  const occurrences = view.review?.preview?.occurrences ?? [];
  check(view.review?.blockers.length === 0 && occurrences.length >= 3, `${name} has a valid review with future occurrences`);
  const times = occurrences.map(localTime);
  check(times.every(time => ['Mon', 'Tue', 'Wed', 'Thu', 'Fri'].includes(time.weekday) && time.hour === '09' && time.minute === '00'), `${name} preview occurrences are weekdays at 09:00 Adelaide`);
  // Starting Tuesday morning, the five preview dates cover the weekend boundary.
  const expected = [0, 1, 2, 3, 6].map(days => 1790028000 + 5400 + days * 86400);
  check(canonical(occurrences) === canonical(expected), `${name} covers every weekday at 09:00 across the weekend boundary`);
}

try {
  const profile = JSON.parse(await readFile(profilePath, 'utf8'));
  report.profile = Object.fromEntries(['model', 'effort', 'timezone', 'timeout_seconds', 'max_context_bytes', 'max_output_bytes'].map(key => [key, profile[key]]));
  const tracked = execFileSync('git', ['ls-files', '-z'], {encoding: 'utf8'}).split('\0').filter(Boolean);
  const files = [...new Set([...tracked, 'tools/conversation-calibration.mjs'])].sort();
  report.source = {
    candidate_commit: execFileSync('git', ['rev-parse', 'HEAD'], {encoding: 'utf8'}).trim(),
    candidate_tree: execFileSync('git', ['rev-parse', 'HEAD^{tree}'], {encoding: 'utf8'}).trim(),
    files: Object.fromEntries(await Promise.all(files.map(async file => [file, hash(await readFile(file))]))),
  };
  report.source.content_sha256 = hash(canonical(report.source.files));
  report.artifacts = {};
  for (const [name, path] of Object.entries({fixture: binary, configured_broker: profile.broker, broker_instructions: resolve(profile.broker, '../instructions.md'), profile: profilePath})) {
    report.artifacts[name] = {sha256: hash(await readFile(path))};
  }
  await progress('source_recorded');
  const runtimePreflight = execFileSync(binary, ['--profile', profilePath, '--preflight'], {encoding: 'utf8', timeout: Math.min(45000, remaining()), maxBuffer: 65536});
  report.runtime_preflight = JSON.parse(runtimePreflight);
  check(report.runtime_preflight.model_calls === 0, 'Runtime containment preflight dispatches no model');
  await progress('runtime_preflight_passed');
  await startFixture('original');
  if (preflight) {
    const catalogue = await request('/tasks/catalogue');
    const conversations = await request('/conversations');
    check(catalogue.items.length === 0 && conversations.items.length === 0, 'No-model HTTP preflight creates no tasks or conversations');
    check((await control()).model_calls === 0, 'No-model fixture preflight dispatches no model');
  } else {
    let view = await send('original', "Help me set up a weekday reminder to review my research queue at 9 am Adelaide time. Don't activate it yet.");
    validateReminder(view, 'Original request');
    check((await control()).catalogue.items.length === 1, 'Original request creates exactly one draft');
    const confirmRequest = {command_id: randomUUID(), conversation_id: view.id, proposal_id: view.review.id, session_id: view.service.session_id};
    view = await request('/conversations/confirm', confirmRequest);
    report.confirmation = {request: confirmRequest, view};
    check(view.task?.status === 'active' && view.receipt?.command_id === confirmRequest.command_id, 'Explicit operator HTTP confirmation activates the exact draft');
    const activeDefinition = canonical(view.task.active);
    const nextWake = view.task.next_wake_at;
    const runs = canonical(view.task.runs);
    const taskId = view.task.id;
    view = await send('revision', 'Make it Monday mornings instead, still at 9 am Adelaide time.', view);
    check(view.task?.id === taskId && view.task.status === 'active' && canonical(view.task.active) === activeDefinition && view.task.next_wake_at === nextWake && canonical(view.task.runs) === runs, 'Revision preserves the active definition, scheduled work and selected identity until confirmation');
    check(view.task.candidate && canonical(view.task.candidate.definition) !== canonical(view.task.active.definition) && view.review?.blockers.length === 0, 'Revision creates a distinct valid candidate');
    const {trigger: previousTrigger, ...previousBehaviour} = view.task.active.definition;
    const {trigger: proposedTrigger, ...proposedBehaviour} = view.task.candidate.definition;
    check(previousTrigger.timezone === 'Australia/Adelaide' && proposedTrigger.kind === 'recurring' && proposedTrigger.timezone === 'Australia/Adelaide' && canonical(previousBehaviour) === canonical(proposedBehaviour), 'Schedule revision preserves all other task behaviour and the named timezone');
    const monday = view.review?.preview?.occurrences ?? [];
    check(monday.length >= 3 && monday.every(epoch => {const time = localTime(epoch); return time.weekday === 'Mon' && time.hour === '09' && time.minute === '00';}), 'Revision previews Monday 09:00 Adelaide occurrences');
    await stopFixture();
    await startFixture('paraphrase');
    view = await send('paraphrase', 'Please draft a reminder saying to review my research queue every Monday through Friday at 9 in the morning, Australia/Adelaide. Leave it switched off.');
    validateReminder(view, 'Paraphrased request');
    check((await control()).catalogue.items.length === 1, 'Independent paraphrase creates exactly one draft');
    await stopFixture();
    await startFixture('ambiguous');
    const seeded = await control({seed_calibration: true});
    check(seeded.catalogue.items.length === 2 && seeded.model_calls === 0 && seeded.details.every(task => task.status === 'draft' && !task.active && task.runs.length === 0), 'Ambiguous catalogue contains two fixed inactive synthetic tasks');
    view = await send('ambiguous', 'Find my research queue reminder.');
    check(view.candidates.length === 2 && seeded.catalogue.items.every(item => view.candidates.some(candidate => candidate.id === item.id)) && view.selected_task_id === null && view.task === null && view.review === null && view.receipt === null, 'Ambiguous lookup exposes both candidates without selecting or changing either');
    check(canonical((await control()).details) === canonical(seeded.details), 'Ambiguous lookup leaves both stored definitions unchanged');
    await stopFixture();
    await startFixture('vague');
    view = await send('vague', 'I’m thinking about a research finder');
    const stats = await control();
    check(stats.details.every(task => task.status === 'draft' && !task.active && task.runs.length === 0 && task.next_wake_at === null), 'Vague exploration creates no active work');
    check(stats.details.length === 0 || (stats.details.length === 1 && view.task?.status === 'draft' && view.task.candidate?.definition.capability === 'research_finder' && view.review?.blockers.length > 0), 'Vague research idea remains discussion or an honestly blocked research draft');
  }
  report.passed = true;
} catch (error) {
  report.errors.push(String(error.stack ?? error));
  process.exitCode = 1;
} finally {
  await stopFixture();
  clearTimeout(deadline);
  report.finished_at = new Date().toISOString();
  report.calls_exact = report.fixtures.every(fixture => !fixture.accounting_error);
  if (!report.calls_exact) { report.passed = false; process.exitCode = 1; }
  await progress('finished', {passed: report.passed, calls_exact: report.calls_exact, evidence, errors: report.errors});
}
