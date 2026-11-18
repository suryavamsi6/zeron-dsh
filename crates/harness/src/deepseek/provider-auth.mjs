import { createHash, randomBytes, randomUUID } from 'node:crypto';

export const name = 'zeron-provider-auth';
export const inject = ['authorization', 'credentials', 'typert'];

const KEY_SCOPE = 'llm-pi-ai';
const MISTRAL_API = 'https://api.mistral.ai/v1';
const safeProvider = value => typeof value === 'string' && value.length <= 64 && /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value);
const keyFor = provider => `${KEY_SCOPE}/${provider}`;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const MAX_TEXT = 2048;

function safeLabel(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 254 && !/[\r\n\0]/.test(value)
    ? value : undefined;
}

function identityFromToken(token) {
  if (typeof token !== 'string' || token.length > 16384) return {};
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return {};
    const claims = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    const profile = claims?.profile ?? claims?.user ?? {};
    const email = safeLabel(claims?.email ?? profile.email);
    const name = safeLabel(claims?.name ?? claims?.preferred_username ?? profile.name ?? profile.username);
    const plan = safeLabel(claims?.plan ?? claims?.plan_type ?? claims?.tier ?? claims?.subscription?.plan);
    return {
      ...(email && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) ? { email } : {}),
      ...(name ? { accountName: name } : {}),
      ...(plan ? { plan: plan.slice(0, 64) } : {}),
    };
  } catch {
    return {};
  }
}

function displayIdentity(record) {
  if (!record || typeof record !== 'object') return {};
  const payload = record.payload;
  if (!payload || typeof payload !== 'object') {
    const identity = record.identity;
    return identity && typeof identity === 'object' ? {
      ...(safeLabel(identity.email) && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(identity.email) ? { email: identity.email } : {}),
      ...(safeLabel(identity.accountName) ? { accountName: identity.accountName } : {}),
      ...(safeLabel(identity.plan) ? { plan: identity.plan.slice(0, 64) } : {}),
    } : {};
  }
  const profile = payload.profile ?? payload.user ?? {};
  const email = safeLabel(payload.email ?? profile.email);
  const accountName = safeLabel(payload.accountName ?? payload.name ?? payload.username ?? profile.name ?? profile.username);
  const plan = safeLabel(payload.plan ?? payload.plan_type ?? payload.tier ?? profile.plan);
  return {
    ...(email && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) ? { email } : {}),
    ...(accountName ? { accountName } : {}),
    ...(plan ? { plan: plan.slice(0, 64) } : {}),
    ...identityFromToken(payload.id_token ?? payload.idToken),
    ...identityFromToken(payload.access ?? payload.access_token ?? payload.accessToken),
  };
}

function accountKey(record) {
  const identity = displayIdentity(record);
  if (identity.email) return `email:${identity.email.toLowerCase()}`;
  if (identity.accountName) return `name:${identity.accountName.toLowerCase()}`;
  const payload = record?.payload;
  const id = payload?.accountId ?? payload?.userId ?? payload?.user_id;
  if (typeof id === 'string' && id) return `id:${id}`;
  return JSON.stringify(record);
}

function archiveEntries(record, provider) {
  if (record === undefined) return [];
  if (record.kind !== 'grant' || record.payload?.version !== 1 || !Array.isArray(record.payload.accounts)) {
    throw new Error(`Zeron: invalid ${provider} account archive`);
  }
  return record.payload.accounts;
}

function configuredRecord(record) {
  return record?.kind === 'grant' && record.payload?.type === 'oauth'
    || record?.kind === 'api-key' && typeof record.key === 'string' && record.key.length > 0;
}

function safeNotice(notice) {
  return {
    message: typeof notice?.message === 'string' ? notice.message.slice(0, MAX_TEXT) : '',
    ...(typeof notice?.url === 'string' && notice.url.length <= MAX_TEXT ? { url: notice.url } : {}),
    ...(typeof notice?.code === 'string' && notice.code.length <= 128 ? { code: notice.code } : {}),
  };
}

function safePrompt(prompt, id) {
  if (!prompt || !['text', 'secret', 'select'].includes(prompt.kind)) return undefined;
  return {
    id,
    kind: prompt.kind,
    message: typeof prompt.message === 'string' ? prompt.message.slice(0, MAX_TEXT) : '',
    ...(typeof prompt.placeholder === 'string' ? { placeholder: prompt.placeholder.slice(0, 256) } : {}),
    ...(prompt.kind === 'select' && Array.isArray(prompt.options)
      ? { options: prompt.options.slice(0, 64).map(option => ({
          id: String(option.id).slice(0, 256), label: String(option.label).slice(0, 256),
          ...(typeof option.description === 'string' ? { description: option.description.slice(0, 512) } : {}),
        })) } : {}),
  };
}

