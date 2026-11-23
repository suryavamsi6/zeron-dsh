import assert from 'node:assert/strict';
import { test } from 'node:test';
import { captainDigestContent, captainDigestProvider, captainDigestSkill, SKILL_NAME } from '../src/skill.js';
import { apply, inject } from '../index.js';

/** A scope that records what the plugin registers, like the runtime's own. */
function host(config, origin = 'root') {
  const providers = [];
  const scope = {
    systemPrompt: { section: () => () => {}, getSectionOrder: () => 10200 },
    tools: { register: () => () => {} },
    agents: { list: () => [] },
    skills: { registerProvider: create => { providers.push(create({ signal: new AbortController().signal, invalidate: () => {} })); return () => {}; } },
    effect: fn => fn(),
  };
  let created;
  const outer = { ...scope, on(event, handler) { if (event === 'agent/created') created = handler; } };
  apply(outer, config);
  if (origin !== 'subagent' && created) created({ agent: { session: { header: { origin } }, ctx: scope } });
  return { providers };
}

const captain = { role: 'captain', chatId: 'c', captainId: 'c', captainName: 'Helm', driveRoot: '/drive/helm' };

test('the skill name is a stable id', () => {
  assert.match(SKILL_NAME, /^[a-z0-9-]+$/);
  assert.equal(captainDigestSkill.name, SKILL_NAME);
  assert.equal(inject.includes('skills'), true, 'the plugin does not inject the skills service');
});

test('the description is one line and says what the skill is for', () => {
  assert.ok(captainDigestSkill.description.length > 20);
  assert.ok(!captainDigestSkill.description.includes('\n'));
});

test('the instructions carry the three digest sections and the handoff format', () => {
  for (const heading of ['## Needs you', '## New', '## In progress']) {
    assert.ok(captainDigestContent.includes(heading), `the body does not name ${heading}`);
  }
  assert.ok(captainDigestContent.includes('handoffs/<yyyy-mm-dd>-<slug>.md'), 'the body does not name the handoff file');
  for (const field of ['Goal', 'State', 'Next', 'Links']) {
    assert.ok(captainDigestContent.includes(field), `the handoff format does not carry ${field}`);
  }
  assert.ok(captainDigestContent.includes('[crew report]'), 'the body does not say when the digest is rewritten');
  assert.ok(captainDigestContent.includes('archive_chat'), 'the body does not say how finished crew is archived');
});

test('the body stays short enough to load on demand', () => {
  assert.ok(captainDigestContent.split('\n').length <= 80, `the body is ${captainDigestContent.split('\n').length} lines`);
});

test('a Captain row registers the skill once and a crew or ordinary row registers none', async () => {
  const helm = host(captain);
  assert.equal(helm.providers.length, 1, 'a captain row registered no skill provider');
  const provider = helm.providers[0];
  assert.equal(provider.name, 'zeron-captains');
  const listed = await provider.list({ cwd: '/drive/helm' });
  assert.equal(listed.length, 1);
  assert.equal(listed[0].name, SKILL_NAME);
  assert.equal(listed[0].invocation.modelInvocable, true);
  assert.equal(listed[0].source, 'runtime');
  const loaded = await provider.get(listed[0], { cwd: '/drive/helm' });
  assert.equal(loaded.name, SKILL_NAME);
  assert.ok(loaded.content.length > 200);
  for (const other of [{ ...captain, role: 'crew' }, { role: 'none' }]) {
    assert.equal(host(other).providers.length, 0, `a ${other.role} row registered a skill provider`);
  }
  assert.equal(captainDigestProvider().name, 'zeron-captains');
});
