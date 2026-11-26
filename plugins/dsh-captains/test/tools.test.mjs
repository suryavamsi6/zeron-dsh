import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtemp, mkdir, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { driveDir, driveToolSpecs, resolveInDrive } from '../src/tools.js';
import { apply } from '../index.js';

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
  await symlink(join(root, 'notes'), join(root, 'notes-alias'));
  const { read } = tools(root);
  const listing = await read.execute({ path: '' });
  const lines = listing.split('\n');
  // Links are part of the listing: they are usable aliases the suite proves
  // work, and hiding them made the Drive look emptier than it is.
  assert.deepEqual(lines, ['README.md', 'notes/', 'notes-alias@']);
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
  // Exactly 256 KiB is inside the limit; one byte more is not (SUR-183).
  const exact = 'x'.repeat(256 * 1024);
  assert.equal(await write.execute({ path: 'exact.txt', content: exact }), 'wrote exact.txt (262144 bytes)');
  await assert.rejects(write.execute({ path: 'huge.txt', content: 'x'.repeat(256 * 1024 + 1) }), /256 KB/);
  await assert.rejects(write.execute({ path: 'state/needs-you.json', content: '{}' }), /engine-owned/);
  await assert.rejects(write.execute({ path: 'reports/turn.md', content: '# report' }), /engine-owned/);
  await write.execute({ path: 'state/ready/crew-a.json', content: '{}' });
  for (const path of ['state/ready/not-json.txt', 'state/ready/nested/crew-a.json', 'state/merge-queue/results/crew-a.json']) {
    await assert.rejects(write.execute({ path, content: '{}' }), /engine-owned/, path);
  }
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
  // The Drive sits inside a parent we can read, so a temp file built beside it
  // is observable too: the root target used to land there before the EISDIR.
  const parent = await mkdtemp(join(tmpdir(), 'captains-parent-'));
  const root = join(parent, 'drive');
  await mkdir(root);
  t.after(() => rm(parent, { recursive: true, force: true }));
  const { write } = tools(root);
  await write.execute({ path: 'notes/ok.txt', content: 'ok' });
  // A non-empty directory at the target is what makes the rename fail *after*
  // the temp file exists, which is the cleanup path under test.
  await mkdir(join(root, 'blocked'), { recursive: true });
  await writeFile(join(root, 'blocked', 'occupied.txt'), 'occupied');
  // A target that resolves to the Drive root or an existing directory is
  // refused before any I/O, with our own message rather than EISDIR from the
  // rename: '' and '.' both resolve to the root.
  for (const path of ['', '.', 'blocked']) {
    await assert.rejects(write.execute({ path, content: 'x' }), /is a directory/, JSON.stringify(path));
  }
  assert.deepEqual(await readdir(parent), ['drive'], 'a temp file landed outside the Drive');
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
  assert.deepEqual(gated(config, 'root').sort(), ['captain_drive_read', 'captain_drive_write', 'captain_ship_ready']);
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

test('an already-created root agent is latched once, not twice', async t => {
  const root = await drive(t);
  const sections = [];
  const tools = [];
  const scope = {
    systemPrompt: {
      section(section) { sections.push(section); return () => {}; },
      getSectionOrder: () => 10200,
    },
    tools: { register(definition) { tools.push(definition.name); return () => {}; } },
    effect: fn => fn(),
  };
  const agent = { session: { header: { origin: 'root' } }, ctx: scope };
  let created;
  apply({
    agents: { list: () => [agent] },
    on(event, handler) { if (event === 'agent/created') created = handler; },
  }, captain(root));
  assert.equal(sections.length, 1, 'a root created before the listener settled was not latched');
  assert.deepEqual([...tools].sort(), ['captain_drive_read', 'captain_drive_write', 'captain_ship_ready']);
  // The event arrives for the same agent afterwards: the latch must not double-register.
  created({ agent });
  assert.equal(sections.length, 1, 'the latch and the event double-registered the section');
  assert.equal(tools.length, 3, 'the latch and the event doubled the Drive tools');
});

test('the stamped drive root is the drive itself, not its parent', async t => {
  // The engine stamps `driveRoot` as the Captain's drive. A payload whose root
  // were the parent directory would put the tools one level up from the files
  // `ensure_drive` creates; this pins the layout the engine must stamp.
  const parent = await mkdtemp(join(tmpdir(), 'captains-parent-'));
  t.after(() => rm(parent, { recursive: true, force: true }));
  const root = join(parent, 'captains', 'captain-1');
  await mkdir(root, { recursive: true });
  assert.equal(driveDir(captain(root)), root, 'the row root is the drive');
  const { read, write } = tools(root);
  assert.equal(
    await write.execute({ path: 'notes/plan.md', content: 'plan' }),
    'wrote notes/plan.md (4 bytes)'
  );
  assert.equal(await read.execute({ path: 'notes/plan.md' }), 'plan');
  // Nothing was created beside the drive: a write resolved against the parent
  // would have landed in `captains/` or beside it.
  assert.deepEqual((await readdir(parent)).sort(), ['captains']);
  assert.deepEqual((await readdir(join(parent, 'captains'))).sort(), ['captain-1']);
  assert.deepEqual((await readdir(root)).sort(), ['notes']);
});

// ── SUR-207: captain_ship_ready writes machine-exact receipts ────────

import { execFileSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';

function gitRepo(t) {
  return (async () => {
    const dir = await mkdtemp(join(tmpdir(), 'ship-ready-repo-'));
    t.after(() => rm(dir, { recursive: true, force: true }));
    const git = args => execFileSync('git', args, { cwd: dir, encoding: 'utf8' }).trim();
    git(['init', '-q', '-b', 'main']);
    git(['config', 'user.email', 'crew@example.com']);
    git(['config', 'user.name', 'Crew']);
    git(['config', 'commit.gpgsign', 'false']);
    await writeFile(join(dir, 'file.txt'), 'one');
    git(['add', 'file.txt']);
    git(['commit', '-q', '-m', 'first']);
    return { dir, git };
  })();
}

function shipReady(root) {
  return driveToolSpecs(captain(root)).find(tool => tool.name === 'captain_ship_ready');
}

test('captain_ship_ready writes a receipt whose git fields come from the worktree', async t => {
  const root = await drive(t);
  const { dir, git } = await gitRepo(t);
  const head = git(['rev-parse', 'HEAD']);
  const result = await shipReady(root).execute({
    crewId: 'crew-a', repo: dir, pr: 100, branch: 'crew/crew-a', reviewedSha: head,
    baseSha: head, gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
  });
  assert.match(result, /ready receipt written for crew-a/);
  const receipt = JSON.parse(await readFile(join(root, 'state', 'ready', 'crew-a.json'), 'utf8'));
  assert.equal(receipt.headSha, head, 'the receipt names the worktree tip, not a parameter');
  assert.equal(receipt.treeSha, git(['rev-parse', 'HEAD^{tree}']));
  assert.equal(receipt.baseSha, head);
  assert.equal(receipt.gateCommand, 'cargo nextest run');
  assert.equal(receipt.diffTier, 'standard');
  assert.deepEqual(receipt.failedOrSkipped, []);
  assert.equal(receipt.slug, 'crew-a-change');
  assert.equal(receipt.crewId, 'crew-a');
  assert.equal(receipt.pr, 100);
});

test('captain_ship_ready refuses a base that is not an ancestor of the tip', async t => {
  const root = await drive(t);
  const { dir, git } = await gitRepo(t);
  const head = git(['rev-parse', 'HEAD']);
  await assert.rejects(
    shipReady(root).execute({
      crewId: 'crew-a', repo: dir, pr: 100, branch: 'crew/crew-a', reviewedSha: head,
      baseSha: '0'.repeat(40), gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
    }),
    /not an ancestor/,
  );
  assert.deepEqual((await readdir(root)), [], 'no receipt landed');
});

test('captain_ship_ready refuses when the worktree moved past the reviewed commit', async t => {
  const root = await drive(t);
  const { dir, git } = await gitRepo(t);
  const reviewed = git(['rev-parse', 'HEAD']);
  await writeFile(join(dir, 'file.txt'), 'two');
  git(['add', 'file.txt']);
  git(['commit', '-q', '-m', 'second']);
  await assert.rejects(
    shipReady(root).execute({
      crewId: 'crew-a', repo: dir, pr: 100, branch: 'crew/crew-a', reviewedSha: reviewed,
      baseSha: reviewed, gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
    }),
    /not the reviewed/,
  );
  assert.deepEqual((await readdir(root)), [], 'the unreviewed tip earned no receipt');
});

test('captain_ship_ready accepts a dotted release branch the engine accepts', async t => {
  const root = await drive(t);
  const { dir, git } = await gitRepo(t);
  const head = git(['rev-parse', 'HEAD']);
  const result = await shipReady(root).execute({
    crewId: 'crew-a', repo: dir, pr: 100, branch: 'feature/v1.2', reviewedSha: head,
    baseSha: head, gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
  });
  assert.match(result, /ready receipt written/);
  assert.deepEqual(JSON.parse(await readFile(join(root, 'state', 'ready', 'crew-a.json'), 'utf8')).branch, 'feature/v1.2');
});

test('captain_ship_ready refuses a stale reviewedSha even before reading git state', async t => {
  const root = await drive(t);
  const { dir } = await gitRepo(t);
  await assert.rejects(
    shipReady(root).execute({
      crewId: 'crew-a', repo: dir, pr: 100, branch: 'crew/crew-a', reviewedSha: '0'.repeat(40),
      baseSha: '0'.repeat(40), gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
    }),
    /not the reviewed/,
  );
  assert.deepEqual((await readdir(root)), [], 'no receipt landed');
});

test('captain_ship_ready refuses a hand-asserted shape before touching git', async t => {
  const root = await drive(t);
  const { dir } = await gitRepo(t);
  const tool = shipReady(root);
  const head = execFileSync('git', ['-C', dir, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const base = {
    crewId: 'crew-a', repo: dir, pr: 100, branch: 'crew/crew-a', reviewedSha: head,
    baseSha: 'a'.repeat(40), gateCommand: 'cargo nextest run', diffTier: 'standard', slug: 'crew-a-change',
  };
  await assert.rejects(tool.execute({ ...base, crewId: '../x' }), /bare file name/);
  await assert.rejects(tool.execute({ ...base, repo: 'relative' }), /absolute path/);
  await assert.rejects(tool.execute({ ...base, pr: 0 }), /pull request number/);
  await assert.rejects(tool.execute({ ...base, baseSha: 'abc' }), /full commit SHA/);
  await assert.rejects(tool.execute({ ...base, gateCommand: ' ' }), /gate/);
  await assert.rejects(tool.execute({ ...base, diffTier: 'risky' }), /diffTier/);
  await assert.rejects(tool.execute({ ...base, slug: 'UPPER' }), /slug/);
});
