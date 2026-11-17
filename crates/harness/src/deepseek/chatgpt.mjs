import { createAccountStore } from './chatgpt-accounts.mjs';
import { randomUUID } from 'node:crypto';

// Zeron's own ChatGPT subscription sign-in, as a DSH plugin.
//
// DSH's pi-ai adapter refuses this flow on purpose: `UNSERVED_OAUTH` holds
// `openai`, because pi-ai's Sign in with ChatGPT identifies the installation
// to OpenAI with a stable UUID that only the app can supply, and the adapter
// passes no `LoginOptions` to `models.login`. Nothing in DSH's configuration
// can change that, and `llm-pi-ai/openai` is already claimed by the API-key
// flow, so a plugin cannot register a flow on that key either.
//
// The plugin stages pi-ai's login in memory, saves accounts through DSH's
// credential service, and puts the selected grant at `llm-pi-ai/openai`,
// where the adapter reads it. No other tool's auth file or token log is used.
//
// The callback listener binds 127.0.0.1:1455, which pi-ai hard-codes. Cancel
// aborts the flow, and pi-ai closes the server and its connections in a
// `finally`, so the port is released rather than left bound.

export const name = 'zeron-chatgpt';
export const inject = ['credentials', 'typert'];

/** pi-ai's provider id for Sign in with ChatGPT. */
const PROVIDER = 'openai';
const CODEX = 'openai-codex';
function checkedProvider(provider = PROVIDER) {
  if (provider !== PROVIDER && provider !== CODEX) throw new Error('Unknown ChatGPT provider');
  return provider;
}
/** The credential record the pi-ai adapter reads for that provider. */
const recordFor = provider => `llm-pi-ai/${provider}`;
/** What OpenAI shows as the agent's name, and the Codex originator. */
const AGENT = 'Zeron';
/** Where Zeron keeps its own installation id, under the Harness home. */
const INSTALLATION_FILE = 'zeron-installation-id';

/** The JSON image of a grant payload: explicit `undefined` members are dropped. */
function jsonImage(value) {
  if (Array.isArray(value)) return value.map(entry => entry === undefined ? null : jsonImage(entry));
  if (typeof value === 'object' && value !== null && Object.getPrototypeOf(value) === Object.prototype) {
    const image = {};
    for (const [key, member] of Object.entries(value)) if (member !== undefined) image[key] = jsonImage(member);
    return image;
  }
  return value;
}

/** A stored record as the credential pi-ai expects. */
function toPiCredential(record) {
  if (record === undefined) return undefined;
  if (record.kind === 'api-key') {
    return {
      type: 'api_key',
      ...record.key === undefined ? {} : { key: record.key },
      ...record.env === undefined ? {} : { env: { ...record.env } },
    };
  }
  return record.payload;
}

/** A credential pi-ai produced as the record to store. */
function toRecord(credential) {
  if (credential.type === 'api_key') {
    return {
      kind: 'api-key',
      ...credential.key === undefined ? {} : { key: credential.key },
      ...credential.env === undefined ? {} : { env: { ...credential.env } },
    };
  }
  return { kind: 'grant', payload: jsonImage(credential) };
}

/** Optional display identity from a stored OAuth access token; never return the token. */
function displayIdentity(record) {
  const token = record?.kind === 'grant' ? record.payload?.access : undefined;
  if (typeof token !== 'string') return {};
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return {};
    const claims = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    const email = claims?.['https://api.openai.com/profile']?.email;
    const plan = claims?.['https://api.openai.com/auth']?.chatgpt_plan_type;
    return {
      ...typeof email === 'string' && email.length <= 254 && /^[^\s@]+@[^\s@]+$/.test(email) ? { email } : {},
      ...typeof plan === 'string' && /^[a-zA-Z0-9_-]{1,64}$/.test(plan) ? { plan } : {},
    };
  } catch {
    // A missing or malformed token does not affect authorization status.
    return {};
  }
}

/**
 * pi-ai, resolved from the SDK root this plugin runs beside rather than from
 * its own directory: the plugin is written into a runtime directory under the
 * Harness home, where no `node_modules` of its own exists.
 */
async function piAi(sdkRoot) {
  const { readFile } = await import('node:fs/promises');
  const { join } = await import('node:path');
  const { pathToFileURL } = await import('node:url');
  const directory = join(sdkRoot, 'node_modules/@earendil-works/pi-ai');
  const manifest = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8'));
  // pi-ai declares `import` conditions only, so `require.resolve` cannot reach
  // it; its own exports map says where the providers entry lives.
  const entry = manifest.exports?.['./providers/*']?.import;
  if (typeof entry !== 'string') throw new Error('Zeron: the installed pi-ai declares no providers export');
  return import(pathToFileURL(join(directory, entry.replace('*', 'all'))).href);
}

