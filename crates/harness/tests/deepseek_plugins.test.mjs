import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { cp, mkdtemp, readdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

// This test drives the installed `dsh` CLI instead of importing it, so a
// missing SDK directory or package manager skips rather than failing at import.
const sdkRoot = process.env.ZERON_DEEPSEEK_SDK_DIR;
const here = fileURLToPath(new URL('.', import.meta.url));
const fixture = resolve(here, 'fixtures/dsh-hello-bundle');
const dshBin = sdkRoot === undefined ? undefined : resolve(sdkRoot, 'node_modules/@deepseek-ai/dsh/lib/bin.js');

function onPath(command) {
  const suffixes = process.platform === 'win32' ? ['.cmd', '.exe', ''] : [''];
  return (process.env.PATH ?? '').split(delimiter).some(directory => directory !== '' && suffixes.some(suffix => {
    try {
      accessSync(join(directory, command + suffix), constants.X_OK);
      return true;
    } catch {
      return false;
    }
  }));
}

function run(args, options) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(process.execPath, args, { cwd: options.cwd, env: options.env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill('SIGKILL'), options.timeout);
    child.once('error', error => { clearTimeout(timer); rejectRun(error); });
    child.once('close', (code, signal) => { clearTimeout(timer); resolveRun({ code, signal, stdout, stderr }); });
  });
}

const report = result => `exit ${result.code}${result.signal === null ? '' : ` (${result.signal})`}\n${result.stdout}\n${result.stderr}`;

// A loopback Anthropic-shaped model: it records each request body so a test can
// assert what the runtime actually sent, not what a plugin registered.
async function mockModel(t) {
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks).toString());
    requests.push({ path: request.url, input });
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const event = (type, data) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
    event('message_start', { message: { id: 'msg-mock', type: 'message', role: 'assistant', model: input.model,
      content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } });
    event('content_block_start', { index: 0, content_block: { type: 'text', text: '' } });
    event('content_block_delta', { index: 0, delta: { type: 'text_delta', text: 'CAPTAINS_ROLE_OK' } });
    event('content_block_stop', { index: 0 });
    event('message_delta', { delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 3 } });
    event('message_stop', {});
    response.end();
  });
  await new Promise(listening => server.listen(0, '127.0.0.1', listening));
  t.after(() => new Promise(closed => { server.closeAllConnections(); server.close(closed); }));
  return { requests, baseURL: `http://127.0.0.1:${server.address().port}/anthropic` };
}

test('a bundle installed in the sdk profile composes its layer and loads', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-plugins-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-plugins-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  // pnpm links the checkout it is given, so install a copy and leave the
  // fixture in the repository untouched.
  const bundle = join(checkout, 'dsh-hello-bundle');
  await cp(fixture, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));

  // The first use initializes the sdk profile from its shipped template, then
  // pnpm links the checkout and dsh appends the bundle to the profile manifest.
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const profile = join(home, 'profiles', 'sdk');
  const manifest = JSON.parse(await readFile(join(profile, 'package.json'), 'utf8'));
  assert.ok(manifest.dependencies?.['dsh-hello-plugin'], `the profile records the dependency: ${JSON.stringify(manifest.dependencies)}`);
  const bundles = manifest.dsh?.profile?.bundles ?? [];
  assert.ok(bundles.includes('dsh-hello-plugin'), `dsh.profile.bundles lists the bundle: ${JSON.stringify(bundles)}`);
  assert.equal(bundles[0], '@deepseek-ai/dsh-base', 'the sdk template keeps its first bundle');
  assert.equal(bundles.at(-1), 'dsh-hello-plugin', 'an installed bundle is appended after the template bundles');

  // The composed tree names each layer after the bundle that contributed it.
  const dumped = await run([dshBin, '--profile', 'sdk', '--dump-config'], { cwd: checkout, env, timeout: 120000 });
  assert.equal(dumped.code, 0, `dsh --dump-config failed: ${report(dumped)}`);
  assert.match(dumped.stdout, /^# == dsh-hello-plugin$/m, 'the composed tree names the bundle layer');
  assert.match(dumped.stdout, /^- id: hello$/m);
  assert.match(dumped.stdout, /^ {2}name: dsh-hello-plugin$/m);

  // The layer's row names the package, so the profile must resolve it by name
  // and its entry point must apply.
  const loaded = await run(['--input-type=module', '-e',
    "const plugin = await import('dsh-hello-plugin'); console.log(plugin.name); plugin.apply();"], { cwd: profile, env, timeout: 60000 });
  assert.equal(loaded.code, 0, `the installed bundle does not load: ${report(loaded)}`);
  assert.match(loaded.stdout, /^hello-plugin$/m);
  assert.match(loaded.stdout, /^\[hello-plugin\] plugin loaded!$/m);
});

