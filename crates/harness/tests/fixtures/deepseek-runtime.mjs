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
  // The published runtime routes `/plan` through its commands service the
  // same way `/permission` goes; a fixture that treated it as a prompt would
  // let the sidecar look right while the real runtime got plain text.
  if (text === '/plan') {
    const commandId = `plan-${id}`;
    // The host's test marker: a fixture that cannot be told to fail `/plan`
    // could never show that a failed entry stops the start.
    let failed = false;
    try { await readFile(join(process.env.DSH_HOME, 'plan-fails')); failed = true; } catch {}
    event(sessionId, 'command/run', { commandId });
    event(sessionId, 'command/done', { commandId });
    notify('session.query.completed', { sessionId, commandId, finalResponse: failed ? 'unavailable' : 'plan', finishReason: failed ? 'error' : 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: failed ? { kind: 'error', text: 'unavailable' } : { kind: 'success', text: 'plan' } } });
  }
  // Leaving is the same builtin with the `off` argument, and it succeeds even
  // when the stored session was never in plan mode, so a resume may send it
  // without knowing what the previous run left behind.
  if (text === '/plan off') {
    const commandId = `plan-off-${id}`;
    let failed = false;
    try { await readFile(join(process.env.DSH_HOME, 'plan-off-fails')); failed = true; } catch {}
    event(sessionId, 'command/run', { commandId });
    event(sessionId, 'command/done', { commandId });
    notify('session.query.completed', { sessionId, commandId, finalResponse: failed ? 'unavailable' : 'off', finishReason: failed ? 'error' : 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: failed ? { kind: 'error', text: 'unavailable' } : { kind: 'success', text: 'Plan mode off.' } } });
  }
  // `/goal` and `/compact` are state-only builtins: their text is the
  // command's direct output and never enters model history, so the run has
  // command events and no assistant message.
  if (text === '/compact') {
    const commandId = `compact-${id}`;
    const text = 'Condensed 4 history items; ~1234 tokens saved.';
    event(sessionId, 'command/run', { commandId, name: 'compact' });
    event(sessionId, 'command/done', { commandId, kind: 'success', text });
    notify('session.query.completed', { sessionId, commandId, finalResponse: text, finishReason: 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: { kind: 'success', text } } });
  }
  // A goal edit with nothing to edit is a direct command error, not a model
  // turn: the failure text is all the run produces.
  if (text === '/goal edit') {
    const commandId = `goal-edit-${id}`;
    const text = 'Goal edit needs an objective.';
    event(sessionId, 'command/run', { commandId, name: 'goal', args: ' edit' });
    event(sessionId, 'command/done', { commandId, kind: 'error', text });
    notify('session.query.completed', { sessionId, commandId, finalResponse: text, finishReason: 'error' });
    return reply(id, { kind: 'command', execution: { commandId, result: { kind: 'error', text } } });
  }
  // A typed `/goal <objective>` that starts goal rounds: the command text is
  // the direct output, and the assistant reply that follows is the visible
  // one, so nothing may be noticed twice.
  if (text === '/goal summarize the repo') {
    const commandId = `goal-${id}`;
    event(sessionId, 'command/run', { commandId, name: 'goal', args: ' summarize the repo' });
    event(sessionId, 'command/done', { commandId, kind: 'success', text: 'Goal created.' });
    notify('session.status', { sessionId, status: 'running' });
    event(sessionId, 'turn/start', { turn: 1 });
    event(sessionId, 'assistant/message', { message: { id: 'assistant-goal', content: [{ type: 'text', text: 'Working on: summarize the repo' }] } });
    event(sessionId, 'turn/end', { turn: 1, reason: { kind: 'completed' } });
    notify('session.status', { sessionId, status: 'idle' });
    notify('session.query.completed', { sessionId, commandId, finalResponse: 'Working on: summarize the repo', finishReason: 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: { kind: 'success', text: 'Goal created.' } } });
  }
  if (text === '/goal') {
    const commandId = `goal-${id}`;
    const text = 'Goal: no active goal.';
    event(sessionId, 'command/run', { commandId, name: 'goal' });
    event(sessionId, 'command/done', { commandId, kind: 'success', text });
    notify('session.query.completed', { sessionId, commandId, finalResponse: text, finishReason: 'completed' });
    return reply(id, { kind: 'command', execution: { commandId, result: { kind: 'success', text } } });
  }
  if (text === 'protocol-error') return reply(id, { kind: 'prompt' });
  const messageId = `input-${id}`;
  reply(id, { kind: 'prompt', messageId });
  event(sessionId, 'agent/inbox/spliced', { inserted: [{ id: messageId, source: { kind: 'user' } }] });
  notify('session.status', { sessionId, status: 'running' });
  // A runtime that has announced a turn has already created the session, so the
  // store record is durable before `turn/start` is observable: a host that stops
  // on that event can always resume the session it watched start.
  await mkdir(process.env.DSH_HOME, { recursive: true });
  await writeFile(join(process.env.DSH_HOME, `${sessionId}.fixture`), 'persisted');
  event(sessionId, 'turn/start', { turn: 1 });
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