/** Zeron's own installation id: a UUID, made once, kept under the Harness home. */
async function installationId(home) {
  const { readFile, writeFile, mkdir } = await import('node:fs/promises');
  const { join } = await import('node:path');
  const { randomUUID } = await import('node:crypto');
  const path = join(home, INSTALLATION_FILE);
  try {
    const existing = (await readFile(path, 'utf8')).trim();
    if (/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(existing)) return existing;
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }
  const created = randomUUID();
  await mkdir(home, { recursive: true, mode: 0o700 });
  await writeFile(path, `${created}\n`, { mode: 0o600 });
  return created;
}

// Codex's account-scoped usage API. Never forward credentials or provider
// error bodies to the UI. Redirects are refused so the bearer cannot travel.
const USAGE_BASE = 'https://chatgpt.com/backend-api/wham';
const safeText = value => typeof value === 'string' && value.length > 0 && value.length <= 256 && !/[\r\n]/.test(value);

export async function accountIdForRecord(record) {
  const account = record?.kind === 'grant' ? record.payload?.accountId : undefined;
  if (!safeText(account)) return undefined;
  const { createHash } = await import('node:crypto');
  return createHash('sha256').update(account).digest('hex');
}

function auth(record) {
  const access = record?.kind === 'grant' ? record.payload?.access : undefined;
  const account = record?.payload?.accountId;
  return safeText(account) && typeof access === 'string' && access.length > 0 && !/[\r\n]/.test(access)
    ? { access, account } : undefined;
}

async function codexRequest(credentials, path, options = {}, fetcher = fetch) {
  const response = await fetcher(`${USAGE_BASE}/${path}`, {
    method: options.body ? 'POST' : 'GET', redirect: 'error',
    signal: AbortSignal.timeout(8000),
    headers: { authorization: `Bearer ${credentials.access}`, 'chatgpt-account-id': credentials.account,
      ...(options.body ? { 'content-type': 'application/json' } : {}) },
    ...(options.body ? { body: JSON.stringify(options.body) } : {}),
  });
  if (!response.ok) throw new Error('Codex request failed');
  const raw = await response.text();
  if (raw.length > 262144) throw new Error('Codex response too large');
  return JSON.parse(raw);
}

function windowFields(window, prefix) {
  const used = window?.used_percent;
  const reset = window?.reset_at;
  return {
    ...(typeof used === 'number' && Number.isFinite(used) && used >= 0 && used <= 100 ? { [prefix]: used / 100 } : {}),
    ...(typeof reset === 'number' && Number.isFinite(reset) && reset > 0 ? { [`${prefix}Resets`]: reset } : {}),
  };
}

export async function readAccount(record, fetcher = fetch) {
  const credentials = auth(record);
  if (!credentials) return undefined;
  const account = { id: await accountIdForRecord(record), active: true, ...displayIdentity(record) };
  try {
    const usage = await codexRequest(credentials, 'usage', {}, fetcher);
    if (usage.account_id !== undefined && usage.account_id !== credentials.account) throw new Error('Codex account mismatch');
    for (const window of [usage.rate_limit?.primary_window, usage.rate_limit?.secondary_window]) {
      const duration = window?.limit_window_seconds;
      if (typeof duration !== 'number' || !Number.isFinite(duration) || duration <= 0) continue;
      const label = duration >= 28 * 86400 ? 'month' : duration >= 5 * 86400 ? 'week' : 'session';
      Object.assign(account, windowFields(window, label));
    }
    if (safeText(usage.plan_type)) account.plan = usage.plan_type;
    const count = usage.rate_limit_reset_credits?.available_count;
    if (Number.isSafeInteger(count) && count >= 0) account.resetCount = count;
    if (count === 0) account.resets = [];
    else {
      try {
        const body = await codexRequest(credentials, 'rate-limit-reset-credits', {}, fetcher);
        if (Array.isArray(body.credits)) {
          account.resets = body.credits.filter(c => c?.status === 'available' && c.reset_type === 'codex_rate_limits' && safeText(c.id))
            .map(c => ({ id: c.id, ...(safeText(c.title) ? { title: c.title } : {}),
              ...(safeText(c.expires_at) ? { expires: c.expires_at } : {}) }));
        }
      } catch { /* count remains usable; the list is unavailable */ }
    }
  } catch { account.usageError = true; }
  return account;
}