// Mistral Vibe's supported browser sign-in is a PKCE exchange hosted by the
// Mistral console. It returns an API key, which is committed using the normal
// pi-ai credential record so DSH continues to own model requests.
async function mistralVibe(session) {
  const origin = 'https://console.mistral.ai';
  const verifier = randomBytes(32).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const request = async (url, init = {}) => {
    const response = await fetch(url, {
      ...init, redirect: 'error', signal: session.signal,
      headers: { ...(init.body ? { 'content-type': 'application/json' } : {}), ...init.headers },
    });
    if (!response.ok) throw new Error(`Mistral sign-in failed (${response.status})`);
    const text = await response.text();
    if (text.length > 65536) throw new Error('Mistral sign-in response was too large');
    return text ? JSON.parse(text) : {};
  };
  const started = await request(`${origin}/api/vibe/sign-in`, {
    method: 'POST', body: JSON.stringify({ code_challenge: challenge, code_challenge_method: 'S256' }),
  });
  const processId = started?.process_id;
  const signInUrl = started?.sign_in_url;
  const pollUrl = started?.poll_url;
  const expiresAt = Date.parse(started?.expires_at);
  const checkedUrl = (value, pathAllowed) => {
    if (typeof value !== 'string' || value.length > 2048) throw new Error('Mistral returned an invalid sign-in URL');
    const parsed = new URL(value, origin);
    if (parsed.protocol !== 'https:' || parsed.origin !== origin || !parsed.pathname.startsWith(pathAllowed)) {
      throw new Error('Mistral returned an untrusted sign-in URL');
    }
    return parsed.href;
  };
  const safePage = checkedUrl(signInUrl, '/');
  const safePoll = checkedUrl(pollUrl, '/api/vibe/');
  if (typeof processId !== 'string' || !/^[a-zA-Z0-9_-]{1,256}$/.test(processId)
      || !Number.isFinite(expiresAt)) throw new Error('Mistral returned an invalid sign-in session');
  session.notify({ message: 'Continue in your browser to sign in to Mistral Vibe.', url: safePage });
  while (!session.signal.aborted && Date.now() < expiresAt) {
    await sleep(2500);
    const polled = await request(safePoll);
    if (polled?.status === 'pending') continue;
    if (polled?.status === 'expired' || polled?.status === 'denied') throw new Error('Mistral sign-in was not completed');
    const exchangeToken = polled?.exchange_token;
    if (polled?.status !== 'completed' || typeof exchangeToken !== 'string' || exchangeToken.length > 4096) {
      throw new Error('Mistral returned an invalid sign-in response');
    }
    const exchanged = await request(`${origin}/api/vibe/sign-in/${encodeURIComponent(processId)}/exchange`, {
      method: 'POST', body: JSON.stringify({ exchange_token: exchangeToken, code_verifier: verifier }),
    });
    const apiKey = exchanged?.api_key;
    if (typeof apiKey !== 'string' || apiKey.length < 8 || apiKey.length > 4096 || /[\r\n]/.test(apiKey)) {
      throw new Error('Mistral did not return an API key');
    }
    await session.commit({ kind: 'api-key', key: apiKey });
    return;
  }
  session.signal.throwIfAborted();
  throw new Error('Mistral sign-in expired');
}

class ProviderAuth {
  constructor(ctx) {
    this.authorization = ctx.authorization;
    this.credentials = ctx.credentials;
    this.attempts = new Map();
    this.queues = new Map();
    this.detailsCache = new Map();
    this.typertRemote = Object.freeze({
      service: this,
      serviceKey: 'zeronProviderAuth',
      namespace: 'zeron-provider-auth',
    });
  }

  enqueue(provider, operation) {
    const previous = this.queues.get(provider) ?? Promise.resolve();
    const result = previous.then(operation, operation);
    this.queues.set(provider, result.catch(() => {}));
    return result;
  }

  async accountIdentity(provider, record) {
    if (provider === 'mistral') {
      const details = await this.mistralDetails(record);
      if (typeof details.email === 'string') return `email:${details.email.toLowerCase()}`;
    }
    return accountKey(record);
  }

