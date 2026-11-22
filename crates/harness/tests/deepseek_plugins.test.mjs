import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { accessSync, constants } from 'node:fs';
import { cp, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
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
  const module = await import(new URL('../../../plugins/dsh-captains/index.js', import.meta.url).href);
  assert.equal(module.name, 'zeron-captains');
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

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' };
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
    const text = typeof input.system === 'string' ? input.system : JSON.stringify(input.system ?? '');
    const child = text.includes('delegated by agent');
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

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' };
  const result = await sidecar({ here, env }, sdkRoot, checkout, home, role, 'dsh-session-a', {
    prompt: 'delegate this to a subagent',
    env: { DEEPSEEK_BASE_URL: `http://127.0.0.1:${server.address().port}/anthropic` },
  });
  assert.equal(result.frame.type, 'done', `the run did not finish: ${JSON.stringify(result.frame).slice(0, 300)}\n${result.lines.slice(-4).join('\n')}`);

  const system = input => typeof input.system === 'string' ? input.system : JSON.stringify(input.system ?? '');
  const root = requests.find(input => system(input).includes('<captain name="Helm">'));
  assert.ok(root, 'the root session never carried the captain prompt');
  // A child session's first user turn is the delegated task, not the person's ask.
  const child = requests.find(input => JSON.stringify((input.messages ?? [])[0] ?? '').includes('child task'));
  assert.ok(child, 'the subagent never ran');
  assert.doesNotMatch(system(child), /<captain|## Needs you/, 'a native child inherited the captain prompt');
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

  const role = { role: 'captain', chatId: 'zeron-helm-a', captainId: 'zeron-helm-a', captainName: 'Helm', driveRoot: '/tmp/drive/helm' };
  const refused = await sidecar({ here, env }, sdkRoot, checkout, home, role, 'dsh-session-a');
  assert.equal(refused.frame.type, 'error', `a captains row without the bundle started: ${refused.lines.join('\n')}`);
  assert.match(refused.frame.message, /bundle|zeron-captains/i, `the refusal did not name the missing bundle: ${refused.frame.message}`);

  // An ordinary chat in the same home still starts: only a stamped role needs
  // the bundle, and the inert path must never pay for it.
  const ordinary = await sidecar({ here, env }, sdkRoot, checkout, home, undefined, 'dsh-session-b');
  assert.equal(ordinary.frame.type, 'ready', `an ordinary chat was refused: ${ordinary.lines.join('\n')}`);
});