// SUR-98: the Captains bundle is zeron's own package under `plugins/`, so this
// case installs the real directory (not a fixture copy of its shape) and the
// composed profile has to carry its row with the inert default role.
test('the captains bundle installs and composes on rc.3', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  // pnpm links the checkout it is given, so install a copy and leave the
  // repository's plugins directory untouched.
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));

  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const profile = join(home, 'profiles', 'sdk');
  const manifest = JSON.parse(await readFile(join(profile, 'package.json'), 'utf8'));
  assert.ok(manifest.dependencies?.['zeron-dsh-captains'], `the profile records the dependency: ${JSON.stringify(manifest.dependencies)}`);
  const bundles = manifest.dsh?.profile?.bundles ?? [];
  assert.ok(bundles.includes('zeron-dsh-captains'), `dsh.profile.bundles lists the bundle: ${JSON.stringify(bundles)}`);
  assert.equal(bundles.at(-1), 'zeron-dsh-captains', 'an installed bundle is appended after the template bundles');

  // A Captain bundle loads in every DeepSeek chat, so its default row has to be
  // inert: role none is what makes a plain chat behave like a plain chat.
  const dumped = await run([dshBin, '--profile', 'sdk', '--dump-config'], { cwd: checkout, env, timeout: 120000 });
  assert.equal(dumped.code, 0, `dsh --dump-config failed: ${report(dumped)}`);
  assert.match(dumped.stdout, /^# == zeron-dsh-captains$/m, 'the composed tree names the captains bundle layer');
  assert.match(dumped.stdout, /^- id: zeron-captains$/m);
  assert.match(dumped.stdout, /^ {2}name: zeron-dsh-captains$/m);
  assert.match(dumped.stdout, /^ {4}role: none$/m, 'the bundled row defaults to the inert role');

  // The layer names a package, so the profile has to resolve it by name and its
  // entry point has to apply without a real host context at the inert role.
  const loaded = await run(['--input-type=module', '-e',
    "const plugin = await import('zeron-dsh-captains'); console.log(plugin.name); plugin.apply({}, { role: 'none' }); console.log('no-op');"], { cwd: profile, env, timeout: 60000 });
  assert.equal(loaded.code, 0, `the installed bundle does not load: ${report(loaded)}`);
  assert.match(loaded.stdout, /^zeron-captains$/m);
  assert.match(loaded.stdout, /^no-op$/m);
});

// The row config is the only thing that turns Captain behaviour on, so the
// validation is the security boundary: an unknown role or a relative Drive root
// must throw before any registration runs.
test('zeron-captains validates its row config', async () => {
  // The validation lives in a plain module the entry point re-exports, so this
  // matrix runs without dsh installed; `apply` itself is exercised below.
  const module = await import(new URL('../../../plugins/dsh-captains/src/config.js', import.meta.url).href);
  const driveFixture = join(tmpdir(), 'd');
  assert.equal(module.normalizeConfig({}).role, 'none');
  assert.equal(module.normalizeConfig(undefined).role, 'none');
  assert.throws(() => module.normalizeConfig({ role: 'boss' }), /unknown role/);
  assert.throws(() => module.normalizeConfig({ role: 'captain', chatId: 'c1', captainId: 'c1', captainName: 'B', driveRoot: 'rel' }), /driveRoot/);
  assert.throws(() => module.normalizeConfig({ role: 'captain', chatId: 'c1', captainId: 'c2', captainName: 'B', driveRoot: driveFixture }), /captainId/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: '', driveRoot: driveFixture }), /captainName/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c 2', captainId: 'c1', captainName: 'B', driveRoot: driveFixture }), /chatId/);
  const crew = module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'B', driveRoot: driveFixture });
  assert.equal(crew.captainId, 'c1');
  assert.equal(crew.role, 'crew');
  // The boundary pairs are part of the contract, not a detail of the regex:
  // 128 characters is a valid id, 129 is not; 80 is a valid name, 81 is not.
  const id128 = 'a'.repeat(128);
  assert.equal(module.normalizeConfig({ role: 'crew', chatId: id128, captainId: 'c1', captainName: 'B', driveRoot: driveFixture }).chatId, id128);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: `${id128}a`, captainId: 'c1', captainName: 'B', driveRoot: driveFixture }), /chatId/);
  assert.equal(module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'b'.repeat(80), driveRoot: driveFixture }).captainName.length, 80);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'b'.repeat(81), driveRoot: driveFixture }), /captainName/);
  // A name is one line: a newline would break the section's attribute shape.
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'a\nb', driveRoot: driveFixture }), /captainName/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'a\rb', driveRoot: driveFixture }), /captainName/);
});