  async saveAccount(provider, record) {
    // DSH credential refs have exactly the shape "scope/id". Keep provider
    // archives in one record per provider within that namespace.
    const archiveKey = `zeron-provider-auth/${provider}-accounts`;
    const identity = await this.accountIdentity(provider, record);
    let id;
    await this.credentials.modifyRecord(archiveKey, current => {
      const accounts = archiveEntries(current, provider).slice();
      const index = accounts.findIndex(item => item.identity === identity || accountKey(item.record) === accountKey(record));
      id = index < 0 ? randomUUID() : accounts[index].id;
      const item = { id, identity, record };
      if (index < 0) accounts.push(item); else accounts[index] = item;
      return { kind: 'grant', payload: { version: 1, accounts } };
    });
    return id;
  }

  async archiveCurrent(provider) {
    const record = await this.credentials.readRecord(keyFor(provider));
    if (configuredRecord(record)) await this.saveAccount(provider, record);
  }

  async accounts(provider) {
    return this.enqueue(provider, async () => {
      await this.archiveCurrent(provider);
      const current = await this.credentials.readRecord(keyFor(provider));
      const currentIdentity = configuredRecord(current) ? await this.accountIdentity(provider, current) : undefined;
      const archive = await this.credentials.readRecord(`zeron-provider-auth/${provider}-accounts`);
      const resolved = await Promise.all(archiveEntries(archive, provider).map(async entry => {
        const details = provider === 'mistral' ? await this.mistralDetails(entry.record) : {};
        const email = details.email ?? displayIdentity(entry.record).email;
        const identity = email ? `email:${email.toLowerCase()}` : await this.accountIdentity(provider, entry.record);
        return { entry, details, identity, email, current: configuredRecord(current) && accountKey(current) === accountKey(entry.record) };
      }));
      const unique = [];
      for (const item of resolved) {
        const key = item.email ? `email:${item.email.toLowerCase()}` : item.identity;
        const index = unique.findIndex(existing => (existing.email
          ? `email:${existing.email.toLowerCase()}` : existing.identity) === key);
        if (index < 0) unique.push(item);
        else if (item.current && !unique[index].current) unique[index] = item;
      }
      const active = unique.find(item => item.current)
        ?? unique.find(item => item.identity === currentIdentity);
      const normalized = unique.map(item => ({ ...item.entry, identity: item.identity }));
      const existing = archiveEntries(archive, provider);
      if (normalized.length !== existing.length || normalized.some((entry, index) => entry.identity !== existing[index]?.identity)) {
        await this.credentials.modifyRecord(`zeron-provider-auth/${provider}-accounts`, () => ({
          kind: 'grant', payload: { version: 1, accounts: normalized },
        }));
      }
      return unique.map(item => {
        const isActive = active?.entry.id === item.entry.id;
        const record = isActive ? current : item.entry.record;
        return {
          id: item.entry.id, active: isActive,
          ...displayIdentity(record), ...item.details,
        };
      });
    });
  }

