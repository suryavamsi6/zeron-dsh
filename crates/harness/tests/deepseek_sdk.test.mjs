import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { apply, requiresApproval, assertRoute } from '../src/deepseek/policy.mjs';
import { runtimeEnvironment, trustedToolEnvironment } from '../src/deepseek/environment.mjs';

const sdkRoot = process.env.ZERON_DEEPSEEK_SDK_DIR;
assert.ok(sdkRoot, 'Set ZERON_DEEPSEEK_SDK_DIR to the installed pinned npm prefix');
const here = fileURLToPath(new URL('.', import.meta.url));
const shim = resolve(here, '../src/deepseek/shim.mjs');
const fixture = resolve(here, 'fixtures/deepseek-runtime.mjs');
const policySource = await readFile(resolve(here, '../src/deepseek/policy.mjs'), 'utf8');
const environmentSource = await readFile(resolve(here, '../src/deepseek/environment.mjs'), 'utf8');

async function runtime(t, options = {}) {
  const home = options.home ?? await mkdtemp(join(tmpdir(), 'zeron-deepseek-test-'));
  // The sidecar hands its child a filtered environment, so a test that wants
  // the fake peer to fail `/plan` leaves a file where that peer can read it.
  if (options.failPlan) await writeFile(join(home, 'plan-fails'), '');
  if (options.failPlanOff) await writeFile(join(home, 'plan-off-fails'), '');
  const child = spawn(process.execPath, [shim, sdkRoot, ...(options.realRuntime ? [] : [fixture])], {
    env: { PATH: process.env.PATH, HOME: process.env.HOME, SystemRoot: process.env.SystemRoot,
      DEEPSEEK_API_KEY: 'synthetic-not-live', DEEPSEEK_BASE_URL: options.baseURL ?? 'http://127.0.0.1:9/anthropic' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const frames = [];
  const seen = [];
  const waiters = [];
  let stderr = '';
  child.stderr.on('data', value => { stderr += value; });
  const exited = new Promise(resolveExit => child.once('exit', (code, signal) => resolveExit({ code, signal })));
  createInterface({ input: child.stdout }).on('line', line => {
    const frame = JSON.parse(line);
    frames.push(frame);
    seen.push(frame.type);
    for (const check of [...waiters]) check();
  });
  function next(type) {
    return new Promise((resolveFrame, reject) => {
      const timer = setTimeout(() => { cleanup(); reject(new Error(`Timeout waiting for ${type}: ${stderr}; ${JSON.stringify(frames)}`)); }, 10000);
      function cleanup() { clearTimeout(timer); const i = waiters.indexOf(check); if (i >= 0) waiters.splice(i, 1); }
      function check() {
        const index = frames.findIndex(f => f.type === type);
        if (index < 0) {
          const failure = frames.find(f => f.type === 'error');
          if (failure && type !== 'error') { cleanup(); reject(new Error(failure.message)); }
          return;
        }
        const [frame] = frames.splice(index, 1);
        cleanup();
        resolveFrame(frame);
      }
      waiters.push(check);
      check();
    });
  }
  const send = command => child.stdin.write(`${JSON.stringify(command)}\n`);
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) send({ type: 'stop' });
    const deadline = setTimeout(() => child.kill('SIGKILL'), 2000);
    await exited;
    clearTimeout(deadline);
    if (!options.home) await rm(home, { recursive: true, force: true });
  });
  send({ type: 'start', dshHome: home, cwd: here, model: options.model ?? 'deepseek-flash', access: 'supervised', sandbox: 'workspace-write', sessionId: options.sessionId ?? 'session-1', resume: options.resume ?? false, policySource, environmentSource,
    ...(options.plan === true ? { plan: true } : {}),
    ...(options.title ? { title: true, systemPrompt: options.systemPrompt, maxTokens: options.maxTokens } : {}) });
  return { home, send, next, frames, seen, exited };
}