// SUR-99: the Drive tools are host code, so the installed bundle has to keep
// every call inside the row's driveRoot. This case exercises the tools the way
// the runtime would: apply() with a fake registration context, then the tool
// definitions themselves, against a real temp Drive.
test('the captains bundle confines Drive tools', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-drive-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-drive-bundle-'));
  const drive = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-drive-root-'));
  const outside = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-drive-outside-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  t.after(() => rm(drive, { recursive: true, force: true }));
  t.after(() => rm(outside, { recursive: true, force: true }));
  await writeFile(join(outside, 'secret.txt'), 'secret');
  await symlink(outside, join(drive, 'link'));

  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  // A real install supplies the dsh-tools peer next to the bundle. This copy
  // lives in a temp dir with no dsh on PATH, so link the peer in the same way
  // the installed layout would; without it the tools cannot be defined at all.
  await symlink(join(sdkRoot, 'node_modules'), join(bundle, 'node_modules'), 'dir');
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);
  const profile = join(home, 'profiles', 'sdk');

  const script = `
import assert from 'node:assert/strict';
import { mkdir, symlink } from 'node:fs/promises';
import { join } from 'node:path';
const drive = process.argv[1];
const plugin = await import('zeron-dsh-captains');
assert.equal(plugin.name, 'zeron-captains');
// The entry re-exports the validation it applies, so callers share one rule.
assert.equal(typeof plugin.normalizeConfig, 'function', 'the entry dropped its normalizeConfig re-export');
assert.throws(() => plugin.normalizeConfig({ role: 'boss' }), /unknown role/);
const captain = { role: 'captain', chatId: 'captain-1', captainId: 'captain-1', captainName: 'B', driveRoot: drive };
const crew = { role: 'crew', chatId: 'crew-1', captainId: 'captain-1', captainName: 'B', driveRoot: drive };

// The plugin registers on the root agent, not at load, so each assertion goes
// through the same gate the runtime uses.
function applyAs(config, origin) {
  const registered = [];
  const scope = {
    systemPrompt: { section: () => () => {}, getSectionOrder: () => 10200 },
    tools: { register: (definition) => { registered.push(definition); return () => {}; } },
    effect: fn => fn(),
  };
  let created;
  plugin.apply({ ...scope, on: (event, handler) => { if (event === 'agent/created') created = handler; } }, config);
  if (created) created({ agent: { session: { header: { origin } }, ctx: scope } });
  return registered;
}

// Crew and ordinary sessions register zero Drive tools; a Captain gets exactly two.
assert.deepEqual(applyAs(crew, 'root'), [], 'a crew chat registers no Drive tools');
assert.deepEqual(applyAs({ role: 'none' }, 'root'), [], 'an ordinary chat registered a tool');
assert.deepEqual(applyAs(captain, 'subagent'), [], 'a native child inherited the Drive tools');
// The installed entry applies the same validation as the plain module.
assert.throws(() => plugin.apply({ on: () => {} },
  { role: 'captain', chatId: 'captain-1', captainId: 'captain-1', captainName: 'B', driveRoot: 'relative' }), /driveRoot/);

const registered = applyAs(captain, 'root');
assert.deepEqual(registered.map(definition => definition.name).sort(), ['captain_drive_read', 'captain_drive_write']);
const read = registered.find(definition => definition.name === 'captain_drive_read');
const write = registered.find(definition => definition.name === 'captain_drive_write');
const outside = /path is outside the Captain Drive/;

// Round trip under a nested path the tool creates.
const wrote = await write.execute({ path: 'notes/deep/a.txt', content: 'hello' });
assert.ok(wrote.startsWith('wrote notes/deep/a.txt (') && wrote.endsWith(' bytes)'), wrote);
assert.equal(await read.execute({ path: 'notes/deep/a.txt' }), 'hello');
// Links are listed with a trailing @, so an alias is visible in the Drive.
assert.equal(await read.execute({ path: '' }), 'link@\\nnotes/');

// Traversal, absolute paths and a symlink that leaves the Drive are rejected.
await assert.rejects(read.execute({ path: 'link/secret.txt' }), outside);
await assert.rejects(write.execute({ path: 'link/evil.txt', content: 'x' }), outside);
await assert.rejects(read.execute({ path: '../secret.txt' }), outside);
await assert.rejects(write.execute({ path: '/etc/passwd', content: 'x' }), outside);

// The engine owns state/ and reports/ for writing; the Captain reads them.
await assert.rejects(write.execute({ path: 'state/needs-you.json', content: '{}' }), /engine-owned/);
await assert.rejects(write.execute({ path: 'reports/turn-1.md', content: '# report' }), /engine-owned/);
// The guard is checked on the resolved path: a display prefix must not smuggle
// a write into either folder.
for (const target of ['.//state/needs-you.json', './/reports/turn-1.md', '././/state/needs-you.json']) {
  await assert.rejects(write.execute({ path: target, content: 'x' }), /engine-owned/, target + ' slipped past the engine-owned guard');
}
// A symlink alias inside the Drive still names the engine's folder — for both
// engine-owned folders, so a guard that special-cased one could not pass.
await mkdir(join(drive, 'state'), { recursive: true });
await mkdir(join(drive, 'reports'), { recursive: true });
await symlink(join(drive, 'state'), join(drive, 'alias'));
await symlink(join(drive, 'reports'), join(drive, 'reports-alias'));
for (const target of ['alias/needs-you.json', 'reports-alias/turn-2.md']) {
  await assert.rejects(write.execute({ path: target, content: 'x' }), /engine-owned/,
    target + ' reached an engine-owned folder through a symlink alias');
}
// Positive controls: a prefix is not the folder.
for (const target of ['notes/x.md', 'stateful/x.md']) {
  await write.execute({ path: target, content: 'ok' });
  assert.equal(await read.execute({ path: target }), 'ok', target);
}
// Writing through a symlinked folder lands at the real path and says so.
await symlink(join(drive, 'notes'), join(drive, 'notes-alias'));
assert.equal(await write.execute({ path: 'notes-alias/y.txt', content: 'abc' }), 'wrote notes/y.txt (3 bytes)');

// A read-only run cannot write the Drive.
process.env.DSH_PERMISSION_MODE = 'read-only';
await assert.rejects(write.execute({ path: 'notes/blocked.txt', content: 'x' }), /read-only/);
delete process.env.DSH_PERMISSION_MODE;

console.log('confined');
`;
  const call = await run(['--input-type=module', '-e', script, drive],
    { cwd: profile, env: { ...env, NODE_PATH: join(sdkRoot, 'node_modules') }, timeout: 60000 });
  assert.equal(call.code, 0, `the Drive tools are not confined: ${report(call)}`);
  assert.match(call.stdout, /^confined$/m);
});