  async mistralDetails(record) {
    if (record?.kind !== 'api-key' || typeof record.key !== 'string' || record.key.length < 8) return {};
    const key = record.key;
    const cacheKey = createHash('sha256').update(key).digest('hex');
    const cached = this.detailsCache.get(cacheKey);
    if (cached && Date.now() - cached.at < 30000) return cached.value;
    const get = async path => {
      try {
        const response = await fetch(`${MISTRAL_API}${path}`, {
          headers: { authorization: `Bearer ${key}` },
          redirect: 'error', signal: AbortSignal.timeout(3500),
        });
        if (!response.ok) return null;
        const body = await response.text();
        if (body.length > 32768) return null;
        return body ? JSON.parse(body) : null;
      } catch {
        return null;
      }
    };
    const [who, subscription, budgetResponse] = await Promise.all([
      get('/users/me'), get('/billing/subscription'), get('/billing/budget'),
    ]);
    const email = safeLabel(who?.email);
    const first = safeLabel(who?.first_name);
    const last = safeLabel(who?.last_name);
    const vibe = budgetResponse?.vibe_budget;
    const percentage = vibe?.usage_percentage;
    const currency = safeLabel(vibe?.currency ?? budgetResponse?.currency);
    const value = {
      ...(email && /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) ? { email } : {}),
      ...(first || last ? { accountName: [first, last].filter(Boolean).join(' ') } : {}),
      ...(safeLabel(subscription?.plan_name) ? { plan: subscription.plan_name.slice(0, 64) } : {}),
      ...(typeof percentage === 'number' && Number.isFinite(percentage)
          ? { vibe_usage: Math.max(0, Math.min(100, percentage)) / 100 } : {}),
      ...(typeof vibe?.initial_budget === 'number' && Number.isFinite(vibe.initial_budget)
          ? { vibe_budget: vibe.initial_budget } : {}),
      ...(currency ? { currency: currency.slice(0, 8) } : {}),
      ...(typeof vibe?.reset_at === 'string' && vibe.reset_at.length <= 64 ? { reset_at: vibe.reset_at } : {}),
    };
    this.detailsCache.set(cacheKey, { at: Date.now(), value });
    return value;
  }

  async status(provider) {
    if (!safeProvider(provider)) throw new Error('Unknown provider');
    const key = keyFor(provider);
    const entry = this.authorization.describe(key);
    const info = await this.credentials.describeRecord(key);
    const record = await this.credentials.readRecord(key);
    const attempt = this.attempts.get(provider);
    const accounts = await this.accounts(provider);
    const active = accounts.find(account => account.active === true);
    return {
      provider, supported: !!entry,
      authorized: info.configured === true,
      ...displayIdentity(record),
      ...(active ? {
        ...active.email ? { email: active.email } : {},
        ...active.accountName ? { accountName: active.accountName } : {},
        ...active.plan ? { plan: active.plan } : {},
      } : {}),
      accounts,
      running: attempt?.running === true || entry?.inFlight === true,
      methods: entry?.methods ?? [],
      ...(attempt?.notice ? { notice: attempt.notice } : {}),
      ...(attempt?.prompt ? { prompt: attempt.prompt } : {}),
      ...(attempt?.outcome ? { outcome: attempt.outcome } : {}),
      ...(attempt?.error ? { error: attempt.error } : {}),
    };
  }

  async start(provider, method) {
    if (!safeProvider(provider)) throw new Error('Unknown provider');
    const key = keyFor(provider);
    const flow = this.authorization.describe(key);
    const existing = this.attempts.get(provider);
    if (existing?.running || flow?.inFlight) return { started: true, alreadyRunning: true };
    if (provider !== 'mistral' && !flow) throw new Error('This provider does not offer sign-in here');
    await this.enqueue(provider, () => this.archiveCurrent(provider));
    if (this.attempts.get(provider)?.running || this.authorization.describe(key)?.inFlight) return { started: true, alreadyRunning: true };
    const controller = new AbortController();
    const attempt = { id: randomUUID(), running: true, notice: undefined, prompt: undefined, resolvePrompt: undefined, controller };
    this.attempts.set(provider, attempt);
    if (provider === 'mistral') {
      void mistralVibe({
        signal: controller.signal,
        notify: notice => { if (this.attempts.get(provider) === attempt) attempt.notice = safeNotice(notice); },
        commit: async record => {
          controller.signal.throwIfAborted();
          await this.credentials.modifyRecord(key, async () => record);
          controller.signal.throwIfAborted();
        },
      }).then(() => {
        void this.enqueue(provider, () => this.archiveCurrent(provider));
        attempt.outcome = 'authorized';
        attempt.running = false;
      }).catch(() => {
        attempt.running = false;
        attempt.outcome = controller.signal.aborted ? 'cancelled' : 'failed';
        if (!controller.signal.aborted) attempt.error = 'Sign-in failed. Try again.';
      });
      return { started: true, attemptId: attempt.id };
    }
    void this.authorization.begin({
      key,
      ...(typeof method === 'string' && flow.methods.some(item => item.id === method) ? { method } : {}),
      interaction: {
        notify: notice => { if (this.attempts.get(provider) === attempt) attempt.notice = safeNotice(notice); },
        prompt: async prompt => {
          const id = randomUUID();
          const visible = safePrompt(prompt, id);
          if (!visible) throw new Error('Unsupported sign-in prompt');
          attempt.prompt = visible;
          return await new Promise((resolve, reject) => {
            attempt.resolvePrompt = { id, resolve, reject };
            if (prompt.signal) prompt.signal.addEventListener('abort', () => {
              if (attempt.resolvePrompt?.id === id) {
                attempt.resolvePrompt = undefined;
                attempt.prompt = undefined;
                reject(new Error('The sign-in prompt was withdrawn'));
              }
            }, { once: true });
          });
        },
      },
    }).then(result => {
      void this.enqueue(provider, () => this.archiveCurrent(provider));
      attempt.outcome = result.status;
      attempt.running = false;
    }).catch(() => {
      attempt.running = false;
      attempt.outcome = 'failed';
      attempt.error = 'Sign-in failed. Try again.';
    });
    return { started: true, attemptId: attempt.id };
  }

  cancel(provider) {
    if (!safeProvider(provider)) throw new Error('Unknown provider');
    const attempt = this.attempts.get(provider);
    if (attempt?.resolvePrompt) {
      attempt.resolvePrompt.reject(new Error('Sign-in cancelled'));
      attempt.resolvePrompt = undefined;
      attempt.prompt = undefined;
    }
    attempt?.controller?.abort();
    this.authorization.cancel(keyFor(provider));
    return { cancelled: true };
  }

  async select(provider, accountId) {
    if (!safeProvider(provider) || typeof accountId !== 'string') throw new Error('Invalid provider account');
    return this.enqueue(provider, async () => {
      await this.archiveCurrent(provider);
      const current = await this.credentials.readRecord(keyFor(provider));
      const archive = await this.credentials.readRecord(`zeron-provider-auth/${provider}-accounts`);
      const selected = archiveEntries(archive, provider).find(item => item.id === accountId);
      if (!selected) throw new Error('Provider account was not found');
      const currentIdentity = configuredRecord(current) ? await this.accountIdentity(provider, current) : undefined;
      const selectedIdentity = selected.identity ?? await this.accountIdentity(provider, selected.record);
      if (currentIdentity !== selectedIdentity && accountKey(current) !== accountKey(selected.record)) {
        await this.credentials.modifyRecord(keyFor(provider), () => selected.record);
      }
      return { selected: true };
    });
  }

  async remove(provider, accountId) {
    if (!safeProvider(provider) || typeof accountId !== 'string') throw new Error('Invalid provider account');
    return this.enqueue(provider, async () => {
      await this.archiveCurrent(provider);
      const archiveKey = `zeron-provider-auth/${provider}-accounts`;
      const current = await this.credentials.readRecord(keyFor(provider));
      const archive = await this.credentials.readRecord(archiveKey);
      const accounts = archiveEntries(archive, provider);
      const selected = accounts.find(item => item.id === accountId);
      if (!selected) throw new Error('Provider account was not found');
      const currentIdentity = configuredRecord(current) ? await this.accountIdentity(provider, current) : undefined;
      const selectedIdentity = await this.accountIdentity(provider, selected.record);
      const active = configuredRecord(current) && (accountKey(current) === accountKey(selected.record)
        || currentIdentity === selectedIdentity);
      const remaining = accounts.filter(item => item.id !== accountId);
      if (active) {
        if (remaining.length > 0) {
          const next = remaining[0].record;
          await this.credentials.modifyRecord(keyFor(provider), value => {
            if (JSON.stringify(value) !== JSON.stringify(current)) throw new Error('The active provider account changed; try again');
            return next;
          });
        } else {
          await this.credentials.deleteRecord(keyFor(provider));
        }
      }
      if (remaining.length > 0) {
        await this.credentials.modifyRecord(archiveKey, () => ({
          kind: 'grant', payload: { version: 1, accounts: remaining },
        }));
      } else {
        await this.credentials.deleteRecord(archiveKey);
      }
      return { removed: true, active, remaining: remaining.length };
    });
  }

  respond(provider, promptId, value) {
    if (!safeProvider(provider) || typeof promptId !== 'string' || typeof value !== 'string' || value.length > 4096) {
      throw new Error('Invalid sign-in response');
    }
    const attempt = this.attempts.get(provider);
    if (!attempt || attempt.resolvePrompt?.id !== promptId) throw new Error('That sign-in prompt is no longer active');
    const pending = attempt.resolvePrompt;
    attempt.resolvePrompt = undefined;
    attempt.prompt = undefined;
    pending.resolve(value);
    return { submitted: true };
  }
}