async function redemptionId(account, creditId, requestId) {
  const { createHash } = await import('node:crypto');
  const bytes = createHash('sha256').update(JSON.stringify([account, creditId, requestId])).digest().subarray(0, 16);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = bytes.toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export async function redeemReset(record, requestedAccount, creditId, requestId, fetcher = fetch) {
  const credentials = auth(record);
  if (!credentials || await accountIdForRecord(record) !== requestedAccount || !safeText(creditId) || !safeText(requestId)) {
    throw new Error('Account or reset is unavailable');
  }
  try {
    const usage = await codexRequest(credentials, 'usage', {}, fetcher);
    if (usage.account_id !== credentials.account) throw new Error('account mismatch');
    const result = await codexRequest(credentials, 'rate-limit-reset-credits/consume',
      { body: { redeem_request_id: await redemptionId(requestedAccount, creditId, requestId), credit_id: creditId } }, fetcher);
    if (!safeText(result.code)) throw new Error('invalid result');
    return { outcome: result.code };
  } catch { throw new Error('Could not verify the account or use the reset'); }
}

/**
 * The sign-in service. The gateway resolves a Remote method on the service's
 * prototype and reads its `typertRemote` binding, so this is a class rather
 * than a plain object; its methods are registered as gateway remotes.
 */
class ZeronChatgpt {
  constructor(ctx, home, sdkRoot) {
    this.ctx = ctx;
    this.home = home;
    this.sdkRoot = sdkRoot;
    this.running = null;
    this.accountsStores = new Map([[PROVIDER, createAccountStore(ctx, PROVIDER)], [CODEX, createAccountStore(ctx, CODEX)]]);
    this.attempts = new Map();
    this.id = undefined;
    this.typertRemote = Object.freeze({
      service: this,
      serviceKey: 'zeronChatgpt',
      namespace: 'zeron-chatgpt',
    });
  }

  async status(provider) {
    provider = checkedProvider(provider);
    const stored = await this.ctx.get('credentials')?.readRecord(recordFor(provider));
    const running = this.running?.provider === provider ? this.running : null;
    const attempt = this.attempts.get(provider);
    return {
      provider,
      authorized: stored?.kind === 'grant' && stored.payload?.type === 'oauth',
      running: running !== null,
      ...(attempt ? { attemptId: attempt.id, outcome: attempt.outcome } : {}),
      url: running?.url ?? null,
      ...displayIdentity(stored),
    };
  }

  async accounts(provider) {
    provider = checkedProvider(provider);
    const saved = await this.accountsStores.get(provider).list();
    const accounts = await Promise.all(saved.map(async entry => {
      const usage = provider === CODEX ? await readAccount(entry.record) : undefined;
      return { ...displayIdentity(entry.record), ...usage, id: entry.id, active: entry.active };
    }));
    return { provider, accounts };
  }

  async useReset(accountId, creditId, idempotencyKey, provider) {
    provider = checkedProvider(provider);
    if (provider !== CODEX) throw new Error('Resets are available only for Codex accounts');
    const saved = await this.accountsStores.get(provider).list();
    const entry = saved.find(item => item.id === accountId);
    if (!entry) throw new Error('Account or reset is unavailable');
    return redeemReset(entry.record, await accountIdForRecord(entry.record), creditId, idempotencyKey);
  }

  async select(accountId, provider) {
    provider = checkedProvider(provider);
    await this.accountsStores.get(provider).select(accountId);
    return { selected: true };
  }

  async cancel(provider) {
    provider = checkedProvider(provider);
    if (this.running === null || this.running.provider !== provider) return { cancelled: false };
    this.running.controller.abort();
    this.running.outcome = 'cancelled';
    this.running = null;
    return { cancelled: true };
  }

  async start(provider) {
    provider = checkedProvider(provider);
    if (this.running !== null) {
      if (this.running.provider !== provider) throw new Error('Another sign-in is already running');
      return this.running.ready;
    }
    const controller = new AbortController();
    const flow = { id: randomUUID(), provider, controller, url: null, instructions: null, outcome: 'running' };
    this.attempts.set(provider, flow);
    this.running = flow;
    flow.ready = this.startFlow(flow).catch(error => {
      flow.outcome = controller.signal.aborted ? 'cancelled' : 'failed';
      if (this.running === flow) this.running = null;
      throw error;
    });
    return flow.ready;
  }

  async startFlow(flow) {
    const controller = flow.controller;
    const provider = flow.provider;
    this.id ??= await installationId(this.home);
    controller.signal.throwIfAborted();
    const { builtinModels } = await piAi(this.sdkRoot);
    controller.signal.throwIfAborted();
    // Login writes into memory until successful. The existing canonical grant
    // stays usable throughout the browser flow and on cancellation.
    let staged;
    const store = {
      async read(id) { return id === provider ? staged : undefined; },
      async list() { return staged ? [{ providerId: provider, type: 'oauth' }] : []; },
      async modify(id, mutate) {
        if (id !== provider) throw new Error('Invalid provider');
        staged = await mutate(staged);
        return staged;
      },
      async delete(id) { if (id === provider) staged = undefined; },
    };
    const models = builtinModels({ credentials: store });
    const interaction = {
      signal: controller.signal,
      notify(event) {
        if (event?.type === 'auth_url') {
          flow.url = event.url;
          flow.instructions = event.instructions ?? null;
        }
      },
      // The only prompt this flow asks is the pasted redirect URL, which the
      // loopback callback usually wins. Leaving it pending until cancel keeps
      // the race honest and settles the promise when the flow is abandoned.
      prompt: prompt => {
        if (provider === CODEX && prompt?.type === 'select' && Array.isArray(prompt.options)
            && prompt.options.some(option => option.id === 'browser')
            && prompt.options.some(option => option.id === 'device_code')) return Promise.resolve('browser');
        return new Promise((_, reject) => {
          if (controller.signal.aborted) return reject(new Error('cancelled'));
          controller.signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true });
        });
      },
    };
    const settled = models
      .login(provider, 'oauth', interaction, { getDeviceId: () => this.id, agentName: AGENT })
      .then(async () => { controller.signal.throwIfAborted(); await this.accountsStores.get(provider).add(toRecord(staged), controller.signal); flow.outcome = 'added'; })
      .catch(error => { flow.outcome = controller.signal.aborted ? 'cancelled' : 'failed'; throw error; })
      .finally(() => { if (this.running === flow) this.running = null; });
    // The URL arrives from `notify` before the flow waits on the callback.
    for (let waited = 0; flow.url === null && waited < 5000; waited += 25) {
      await new Promise(done => setTimeout(done, 25));
      if (this.running !== flow) break;
    }
    if (flow.url === null) {
      controller.abort();
      if (this.running === flow) this.running = null;
      await settled.catch(() => {});
      throw new Error('Zeron: the ChatGPT sign-in produced no authorize URL');
    }
    // A failure after this point is the flow's own; `status` reports it.
    void settled.catch(() => {});
    return { attemptId: flow.id, url: flow.url, instructions: flow.instructions };
  }
}