// SUR-84: the role prompt is a system section, so the only assertion that
// proves it works is the text inside the model request. A registration-only
// check would pass even if the section never reached the model.
test('Helm bundle adds the role prompt to model input', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-role-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-role-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const { requests, baseURL } = await mockModel(t);
  const require_ = createRequire(join(sdkRoot, 'package.json'));
  const { DeepSeekHarness } = await import(pathToFileURL(require_.resolve('@deepseek-ai/dsh-sdk-client')).href);

  // The harness will override this row from the host-stamped chat role
  // (SUR-104); until then the test supplies the same invocation patch so the
  // plugin's half of the contract is pinned end to end.
  const roles = {
    helm: { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: join(tmpdir(), 'drive', 'helm') },
    crew: { role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: join(tmpdir(), 'drive', 'helm') },
    none: { role: 'none' },
  };
  const sessions = { helm: 'dsh-session-a', crew: 'dsh-session-b', none: 'dsh-session-c' };
  const systems = {};
  for (const [role, config] of Object.entries(roles)) {
    const patch = join(checkout, `captains-${role}.yml`);
    await writeFile(patch, `- ${JSON.stringify({ id: 'zeron-captains', config })}\n`);
    const start = requests.length;
    const harness = new DeepSeekHarness({
      profile: 'sdk', dshHome: home, cwd: checkout, processCwd: checkout, model: 'deepseek-flash',
      env: { ...env, DEEPSEEK_API_KEY: 'synthetic-not-live', DEEPSEEK_BASE_URL: baseURL },
      patches: [patch],
    });
    harness.client.start();
    try {
      await harness.start();
      const result = await harness.session(sessions[role]).run('Name the role you are running as.');
      assert.equal(result.finishReason, 'completed', `the ${role} run did not finish: ${result.finishReason ?? result}`);
    } finally {
      await harness.client.close();
    }
    const messages = requests.slice(start).filter(request => request.path.endsWith('/messages'));
    assert.equal(messages.length, 1, `the ${role} run sent ${messages.length} model requests`);
    const system = messages[0].input.system;
    systems[role] = typeof system === 'string' ? system : JSON.stringify(system);
  }

  assert.match(systems.helm, /<captain name="Helm">/, 'the captain row did not add the role section');
  assert.match(systems.helm, /## Needs you/, 'the role section is not the captain prompt');
  assert.match(systems.helm, /\/tmp\/drive\/helm/, 'the role section does not name the Drive');
  // Crew rules arrive from CREW.md on the engine side (SUR-85); this bundle
  // registers the Captain's own section and nothing else.
  for (const role of ['crew', 'none']) {
    assert.doesNotMatch(systems[role], /<captain/, `the ${role} row registered a captain section`);
    assert.doesNotMatch(systems[role], /## Needs you/, `the ${role} row carried the captain prompt`);
  }
});

test('the bundle loads and stays inert without the dsh tools peer', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-nopeer-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const bundle = join(dir, 'bundle');
  await cp(resolve(here, '../../../plugins/dsh-captains'), bundle, { recursive: true });
  await rm(join(bundle, 'node_modules'), { recursive: true, force: true });
  // With no PATH, no cwd profile and no local node_modules, nothing can satisfy
  // the peer: the module still has to load, an ordinary chat must stay inert,
  // and a captain must fail loudly rather than lose its tools silently.
  const script = `
const plugin = await import(process.argv[1]);
console.log('name:' + plugin.name);
plugin.apply({}, { role: 'none' });
console.log('inert');
const ctx = {
  systemPrompt: { section: () => () => {}, getSectionOrder: () => 10200 },
  tools: { register: () => () => {} },
  effect: fn => fn(),
};
let created;
const host = { ...ctx, on: (event, handler) => { if (event === 'agent/created') created = handler; } };
try {
  plugin.apply(host, { role: 'captain', chatId: 'c1', captainId: 'c1', captainName: 'B', driveRoot: process.argv[2] });
  created({ agent: { session: { header: { origin: 'root' } }, ctx } });
  console.log('captain:ran');
} catch (error) {
  console.log('captain:' + String(error.message).slice(0, 120));
}
`;
  const isolated = Object.fromEntries(Object.entries({ HOME: dir, TMPDIR: tmpdir(), DSH_HOME: join(dir, 'home') }));
  const probed = await new Promise(runProbe => {
    const child = spawn(process.execPath, ['--input-type=module', '-e', script, join(bundle, 'index.js'), join(dir, 'drive')], { cwd: dir, env: isolated });
    let out = '';
    child.stdout.on('data', chunk => { out += chunk; });
    child.stderr.on('data', chunk => { out += chunk; });
    child.once('close', code => runProbe({ code, out }));
  });
  assert.equal(probed.code, 0, `the isolated bundle did not load: ${probed.out}`);
  assert.match(probed.out, /^name:zeron-captains$/m);
  assert.match(probed.out, /^inert$/m);
  assert.match(probed.out, /^captain:.*dsh-tools/m, `a captain without the peer did not fail loudly: ${probed.out}`);
});

