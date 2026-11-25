import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { apply } from '../index.js';

// SUR-85: in a crew chat the bundle serves the Captain's CREW.md as a
// `zeron-captains:crew-rules` prompt section that re-reads the file at every
// assembly, plus an `agent/pre-step` hook that declares a request-series
// rebase when the served bytes changed — a changed file must not leave an
// old copy of the rules sitting in the request.

const CREW_RULES = 'zeron-captains:crew-rules';
const TRUNCATION_NOTE = '\n[CREW.md truncated at 8 KB]';
const CREW_CAP = 8 * 1024;

const crew = driveRoot => ({ role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot });
const helm = driveRoot => ({ role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot });

async function drive(t) {
  const root = await mkdtemp(join(tmpdir(), 'captains-crew-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

// The plugin surface the runtime hands a bundle: sections land on each
// created agent's own scope, while agent/* hooks ride the plugin context —
// one listener table behind both `on` surfaces captures either registration
// path. `agent/created` fires for every origin (the bundle gates on
// `header.origin`), and `preStep` runs the captured `agent/pre-step`
// listeners like the runtime waterfall: each handler's next() is the
// following listener, and the innermost one returns the base decision.
function harness(config, { origin = 'root' } = {}) {
  const sections = [];
  const orders = [];
  const tools = [];
  const listeners = new Map();
  const on = (event, handler) => {
    if (!listeners.has(event)) listeners.set(event, []);
    listeners.get(event).push(handler);
  };
  const scope = {
    systemPrompt: {
      section(section) { sections.push(section); return () => {}; },
      getSectionOrder(name) { orders.push(name); return 10200; },
    },
    tools: { register(definition) { tools.push(definition.name); return () => {}; } },
    agents: { list: () => [] },
    effect(fn) { return fn(); },
    on,
  };
  apply(scope, config);
  const agent = { session: { header: { origin } }, ctx: scope };
  for (const handler of listeners.get('agent/created') ?? []) handler({ agent });

  const preStep = async (event, base) => {
    let nextCalls = 0;
    let next = async () => { nextCalls += 1; return base; };
    for (const handler of [...(listeners.get('agent/pre-step') ?? [])].reverse()) {
      const inner = next;
      next = () => handler(event, inner);
    }
    const result = await next();
    return { result, nextCalls };
  };
  const crewRules = () => sections.find(section => section.name === CREW_RULES);
  return { sections, orders, tools, agent, preStep, crewRules };
}

test('a crew root registers one crew-rules section; captain, inert and native-child rows do not', async t => {
  const root = await drive(t);
  const h = harness(crew(root));
  assert.equal(h.sections.length, 1, 'the crew row did not register exactly one section');
  const [section] = h.sections;
  assert.equal(section.name, CREW_RULES);
  assert.equal(section.interpolate, false);
  assert.equal(typeof section.text, 'function');
  assert.equal(typeof section.order, 'number');

  const captain = harness(helm(root));
  assert.equal(captain.sections.filter(s => s.name === CREW_RULES).length, 0,
    'the captain row registered the crew-rules section');

  const none = harness({ role: 'none' });
  assert.deepEqual(none.sections, [], 'the inert row registered a section');

  const child = harness(crew(root), { origin: 'subagent' });
  assert.deepEqual(child.sections, [], 'a native child registered the crew-rules section');
});

test('the crew-rules section re-reads CREW.md at every assembly', async t => {
  const root = await drive(t);
  const file = join(root, 'CREW.md');
  await writeFile(file, 'Rule A71: file reports under reports/.');
  const section = harness(crew(root)).crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const signal = new AbortController().signal;

  const first = section.text({ signal });
  assert.ok(first.startsWith('<crew-rules captain="Helm">'), first.slice(0, 80));
  assert.ok(first.includes('Rule A71'), 'the section did not serve the file');
  assert.match(first, /<\/crew-rules>\n?$/);

  await writeFile(file, 'Rule B29: handoffs/ briefs belong to the Captain.');
  const second = section.text({ signal });
  assert.ok(second.includes('Rule B29'), 'the section served a cached file');
  assert.ok(!second.includes('Rule A71'), 'a stale copy of the old file rode the assembly');
});

test('a missing or blank CREW.md contributes an empty section', async t => {
  const root = await drive(t);
  const section = harness(crew(root)).crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const signal = new AbortController().signal;
  assert.equal(section.text({ signal }), '', 'a missing file produced text');
  await writeFile(join(root, 'CREW.md'), '  \n\t \n');
  assert.equal(section.text({ signal }), '', 'a blank file produced text');
});

test('a CREW.md over 8 KB is cut at the cap without splitting a codepoint', async t => {
  const root = await drive(t);
  // '→' is three UTF-8 bytes starting at byte 8190: a naive 8 KB cut lands
  // mid-codepoint, so a byte-honest cut has to end just before it.
  const file = 'a'.repeat(8190) + '→' + 'b'.repeat(400);
  assert.ok(Buffer.byteLength(file) > CREW_CAP);
  await writeFile(join(root, 'CREW.md'), file);

  const section = harness(crew(root)).crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const text = section.text({ signal: new AbortController().signal });
  assert.ok(text.startsWith('<crew-rules captain="Helm">'), text.slice(0, 80));
  assert.match(text, /<\/crew-rules>\n?$/);

  const inner = text.slice(text.indexOf('>') + 1, text.lastIndexOf('</crew-rules>'));
  assert.match(inner, /\n\[CREW\.md truncated at 8 KB\]\n?$/, 'the cut edge does not name the cap');
  const served = inner.slice(0, inner.indexOf(TRUNCATION_NOTE)).replace(/^\n/, '');
  assert.ok(Buffer.byteLength(served) <= CREW_CAP, 'the served file crossed the 8 KB cap');
  assert.ok(!served.includes('�'), 'the cut ended mid-codepoint — a replacement char rode the edge');
  assert.ok(file.startsWith(served), 'the served body is not a prefix of the file');
});

test('captain-only files in the same Drive never reach the crew section', async t => {
  const root = await drive(t);
  const sentinel = 'AGENTS-MD-SENTINEL-9f4e';
  await writeFile(join(root, 'AGENTS.md'), `Captain-only rule: ${sentinel}`);
  await writeFile(join(root, 'CREW.md'), 'Crew rule: file reports under reports/.');
  const section = harness(crew(root)).crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const text = section.text({ signal: new AbortController().signal });
  assert.ok(text.includes('Crew rule'), 'the section did not serve CREW.md');
  assert.ok(!text.includes(sentinel), 'AGENTS.md rode the crew-rules section');
});

test('a changed CREW.md rebases the series on the next admitted first step', async t => {
  const root = await drive(t);
  const file = join(root, 'CREW.md');
  await writeFile(file, 'rules v1');
  const h = harness(crew(root));
  const section = h.crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const enter = { kind: 'enter', messages: [{}] };

  // A process with no recorded baseline must rebase: the resumed series may
  // already hold CREW.md bytes this process never saw, so its first admitted
  // step cannot pass through clean.
  const s1 = new AbortController().signal;
  section.text({ signal: s1 });
  const first = await h.preStep({ agent: h.agent, turn: 1, step: 1, signal: s1 }, enter);
  assert.equal(first.nextCalls, 1, 'the hook did not call next()');
  assert.deepEqual(first.result, { ...enter, startsRequestSeries: true },
    'an unknown baseline was trusted instead of rebased');

  // An edited file serves different bytes; the next admitted step-1 decision
  // must rebase the series so the stale copy cannot linger in it.
  await writeFile(file, 'rules v2 — changed');
  const s2 = new AbortController().signal;
  section.text({ signal: s2 });
  const second = await h.preStep({ agent: h.agent, turn: 2, step: 1, signal: s2 }, enter);
  assert.equal(second.nextCalls, 1, 'the hook did not call next()');
  assert.deepEqual(second.result, { ...enter, startsRequestSeries: true },
    'a changed CREW.md did not rebase the request series');

  // The following assembly is unchanged: the decision passes through clean.
  const s3 = new AbortController().signal;
  section.text({ signal: s3 });
  const third = await h.preStep({ agent: h.agent, turn: 3, step: 1, signal: s3 }, enter);
  assert.deepEqual(third.result, enter, 'an unchanged assembly rebased again');

  // A decision that admits nothing is returned untouched even with new bytes.
  await writeFile(file, 'rules v3');
  const s4 = new AbortController().signal;
  section.text({ signal: s4 });
  const skipped = await h.preStep({ agent: h.agent, turn: 4, step: 1, signal: s4 }, { kind: 'skip' });
  assert.deepEqual(skipped.result, { kind: 'skip' }, 'a skipped step was rewritten');

  // Only a turn's first step carries the rebase.
  await writeFile(file, 'rules v4');
  const s5 = new AbortController().signal;
  section.text({ signal: s5 });
  const midTurn = await h.preStep({ agent: h.agent, turn: 5, step: 2, signal: s5 }, enter);
  assert.deepEqual(midTurn.result, enter, 'a non-first step rebased');
});

test('sessions without ids keep independent crew-rules baselines', async t => {
  const root = await drive(t);
  const file = join(root, 'CREW.md');
  await writeFile(file, 'rules v1');
  const h = harness(crew(root));
  const section = h.crewRules();
  assert.ok(section, 'the crew-rules section was not registered');
  const enter = { kind: 'enter', messages: [{}] };
  const secondAgent = { session: { header: { origin: 'root' } }, ctx: h.agent.ctx };
  assert.equal(h.agent.session.id, undefined, 'the first fixture session unexpectedly carried an id');
  assert.equal(secondAgent.session.id, undefined, 'the second fixture session unexpectedly carried an id');

  const admitted = async (agent, turn) => {
    const signal = new AbortController().signal;
    section.text({ signal });
    return (await h.preStep({ agent, turn, step: 1, signal }, enter)).result;
  };

  assert.deepEqual(await admitted(h.agent, 1), { ...enter, startsRequestSeries: true },
    'the first session did not rebase its unknown baseline');
  assert.deepEqual(await admitted(secondAgent, 1), { ...enter, startsRequestSeries: true },
    'a second id-less session shared the first session\'s baseline');
  assert.deepEqual(await admitted(h.agent, 2), enter,
    'the first session rebased unchanged bytes');
  assert.deepEqual(await admitted(secondAgent, 2), enter,
    'the second session rebased unchanged bytes');

  await writeFile(file, 'rules v2 — changed');
  assert.deepEqual(await admitted(h.agent, 3), { ...enter, startsRequestSeries: true },
    'a changed CREW.md did not rebase the first session');
  assert.deepEqual(await admitted(secondAgent, 3), { ...enter, startsRequestSeries: true },
    'a changed CREW.md did not rebase the second session');
});

// SUR-85 r2: the engine stops a parked runtime after idle and resumes the
// session on the next message, so the first turn of a NEW process is the
// normal path. That process has no in-memory baseline — it cannot know which
// bytes the resumed series was built from — so it must rebase to CREW.md as it
// is now rather than pass the decision through and leave the retired rules.
test('a restarted process rebases the first turn to the current CREW.md', async t => {
  const root = await drive(t);
  const file = join(root, 'CREW.md');
  await writeFile(file, 'rules v1 — before the restart');
  const enter = { kind: 'enter', messages: [{}] };

  const before = harness(crew(root));
  const beforeSection = before.crewRules();
  assert.ok(beforeSection, 'the crew-rules section was not registered');
  const s1 = new AbortController().signal;
  assert.ok(beforeSection.text({ signal: s1 }).includes('before the restart'));
  const baseline = await before.preStep({ agent: before.agent, turn: 1, step: 1, signal: s1 }, enter);
  assert.deepEqual(baseline.result, { ...enter, startsRequestSeries: true },
    'the pre-restart admission did not rebase its unknown baseline');

  // The runtime was stopped while the chat was parked; the file changed while
  // it was down, and the next process resumes the same session with a fresh
  // listener table and a fresh in-memory baseline.
  await writeFile(file, 'rules v2 — changed while the runtime was down');
  const after = harness(crew(root));
  const afterSection = after.crewRules();
  assert.ok(afterSection, 'the restarted crew row registered no section');
  const s2 = new AbortController().signal;
  const served = afterSection.text({ signal: s2 });
  assert.ok(served.includes('changed while the runtime was down'), 'the restarted section did not serve the new file');
  assert.ok(!served.includes('before the restart'), 'the restarted section served the old file');

  const restarted = await after.preStep({ agent: after.agent, turn: 1, step: 1, signal: s2 }, enter);
  assert.equal(restarted.nextCalls, 1, 'the hook did not call next()');
  assert.deepEqual(restarted.result, { ...enter, startsRequestSeries: true },
    'the first turn after a restart left the retired rules in the resumed series');
});
