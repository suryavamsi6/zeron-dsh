import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { driveDir, driveToolSpecs, resolveInDrive } from '../src/tools.js';
import { apply, normalizeConfig } from '../index.js';
import { normalizeConfig as sourceNormalizeConfig } from '../src/config.js';

const captain = root => ({ role: 'captain', chatId: 'captain-1', captainId: 'captain-1', captainName: 'B', driveRoot: root });

async function drive(t) {
  const root = await mkdtemp(join(tmpdir(), 'captains-drive-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

function tools(root) {
  const [read, write] = driveToolSpecs(captain(root));
  return { read, write };
}

test('the drive dir is the validated row root', async t => {
  const root = await drive(t);
  assert.equal(driveDir(captain(root)), root);
  assert.throws(() => resolveInDrive(root, '../x'), /path is outside the Captain Drive/);
  // The resolution carries the physical path I/O must use: a symlinked folder
  // inside the Drive would otherwise redirect the write after the check.
  await mkdir(join(root, 'notes'), { recursive: true });
  const resolved = resolveInDrive(root, '././/notes/./a.txt');
  assert.equal(typeof resolved, 'object', 'resolveInDrive must return { target, realTarget, realDrive }');
  assert.equal(resolved.target, join(root, 'notes', 'a.txt'));
  assert.equal(resolved.realDrive, realpathSync(root));
  assert.equal(resolved.realTarget, join(realpathSync(root), 'notes', 'a.txt'));
});

test('read lists the root with folders marked by a trailing slash', async t => {
  const root = await drive(t);
  await mkdir(join(root, 'notes'));
  await writeFile(join(root, 'README.md'), 'hello drive');
  await writeFile(join(root, 'notes', 'one.md'), 'one');
  const { read } = tools(root);
  const listing = await read.execute({ path: '' });
  const lines = listing.split('\n');
  assert.deepEqual(lines, ['README.md', 'notes/']);
});

test('write then read round-trips a file under a nested path', async t => {
  const root = await drive(t);
  const { read, write } = tools(root);
  const wrote = await write.execute({ path: 'notes/deep/a.txt', content: 'hello' });
  assert.match(wrote, /^wrote notes\/deep\/a\.txt \(5 bytes\)$/);
  assert.equal(await read.execute({ path: 'notes/deep/a.txt' }), 'hello');
  // The write created the parents it needed.
  assert.deepEqual((await readdir(join(root, 'notes', 'deep'))).sort(), ['a.txt']);
  // The reported path is the resolved one, not the raw display string.
  assert.equal(await write.execute({ path: '././/notes/deep/b.txt', content: 'abc' }), 'wrote notes/deep/b.txt (3 bytes)');
});

test('paths outside the Drive are rejected, including through a symlink', async t => {
  const root = await drive(t);
  const outside = await mkdtemp(join(tmpdir(), 'captains-outside-'));
  t.after(() => rm(outside, { recursive: true, force: true }));
  await writeFile(join(outside, 'secret.txt'), 'secret');
  await symlink(outside, join(root, 'link'));
  const { read, write } = tools(root);

  await assert.rejects(read.execute({ path: '../secret.txt' }), /path is outside the Captain Drive/);
  await assert.rejects(read.execute({ path: '/etc/passwd' }), /path is outside the Captain Drive/);
  await assert.rejects(write.execute({ path: '../escape.txt', content: 'x' }), /path is outside the Captain Drive/);
  await assert.rejects(write.execute({ path: `${root}/absolute.txt`, content: 'x' }), /path is outside the Captain Drive/);
  // A symlink inside the Drive that points outside must not carry I/O with it.
  await assert.rejects(read.execute({ path: 'link/secret.txt' }), /path is outside the Captain Drive/);
  await assert.rejects(write.execute({ path: 'link/evil.txt', content: 'x' }), /path is outside the Captain Drive/);
  // A NUL is never a path.
  await assert.rejects(read.execute({ path: 'a\0b' }), /path is outside the Captain Drive/);
});

test('a 70 KB file reads truncated at 64 KB', async t => {
  const root = await drive(t);
  await writeFile(join(root, 'big.txt'), 'a'.repeat(70 * 1024));
  const { read } = tools(root);
  const value = await read.execute({ path: 'big.txt' });
  assert.ok(value.endsWith('[truncated]'), 'the note names the cap');
  const body = value.slice(0, value.length - '[truncated]'.length).replace(/\n$/, '');
  assert.equal(Buffer.byteLength(body), 64 * 1024, 'the body is exactly the cap');
  assert.ok(Buffer.byteLength(value) < 70 * 1024, 'the whole file never rides the reply');
});

test('writes respect the byte limit and engine-owned folders', async t => {
  const root = await drive(t);
  const { write } = tools(root);
  await assert.rejects(write.execute({ path: 'huge.txt', content: 'x'.repeat(256 * 1024 + 1) }), /256 KB/);
  await assert.rejects(write.execute({ path: 'state/needs-you.json', content: '{}' }), /engine-owned/);
  await assert.rejects(write.execute({ path: 'reports/turn.md', content: '# report' }), /engine-owned/);
  // The guard runs on the resolved path, not on the string the model sent: a
  // display prefix must not smuggle a write into the engine's folders.
  for (const path of ['.//state/needs-you.json', './/reports/turn-1.md', '././/state/needs-you.json']) {
    await assert.rejects(write.execute({ path, content: 'x' }), /engine-owned/, `${path} slipped past the engine-owned guard`);
  }
  // A symlink alias inside the Drive still names the engine's folder.
  await mkdir(join(root, 'state'), { recursive: true });
  await symlink(join(root, 'state'), join(root, 'alias'));
  await assert.rejects(write.execute({ path: 'alias/needs-you.json', content: '{}' }), /engine-owned/,
    'a symlink alias reached the engine-owned folder');
  // Positive controls: a prefix is not the folder, and notes stays writable.
  for (const path of ['notes/x.md', 'stateful/x.md']) {
    await write.execute({ path, content: 'ok' });
    assert.deepEqual(await readdir(join(root, path.split('/')[0])), ['x.md'], path);
  }
  // Writing through a symlinked folder lands at the real path and says so.
  await symlink(join(root, 'notes'), join(root, 'notes-alias'));
  assert.equal(await write.execute({ path: 'notes-alias/y.txt', content: 'abc' }), 'wrote notes/y.txt (3 bytes)');
  // Reads are not blocked by ownership: the Captain reads what the engine writes.
  const { read } = tools(root);
  await mkdir(join(root, 'reports'), { recursive: true });
  await writeFile(join(root, 'reports', 'turn.md'), '# report');
  assert.equal(await read.execute({ path: 'reports/turn.md' }), '# report');
});

test('a write leaves no temp file behind, even when it fails', async t => {
  const root = await drive(t);
  const { write } = tools(root);
  await write.execute({ path: 'notes/ok.txt', content: 'ok' });
  // A non-empty directory at the target is what makes the rename fail *after*
  // the temp file exists, which is the cleanup path under test.
  await mkdir(join(root, 'blocked'), { recursive: true });
  await writeFile(join(root, 'blocked', 'occupied.txt'), 'occupied');
  await assert.rejects(write.execute({ path: 'blocked', content: 'x' }));
  const names = (await readdir(root, { recursive: true })).map(name => String(name));
  assert.deepEqual(names.filter(name => name.includes('.tmp')), []);
});

test('a read-only run refuses to write the Drive', async t => {
  const root = await drive(t);
  const { write } = tools(root);
  const previous = process.env.DSH_PERMISSION_MODE;
  process.env.DSH_PERMISSION_MODE = 'read-only';
  t.after(() => {
    if (previous === undefined) delete process.env.DSH_PERMISSION_MODE;
    else process.env.DSH_PERMISSION_MODE = previous;
  });
  await assert.rejects(write.execute({ path: 'notes/blocked.txt', content: 'x' }), /read-only/);
  assert.deepEqual(await readdir(root), []);
});

// SUR-104 scopes the captain contribution to the root agent; the Drive tools
// ride the same gate, or every native child inherits the Captain's hands.
function gated(config, origin) {
  const registered = [];
  const scope = {
    systemPrompt: { section: () => () => {}, getSectionOrder: () => 10200 },
    tools: { register(definition) { registered.push(definition.name); return () => {}; } },
    effect: fn => fn(),
  };
  let created;
  apply({ ...scope, on(event, handler) { if (event === 'agent/created') created = handler; } }, config);
  if (origin !== 'subagent' && created) created({ agent: { session: { header: { origin } }, ctx: scope } });
  return registered;
}

test('only the root agent registers the Drive tools', async t => {
  const root = await drive(t);
  const config = captain(root);
  assert.deepEqual(gated(config, 'root').sort(), ['captain_drive_read', 'captain_drive_write']);
  const crew = { ...config, role: 'crew', chatId: 'crew-1' };
  assert.deepEqual(gated(crew, 'root'), [], 'a crew chat registered a Drive tool');
  assert.deepEqual(gated(config, 'subagent'), [], 'a native child inherited the Drive tools');
});

test('the engine-owned predicate names the first segment, per platform', async () => {
  const { isEngineOwned } = await import('../src/tools.js');
  assert.equal(typeof isEngineOwned, 'function', 'src/tools.js does not export the engine-owned predicate');
  for (const segment of ['state', 'reports']) assert.equal(isEngineOwned(segment, 'linux'), true, segment);
  for (const platform of ['darwin', 'win32']) {
    assert.equal(isEngineOwned('State', platform), true, platform);
    assert.equal(isEngineOwned('REPORTS', platform), true, platform);
  }
  // Windows strips a trailing dot from a folder name, so `state.` is `state`.
  assert.equal(isEngineOwned('state.', 'win32'), true);
  assert.equal(isEngineOwned('reports.', 'win32'), true);
  assert.equal(isEngineOwned('state.', 'linux'), false);
  assert.equal(isEngineOwned('State', 'linux'), false);
  // Positive controls: a prefix is not the folder.
  for (const segment of ['notes', 'stateful', '', '.']) assert.equal(isEngineOwned(segment, 'darwin'), false, segment);
});