// SUR-104: the role rides the host-stamped environment, and zeron's own shim
// has to turn it into the invocation-patch row that overrides the bundle's
// config. This case drives the real sidecar and inspects the patch the SDK
// loads, which is the only place the hand-off can be observed before the
// plugin grows a prompt of its own.
async function sidecar(context, sdkRoot, checkout, home, captains, sessionId, options = {}) {
  const shim = resolve(context.here, '../src/deepseek/shim.mjs');
  const [policySource, environmentSource] = await Promise.all([
    readFile(resolve(context.here, '../src/deepseek/policy.mjs'), 'utf8'),
    readFile(resolve(context.here, '../src/deepseek/environment.mjs'), 'utf8'),
  ]);
  const child = spawn(process.execPath, [shim, sdkRoot], { cwd: checkout, stdio: ['pipe', 'pipe', 'pipe'],
    env: { ...context.env, DEEPSEEK_API_KEY: 'synthetic-not-live', ...(options.env ?? {}) } });
  let errors = '';
  child.stderr.on('data', chunk => { errors += chunk; });
  const lines = [];
  const result = await new Promise((resolveRun, rejectRun) => {
    let buffer = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); rejectRun(new Error(`the sidecar did not settle: ${errors}`)); }, 60000);
    child.stdout.on('data', chunk => {
      buffer += chunk;
      for (;;) {
        const end = buffer.indexOf('\n');
        if (end < 0) break;
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        lines.push(line);
        let frame;
        try { frame = JSON.parse(line); } catch { continue; }
        if (frame.type === 'error') { clearTimeout(timer); resolveRun(frame); return; }
        if (frame.type === 'request') {
          const result = frame.method === 'approval/request' ? { outcome: 'allowed-once' } : { answers: [] };
          child.stdin.write(`${JSON.stringify({ type: 'answer', id: frame.id, result })}\n`);
        }
        if (frame.type === 'ready' && options.prompt === undefined) { clearTimeout(timer); resolveRun(frame); return; }
        if (frame.type === 'ready') { child.stdin.write(`${JSON.stringify({ type: 'prompt', prompt: options.prompt })}\n`); }
        if (frame.type === 'done') {
          // A subagent runs beside the turn that started it: give the child its
          // own model step before the sidecar is stopped.
          clearTimeout(timer);
          setTimeout(() => resolveRun(frame), 4000);
          return;
        }
      }
    });
    child.once('exit', code => { clearTimeout(timer); rejectRun(new Error(`the sidecar exited ${code}: ${errors}`)); });
    child.stdin.write(`${JSON.stringify({
      type: 'start', sessionId, resume: false, dshHome: home, cwd: checkout,
      model: 'deepseek-flash', provider: 'deepseek-official', sharedHome: false,
      access: 'supervised', plan: false, sandbox: 'workspace-write', policySource, environmentSource,
      mcp: [], webSearch: false, ...(captains === undefined ? {} : { captains }),
    })}\n`);
  });
  try {
    const [runtime] = (await readdir(home)).filter(name => name.startsWith('.zeron-runtime-'));
    const patch = runtime === undefined ? '' : await readFile(join(home, runtime, 'cordis.patch.yml'), 'utf8');
    return { frame: result, patch, lines };
  } finally {
    if (child.exitCode === null) {
      child.stdin.write('{"type":"stop"}\n');
      await new Promise(done => child.once('exit', done));
    }
  }
}

test('the sidecar stamps the host role into the captains row', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-stamp-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-stamp-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: join(tmpdir(), 'drive', 'helm') };
  const context = { here, env };
  const stamped = await sidecar(context, sdkRoot, checkout, home, role, 'dsh-session-a');
  assert.equal(stamped.frame.type, 'ready', `the stamped run did not start: ${stamped.lines.join('\n')}`);
  const row = stamped.patch.split('\n').map(line => line.trim()).filter(line => line.startsWith('- {'))
    .map(line => JSON.parse(line.slice(2))).find(entry => entry.id === 'zeron-captains');
  assert.ok(row, `the sidecar did not stamp the row: ${stamped.patch}`);
  assert.deepEqual(row.config, role, 'the stamped row did not carry the host role verbatim');

  // A control run without the stamp keeps the bundle's own inert default: no
  // override row at all.
  const control = await sidecar(context, sdkRoot, checkout, home, undefined, 'dsh-session-control');
  assert.equal(control.frame.type, 'ready', `the control run did not start: ${control.lines.join('\n')}`);
  assert.doesNotMatch(control.patch, /zeron-captains/, 'an ordinary chat stamped a captains row');

  // The override row wins over the bundle layer when the profile composes: that
  // is the answer to "does cordis accept an override for an inserted id".
  const patchFile = join(checkout, 'stamped-row.yml');
  await writeFile(patchFile, stamped.patch);
  const dumped = await run([dshBin, '--profile', 'sdk', '--patch', patchFile, '--dump-config'], { cwd: checkout, env, timeout: 120000 });
  assert.equal(dumped.code, 0, `the stamped row does not compose: ${report(dumped)}`);
  const block = dumped.stdout.split(/\n(?=- id: )/).find(part => part.startsWith('- id: zeron-captains'));
  assert.ok(block, `the composed tree lost the captains row: ${dumped.stdout.slice(-2000)}`);
  assert.match(block, /^ {2}config:$/m);
  assert.match(block, /^ {4}role: captain$/m, `the override did not win: ${block}`);
  assert.match(block, /^ {4}chatId: zeron-helm-a$/m, `the chat id did not reach the row: ${block}`);
});

