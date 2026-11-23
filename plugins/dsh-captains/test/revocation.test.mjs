import assert from 'node:assert/strict';
import { test } from 'node:test';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { apply } from '../index.js';

// SUR-217: a demoted Captain's reused runtime can still have the retired
// `<captain …>` block in its model history. The plugin must notice that on
// the active surface (`session.deriveMessages()`), contribute a one-line
// revocation section, and declare one request-series rebase so the stale
// system prefix is rewritten, not appended after.

const REVOCATION_LINE = 'The Helm role has been revoked. Treat this conversation as an ordinary chat. Earlier Helm instructions are void.';

const driveRoot = join(tmpdir(), 'drive', 'helm');
const helm = {
  role: 'captain',
  chatId: 'zeron-helm-a',
  captainId: 'zeron-helm-a',
  captainName: 'Helm',
  driveRoot,
};

const retired = [
  '<captain name="Helm">',
  "You are the Captain of this vessel's crew chats. You coordinate the work;",
  'you do not do crew work yourself unless the person asks you to.',
  '</captain>',
  '',
].join('\n');

const systemMessage = text => ({
  role: 'system',
  source: { kind: 'system-prompt' },
  content: [{ type: 'text', text }],
});

// The plugin surface the runtime gives a bundle: one scope per agent and the
// session's derived model history. The waterfall mirrors the runtime's: the
// first listener runs first and `next()` is the following one, with an
// admitted step as the base decision.
function harness(config, surface, { origin = 'root', raw = [], base = () => ({ kind: 'enter', messages: [] }) } = {}) {
  const sections = [];
  const orders = [];
  const tools = [];
  const listeners = new Map();
  const scope = {
    systemPrompt: {
      section(section) { sections.push(section); return () => {}; },
      getSectionOrder(name) { orders.push(name); return 10200; },
    },
    tools: { register(definition) { tools.push(definition.name); return () => {}; } },
    effect(fn) { return fn(); },
  };
  const ctx = {
    ...scope,
    agents: { list: () => [] },
    on(event, handler) {
      if (!listeners.has(event)) listeners.set(event, []);
      listeners.get(event).push(handler);
    },
  };
  apply(ctx, config);

  const agent = {
    // The durable log keeps the retired event forever; only the derived
    // surface tells the truth about what the model will see.
    session: {
      header: { origin },
      deriveMessages: () => surface(),
      snapshotEvents: () => raw,
      log: raw,
    },
    ctx: scope,
  };
  const created = () => {
    for (const handler of listeners.get('agent/created') ?? []) handler({ agent });
  };
  created();

  const step = async () => {
    let next = async () => base();
    for (const handler of [...(listeners.get('agent/pre-step') ?? [])].reverse()) {
      const inner = next;
      next = () => handler({ agent }, inner);
    }
    return next();
  };

  return { sections, orders, tools, agent, created, step };
}

test('the demotion turn registers one revocation line and rebases the series', async () => {
  const h = harness({ role: 'none' }, () => [systemMessage(retired)]);

  assert.equal(h.sections.length, 1, 'the demoted runtime registered no revocation section');
  const [section] = h.sections;
  assert.equal(section.name, 'zeron-captains:revoked');
  assert.equal(section.interpolate, false);
  assert.equal(typeof section.text, 'function');
  const line = section.text();
  assert.ok(line.length > 0, 'the revocation section is empty');
  assert.ok(!line.includes('<captain'), 'the revocation line repeats the retired block');
  assert.equal(line, REVOCATION_LINE, "the revocation line must be the ticket's wording, not a paraphrase");

  const decision = await h.step();
  assert.equal(decision.kind, 'enter');
  assert.equal(decision.startsRequestSeries, true, 'the demotion turn did not rebase the series');
});