export function apply(ctx) {
  const service = new ProviderAuth(ctx);
  ctx.provide('zeronProviderAuth', service);
  ctx.typert.register({
    package: 'zeron-provider-auth', face: 'host', schemas: [],
    invocations: ['status', 'start', 'cancel', 'respond', 'select', 'remove'].map(method => ({
      id: `zeron-provider-auth/${method}`, service: 'zeronProviderAuth',
      namespace: 'zeron-provider-auth', method, invocation: { kind: 'direct' },
      parameters: [
        { name: 'provider', wire: 'provider', source: 'json', codec: { mode: 'src-json' } },
        ...(method === 'start' ? [{ name: 'method', wire: 'method', source: 'json', codec: { mode: 'src-json' } }] : []),
        ...(method === 'select' ? [{ name: 'accountId', wire: 'accountId', source: 'json', codec: { mode: 'src-json' } }] : []),
        ...(method === 'remove' ? [{ name: 'accountId', wire: 'accountId', source: 'json', codec: { mode: 'src-json' } }] : []),
        ...(method === 'respond' ? [
          { name: 'promptId', wire: 'promptId', source: 'json', codec: { mode: 'src-json' } },
          { name: 'value', wire: 'value', source: 'json', codec: { mode: 'src-json' } },
        ] : []),
      ], result: { mode: 'src-json' },
    })),
  });
}