// SUR-104 blocker 1: the row is process-wide, so a plugin that registers at
// load time also reaches native child sessions. The captain row belongs to the
// root session only; a subagent must see neither the prompt nor its markers.
test('the captain row stays out of a native child session', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-child-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-child-bundle-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  // Scripted loopback: the root asks for a subagent, the child answers text,
  // the root then answers text. The child is the request whose system prompt
  // names the delegation.
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks).toString());
    requests.push(input);
    // A native child's first user turn is the delegated task; its system
    // prompt does not name the delegation, so detect it the same way the
    // assertions below do.
    const child = JSON.stringify((input.messages ?? [])[0] ?? '').includes('child task');
    const followUp = JSON.stringify(input.messages ?? '').includes('tool_result');
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const event = (type, data) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
    event('message_start', { message: { id: 'msg-mock', type: 'message', role: 'assistant', model: input.model,
      content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } });
    if (!child && !followUp) {
      event('content_block_start', { index: 0, content_block: { type: 'tool_use', id: 'toolu_child', name: 'subagent', input: {} } });
      event('content_block_delta', { index: 0, delta: { type: 'input_json_delta', partial_json: JSON.stringify({ description: 'child notes review', prompt: 'child task' }) } });
      event('content_block_stop', { index: 0 });
      event('message_delta', { delta: { stop_reason: 'tool_use', stop_sequence: null }, usage: { output_tokens: 9 } });
    } else {
      event('content_block_start', { index: 0, content_block: { type: 'text', text: '' } });
      event('content_block_delta', { index: 0, delta: { type: 'text_delta', text: child ? 'CHILD_OK' : 'PARENT_OK' } });
      event('content_block_stop', { index: 0 });
      event('message_delta', { delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 3 } });
    }
    event('message_stop', {});
    response.end();
  });
  await new Promise(listening => server.listen(0, '127.0.0.1', listening));
  t.after(() => new Promise(closed => { server.closeAllConnections(); server.close(closed); }));

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: join(tmpdir(), 'drive', 'helm') };
  const result = await sidecar({ here, env }, sdkRoot, checkout, home, role, 'dsh-session-a', {
    prompt: 'delegate this to a subagent',
    env: { DEEPSEEK_BASE_URL: `http://127.0.0.1:${server.address().port}/anthropic` },
  });
  assert.equal(result.frame.type, 'done', `the run did not finish: ${JSON.stringify(result.frame).slice(0, 300)}\n${result.lines.slice(-4).join('\n')}`);

  const system = input => typeof input.system === 'string' ? input.system : JSON.stringify(input.system ?? '');
  const advertised = input => (input.tools ?? []).map(tool => tool.name);
  const root = requests.find(input => system(input).includes('<captain name="Helm">'));
  assert.ok(root, 'the root session never carried the captain prompt');
  // A child session's first user turn is the delegated task, not the person's ask.
  const child = requests.find(input => JSON.stringify((input.messages ?? [])[0] ?? '').includes('child task'));
  assert.ok(child, 'the subagent never ran');
  assert.doesNotMatch(system(child), /<captain|## Needs you/, 'a native child inherited the captain prompt');

  // The Drive tools ride the same scope: the root advertises both, the child
  // neither. Removing the `agent/created` gate fails on this pair, not on a
  // timeout.
  for (const tool of ['captain_drive_read', 'captain_drive_write']) {
    assert.ok(advertised(root).includes(tool), `the root session did not advertise ${tool}: ${advertised(root).join(', ')}`);
    assert.ok(!advertised(child).includes(tool), `a native child inherited the ${tool} tool`);
  }
});

// SUR-104 blocker 2: an override row for an id no bundle inserted is dropped by
// cordis with only a warning, so a stamped role would run captainless. The
// sidecar has to refuse before it starts.
test('the sidecar refuses a captains row without the bundle', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-nobundle-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-nobundle-cwd-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: join(tmpdir(), 'drive', 'helm') };
  const refused = await sidecar({ here, env }, sdkRoot, checkout, home, role, 'dsh-session-a');
  assert.equal(refused.frame.type, 'error', `a captains row without the bundle started: ${refused.lines.join('\n')}`);
  assert.match(refused.frame.message, /bundle|zeron-captains/i, `the refusal did not name the missing bundle: ${refused.frame.message}`);

  // An ordinary chat in the same home still starts: only a stamped role needs
  // the bundle, and the inert path must never pay for it.
  const ordinary = await sidecar({ here, env }, sdkRoot, checkout, home, undefined, 'dsh-session-b');
  assert.equal(ordinary.frame.type, 'ready', `an ordinary chat was refused: ${ordinary.lines.join('\n')}`);
});