export function apply(ctx, config) {
  if (typeof config?.home !== 'string' || !config.home.startsWith('/')) {
    throw new Error('Zeron: the ChatGPT plugin needs an absolute Harness home');
  }
  if (typeof config?.sdkRoot !== 'string' || !config.sdkRoot.startsWith('/')) {
    throw new Error('Zeron: the ChatGPT plugin needs the absolute SDK root it runs beside');
  }
  ctx.provide('zeronChatgpt', new ZeronChatgpt(ctx, config.home, config.sdkRoot));
  // A hand-written Host contribution: the gateway claims `/api/<namespace>/<method>`
  // for whatever `ctx.typert.local` holds.
  ctx.typert.register({
    package: 'zeron-chatgpt',
    face: 'host',
    schemas: [],
    invocations: [
      {
        id: 'zeron-chatgpt/start', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'start',
        invocation: { kind: 'direct' }, parameters: [{ name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/cancel', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'cancel',
        invocation: { kind: 'direct' }, parameters: [{ name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/accounts', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'accounts',
        invocation: { kind: 'direct' }, parameters: [{ name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/select', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'select',
        invocation: { kind: 'direct' }, parameters: [{ name: 'accountId', wire: 'accountId', source: 'json', codec: { mode: 'src-json' } }, { name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/remove', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'remove',
        invocation: { kind: 'direct' }, parameters: [{ name: 'accountId', wire: 'accountId', source: 'json', codec: { mode: 'src-json' } }, { name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/useReset', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'useReset',
        invocation: { kind: 'direct' }, parameters: [
          { name: 'accountId', wire: 'accountId', source: 'json', codec: { mode: 'src-json' } }, { name: 'creditId', wire: 'creditId', source: 'json', codec: { mode: 'src-json' } }, { name: 'idempotencyKey', wire: 'idempotencyKey', source: 'json', codec: { mode: 'src-json' } }, { name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } },
        ], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/status', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'status',
        invocation: { kind: 'direct' }, parameters: [{ name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } }], result: { mode: 'src-json' },
      },
    ],
  });
}