async function mockModel(t) {
  const requests = [];
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    // The runtime uploads prompt images through the Files API before the
    // messages call references their ids; a file object echoing the submitted
    // byte count is all an upload needs back.
    if (request.url.endsWith('/files')) {
      const form = body.toString('latin1');
      const start = form.indexOf('\r\n\r\n', form.indexOf('name="file"')) + 4;
      const bytes = form.indexOf('\r\n--', start) - start;
      requests.push({ path: request.url, bytes, key: request.headers['x-api-key'] });
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ id: 'file-mock', type: 'file', filename: 'a.png',
        mime_type: 'image/png', size_bytes: bytes, created_at: '2026-11-18T00:00:00.000Z' }));
      return;
    }
    const input = JSON.parse(body.toString());
    requests.push({ path: request.url, input, key: request.headers['x-api-key'] });
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const event = (type, data) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`);
    event('message_start', { message: { id: 'msg-mock', type: 'message', role: 'assistant', model: input.model,
      content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 0 } } });
    event('content_block_start', { index: 0, content_block: { type: 'text', text: '' } });
    event('content_block_delta', { index: 0, delta: { type: 'text_delta', text: 'SDK_RUNTIME_OK' } });
    event('content_block_stop', { index: 0 });
    event('message_delta', { delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 3 } });
    event('message_stop', {});
    response.end();
  });
  await new Promise(resolveListen => server.listen(0, '127.0.0.1', resolveListen));
  t.after(() => new Promise(resolveClose => { server.closeAllConnections(); server.close(resolveClose); }));
  return { requests, baseURL: `http://127.0.0.1:${server.address().port}/anthropic` };
}

test('actual SDK initializes, preserves ordered events, and settles two turns', async t => {
  const peer = await runtime(t);
  assert.equal((await peer.next('ready')).sessionId, 'session-1');
  for (const prompt of ['first', 'second']) {
    peer.send({ type: 'prompt', prompt });
    const done = await peer.next('done');
    assert.equal(done.finishReason, 'completed');
    assert.equal(done.finalResponse, `reply:${prompt}`);
  }
  const methods = peer.frames.filter(f => f.type === 'notification').map(f => f.method);
  assert.ok(methods.includes('session.event'));
  assert.ok(methods.includes('session.status'));
});

test('published runtime boots mandatory policy and managed permissions without model I/O', async t => {
  const peer = await runtime(t, { realRuntime: true });
  await peer.next('ready');
  peer.send({ type: 'stop' });
  await peer.next('stopped');
  assert.equal((await peer.exited).code, 0);
});

test('actual SDK and runtime complete a turn through a loopback-only mock model', async t => {
  const { requests, baseURL } = await mockModel(t);
  const peer = await runtime(t, { realRuntime: true, baseURL });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'Return the mock marker' });
  assert.equal((await peer.next('done')).finalResponse, 'SDK_RUNTIME_OK');
  assert.equal(requests.length, 1);
  assert.equal(requests[0].path, '/anthropic/v1/messages');
  assert.equal(requests[0].input.model, 'deepseek-flash');
  assert.equal(requests[0].key, 'synthetic-not-live');
  peer.send({ type: 'stop' });
  await peer.next('stopped');
});

test('a title run boots sdk-minimal with no tools, the title system prompt, and the token cap', async t => {
  const { requests, baseURL } = await mockModel(t);
  const peer = await runtime(t, { realRuntime: true, baseURL, title: true, systemPrompt: 'TITLE_SYS', maxTokens: 32 });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'Title this session' });
  assert.equal((await peer.next('done')).finishReason, 'completed');
  const messages = requests.filter(r => r.path.endsWith('/messages'));
  assert.equal(messages.length, 1);
  assert.ok(!messages[0].input.tools?.length, `title run offered tools: ${JSON.stringify(messages[0].input.tools)}`);
  const system = typeof messages[0].input.system === 'string' ? messages[0].input.system : JSON.stringify(messages[0].input.system);
  assert.match(system, /TITLE_SYS/);
  assert.equal(messages[0].input.max_tokens, 32);
  peer.send({ type: 'stop' });
  await peer.next('stopped');
});

