import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { driveDir, driveToolSpecs, resolveInDrive } from '../src/tools.js';

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
  await writeFile(join(root, 'blocked'), 'a file, not a folder');
  await assert.rejects(write.execute({ path: 'blocked/child.txt', content: 'x' }));
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