// SUR-85 (superseding acceptance): a crew chat serves the Captain's CREW.md as
// a prompt section re-read at every assembly, and a changed file — including
// one that reverses a rule — restarts the request series so the retired copy
// cannot linger. The only proof is the HTTP body itself: every turn must send
// exactly one <crew-rules> block carrying the current file and nothing older.
test('a reversed CREW.md still sends one current crew-rules block and no stale copy', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-crew-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-crew-bundle-'));
  const drive = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-crew-drive-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  t.after(() => rm(drive, { recursive: true, force: true }));
  // pnpm links the checkout it is given, so install a copy and leave the
  // repository's plugins directory untouched.
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const { requests, baseURL } = await mockModel(t);
  const require_ = createRequire(join(sdkRoot, 'package.json'));
  let sdkClient;
  try {
    sdkClient = require_.resolve('@deepseek-ai/dsh-sdk-client');
  } catch {
    return t.skip('the SDK client cannot be resolved');
  }
  const { DeepSeekHarness } = await import(pathToFileURL(sdkClient).href);

  // The bundle declares zeron-captains inert; this row replaces its config
  // with the crew role pointed at the temp Drive.
  const row = { role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: drive };
  const patch = join(checkout, 'crew-row.yml');
  await writeFile(patch, `- ${JSON.stringify({ insert: [{ id: 'zeron-captains', name: 'zeron-dsh-captains', config: row }] })}\n`);

  const harness = new DeepSeekHarness({
    profile: 'sdk', dshHome: home, dshBin, cwd: checkout, processCwd: checkout, model: 'deepseek-flash',
    env: { ...env, DEEPSEEK_API_KEY: 'synthetic-not-live', DEEPSEEK_BASE_URL: baseURL },
    patches: [patch], initializeTimeoutMs: 60000, requestTimeoutMs: 120000,
  });

  const alpha = 'RULE-ALPHA: file every report under reports/.';
  const beta = 'RULE-BETA: never file a report; hand the work back to the Captain.';
  const crew = join(drive, 'CREW.md');
  const turns = [];
  try {
    await harness.start();
    const session = harness.session('zeron-crew-rules');
    const runTurn = async turn => {
      const start = requests.length;
      const result = await session.run(`Reply with a brief acknowledgement for turn ${turn}.`);
      assert.equal(result.sessionId, session.id);
      assert.equal(result.finishReason, 'completed', `turn ${turn} did not finish: ${result.finishReason ?? result}`);
      const calls = requests.slice(start).filter(request => request.path.endsWith('/messages')).map(request => request.input);
      assert.ok(calls.length > 0, `turn ${turn} produced no model request`);
      return calls;
    };
    await writeFile(crew, alpha);
    turns.push(await runTurn(1));
    await writeFile(crew, beta);
    turns.push(await runTurn(2));
    turns.push(await runTurn(3));
  } finally {
    await harness.close();
  }

  const count = (value, needle) => value.split(needle).length - 1;
  const system = input => typeof input.system === 'string' ? input.system
    : (input.system ?? []).map(block => block?.text ?? '').join('');
  const crewBlocks = text => [...text.matchAll(/<crew-rules\b[^>]*>[\s\S]*?<\/crew-rules>/g)].map(([block]) => block);
  // The runtime piggybacks its session log on the request and journals each
  // series' rendered system message inside it; that echo is the same current
  // block, not a copy the model consumes, so the one-block rule is counted on
  // the model-facing request. The retired-marker sweep stays body-wide: an
  // older block must not survive anywhere, journal included.
  const surface = input => {
    const { dsh_session_log, ...rest } = input;
    return JSON.stringify(rest);
  };
  for (const [index, calls] of turns.entries()) {
    const turn = index + 1;
    for (const input of calls) {
      assert.equal(count(surface(input), '<crew-rules'), 1,
        `turn ${turn} carried ${count(surface(input), '<crew-rules')} crew-rules opening tags in the request`);
      const blocks = crewBlocks(system(input));
      assert.equal(blocks.length, 1, `turn ${turn} has ${blocks.length} crew-rules blocks in system`);
      assert.ok(blocks[0].startsWith('<crew-rules captain="Helm">'), `turn ${turn} crew-rules block is malformed: ${blocks[0].slice(0, 80)}`);
      assert.ok(blocks[0].includes(turn === 1 ? alpha : beta), `turn ${turn} did not serve the current rule`);
      if (turn > 1) {
        const body = JSON.stringify(input);
        assert.ok(!body.includes('RULE-ALPHA'), `turn ${turn} still carries the retired rule`);
      }
    }
  }

  // The runtime may embed its session log in the request; when it does, the
  // reversal turn must show a series restart. The body assertions above are
  // the contract, so the field's absence is not a failure.
  if (turns.flat().some(input => input.dsh_session_log !== undefined)) {
    const restarted = turns[1].some(input => (input.dsh_session_log?.events ?? [])
      .some(event => event?.type === 'request/header' && (event.data?.startsSeries === true || event.data?.reason === 'series')));
    assert.ok(restarted, 'the reversal turn carried no series restart in its session log');
  }
});

