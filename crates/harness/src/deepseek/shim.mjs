import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import { dirname, resolve, join, isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';

const VERSION = '0.2.2-rc.3';
const models = new Set(['deepseek-flash', 'deepseek-v4-pro']);
// The `llm-pi-ai` entries the gateway wrote into the web profile, as YAML
// patch lines (empty when there are none, or no profile yet).
async function gatewayRoutes(dshHome) {
  try {
    const text = await readFile(join(dshHome, 'profiles', 'web', 'cordis.patch.yml'), 'utf8');
    return text
      .split(/^(?=- )/m)
      .filter(block => /^- id: llm-pi-ai\b/.test(block))
      .join('');
  } catch {
    return '';
  }
}

const sdkRoot = resolve(process.argv[1].endsWith('shim.mjs') ? process.argv[2] : process.argv[1]);
const testRuntime = process.argv[1].endsWith('shim.mjs') ? process.argv[3] : process.argv[2];
const requests = new Map();
let harness;
let session;
let launch;
let runtimeDirectory;
let startup;
let queue = Promise.resolve();
let closing;
let notifications;
let notificationTask;
let stopped = false;
const secrets = new Set();

function send(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`);
}

function diagnostic(error) {
  let text = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  for (const secret of secrets) if (secret.length > 3) text = text.replaceAll(secret, '[redacted]');
  return text.slice(-12000);
}

async function hostRequest(method, params, signal) {
  if (stopped || signal?.aborted) return { cancelled: true };
  const id = randomUUID();
  return new Promise(resolveResult => {
    const cancel = () => {
      requests.delete(id);
      send({ type: 'requestCancelled', id });
      resolveResult({ cancelled: true });
    };
    const finish = result => {
      signal?.removeEventListener('abort', cancel);
      requests.delete(id);
      resolveResult(result);
    };
    requests.set(id, finish);
    signal?.addEventListener('abort', cancel, { once: true });
    send({ type: 'request', id, method, params });
  });
}

async function start(config) {
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 24 || major === 24 && minor < 9) throw new Error('DeepSeek SDK requires Node >=24.9');
  if (!isAbsolute(config.dshHome) || !isAbsolute(config.cwd)) throw new Error('Explicit absolute Harness home and workspace are required');
  // A shared home reads Zeron's own credential store, so the provider and the
  // model are whatever that store configures: the official route keeps its two
  // models, and any other route is resolved by DSH's own provider layer.
  const shared = config.sharedHome === true;
  const provider = config.provider ?? 'deepseek-official';
  if (typeof provider !== 'string' || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(provider)) throw new Error('Unsupported DeepSeek provider route');
  if (provider === 'deepseek-official') {
    if (!models.has(config.model)) throw new Error('Unsupported official DeepSeek model');
  } else if (typeof config.model !== 'string' || !/^[\w.:/-]{1,128}$/.test(config.model)) {
    throw new Error('Unsupported model for this provider route');
  }
  if (!['supervised', 'auto-accept-edits', 'full-access'].includes(config.access)) throw new Error('Unknown access mode');
  // The store is the authority once it is shared; a fenced run has only the
  // environment to read its key from.
  if (!shared && !process.env.DEEPSEEK_API_KEY?.trim()) throw new Error('Set DEEPSEEK_API_KEY before starting DeepSeek Harness');
  // Every level the pinned DSH names, not only the official route's three: a
  // gateway model's catalog says which it accepts (the picker offers that
  // intersection), and DSH itself refuses one the model lacks. Refusing the
  // rest here made a Mistral model's own default (`medium`) a run failure.
  if (config.reasoningEffort && !['minimal', 'low', 'medium', 'high', 'xhigh', 'max'].includes(config.reasoningEffort)) throw new Error('Unsupported DeepSeek reasoning effort');
  if (config.maxTokens !== undefined && (!Number.isSafeInteger(config.maxTokens) || config.maxTokens <= 0)) throw new Error('Invalid maxTokens');
  const require = createRequire(join(sdkRoot, 'package.json'));
  for (const name of ['dsh-sdk-client', 'dsh', 'dsh-sdk-protocol']) {
    const entry = require.resolve(`@deepseek-ai/${name}` + (name === 'dsh' ? '/package.json' : ''));
    const manifest = name === 'dsh' ? entry : resolve(dirname(entry), '..', 'package.json');
    const metadata = JSON.parse(await readFile(manifest, 'utf8'));
    if (metadata.version !== VERSION) throw new Error(`Expected @deepseek-ai/${name}@${VERSION}`);
  }
  const title = config.title === true;
  // The captains row appended below is an invocation-layer override of a plugin
  // row only an installed bundle inserts; cordis drops an override whose id
  // nothing owns with only a warning, so a stamped role in a profile without
  // the bundle would run captainless while the sidecar still reported ready.
  // Refuse before the run boots anything. An ordinary chat never stamps the
  // row, so it never pays for this manifest read.
  if (typeof config.captains === 'object' && config.captains !== null) {
    let bundles;
    try {
      bundles = JSON.parse(await readFile(join(config.dshHome, 'profiles', title ? 'sdk-minimal' : 'sdk', 'package.json'), 'utf8')).dsh?.profile?.bundles;
    } catch { /* a missing or unreadable manifest installs nothing */ }
    if (bundles?.includes?.('zeron-dsh-captains') !== true) throw new Error('the captains bundle is not installed in the sdk profile (zeron-dsh-captains)');
  }
  const { DeepSeekHarness } = await import(pathToFileURL(require.resolve('@deepseek-ai/dsh-sdk-client')).href);
  await mkdir(config.dshHome, { recursive: true, mode: 0o700 });
  runtimeDirectory = await mkdtemp(join(config.dshHome, '.zeron-runtime-'));
  const policyPath = join(runtimeDirectory, 'policy.mjs');
  await writeFile(policyPath, config.policySource, { mode: 0o600 });
  const environmentPath = join(runtimeDirectory, 'environment.mjs');
  await writeFile(environmentPath, config.environmentSource, { mode: 0o600 });
  const { runtimeEnvironment } = await import(pathToFileURL(environmentPath).href);
  const environment = { ...runtimeEnvironment(process.env), DSH_PERMISSION_MODE: config.sandbox };
  // A title run boots the standalone `sdk-minimal` profile: none of the `sdk`
  // profile's zeron-policy, credentials, plugin, web, or MCP rows exist there
  // to patch, so its per-run patch only switches off the agent tools the
  // profile does have, and the title instructions ride in as the persona.
  if (title) {
    if (typeof config.systemPrompt !== 'string' || !config.systemPrompt.trim()) throw new Error('A title run needs a system prompt');
    environment.DSH_SYSTEM_PROMPT = config.systemPrompt;
  }
  let patch = title ? [
    { id: 'tool-fs', disabled: true },
    { id: 'tool-working-directory', disabled: true },
    { id: 'terminal-bash', disabled: true },
    { id: 'terminal-pwsh', disabled: true },
    // `terminal-*` are only the PTY backends; the model-facing `bash` tool is
    // `persistent-*`, which stays advertised (and then fails) unless the tool
    // plugins themselves are off.
    { id: 'persistent-bash', disabled: true },
    { id: 'persistent-pwsh', disabled: true },
  ] : [
    // A shared home is the store the gateway writes, watched so a settings
    // change reaches a session already running; a fenced run gets its own file
    // and the account and pi-ai routes stay off.
    shared
      ? { id: 'credentials', disabled: false, config: { path: join(config.dshHome, '.credentials.yaml'), watch: true } }
      : { id: 'credentials', disabled: false, config: { path: join(runtimeDirectory, 'credentials.yaml'), watch: false } },
    ...(shared ? [] : [
      { id: 'deepseek-account', disabled: true },
      { id: 'llm-deepseek-account', disabled: true },
      { id: 'llm-pi-ai', disabled: true },
    ]),
    { id: 'tool-plugin-manager', disabled: true },
    // The chat's "Web search" switch: a patch row's config replaces the base
    // `tool-web` row's whole, so `fetch` and the search budget are restated
    // when the run turns only the search half off.
    ...(config.webSearch === false
      ? [{ id: 'tool-web', config: { search: false, fetch: true, searchTimeoutMs: 60000 } }]
      : []),
    { id: 'approval', config: { policy: 'ask' } },
    { id: 'permission', config: {
      presets: { 'zeron-managed': { sandbox: config.sandbox, approval: 'ask' } },
      defaultPreset: 'zeron-managed',
    } },
    { id: 'sdk-jsonrpc-server', inject: ['sdkAppStartup', 'loader', 'zeronPolicy'] },
    { insert: [{ id: 'zeron-policy', name: pathToFileURL(policyPath).href, config: { access: config.access, captains: config.captains } }] },
  ];
  let yaml = patch.map(row => `- ${JSON.stringify(row)}`).join('\n') + '\n';
  const names = new Set();
  for (const [index, mcp] of (title ? [] : (config.mcp ?? [])).entries()) {
    if (!/^[A-Za-z0-9_-]{1,32}$/.test(mcp.name) || names.has(mcp.name)) throw new Error('Invalid or duplicate MCP server name');
    if (!mcp.command) throw new Error('An MCP server needs a command');
    names.add(mcp.name);
    const variable = `ZERON_DEEPSEEK_MCP_SECRET_${index}`;
    environment[variable] = JSON.stringify({
      serverName: mcp.name, transport: 'stdio', command: mcp.command,
      args: mcp.args ?? [], env: mcp.env ?? {}, cwd: config.cwd,
      failOnStartupError: true, startupTimeoutMs: 30000,
    });
    for (const value of Object.values(mcp.env ?? {})) if (value) secrets.add(value);
    yaml += `- insert:\n    - id: zeron-mcp-${index}\n      name: '@deepseek-ai/dsh-mcp-client'\n      config: !!js JSON.parse(process.env.${variable})\n`;
  }
  // The host-stamped Captain role is the invocation layer's override row: it
  // lands after the bundle's own `role: none` row and wins. The start already
  // refused a profile that never installed the bundle, so this id is always
  // one cordis knows.
  if (typeof config.captains === 'object' && config.captains !== null) {
    yaml += `- ${JSON.stringify({ id: 'zeron-captains', config: config.captains })}\n`;
  }
  // The gateway's route registrations (the Providers page's writes) land in
  // the `web` profile; this run's is `sdk`, so a route configured there has
  // no adapter here ("no adapter registered for provider"). Carry the pi-ai
  // entry across: the credential store is shared, the settings are not. A
  // title run keeps its tool switches as the whole patch.
  if (!title) yaml += await gatewayRoutes(config.dshHome);
  const patchPath = join(runtimeDirectory, 'cordis.patch.yml');
  await writeFile(patchPath, yaml, { mode: 0o600 });
  for (const [key, value] of Object.entries(environment)) if (/key|token|secret/i.test(key) && value) secrets.add(value);
  harness = new DeepSeekHarness({
    profile: title ? 'sdk-minimal' : 'sdk', dshHome: config.dshHome, cwd: config.cwd, processCwd: config.cwd,
    provider, model: config.model,
    ...(config.reasoningEffort ? { reasoningEffort: config.reasoningEffort } : {}),
    ...(config.maxTokens === undefined ? {} : { maxTokens: config.maxTokens }),
    env: environment, patches: [patchPath], approvalRequests: true,
    ...(testRuntime ? { dshBin: resolve(testRuntime) } : {}),
    onUserQuestion: (request, { signal }) => hostRequest('user-question/ask', request, signal),
    initializeTimeoutMs: 30000,
  });
  harness.client.start();
  harness.client.onRequest(async (method, params) => {
    if (method !== 'approval/request') throw new Error('Unsupported runtime request');
    const result = await hostRequest(method, params);
    return { outcome: result.outcome === 'allowed-once' ? 'allowed-once' : 'rejected' };
  });
  await harness.start();
  if (stopped) return;
  session = config.resume ? await resume(config.sessionId) : harness.session(config.sessionId);
  notifications = harness.client.subscribeSessionTree(session.id);
  notificationTask = (async () => {
    for await (const notification of notifications) send({ type: 'notification', ...notification });
  })();
  void notificationTask.catch(error => { if (!stopped) void fail(error); });
  // `sdk-minimal` carries no commands service or permission presets, so a
  // title run has nothing to enforce: the text would reach the model as a
  // prompt instead of routing to `/permission`.
  if (!title) {
    const permission = await session.run('/permission zeron-managed');
    if (permission.finishReason === 'error') throw new Error('Unable to enforce DeepSeek session permissions');
    // Plan mode is the run's, entered before the model ever sees a prompt:
    // the same slash path `session.run` gives `/permission`, so the runtime
    // persists the mode with the session rather than reading it as text.
    if (config.plan === true) {
      const plan = await session.run('/plan');
      if (plan.finishReason === 'error') throw new Error('Unable to enter DeepSeek plan mode');
    } else if (config.resume === true) {
      // Plan mode is stored with the session, so a runtime resumed by a run
      // whose toggle is off starts out still planning. Leave it at start, on
      // the same slash path: `/plan off` succeeds even when the stored session
      // was never in plan mode, so no previous state has to be guessed.
      const plan = await session.run('/plan off');
      if (plan.finishReason === 'error') throw new Error('Unable to leave DeepSeek plan mode');
    }
  }
  send({ type: 'ready', sessionId: session.id });
}

// The SDK reports a session its store no longer has as `session "<id>" not
// found`. The engine retries a rejected resume exactly once in a fresh
// session, and only when it can read the rejection, so surface that shape.
async function resume(id) {
  try {
    return await harness.resumeSession(id);
  } catch (error) {
    const message = String(error?.message ?? error);
    if (!/session\b[\s\S]*\bnot found/i.test(message)) throw error;
    throw new Error(`session not found: ${id} (${message})`);
  }
}

// A prompt may not type a slash command the run decides for itself. The
// runtime resolves its localized token to the same builtin as the ASCII name
// (`dsh-commands`' `builtin.ts`: `zh: { plan: '计划', permission: '权限' }`), so
// both forms are refused: a guard that knows only `/plan` lets `/计划` through.
function refusesCommand(prompt, names, message) {
  const alternatives = names.map((name) => name.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')).join('|');
  if (new RegExp(`^\\s*/(?:${alternatives})(?:\\s|$)`, 'u').test(prompt)) throw new Error(message);
}

async function prompt(command) {
  await startup;
  if (stopped) return;
  refusesCommand(command.prompt, ['permission', '权限'], 'Change permissions through Zeron access controls');
  // The same rule for plan: the run's flag is set once, at start, and a
  // prompt that reads the command would otherwise toggle the runtime's mode.
  refusesCommand(command.prompt, ['plan', '计划'], 'Plan mode is set by Zeron when a run starts');
  const input = [{ type: 'text', text: command.prompt }];
  for (const path of command.attachments ?? []) {
    const extension = path.split('.').at(-1).toLowerCase();
    const mimeType = { png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', webp: 'image/webp', gif: 'image/gif' }[extension];
    if (!mimeType) continue;
    if (launch.model !== 'deepseek-flash') throw new Error('This DeepSeek model does not support image input');
    const bytes = await readFile(path);
    if (bytes.length > 20 * 1024 * 1024) throw new Error('Image attachment exceeds 20 MiB');
    input.push({ type: 'image', data: bytes.toString('base64'), mimeType });
  }
  const result = await session.run(input);
  if (stopped) return;
  if (result.finishReason === null) throw new Error('SDK activity ended without a terminal outcome');
  await new Promise(resolveDrain => setImmediate(resolveDrain));
  // A native command's direct output stays in the UI and never enters model
  // history, so a state-only command (`/goal`, `/compact`) leaves no assistant
  // message for the transcript. Forward its text once, before the turn ends;
  // a command that started model activity already has that reply in the
  // assistant stream, so its `finalResponse` must not be repeated here.
  const ranCommand = result.events.some(event => event.type === 'command/run');
  const answered = result.events.some(event => event.type === 'assistant/message');
  if (ranCommand && !answered && result.finalResponse.trim() !== '') {
    send({ type: 'notice', text: result.finalResponse });
  }
  send({ type: 'done', sessionId: session.id, finishReason: result.finishReason, finalResponse: result.finalResponse });
}

async function close() {
  closing ??= (async () => {
    stopped = true;
    for (const finish of requests.values()) finish({ cancelled: true });
    notifications?.close();
    await notificationTask?.catch(() => {});
    if (harness) await harness.close();
    if (runtimeDirectory) await rm(runtimeDirectory, { recursive: true, force: true });
  })();
  return closing;
}

async function fail(error) {
  if (!stopped) send({ type: 'error', message: diagnostic(error) });
  try { await close(); } catch (cleanupError) { send({ type: 'error', message: diagnostic(cleanupError) }); }
  process.exitCode = 1;
  process.stdin.destroy();
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on('line', line => {
  try {
    const command = JSON.parse(line);
    if (command.type === 'answer') {
      requests.get(command.id)?.(command.result ?? { cancelled: true });
    } else if (command.type === 'stop') {
      void close().then(() => { send({ type: 'stopped' }); process.exit(0); }, fail);
    } else if (command.type === 'start') {
      if (startup) throw new Error('DeepSeek runtime was already started');
      launch = command;
      startup = start(command);
      void startup.catch(fail);
    } else if (command.type === 'prompt') {
      if (!startup) throw new Error('Initialize the DeepSeek SDK before prompting');
      queue = queue.then(() => prompt(command));
      void queue.catch(fail);
    } else throw new Error('Unknown DeepSeek sidecar command');
  } catch (error) { void fail(error); }
});
lines.on('close', () => { void close().catch(error => { send({ type: 'error', message: diagnostic(error) }); process.exitCode = 1; }); });