test('a mixed-attachment prompt sends deepseek-flash exactly the image', async t => {
  const { requests, baseURL } = await mockModel(t);
  const dir = await mkdtemp(join(tmpdir(), 'zeron-deepseek-attach-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==', 'base64');
  await writeFile(join(dir, 'a.png'), png);
  await writeFile(join(dir, 'report.pdf'), '%PDF-1.4\n');
  const peer = await runtime(t, { realRuntime: true, baseURL });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'look at these', attachments: [join(dir, 'report.pdf'), join(dir, 'a.png')] });
  assert.equal((await peer.next('done')).finalResponse, 'SDK_RUNTIME_OK');
  assert.equal(requests.filter(r => r.path.endsWith('/files')).length, 1);
  const message = requests.find(r => r.path.endsWith('/messages'));
  const blocks = message.input.messages.flatMap(m => Array.isArray(m.content) ? m.content : []);
  const images = blocks.filter(block => block.type === 'image');
  assert.equal(images.length, 1);
  assert.equal(images[0].source.file_id, 'file-mock');
});

test('a prompt carrying only a non-image attachment runs on deepseek-v4-pro', async t => {
  const { requests, baseURL } = await mockModel(t);
  const dir = await mkdtemp(join(tmpdir(), 'zeron-deepseek-attach-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const pdf = join(dir, 'report.pdf');
  await writeFile(pdf, '%PDF-1.4\n');
  const peer = await runtime(t, { realRuntime: true, baseURL, model: 'deepseek-v4-pro' });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'look at this', attachments: [pdf] });
  assert.equal((await peer.next('done')).finalResponse, 'SDK_RUNTIME_OK');
  assert.equal(requests.length, 1);
  const blocks = requests[0].input.messages.flatMap(m => Array.isArray(m.content) ? m.content : []);
  assert.ok(!blocks.some(block => block.type === 'image'));
});

test('an image attachment on deepseek-v4-pro is refused', async t => {
  const dir = await mkdtemp(join(tmpdir(), 'zeron-deepseek-attach-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const png = join(dir, 'a.png');
  await writeFile(png, Buffer.from('89504e470d0a1a0a', 'hex'));
  const peer = await runtime(t, { model: 'deepseek-v4-pro' });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'look', attachments: [png] });
  const error = await peer.next('error');
  assert.match(error.message, /does not support image input/);
  assert.equal((await peer.exited).code, 1);
});

test('a resume of an unknown session surfaces session not found', async t => {
  const peer = await runtime(t, { sessionId: 'unknown-session', resume: true });
  const error = await peer.next('error');
  assert.match(error.message, /session not found/);
  assert.match(error.message, /unknown-session/);
  assert.equal((await peer.exited).code, 1);
  assert.ok(!peer.frames.some(f => f.type === 'ready'));
});

test('actual SDK forwards approvals and denies dismissal', async t => {
  const peer = await runtime(t);
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'approval' });
  const question = await peer.next('request');
  assert.equal(question.method, 'approval/request');
  peer.send({ type: 'answer', id: question.id, result: { cancelled: true } });
  await peer.next('done');
  assert.ok(peer.frames.some(f => f.params?.event?.type === 'tool/result' && f.params.event.data.message.isError));
});

test('actual SDK answers native structured questions', async t => {
  const peer = await runtime(t);
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'question' });
  const question = await peer.next('request');
  assert.equal(question.method, 'user-question/ask');
  peer.send({ type: 'answer', id: question.id, result: { answers: [{ id: 'choice', selected: ['B'] }] } });
  assert.equal((await peer.next('done')).finalResponse, 'reply:question');
});

