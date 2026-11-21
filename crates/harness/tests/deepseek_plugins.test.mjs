import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { cp, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
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
  assert.equal(module.normalizeConfig({}).role, 'none');
  assert.equal(module.normalizeConfig(undefined).role, 'none');
  assert.throws(() => module.normalizeConfig({ role: 'boss' }), /unknown role/);
  assert.throws(() => module.normalizeConfig({ role: 'captain', chatId: 'c1', captainId: 'c1', captainName: 'B', driveRoot: 'rel' }), /driveRoot/);
  assert.throws(() => module.normalizeConfig({ role: 'captain', chatId: 'c1', captainId: 'c2', captainName: 'B', driveRoot: '/tmp/d' }), /captainId/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: '', driveRoot: '/tmp/d' }), /captainName/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c 2', captainId: 'c1', captainName: 'B', driveRoot: '/tmp/d' }), /chatId/);
  const crew = module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'B', driveRoot: '/tmp/d' });
  assert.equal(crew.captainId, 'c1');
  assert.equal(crew.role, 'crew');
  // The boundary pairs are part of the contract, not a detail of the regex:
  // 128 characters is a valid id, 129 is not; 80 is a valid name, 81 is not.
  const id128 = 'a'.repeat(128);
  assert.equal(module.normalizeConfig({ role: 'crew', chatId: id128, captainId: 'c1', captainName: 'B', driveRoot: '/tmp/d' }).chatId, id128);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: `${id128}a`, captainId: 'c1', captainName: 'B', driveRoot: '/tmp/d' }), /chatId/);
  assert.equal(module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'b'.repeat(80), driveRoot: '/tmp/d' }).captainName.length, 80);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'b'.repeat(81), driveRoot: '/tmp/d' }), /captainName/);
  // A name is one line: a newline would break the section's attribute shape.
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'a\nb', driveRoot: '/tmp/d' }), /captainName/);
  assert.throws(() => module.normalizeConfig({ role: 'crew', chatId: 'c2', captainId: 'c1', captainName: 'a\rb', driveRoot: '/tmp/d' }), /captainName/);
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
  const env = Object.fromEntries(Object.entries({ PATH: process.env.PATH, HOME: process.env.HOME,
    SystemRoot: process.env.SystemRoot, DSH_HOME: home }).filter(([, value]) => value !== undefined));
  const added = await run([dshBin, 'plugin', '--profile', 'sdk', 'add', bundle], { cwd: checkout, env, timeout: 240000 });
  assert.equal(added.code, 0, `dsh plugin add failed: ${report(added)}`);
  const profile = join(home, 'profiles', 'sdk');

  const script = `
import assert from 'node:assert/strict';
const drive = process.argv[1];
const plugin = await import('zeron-dsh-captains');
assert.equal(plugin.name, 'zeron-captains');
const captain = { role: 'captain', chatId: 'captain-1', captainId: 'captain-1', captainName: 'B', driveRoot: drive };
const crew = { role: 'crew', chatId: 'crew-1', captainId: 'captain-1', captainName: 'B', driveRoot: drive };

// Crew and ordinary sessions register zero Drive tools; a Captain gets exactly two.
const crewTools = [];
plugin.apply({ tools: { register: (definition) => { crewTools.push(definition.name); return () => {}; } } }, crew);
assert.deepEqual(crewTools, [], 'a crew chat registers no Drive tools');
plugin.apply({ tools: { register: () => { throw new Error('an ordinary chat registered a tool'); } } }, { role: 'none' });
// The installed entry applies the same validation as the plain module.
assert.throws(() => plugin.apply({ tools: { register: () => {} } },
  { role: 'captain', chatId: 'captain-1', captainId: 'captain-1', captainName: 'B', driveRoot: 'relative' }), /driveRoot/);

const registered = [];
plugin.apply({ tools: { register: (definition) => { registered.push(definition); return () => {}; } } }, captain);
assert.deepEqual(registered.map(definition => definition.name).sort(), ['captain_drive_read', 'captain_drive_write']);
const read = registered.find(definition => definition.name === 'captain_drive_read');
const write = registered.find(definition => definition.name === 'captain_drive_write');
const outside = /path is outside the Captain Drive/;

// Round trip under a nested path the tool creates.
const wrote = await write.execute({ path: 'notes/deep/a.txt', content: 'hello' });
assert.ok(wrote.startsWith('wrote notes/deep/a.txt (') && wrote.endsWith(' bytes)'), wrote);
assert.equal(await read.execute({ path: 'notes/deep/a.txt' }), 'hello');
assert.equal(await read.execute({ path: '' }), 'notes/');

// Traversal, absolute paths and a symlink that leaves the Drive are rejected.
await assert.rejects(read.execute({ path: 'link/secret.txt' }), outside);
await assert.rejects(write.execute({ path: 'link/evil.txt', content: 'x' }), outside);
await assert.rejects(read.execute({ path: '../secret.txt' }), outside);
await assert.rejects(write.execute({ path: '/etc/passwd', content: 'x' }), outside);

// The engine owns state/ and reports/ for writing; the Captain reads them.
await assert.rejects(write.execute({ path: 'state/needs-you.json', content: '{}' }), /engine-owned/);
await assert.rejects(write.execute({ path: 'reports/turn-1.md', content: '# report' }), /engine-owned/);

// A read-only run cannot write the Drive.
process.env.DSH_PERMISSION_MODE = 'read-only';
await assert.rejects(write.execute({ path: 'notes/blocked.txt', content: 'x' }), /read-only/);
delete process.env.DSH_PERMISSION_MODE;

console.log('confined');
`;
  const call = await run(['--input-type=module', '-e', script, drive], { cwd: profile, env, timeout: 60000 });
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
    helm: { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' },
    crew: { role: 'crew', chatId: 'zeron-crew-b', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' },
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
try {
  plugin.apply(ctx, { role: 'captain', chatId: 'c1', captainId: 'c1', captainName: 'B', driveRoot: '/tmp/d' });
  console.log('captain:ran');
} catch (error) {
  console.log('captain:' + String(error.message).slice(0, 120));
}
`;
  const isolated = Object.fromEntries(Object.entries({ HOME: dir, TMPDIR: tmpdir(), DSH_HOME: join(dir, 'home') }));
  const probed = await new Promise(runProbe => {
    const child = spawn(process.execPath, ['--input-type=module', '-e', script, join(bundle, 'index.js')], { cwd: dir, env: isolated });
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
