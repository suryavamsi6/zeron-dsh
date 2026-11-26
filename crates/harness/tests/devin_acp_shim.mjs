// A mock `devin acp` server for the Devin harness's tests: JSON-RPC 2.0 over
// stdio, speaking just enough ACP to drive a turn. Behavior is scripted by
// env vars so one shim serves every test:
//
//   SHIM_PROMPT_DELAY_MS   hold the prompt response this long (default 0)
//   SHIM_AUTH_REQUIRED     answer session/new with an auth_required error
//   SHIM_PERMISSION        ask a permission mid-turn; log the answer's outcome
//   SHIM_HANG_HANDSHAKE    never answer initialize
//   SHIM_LOAD_FAIL         answer session/load with an error (fallback path)
//
// Every notification it emits carries the session id it was given. The shim
// writes a line-delimited JSONL log of what it received to SHIM_LOG (one
// object per line) so tests can assert on the wire shape.

import * as fs from "node:fs";
import * as readline from "node:readline";

const log = process.env.SHIM_LOG
  ? (obj) => fs.appendFileSync(process.env.SHIM_LOG, JSON.stringify(obj) + "\n")
  : () => {};

const send = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");
const answer = (id, result) => send({ jsonrpc: "2.0", id, result });
const fail = (id, code, message) => send({ jsonrpc: "2.0", id, error: { code, message } });

const sessions = new Map();
let sessionSeq = 0;

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  log({ received: msg.method ?? "response", params: msg.params ?? null, id: msg.id ?? null });
  const { id, method, params } = msg;
  if (!method) return;

  if (method === "initialize") {
    if (process.env.SHIM_HANG_HANDSHAKE) return;
    answer(id, {
      protocolVersion: 1,
      agentCapabilities: { promptCapabilities: { image: false } },
      authMethods: [],
    });
    return;
  }
  if (method === "session/new") {
    if (process.env.SHIM_AUTH_REQUIRED) {
      fail(id, -32000, "auth_required: sign in first");
      return;
    }
    sessionSeq += 1;
    const sessionId = `shim-session-${sessionSeq}`;
    sessions.set(sessionId, { cwd: params?.cwd });
    answer(id, { sessionId });
    return;
  }
  if (method === "session/load") {
    if (process.env.SHIM_LOAD_FAIL || !sessions.has(params?.sessionId)) {
      fail(id, -32000, "no such session");
      return;
    }
    answer(id, {});
    return;
  }
  if (method === "session/prompt") {
    const sessionId = params?.sessionId;
    const text = (params?.prompt ?? [])
      .filter((b) => b.type === "text")
      .map((b) => b.text)
      .join("");
    send({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId,
        update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: `echo: ${text}` } },
      },
    });
    if (process.env.SHIM_PERMISSION) {
      // Ask once, then complete however the answer comes back.
      const permId = 9000;
      send({
        jsonrpc: "2.0",
        id: permId,
        method: "session/request_permission",
        params: {
          sessionId,
          options: [
            { optionId: "allow-once-id", kind: "allow_once", name: "Allow once" },
            { optionId: "reject-once-id", kind: "reject_once", name: "Reject once" },
          ],
        },
      });
      // The response arrives as a line with no method; handled in the
      // response branch below via SHIM_PENDING_PERMISSION.
      globalThis.__permFor = id;
      return;
    }
    const delay = Number(process.env.SHIM_PROMPT_DELAY_MS ?? 0);
    const finish = () => answer(id, { stopReason: "end_turn" });
    if (delay) setTimeout(finish, delay);
    else finish();
    return;
  }
  if (method === "session/cancel") {
    log({ received: "cancel-noted" });
    return;
  }
  if (id !== undefined) fail(id, -32601, `unsupported: ${method}`);
});

// Responses to shim-issued requests (permissions) arrive with an id and no method.
rl.on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    return;
  }
  if (msg.method || msg.id === undefined) return;
  log({ received: "permission-answer", outcome: msg.result?.outcome ?? msg.error ?? null });
  if (globalThis.__permFor !== undefined) {
    answer(globalThis.__permFor, { stopReason: "end_turn" });
    delete globalThis.__permFor;
  }
});

process.stdin.on("end", () => process.exit(0));