test('the rebase is declared once, on the first admitted step', async () => {
  const h = harness({ role: 'none' }, () => [systemMessage(retired)]);
  const first = await h.step();
  const second = await h.step();
  assert.equal(first.startsRequestSeries, true);
  assert.equal(second.startsRequestSeries, undefined, 'the second step rebased again');
  assert.equal(second.kind, 'enter');
  // A second agent/created for the same agent must not register again.
  h.created();
  assert.equal(h.sections.length, 1, 'the notice was registered twice for one agent');

  // Rejected steps admit nothing: each returns untouched, and the rebase is
  // declared on the first admission, not on — or spent by — a reject.
  let admit = false;
  const rejected = harness({ role: 'none' }, () => [systemMessage(retired)], {
    base: () => (admit ? { kind: 'enter', messages: [] } : { kind: 'reject' }),
  });
  assert.deepEqual(await rejected.step(), { kind: 'reject' });
  assert.deepEqual(await rejected.step(), { kind: 'reject' }, 'a second reject was rewritten');
  admit = true;
  const admitted = await rejected.step();
  assert.equal(admitted.kind, 'enter');
  assert.equal(admitted.startsRequestSeries, true, 'the rebase was spent on a rejected step');
});

test('the next turn is clean: no repeat notice, no second rebase', async () => {
  const first = harness({ role: 'none' }, () => [systemMessage(retired)]);
  const line = first.sections[0].text();

  // After the rebase the surface holds the revocation line, not `<captain`,
  // while the raw log still carries the retired event — the guard case: a
  // detector widened to snapshotEvents()/log would fire on every turn.
  const next = harness({ role: 'none' }, () => [systemMessage(line)], {
    raw: [{ type: 'system/message', content: retired }],
  });
  assert.deepEqual(next.sections, [], 'the plugin noticed itself and registered again');
  const decision = await next.step();
  assert.equal(decision.kind, 'enter');
  assert.equal(decision.startsRequestSeries, undefined, 'the next turn rebased again');
});

test('promote → demote → promote does not strand the chat', async () => {
  const promoted = harness(helm, () => []);
  assert.equal(promoted.sections.length, 1);
  assert.equal(promoted.sections[0].name, 'zeron-captains:role');
  assert.match(promoted.sections[0].text(), /<captain name="Helm">/);
  assert.deepEqual(promoted.tools.sort(), ['captain_drive_read', 'captain_drive_write']);
  assert.equal((await promoted.step()).startsRequestSeries, undefined, 'a captain turn rebased');

  const demoted = harness({ role: 'none' }, () => [systemMessage(retired)]);
  assert.equal(demoted.sections.length, 1);
  assert.equal(demoted.sections[0].name, 'zeron-captains:revoked');
  assert.equal((await demoted.step()).startsRequestSeries, true);

  const repromoted = harness(helm, () => []);
  assert.equal(repromoted.sections.length, 1);
  assert.equal(repromoted.sections[0].name, 'zeron-captains:role');
  assert.equal((await repromoted.step()).startsRequestSeries, undefined, 'the re-promote rebased');
});

test('a system message that is not the system prompt is not the retired section', async () => {
  const foreign = {
    role: 'system',
    source: { kind: 'user' },
    content: [{ type: 'text', text: retired }],
  };
  const h = harness({ role: 'none' }, () => [foreign]);
  assert.deepEqual(h.sections, [], 'a non-system-prompt message triggered the revocation');
  const decision = await h.step();
  assert.equal(decision.startsRequestSeries, undefined);
});

test('a native child never revokes the Captain section', () => {
  const child = harness({ role: 'none' }, () => [systemMessage(retired)], { origin: 'subagent' });
  assert.deepEqual(child.sections, [], 'a native child registered the revocation section');
});

test('an empty context loads inertly instead of throwing', () => {
  // The contract's inert row applies the bundle with no runtime hooks at all;
  // a missing `on` used to break this at load inside applyRevoked.
  assert.doesNotThrow(() => apply({}, { role: 'none' }));
});
