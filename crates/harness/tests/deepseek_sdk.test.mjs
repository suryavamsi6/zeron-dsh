import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
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
  const child = spawn(process.execPath, [shim, sdkRoot, ...(options.realRuntime ? [] : [fixture])], {
    env: { PATH: process.env.PATH, HOME: process.env.HOME, SystemRoot: process.env.SystemRoot,
      DEEPSEEK_API_KEY: 'synthetic-not-live', DEEPSEEK_BASE_URL: options.baseURL ?? 'http://127.0.0.1:9/anthropic' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const frames = [];
  const waiters = [];
  let stderr = '';
  child.stderr.on('data', value => { stderr += value; });
  const exited = new Promise(resolveExit => child.once('exit', (code, signal) => resolveExit({ code, signal })));
  createInterface({ input: child.stdout }).on('line', line => {
    const frame = JSON.parse(line);
    frames.push(frame);
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
  send({ type: 'start', dshHome: home, cwd: here, model: 'deepseek-flash', access: 'supervised', sandbox: 'workspace-write', sessionId: options.sessionId ?? 'session-1', resume: options.resume ?? false, policySource, environmentSource });
  return { home, send, next, frames, exited };
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
  const requests = [];
  const server = createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    const input = JSON.parse(body);
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
  const peer = await runtime(t, { realRuntime: true, baseURL: `http://127.0.0.1:${server.address().port}/anthropic` });
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
  const first = await runtime(t, { home, sessionId: 'saved' });
  await first.next('ready');
  first.send({ type: 'prompt', prompt: 'wait-for-stop' });
  while (!(await first.next('notification')).params?.event?.type?.includes('turn/start')) {}
  first.send({ type: 'stop' });
  await first.next('stopped');
  assert.equal((await first.exited).code, 0);
  assert.ok(!first.frames.some(f => f.type === 'done'));
  const second = await runtime(t, { home, sessionId: 'saved', resume: true });
  await second.next('ready');
  second.send({ type: 'prompt', prompt: 'continued' });
  assert.equal((await second.next('done')).finalResponse, 'reply:continued');
});

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
  assert.throws(() => modelGuard({ provider: 'openai', model: 'other' }, () => { dispatched = true; }), /only permits/);
  assert.equal(dispatched, false);
});

test('all root and descendant LLM requests must use an official DeepSeek route', () => {
  for (const model of ['deepseek-flash', 'deepseek-v4-pro']) assert.doesNotThrow(() => assertRoute({ provider: 'deepseek-official', model }));
  for (const options of [
    { provider: 'deepseek-account', model: 'deepseek-flash' },
    { provider: 'openai', model: 'deepseek-flash' },
    { provider: 'deepseek-official', model: 'deepseek-chat' },
    { provider: 'deepseek-official', model: 'another-model' },
  ]) assert.throws(() => assertRoute(options), /only permits official DeepSeek/);
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