// SUR-85 r2: the engine stops a parked crew sidecar after idle and resumes its
// session on the next message, so the first turn of a NEW process is the normal
// path, not an edge. That process has no in-memory baseline; the resumed series
// still holds the CREW.md it was built from, so the turn must rebase to the file
// as it is now. The proof stays the HTTP body: exactly one current block, and no
// retired rule anywhere.
test('a restarted runtime sends one current crew-rules block for the resumed session', { timeout: 300000 }, async t => {
  if (sdkRoot === undefined) return t.skip('ZERON_DEEPSEEK_SDK_DIR is not set');
  if (!onPath('pnpm')) return t.skip('pnpm is not on PATH');
  const home = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-restart-home-'));
  const checkout = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-restart-bundle-'));
  const drive = await mkdtemp(join(tmpdir(), 'zeron-dsh-captains-restart-drive-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  t.after(() => rm(drive, { recursive: true, force: true }));
  // pnpm links the checkout it is given, so install a copy and leave the
  // repository's plugins directory untouched.
  const source = resolve(here, '../../../plugins/dsh-captains');
  const bundle = join(checkout, 'dsh-captains');
  await cp(source, bundle, { recursive: true });
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);

  const { requests, baseURL } = await mockModel(t);
  const require_ = createRequire(join(sdkRoot, 'package.json'));
  let sdkClient;
  try {
    sdkClient = require_.resolve('@deepseek-ai/dsh-sdk-client');
  } catch {
    return t.skip('the SDK client cannot be resolved');
  }
  const { DeepSeekHarness } = await import(pathToFileURL(sdkClient).href);

  // The bundle declares zeron-captains inert; this row replaces its config
  // with the crew role pointed at the temp Drive.
  const row = { role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: drive };
  const patch = join(checkout, 'crew-row.yml');
  await writeFile(patch, `- ${JSON.stringify({ insert: [{ id: 'zeron-captains', name: 'zeron-dsh-captains', config: row }] })}\n`);

  const alpha = 'RULE-ALPHA: file every report under reports/.';
  const beta = 'RULE-BETA: never file a report; hand the work back to the Captain.';
  const crew = join(drive, 'CREW.md');
  // Two runtimes, same profile: the second one is a new process resuming the
  // session the first one persisted.
  const options = () => ({
    profile: 'sdk', dshHome: home, dshBin, cwd: checkout, processCwd: checkout, model: 'deepseek-flash',
    env: { ...env, DEEPSEEK_API_KEY: 'synthetic-not-live', DEEPSEEK_BASE_URL: baseURL },
    patches: [patch], initializeTimeoutMs: 60000, requestTimeoutMs: 120000,
  });
  const callsSince = start => requests.slice(start)
    .filter(request => request.path.endsWith('/messages')).map(request => request.input);

  // Turn 1 in the first runtime: the series is built against alpha.
  let sessionId;
  let firstTurn;
  const before = new DeepSeekHarness(options());
  try {
    await before.start();
    const session = before.session('zeron-crew-rules');
    await writeFile(crew, alpha);
    const start = requests.length;
    const result = await session.run('Reply with a brief acknowledgement before the restart.');
    assert.equal(result.finishReason, 'completed', `turn 1 did not finish: ${result.finishReason ?? result}`);
    assert.equal(result.sessionId, session.id);
    sessionId = session.id;
    firstTurn = callsSince(start);
  } finally {
    await before.close();
  }
  assert.ok(firstTurn.length > 0, 'turn 1 produced no model request');

  // The runtime is gone; CREW.md changes while it is down; a new process
  // resumes the persisted session for the next message.
  await writeFile(crew, beta);
  const after = new DeepSeekHarness(options());
  let resumedId;
  let restartedTurn;
  try {
    await after.start();
    const resumed = await after.resumeSession(sessionId);
    const start = requests.length;
    const result = await resumed.run('Reply with a brief acknowledgement after the restart.');
    assert.equal(result.finishReason, 'completed', `the resumed turn did not finish: ${result.finishReason ?? result}`);
    resumedId = result.sessionId;
    restartedTurn = callsSince(start);
  } finally {
    await after.close();
  }
  assert.equal(resumedId, sessionId, 'the restarted runtime did not resume the same session');
  assert.ok(restartedTurn.length > 0, 'the resumed turn produced no model request');

  const count = (value, needle) => value.split(needle).length - 1;
  const system = input => typeof input.system === 'string' ? input.system
    : (input.system ?? []).map(block => block?.text ?? '').join('');
  const crewBlocks = text => [...text.matchAll(/<crew-rules\b[^>]*>[\s\S]*?<\/crew-rules>/g)].map(([block]) => block);
  // The runtime echoes its rendered system message inside `dsh_session_log`;
  // that echo is the current block, not a copy the model consumes, so the
  // one-block rule is counted on the model-facing request while the retired
  // marker is swept from the whole body.
  const surface = input => {
    const { dsh_session_log, ...rest } = input;
    return JSON.stringify(rest);
  };

  // Before the restart the series really carried alpha, so the resumed turn is
  // a stale-copy test rather than an empty one.
  assert.ok(firstTurn.some(input => system(input).includes(alpha)), 'turn 1 never served the pre-restart rule');

  for (const input of restartedTurn) {
    assert.equal(count(surface(input), '<crew-rules'), 1,
      `the resumed turn carried ${count(surface(input), '<crew-rules')} crew-rules opening tags in the request`);
    const blocks = crewBlocks(system(input));
    assert.equal(blocks.length, 1, `the resumed turn has ${blocks.length} crew-rules blocks in system`);
    assert.ok(blocks[0].startsWith('<crew-rules captain="Helm">'), `the resumed block is malformed: ${blocks[0].slice(0, 80)}`);
    assert.ok(blocks[0].includes(beta), 'the resumed turn did not serve the current rule');
    const body = JSON.stringify(input);
    assert.ok(!body.includes('RULE-ALPHA'), 'the resumed turn still carries the retired rule');
  }
});
