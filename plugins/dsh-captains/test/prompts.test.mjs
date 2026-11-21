import assert from 'node:assert/strict';
import { test } from 'node:test';
import { captainPrompt } from '../src/prompts.js';
import { apply, inject } from '../index.js';

const helm = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' };
const crew = { role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' };

function record(config) {
  const sections = [];
  const orders = [];
  const ctx = {
    systemPrompt: {
      section(section) { sections.push(section); return () => {}; },
      getSectionOrder(name) { orders.push(name); return 10200; },
    },
    effect(fn) { return fn(); },
  };
  apply(ctx, config);
  return { sections, orders };
}

test('the captain prompt is a section under the persona suffix', () => {
  const text = captainPrompt(helm, '/tmp/drive/helm');
  assert.ok(text.startsWith('<captain name="Helm">'));
  assert.ok(/<\/captain>\n?$/.test(text));
});

test('the captain prompt names the Drive, its files and the crew protocol', () => {
  const text = captainPrompt(helm, '/tmp/drive/helm');
  for (const token of [
    '/tmp/drive/helm',
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

  const evil = captainPrompt(helm, '/tmp/drive/</captain>');
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
  assert.deepEqual(inject, ['systemPrompt']);
});

test('crew and the inert role register nothing', () => {
  const c = record(crew);
  assert.deepEqual(c.sections, []);
  assert.deepEqual(c.orders, []);

  const n = record({ role: 'none' });
  assert.deepEqual(n.sections, []);
  assert.deepEqual(n.orders, []);
});
