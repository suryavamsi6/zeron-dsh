// Zeron's own ChatGPT subscription sign-in, as a DSH plugin.
//
// DSH's pi-ai adapter refuses this flow on purpose: `UNSERVED_OAUTH` holds
// `openai`, because pi-ai's Sign in with ChatGPT identifies the installation
// to OpenAI with a stable UUID that only the app can supply, and the adapter
// passes no `LoginOptions` to `models.login`. Nothing in DSH's configuration
// can change that, and `llm-pi-ai/openai` is already claimed by the API-key
// flow, so a plugin cannot register a flow on that key either.
//
// What a plugin *can* do is run pi-ai's login itself and let it persist the
// grant through the credential seam: `models.login` writes the record for you
// (`dist/models.js`), and the adapter reads whatever grant sits under
// `llm-pi-ai/openai` no matter who wrote it. So this plugin owns the
// installation id, runs the flow, and stores nothing of its own: no second
// credential file, no reading another tool's auth file, no token in a log.
//
// The callback listener binds 127.0.0.1:1455, which pi-ai hard-codes. Cancel
// aborts the flow, and pi-ai closes the server and its connections in a
// `finally`, so the port is released rather than left bound.

export const name = 'zeron-chatgpt';
export const inject = ['credentials', 'typert'];

/** pi-ai's provider id for Sign in with ChatGPT. */
const PROVIDER = 'openai';
/** The credential record the pi-ai adapter reads for that provider. */
const RECORD = 'llm-pi-ai/openai';
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

/** pi-ai's `CredentialStore` over DSH's credential records. */
function credentialStore(ctx) {
  const writable = () => {
    const credentials = ctx.get('credentials');
    if (credentials === undefined) throw new Error('Zeron: this composition mounts no credentials service, so a sign-in has nowhere to land');
    return credentials;
  };
  return {
    async read(providerId) {
      const credentials = ctx.get('credentials');
      if (credentials === undefined || providerId !== PROVIDER) return undefined;
      return toPiCredential(await credentials.readRecord(RECORD));
    },
    async list() {
      const stored = await ctx.get('credentials')?.listRecords() ?? [];
      return stored
        .filter(entry => entry.key === RECORD)
        .map(entry => ({ providerId: PROVIDER, type: entry.kind === 'api-key' ? 'api_key' : 'oauth' }));
    },
    async modify(providerId, mutate) {
      if (providerId !== PROVIDER) throw new Error(`Zeron: ${providerId} is not the ChatGPT route`);
      return toPiCredential(await writable().modifyRecord(RECORD, async current => {
        const next = await mutate(toPiCredential(current));
        return next === undefined ? undefined : toRecord(next);
      }));
    },
    async delete(providerId) {
      if (providerId !== PROVIDER) return;
      await writable().deleteRecord(RECORD);
    },
  };
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

/**
 * The sign-in service. The gateway resolves a Remote method on the service's
 * prototype and reads its `typertRemote` binding, so this is a class rather
 * than a plain object: `start`, `cancel` and `status` are the whole surface.
 */
class ZeronChatgpt {
  constructor(ctx, home, sdkRoot) {
    this.ctx = ctx;
    this.home = home;
    this.sdkRoot = sdkRoot;
    this.running = null;
    this.id = undefined;
    this.typertRemote = Object.freeze({
      service: this,
      serviceKey: 'zeronChatgpt',
      namespace: 'zeron-chatgpt',
    });
  }

  async status() {
    const stored = await this.ctx.get('credentials')?.readRecord(RECORD);
    return {
      authorized: stored !== undefined,
      running: this.running !== null,
      url: this.running?.url ?? null,
    };
  }

  async cancel() {
    if (this.running === null) return { cancelled: false };
    this.running.controller.abort();
    this.running = null;
    return { cancelled: true };
  }

  async start() {
    if (this.running !== null) {
      return { url: this.running.url, instructions: this.running.instructions };
    }
    this.id ??= await installationId(this.home);
    const { builtinModels } = await piAi(this.sdkRoot);
    const models = builtinModels({ credentials: credentialStore(this.ctx) });
    const controller = new AbortController();
    const flow = { controller, url: null, instructions: null };
    this.running = flow;
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
      prompt: () => new Promise((_, reject) => {
        if (controller.signal.aborted) return reject(new Error('cancelled'));
        controller.signal.addEventListener('abort', () => reject(new Error('cancelled')), { once: true });
      }),
    };
    const settled = models
      .login(PROVIDER, 'oauth', interaction, { getDeviceId: () => this.id, agentName: AGENT })
      .finally(() => { if (this.running === flow) this.running = null; });
    // The URL arrives from `notify` before the flow waits on the callback.
    for (let waited = 0; flow.url === null && waited < 5000; waited += 25) {
      await new Promise(done => setTimeout(done, 25));
      if (this.running !== flow) break;
    }
    if (flow.url === null) {
      controller.abort();
      this.running = null;
      await settled.catch(() => {});
      throw new Error('Zeron: the ChatGPT sign-in produced no authorize URL');
    }
    // A failure after this point is the flow's own; `status` reports it.
    void settled.catch(() => {});
    return { url: flow.url, instructions: flow.instructions };
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
  // for whatever `ctx.typert.local` holds, so these three are the whole surface.
  ctx.typert.register({
    package: 'zeron-chatgpt',
    face: 'host',
    schemas: [],
    invocations: [
      {
        id: 'zeron-chatgpt/start', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'start',
        invocation: { kind: 'direct' }, parameters: [], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/cancel', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'cancel',
        invocation: { kind: 'direct' }, parameters: [], result: { mode: 'src-json' },
      },
      {
        id: 'zeron-chatgpt/status', service: 'zeronChatgpt', namespace: 'zeron-chatgpt', method: 'status',
        invocation: { kind: 'direct' }, parameters: [], result: { mode: 'src-json' },
      },
    ],
  });
}