test('actual SDK stops by shutdown and resumes the saved identity in a fresh runtime', async t => {
  const home = await mkdtemp(join(tmpdir(), 'zeron-deepseek-resume-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  // The first run is in plan mode, and the runtime stores that mode with the
  // session, so the resumed run starts out still planning.
  const first = await runtime(t, { home, sessionId: 'saved', plan: true });
  await first.next('ready');
  first.send({ type: 'prompt', prompt: 'wait-for-stop' });
  while (!(await first.next('notification')).params?.event?.type?.includes('turn/start')) {}
  first.send({ type: 'stop' });
  await first.next('stopped');
  assert.equal((await first.exited).code, 0);
  assert.ok(!first.frames.some(f => f.type === 'done'));
  const second = await runtime(t, { home, sessionId: 'saved', resume: true });
  await second.next('ready');
  const command = id => String(id).startsWith('plan-off-') ? '/plan off'
    : String(id).startsWith('plan-') ? '/plan'
    : String(id).startsWith('permission-') ? '/permission' : String(id);
  const commands = second.frames
    .filter(f => f.type === 'notification' && f.params?.event?.type === 'command/run')
    .map(f => command(f.params.event.data.commandId));
  assert.deepEqual(commands, ['/permission', '/plan off'],
    'a resume that runs with the plan toggle off leaves the stored plan mode');
  second.send({ type: 'prompt', prompt: 'continued' });
  assert.equal((await second.next('done')).finalResponse, 'reply:continued');
  const leaveAt = second.frames.findIndex(f => f.type === 'notification' && command(f.params?.event?.data?.commandId) === '/plan off');
  const promptAt = second.frames.findIndex(f => f.type === 'notification' && f.params?.event?.type === 'turn/start');
  assert.ok(leaveAt >= 0 && promptAt >= 0 && leaveAt < promptAt,
    'plan mode is left before the prompt reaches the model');
});

test('a failed /plan off stops a resumed start before any prompt', async t => {
  const home = await mkdtemp(join(tmpdir(), 'zeron-deepseek-resume-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  // A persisted session is what the resume looks for; the mode it was left in
  // is the runtime's, which is why a run that cannot leave it must not start.
  const first = await runtime(t, { home, sessionId: 'saved', plan: true });
  await first.next('ready');
  first.send({ type: 'prompt', prompt: 'draft a change' });
  await first.next('done');
  first.send({ type: 'stop' });
  await first.next('stopped');
  await first.exited;
  const second = await runtime(t, { home, sessionId: 'saved', resume: true, failPlanOff: true });
  const error = await second.next('error');
  assert.match(error.message, /Unable to leave DeepSeek plan mode/);
  assert.equal((await second.exited).code, 1);
  assert.ok(!second.frames.some(f => f.type === 'ready'), 'ready is never sent');
  assert.ok(!second.frames.some(f => f.type === 'notification' && f.params?.event?.type === 'turn/start'),
    'no prompt reaches the runtime');
});

test('plan mode enters through the same slash path as the permission preset, before the prompt', async t => {
  const peer = await runtime(t, { plan: true });
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: 'draft a change' });
  assert.equal((await peer.next('done')).finishReason, 'completed');
  const command = id => String(id).startsWith('plan-') ? 'plan'
    : String(id).startsWith('permission-') ? 'permission' : String(id);
  const commands = peer.frames
    .filter(f => f.type === 'notification' && f.params?.event?.type === 'command/run')
    .map(f => command(f.params.event.data.commandId));
  assert.deepEqual(commands, ['permission', 'plan'], 'both modes are runtime commands, in that order');
  const promptAt = peer.frames.findIndex(f => f.type === 'notification' && f.params?.event?.type === 'turn/start');
  const planAt = peer.frames.findIndex(f => f.type === 'notification' && command(f.params?.event?.data?.commandId) === 'plan');
  assert.ok(planAt >= 0, 'the runtime saw a /plan command');
  assert.ok(promptAt >= 0 && planAt < promptAt, 'plan mode is entered before the user prompt reaches the model');
});

test('a failed /plan stops the start before any prompt reaches the runtime', async t => {
  const peer = await runtime(t, { plan: true, failPlan: true });
  const error = await peer.next('error');
  assert.match(error.message, /Unable to enter DeepSeek plan mode/);
  assert.equal((await peer.exited).code, 1);
  assert.ok(!peer.frames.some(f => f.type === 'ready'), 'ready is never sent');
  assert.ok(!peer.frames.some(f => f.type === 'notification' && f.params?.event?.type === 'turn/start'),
    'no prompt reaches the runtime');
});

test("a state-only command's text is a notice exactly once, before the turn ends", async t => {
  const peer = await runtime(t);
  await peer.next('ready');
  for (const [prompt, reply] of [['/goal', 'Goal: no active goal.'],
    ['/compact', 'Condensed 4 history items; ~1234 tokens saved.']]) {
    const start = peer.seen.length;
    peer.send({ type: 'prompt', prompt });
    const notice = await peer.next('notice');
    const done = await peer.next('done');
    assert.equal(notice.text, reply);
    assert.equal(done.finishReason, 'completed');
    const tail = peer.seen.slice(start);
    assert.equal(tail.filter(type => type === 'notice').length, 1, `${prompt} noticed its text once`);
    assert.ok(tail.indexOf('notice') < tail.indexOf('done'), `${prompt} noticed before the turn ended`);
    assert.ok(!peer.frames.some(frame => frame.type === 'notice'), `${prompt} left no second notice`);
  }
});

test('a typed /goal whose reply is already visible gets no notice', async t => {
  const peer = await runtime(t);
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: '/goal summarize the repo' });
  assert.equal((await peer.next('done')).finalResponse, 'Working on: summarize the repo');
  assert.equal(peer.seen.filter(type => type === 'notice').length, 0,
    'the assistant reply is the only visible text');
  const run = peer.frames.find(frame => frame.type === 'notification'
    && frame.params?.event?.type === 'command/run'
    && frame.params.event.data.name === 'goal');
  assert.equal(run?.params.event.data.args, ' summarize the repo');
});

