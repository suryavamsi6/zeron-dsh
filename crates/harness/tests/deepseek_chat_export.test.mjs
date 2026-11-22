import assert from 'node:assert/strict';
import { test } from 'node:test';
import { deflateRawSync } from 'node:zlib';
import { readExport } from '../src/deepseek/chat-export.mjs';

// Minimal ZIP writer: stored entries, or deflate when `deflate` is set.
function makeZip(entries) {
  const parts = [], central = [];
  let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.name);
    const data = Buffer.from(e.data);
    const body = e.deflate ? deflateRawSync(data) : data;
    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0); lh.writeUInt16LE(20, 4); lh.writeUInt16LE(e.deflate ? 8 : 0, 8);
    lh.writeUInt32LE(body.length, 18); lh.writeUInt32LE(data.length, 22); lh.writeUInt16LE(name.length, 26);
    parts.push(lh, name, body);
    const ch = Buffer.alloc(46);
    ch.writeUInt32LE(0x02014b50, 0); ch.writeUInt16LE(20, 4); ch.writeUInt16LE(20, 6); ch.writeUInt16LE(e.deflate ? 8 : 0, 10);
    ch.writeUInt32LE(body.length, 20); ch.writeUInt32LE(data.length, 24); ch.writeUInt16LE(name.length, 28);
    ch.writeUInt32LE(offset, 42);
    central.push(ch, name);
    offset += 30 + name.length + body.length;
  }
  const cd = Buffer.concat(central);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0); eocd.writeUInt16LE(entries.length, 8); eocd.writeUInt16LE(entries.length, 10);
  eocd.writeUInt32LE(cd.length, 12); eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, cd, eocd]);
}

const msg = (id, parent, role, fragments, status = 'FINISHED') =>
  ({ id, parent_id: parent, role, status, inserted_at: '2026-11-01T00:00:00Z', fragments });
const session = (id, title, current, messages) => JSON.stringify(
  { id, title, current_message_id: current, inserted_at: '2026-11-01T00:00:00Z', messages });
const exportZip = (version, index, sessions) => makeZip([
  { name: 'manifest.json', data: JSON.stringify({ export_format_version: version }) },
  { name: 'chat_session_index.json', data: JSON.stringify(index) },
  ...sessions,
]);

test('reads a two-turn conversation with thinking and links', () => {
  const zip = exportZip(3, [{ id: 's1', inserted_at: '2026-11-01T00:00:00Z', updated_at: '2026-11-02T00:00:00Z' }], [
    { name: 'chat_sessions/s1.json', deflate: true, data: session('s1', 'Chat', 4, [
      msg(1, null, 'USER', [{ type: 'REQUEST', content: 'first question' }]),
      msg(2, 1, 'ASSISTANT', [
        { type: 'THINK', content: 'reasoning' },
        { type: 'TOOL_SEARCH', queries: ['q'], results: [{ url: 'https://a.example', title: 'A' }, { url: 'https://b.example' }] },
        { type: 'RESPONSE', content: 'first answer' },
      ]),
      msg(3, 2, 'USER', [{ type: 'REQUEST', content: 'second question' }]),
      msg(4, 3, 'ASSISTANT', [
        { type: 'READ_LINK', results: [{ url: 'https://c.example', title: 'C' }] },
        { type: 'TOOL_OPEN', status: 'FINISHED', url: 'https://c.example', title: 'C' },
        { type: 'TIP', content: 'skipped' },
        { type: 'RESPONSE', content: 'second answer' },
      ]),
    ]) },
  ]);
  assert.deepEqual(readExport(zip), [{
    id: 's1', title: 'Chat', insertedAt: '2026-11-01T00:00:00Z', updatedAt: '2026-11-02T00:00:00Z',
    turns: [
      { role: 'user', text: 'first question' },
      { role: 'assistant', text: 'first answer', thinking: 'reasoning',
        links: [{ title: 'A', url: 'https://a.example' }, { title: null, url: 'https://b.example' }] },
      { role: 'user', text: 'second question' },
      { role: 'assistant', text: 'second answer', links: [{ title: 'C', url: 'https://c.example' }, { title: 'C', url: 'https://c.example' }] },
    ],
  }]);
});

test('selects the current_message_id branch, falling back to the max id', () => {
  const messages = [
    msg(1, null, 'USER', [{ type: 'REQUEST', content: 'question' }]),
    msg(2, 1, 'ASSISTANT', [{ type: 'RESPONSE', content: 'answer' }]),
    msg(3, 2, 'USER', [{ type: 'REQUEST', content: 'follow-up A' }]),
    msg(4, 3, 'ASSISTANT', [{ type: 'RESPONSE', content: 'reply A' }]),
    msg(5, 2, 'USER', [{ type: 'REQUEST', content: 'follow-up B' }]),
  ];
  const index = [
    { id: 'cur', inserted_at: '2026-11-01T00:00:00Z', updated_at: '2026-11-01T00:00:00Z' },
    { id: 'max', inserted_at: '2026-11-01T00:00:00Z', updated_at: '2026-11-03T00:00:00Z' },
  ];
  const zip = exportZip(3, index, [
    { name: 'chat_sessions/cur.json', data: session('cur', null, 4, messages) },
    { name: 'chat_sessions/max.json', deflate: true, data: session('max', null, null, messages) },
  ]);
  const [latest, oldest] = readExport(zip); // newest by updated_at first
  assert.equal(latest.id, 'max');
  assert.deepEqual(latest.turns.map(t => t.text), ['question', 'answer', 'follow-up B']);
  assert.equal(oldest.id, 'cur');
  assert.deepEqual(oldest.turns.map(t => t.text), ['question', 'answer', 'follow-up A', 'reply A']);
});

test('drops a content-filtered turn', () => {
  const zip = exportZip(3, [{ id: 's1', inserted_at: '2026-11-01T00:00:00Z', updated_at: '2026-11-01T00:00:00Z' }], [
    { name: 'chat_sessions/s1.json', data: session('s1', 'Filtered', 2, [
      msg(1, null, 'USER', [{ type: 'REQUEST', content: 'question' }]),
      msg(2, 1, 'ASSISTANT', [{ type: 'RESPONSE', content: 'withdrawn' }], 'CONTENT_FILTER'),
    ]) },
  ]);
  assert.deepEqual(readExport(zip)[0].turns, [{ role: 'user', text: 'question' }]);
});

test("the driver's ceiling is honoured", () => {
  const base = Date.parse('2026-11-01T00:00:00Z');
  const index = [], sessions = [];
  for (let i = 1001; i >= 1; i--) {
    const id = `s${String(i).padStart(4, '0')}`;
    index.push({ id, inserted_at: '2026-11-01T00:00:00Z', updated_at: new Date(base + i * 1000).toISOString() });
    sessions.push({ name: `chat_sessions/${id}.json`, data: session(id, null, 1, [
      msg(1, null, 'USER', [{ type: 'REQUEST', content: 'q' }]),
    ]) });
  }
  const zip = exportZip(3, index, sessions);
  assert.equal(readExport(zip).length, 1000);
  assert.equal(readExport(zip, { maxSessions: 10_000 }).length, 1001);
});

test('rejects other format versions', () => {
  const zip = exportZip(4, [], []);
  assert.throws(() => readExport(zip), error =>
    error.message === 'unsupported-version' && error.code === 'unsupported-version');
});
