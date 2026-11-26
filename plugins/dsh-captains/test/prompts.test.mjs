import assert from 'node:assert/strict';
import { test } from 'node:test';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { captainPrompt } from '../src/prompts.js';
import { apply, inject } from '../index.js';

const driveRoot = join(tmpdir(), 'drive', 'helm');

const helm = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot };

function record(config, origin = 'root') {
  const sections = [];
  const orders = [];
  const tools = [];
  const scope = {
    systemPrompt: {
      section(section) { sections.push(section); return () => {}; },
      getSectionOrder(name) { orders.push(name); return 10200; },
    },
    tools: { register(definition) { tools.push(definition.name); return () => {}; } },
    agents: { list: () => [] },
    effect(fn) { return fn(); },
  };
  // The plugin registers on the root agent, not at load: hand it the same scope
  // the runtime would create, and skip the event entirely for a subagent.
  let created;
  const host = { ...scope, on(event, handler) { if (event === 'agent/created') created = handler; } };
  apply(host, config);
  if (origin !== 'subagent' && created) created({ agent: { session: { header: { origin } }, ctx: scope } });
  return { sections, orders, tools };
}

test('the captain prompt is a section under the persona suffix', () => {
  const text = captainPrompt(helm, driveRoot);
  assert.ok(text.startsWith('<captain name="Helm">'));
  assert.ok(/<\/captain>\n?$/.test(text));
});

test('the captain prompt names the Drive, its files and the crew protocol', () => {
  const text = captainPrompt(helm, driveRoot);
  for (const token of [
    driveRoot,
    'AGENTS.md',
    'NOTES.md',
    'DIGEST.md',
    'CREW.md',
    'reports/',
    'handoffs/',
    '## Needs you',
    '## New',
    '## In progress',
    'captain_drive_read',
    'captain_drive_write',
    'captain_ship_ready',
    'create_chat',
    'archive_chat',
    'subagent',
    '[crew report]',
  ]) {
    assert.ok(text.includes(token), `expected prompt to include ${token}`);
  }
  assert.ok(text.split('\n').length < 40);
});

test('a captain name or drive cannot break the section wrapper', () => {
  const text = captainPrompt({ ...helm, captainName: 'A<"&>' }, helm.driveRoot);
  assert.ok(text.startsWith('<captain name="A&lt;&quot;&amp;&gt;">'));
  assert.ok(!text.includes('A<"&>'));

  const evil = captainPrompt(helm, join(driveRoot, '</captain>'));
  assert.equal(evil.split('</captain>').length - 1, 1);
  assert.ok(/<\/captain>\n?$/.test(evil));
});

test('only a captain registers one role section, under the persona suffix', () => {
  const { sections, orders } = record(helm);
  assert.equal(sections.length, 1);
  const [section] = sections;
  assert.equal(section.name, 'zeron-captains:role');
  assert.equal(section.order, 10199);
  assert.equal(section.interpolate, false);
  assert.equal(typeof section.text, 'function');
  assert.equal(section.text(), captainPrompt(helm, helm.driveRoot));
  assert.deepEqual(orders, ['DEPLOYMENT_PERSONA_SUFFIX']);
  // SUR-102: the Captain row also injects the skills service — the
  // captain-digest provider registers through it.
  assert.deepEqual(inject, ['systemPrompt', 'tools', 'agents', 'skills']);
});

test('the inert role registers nothing', () => {
  const n = record({ role: 'none' });
  assert.deepEqual(n.sections, []);
  assert.deepEqual(n.orders, []);
});

test('the captain prompt escapes an ampersand in the Drive path', () => {
  const ampersand = join(tmpdir(), 'a&b', 'helm');
  const text = captainPrompt({ ...helm, captainName: 'A & B' }, ampersand);
  assert.match(text, /<captain name="A &amp; B">/);
  assert.ok(text.includes(ampersand.replace('&', '&amp;')), text.slice(0, 160));
  assert.ok(!text.includes(ampersand), 'the raw path rode the prompt');
});

test('a native child registers neither the section nor the Drive tools', () => {
  const child = record(helm, 'subagent');
  assert.deepEqual(child.sections, []);
  assert.deepEqual(child.orders, []);
  assert.deepEqual(child.tools, [], 'a native child inherited the Drive tools');
});