test("a failing command's own text is a notice exactly once", async t => {
  const peer = await runtime(t);
  await peer.next('ready');
  peer.send({ type: 'prompt', prompt: '/goal edit' });
  const notice = await peer.next('notice');
  assert.equal(notice.text, 'Goal edit needs an objective.');
  assert.equal((await peer.next('done')).finishReason, 'error');
  assert.equal(peer.seen.filter(type => type === 'notice').length, 1);
});

// The runtime resolves its localized token to the same command, so the guard
// has to know both: `/计划` reaches the plan builtin exactly as `/plan` does.
for (const prompt of ['/plan', '/plan off', '/计划', '/计划 off']) {
  test(`a prompt cannot enter plan mode by starting with ${prompt}`, async t => {
    const peer = await runtime(t);
    await peer.next('ready');
    peer.send({ type: 'prompt', prompt });
    const error = await peer.next('error');
    assert.match(error.message, /Plan mode is set by Zeron when a run starts/);
    assert.equal((await peer.exited).code, 1);
    assert.ok(!peer.frames.some(f => f.type === 'notification' && f.params?.event?.type === 'turn/start'),
      `a ${prompt} prompt never becomes a command`);
  });
}

for (const prompt of ['protocol-error', 'malformed-event']) {
  test(`actual SDK rejects ${prompt} without successful completion`, async t => {
    const peer = await runtime(t);
    await peer.next('ready');
    peer.send({ type: 'prompt', prompt });
    const error = await peer.next('error');
    assert.match(error.message, /SdkProtocolError/);
    assert.equal((await peer.exited).code, 1);
    assert.ok(!peer.frames.some(f => f.type === 'done'));
  });
}

