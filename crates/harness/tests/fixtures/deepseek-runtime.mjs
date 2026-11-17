import assert from 'node:assert/strict';
import { createInterface } from 'node:readline';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { join } from 'node:path';

assert.equal(process.argv[2], '--profile');
assert.equal(process.argv[3], 'sdk');
assert.ok(process.env.DSH_HOME);
assert.equal(process.env.OPENAI_API_KEY, undefined);
assert.equal(process.env.ANTHROPIC_API_KEY, undefined);
const patchPath = process.argv[process.argv.indexOf('--patch') + 1];
const patch = await readFile(patchPath, 'utf8');
assert.match(patch, /zeronPolicy/);
assert.match(patch, /"access":"(?:supervised|auto-accept-edits|full-access)"/);
let initialized = false;
let seq = 0;
let serial = 1000;
const sessions = new Set();
const replies = new Map();
const lines = createInterface({ input: process.stdin });
const send = frame => process.stdout.write(`${JSON.stringify(frame)}\n`);
const notify = (method, params) => send({ jsonrpc: '2.0', method, params });
const event = (sessionId, type, data) => notify('session.event', { sessionId, event: { seq: seq++, time: Date.now(), type, data } });
const reply = (id, result) => send({ jsonrpc: '2.0', id, result });
const failure = (id, message) => send({ jsonrpc: '2.0', id, error: { code: -32603, message } });
const serverRequest = (method, params) => new Promise(resolve => {
  const id = serial++;
  replies.set(id, resolve);
  send({ jsonrpc: '2.0', id, method, params });
});

async function handle(request) {
  if (!request.method) {
    replies.get(request.id)?.(request.result);
    replies.delete(request.id);
    return;
  }
  const { id, method, params } = request;
  if (method === 'initialize') {
    assert.equal(params.provider, 'deepseek-official');
    assert.ok(['deepseek-flash', 'deepseek-v4-pro'].includes(params.model));
    assert.equal(params.approvalRequests, true);
    assert.equal(params.capabilities.userQuestions, true);
    initialized = true;
    return reply(id, { serverInfo: { name: 'deepseek-harness-sdk-runtime', version: '0.0.1' } });
  }
  if (method === 'shutdown') {
    reply(id, {});
    return setImmediate(() => process.exit(0));
  }
  assert.ok(initialized);
  if (method === 'session/resume') {
    const saved = join(process.env.DSH_HOME, `${params.sessionId}.fixture`);
    // The published runtime's own wording for a session its store no longer has.
    try { await readFile(saved); } catch { return failure(id, `session "${params.sessionId}" not found`); }
    sessions.add(params.sessionId);
    return reply(id, {});
  }
  assert.equal(method, 'session/query');
  const sessionId = params.sessionId;
  const text = params.contentBlocks.filter(b => b.type === 'text').map(b => b.text).join('');
  if (text === '/permission zeron-managed') {
    const commandId = `permission-${id}`;
    event(sessionId, 'command/run', { commandId });
    event(sessionId, 'command/done', { commandId });
    notify('session.query.completed', { sessionId, commandId, finalResponse: 'managed', finishReason: 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: { kind: 'success', text: 'managed' } } });
  }
  if (text === 'protocol-error') return reply(id, { kind: 'prompt' });
  const messageId = `input-${id}`;
  reply(id, { kind: 'prompt', messageId });
  event(sessionId, 'agent/inbox/spliced', { inserted: [{ id: messageId, source: { kind: 'user' } }] });
  notify('session.status', { sessionId, status: 'running' });
  event(sessionId, 'turn/start', { turn: 1 });
  await mkdir(process.env.DSH_HOME, { recursive: true });
  await writeFile(join(process.env.DSH_HOME, `${sessionId}.fixture`), 'persisted');
  if (text === 'wait-for-stop') return;
  if (text === 'approval') {
    event(sessionId, 'tool/call', { callId: 'call-1', name: 'bash', arguments: '{"command":"true"}' });
    const answer = await serverRequest('approval/request', { sessionId, toolName: 'bash', callId: 'call-1', reason: 'test command' });
    event(sessionId, 'tool/result', { message: { toolCallId: 'call-1', isError: answer.outcome !== 'allowed-once', content: [{ type: 'text', text: answer.outcome }] } });
  }
  if (text === 'question') {
    const answer = await serverRequest('user-question/ask', { requestId: 'question-1', sessionId, questions: [{ id: 'choice', question: 'Which?', options: [{ label: 'A' }, { label: 'B' }] }] });
    assert.deepEqual(answer, { answers: [{ id: 'choice', selected: ['B'] }] });
  }
  if (text === 'malformed-event') {
    event(sessionId, 'assistant/message', { message: { content: 'invalid' } });
    return;
  }
  event(sessionId, 'assistant/message', { message: { id: 'assistant-1', content: [{ type: 'reasoning', text: 'think' }, { type: 'text', text: `reply:${text}` }] }, stream: [{ type: 'reasoning-chunks', texts: ['think'] }, { type: 'text-chunks', texts: ['reply:', text] }], usage: { inputTokens: 10, outputTokens: 2 } });
  event(sessionId, 'turn/end', { turn: 1, reason: { kind: 'completed' } });
  notify('session.status', { sessionId, status: 'idle' });
}

lines.on('line', line => { void handle(JSON.parse(line)).catch(error => { console.error(error); process.exit(1); }); });
lines.on('close', () => process.exit(0));
