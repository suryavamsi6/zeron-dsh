// Read a DeepSeek chat export (export_format_version 3) into plain
// conversations. `node chat-export.mjs <deepseek_data-*.zip>` prints JSON.
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { inflateRawSync } from 'node:zlib';

const MAX_ENTRY = 256 * 1024 * 1024;

export function readExport(zipBytes, { maxSessions = 1000 } = {}) {
  const buf = Buffer.isBuffer(zipBytes) ? zipBytes : Buffer.from(zipBytes.buffer, zipBytes.byteOffset, zipBytes.byteLength);
  const entries = zipDirectory(buf);
  const json = name => {
    const entry = entries.get(name);
    if (!entry) throw new Error(`the export has no ${name}`);
    return JSON.parse(entryData(buf, entry).toString('utf8'));
  };
  if (json('manifest.json')?.export_format_version !== 3) {
    throw Object.assign(new Error('unsupported-version'), { code: 'unsupported-version' });
  }
  const rows = new Map();
  for (const row of json('chat_session_index.json') ?? []) {
    if (row && typeof row.id === 'string' && !rows.has(row.id)) rows.set(row.id, row);
  }
  const ranked = [...rows.values()].sort((a, b) =>
    time(b.updated_at) - time(a.updated_at) || time(b.inserted_at) - time(a.inserted_at) || (a.id < b.id ? -1 : 1));
  return ranked.slice(0, maxSessions).map(row => {
    const file = json(`chat_sessions/${row.id}.json`);
    return { id: row.id, title: file?.title ?? null, insertedAt: row.inserted_at ?? null,
      updatedAt: row.updated_at ?? null, turns: turns(branch(file)) };
  });
}

function time(value) {
  const t = Date.parse(value);
  return Number.isNaN(t) ? 0 : t;
}

// The selected branch: back from current_message_id (or the greatest id when
// it is absent, null, or unknown) via parent_id, stopping on a missing parent
// or a cycle.
function branch(file) {
  const byId = new Map();
  let newest;
  for (const m of Array.isArray(file?.messages) ? file.messages : []) {
    if (!m || typeof m.id !== 'number') continue;
    byId.set(m.id, m);
    if (!newest || m.id > newest.id) newest = m;
  }
  const chain = [];
  const seen = new Set();
  for (let m = byId.get(file?.current_message_id) ?? newest; m && !seen.has(m.id); m = byId.get(m.parent_id)) {
    seen.add(m.id);
    chain.unshift(m);
  }
  return chain;
}

function turns(chain) {
  const out = [];
  for (const m of chain) {
    const role = m.role === 'USER' ? 'user' : m.role === 'ASSISTANT' ? 'assistant' : null;
    if (!role || m.status === 'CONTENT_FILTER') continue;
    const text = [], thinking = [], links = [];
    const link = value => {
      if (!value || typeof value.url !== 'string' || value.url.length === 0) return;
      const title = typeof value.title === 'string' && value.title.length > 0 ? value.title : null;
      links.push({ title, url: value.url });
    };
    for (const f of Array.isArray(m.fragments) ? m.fragments : []) {
      switch (f?.type) {
        case 'REQUEST': if (role === 'user' && typeof f.content === 'string') text.push(f.content); break;
        case 'RESPONSE': if (role === 'assistant' && typeof f.content === 'string') text.push(f.content); break;
        case 'THINK': if (role === 'assistant' && typeof f.content === 'string') thinking.push(f.content); break;
        case 'TOOL_SEARCH': case 'READ_LINK':
          if (role === 'assistant' && Array.isArray(f.results)) f.results.forEach(link);
          break;
        case 'TOOL_OPEN': if (role === 'assistant') link(f); break;
      }
    }
    const turn = { role, text: text.join('\n') };
    if (thinking.length > 0) turn.thinking = thinking.join('\n');
    if (links.length > 0) turn.links = links;
    out.push(turn);
  }
  return out;
}

// Minimal ZIP reader: central directory only, stored + deflate entries.
function zipDirectory(buf) {
  let eocd = -1;
  for (let i = buf.length - 22; i >= Math.max(0, buf.length - 22 - 0xffff); i--) {
    if (buf.readUInt32LE(i) === 0x06054b50 && i + 22 + buf.readUInt16LE(i + 20) === buf.length) { eocd = i; break; }
  }
  if (eocd < 0) throw new Error('not a ZIP archive');
  const count = buf.readUInt16LE(eocd + 10), size = buf.readUInt32LE(eocd + 12), offset = buf.readUInt32LE(eocd + 16);
  if (buf.readUInt16LE(eocd + 4) || buf.readUInt16LE(eocd + 6) || buf.readUInt16LE(eocd + 8) !== count ||
      count === 0xffff || size === 0xffffffff || offset === 0xffffffff) {
    throw new Error('unsupported: multi-disk or ZIP64 archive');
  }
  const entries = new Map();
  for (let i = 0, p = offset; i < count; i++) {
    if (buf.readUInt32LE(p) !== 0x02014b50) throw new Error('not a ZIP archive');
    const flags = buf.readUInt16LE(p + 8), method = buf.readUInt16LE(p + 10);
    const csize = buf.readUInt32LE(p + 20), usize = buf.readUInt32LE(p + 24), local = buf.readUInt32LE(p + 42);
    if (csize === 0xffffffff || usize === 0xffffffff || local === 0xffffffff) throw new Error('unsupported: ZIP64 entry');
    const nlen = buf.readUInt16LE(p + 28), xlen = buf.readUInt16LE(p + 30), clen = buf.readUInt16LE(p + 32);
    entries.set(buf.subarray(p + 46, p + 46 + nlen).toString('utf8'), { flags, method, csize, usize, local });
    p += 46 + nlen + xlen + clen;
  }
  return entries;
}

function entryData(buf, e) {
  if (e.usize > MAX_ENTRY || e.csize > MAX_ENTRY) throw new Error('ZIP entry exceeds 256 MiB');
  if (e.flags & 1) throw new Error('unsupported: encrypted ZIP entry');
  if (e.method !== 0 && e.method !== 8) throw new Error(`unsupported: compression method ${e.method}`);
  if (buf.readUInt32LE(e.local) !== 0x04034b50) throw new Error('corrupt ZIP entry');
  const start = e.local + 30 + buf.readUInt16LE(e.local + 26) + buf.readUInt16LE(e.local + 28);
  const raw = buf.subarray(start, start + e.csize);
  if (raw.length !== e.csize || e.method === 0 && e.usize !== e.csize) throw new Error('corrupt ZIP entry');
  if (e.method === 0) return raw;
  const data = inflateRawSync(raw, { maxOutputLength: MAX_ENTRY });
  if (data.length !== e.usize) throw new Error('corrupt ZIP entry');
  return data;
}

async function main() {
  const file = process.argv[2];
  if (!file) throw new Error('usage: node chat-export.mjs <deepseek_data-*.zip>');
  process.stdout.write(`${JSON.stringify(readExport(await readFile(file)), null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch(error => { console.error(`chat-export: ${error?.message ?? error}`); process.exitCode = 1; });
}