test('native policy asks before unknown tools and cannot turn a refusal into permission', async () => {
  assert.equal(requiresApproval('supervised', 'bash'), true);
  assert.equal(requiresApproval('auto-accept-edits', 'write'), false);
  assert.equal(requiresApproval('auto-accept-edits', 'mcp__server__mutate'), true);
  assert.equal(requiresApproval('full-access', 'bash'), false);
  assert.throws(() => requiresApproval('invented', 'bash'));
  let gate;
  let guard;
  let modelGuard;
  let outcome = 'unavailable';
  let marker;
  const ctx = {
    on: (name, callback) => {
      if (name === 'llm/stream') modelGuard = callback;
      else { assert.equal(name, 'tools/pre-execute'); gate = callback; }
    },
    tools: { guard: callback => { guard = callback; } },
    approval: { request: async () => outcome },
    provide: (name, value) => { assert.equal(name, 'zeronPolicy'); marker = value; },
  };
  apply(ctx, { access: 'supervised' });
  assert.equal(marker.ready, true);
  const execution = () => ({ name: 'bash', callId: 'call', token: {}, agent: {}, signal: new AbortController().signal });
  const refused = execution();
  assert.equal((await gate(refused, async () => ({ kind: 'allow' }))).kind, 'deny');
  assert.match(guard(refused), /explicit approval/);
  outcome = 'allowed-once';
  const accepted = execution();
  assert.equal((await gate(accepted, async () => ({ kind: 'allow' }))).kind, 'allow');
  assert.equal(guard(accepted), undefined);
  assert.match(guard(execution()), /explicit approval/);
  let dispatched = false;
  // A route the host stamped is one DSH resolves; the official route keeps
  // its own two models, and a route id that is not one is refused.
  assert.doesNotThrow(() => modelGuard({ provider: 'openai', model: 'other' }, () => { dispatched = true; }));
  assert.equal(dispatched, true, "a host-stamped route's request is dispatched");
  dispatched = false;
  assert.throws(() => modelGuard({ provider: 'Not A Route', model: 'other' }, () => { dispatched = true; }), /only permits/);
  assert.equal(dispatched, false, "a refused route's request never is");
});

test('the official route runs its own models, and a host-stamped route its own', () => {
  for (const model of ['deepseek-flash', 'deepseek-v4-pro']) assert.doesNotThrow(() => assertRoute({ provider: 'deepseek-official', model }));
  // The official route offers nothing else.
  for (const model of ['deepseek-chat', 'another-model']) {
    assert.throws(() => assertRoute({ provider: 'deepseek-official', model }), /only permits official DeepSeek/);
  }
  // Any other route is one the host stamped (`ZERON_DEEPSEEK_PROVIDER`, which
  // never rides the wire); DSH resolves its models and reports an unknown one.
  for (const provider of ['openai', 'opencode-go', 'deepseek-account']) {
    assert.doesNotThrow(() => assertRoute({ provider, model: 'whatever' }));
  }
  for (const provider of ['Not A Route', '', 'UPPER', '-leading']) {
    assert.throws(() => assertRoute({ provider, model: 'x' }), /only permits official DeepSeek/);
  }
});

test('credential filtering retains only DeepSeek auth and explicit Git tools overlays', () => {
  const source = { PATH: '/bin', DEEPSEEK_API_KEY: 'synthetic-deepseek', OPENAI_API_KEY: 'forbidden',
    ANTHROPIC_API_KEY: 'forbidden', GH_TOKEN: 'synthetic-github', GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: 'core.hooksPath', GIT_CONFIG_VALUE_0: '/trusted/hooks',
    GIT_CONFIG_KEY_9: 'forbidden', NODE_OPTIONS: '--require malicious.cjs' };
  const env = runtimeEnvironment(source);
  assert.equal(env.DEEPSEEK_API_KEY, source.DEEPSEEK_API_KEY);
  assert.equal(env.OPENAI_API_KEY, undefined);
  assert.equal(env.ANTHROPIC_API_KEY, undefined);
  assert.equal(env.NODE_OPTIONS, undefined);
  assert.equal(env.GIT_CONFIG_KEY_9, undefined);
  assert.deepEqual(trustedToolEnvironment(env), { GH_TOKEN: source.GH_TOKEN, GIT_CONFIG_COUNT: '1',
    GIT_CONFIG_KEY_0: 'core.hooksPath', GIT_CONFIG_VALUE_0: '/trusted/hooks' });
  assert.throws(() => runtimeEnvironment({ GIT_CONFIG_COUNT: '1' }), /Incomplete/);
});
