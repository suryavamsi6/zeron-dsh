// DSH owns the credential document and its cross-process lock. Keep inactive
// grants in one DSH record; pi-ai continues to use the canonical record.
import { randomUUID } from 'node:crypto';

function identity(record, provider) {
  if (record?.kind !== 'grant') return null;
  const p = record.payload;
  if (provider === 'openai') {
    return typeof p?.clientId === 'string' && p.clientId ? JSON.stringify([p.clientId]) : null;
  }
  if (typeof p?.accountId !== 'string' || !p.accountId) return null;
  let user = '';
  try {
    const claims = JSON.parse(Buffer.from(p.access.split('.')[1], 'base64url').toString('utf8'));
    const auth = claims['https://api.openai.com/auth'];
    const profile = claims['https://api.openai.com/profile'];
    user = auth?.chatgpt_user_id || auth?.user_id || profile?.email || claims.email || claims.sub || '';
    if (typeof user !== 'string') user = '';
  } catch { /* Older grants may have no readable token. */ }
  return user ? JSON.stringify([p.accountId, user]) : null;
}
function sameAccount(a, b, provider) {
  const x = identity(a, provider), y = identity(b, provider);
  return x !== null && x === y || JSON.stringify(a) === JSON.stringify(b);
}
function entries(record) {
  if (record === undefined) return [];
  if (record.kind !== 'grant' || record.payload?.version !== 1 || !Array.isArray(record.payload.accounts)) {
    throw new Error('Zeron: invalid ChatGPT account archive');
  }
  return record.payload.accounts;
}
function isGrant(record) { return record?.kind === 'grant' && record.payload?.type === 'oauth'; }

export function createAccountStore(ctx, provider = 'openai') {
  if (provider !== 'openai' && provider !== 'openai-codex') throw new Error('Zeron: unsupported ChatGPT provider');
  const CANONICAL = `llm-pi-ai/${provider}`;
  const ARCHIVE = provider === 'openai' ? 'zeron-chatgpt/accounts' : 'zeron-chatgpt/openai-codex-accounts';
  const credentials = () => {
    const value = ctx.get('credentials');
    if (!value) throw new Error('Zeron: credentials service is unavailable');
    return value;
  };
  let queue = Promise.resolve();
  function serial(fn) {
    const result = queue.then(fn, fn);
    queue = result.catch(() => {});
    return result;
  }
  // No writes are nested: modifyRecord holds the document-wide lock while
  // calling its mutator. Each archive write merges under that same lock.
  async function save(record) {
    let id;
    await credentials().modifyRecord(ARCHIVE, current => {
      const accounts = entries(current).slice();
      let index = accounts.findIndex(item => sameAccount(item.record, record, provider));
      id = index < 0 ? randomUUID() : accounts[index].id;
      const item = { id, record };
      if (index < 0) accounts.push(item); else accounts[index] = item;
      return { kind: 'grant', payload: { version: 1, accounts } };
    });
    return id;
  }
  async function listInternal() {
    const current = await credentials().readRecord(CANONICAL);
    let accounts = entries(await credentials().readRecord(ARCHIVE));
    if (isGrant(current) && !accounts.some(item => sameAccount(item.record, current, provider))) {
      await save(current);
      accounts = entries(await credentials().readRecord(ARCHIVE));
    }
    return accounts.map(item => ({ id: item.id, active: isGrant(current) && sameAccount(item.record, current, provider), record: isGrant(current) && sameAccount(item.record, current, provider) ? current : item.record }));
  }
  async function install(target, signal) {
    for (let tries = 0; tries < 8; tries++) {
      signal?.throwIfAborted();
      const before = await credentials().readRecord(CANONICAL);
      if (before !== undefined && !isGrant(before)) throw new Error('Zeron: the OpenAI credential is not a ChatGPT grant');
      if (isGrant(before)) await save(before);
      const id = await save(target);
      signal?.throwIfAborted();
      let changed = false;
      await credentials().modifyRecord(CANONICAL, current => {
        signal?.throwIfAborted();
        if (JSON.stringify(current) !== JSON.stringify(before)) return undefined;
        changed = true;
        return target;
      });
      if (changed) return id;
    }
    throw new Error('Zeron: the active ChatGPT credential changed repeatedly; please retry');
  }
  return {
    list: () => serial(listInternal),
    add: (record, signal) => serial(async () => {
      if (!isGrant(record)) throw new Error('Zeron: sign-in did not produce a ChatGPT grant');
      return install(record, signal);
    }),
    select: id => serial(async () => {
      if (typeof id !== 'string') throw new Error('Zeron: invalid ChatGPT account id');
      const accounts = await listInternal();
      const account = accounts.find(item => item.id === id);
      if (!account) throw new Error('Zeron: ChatGPT account was not found');
      if (!account.active) await install(account.record);
    }),
    remove: id => serial(async () => {
      if (typeof id !== 'string') throw new Error('Zeron: invalid ChatGPT account id');
      const accounts = await listInternal();
      const account = accounts.find(item => item.id === id);
      if (!account) throw new Error('Zeron: ChatGPT account was not found');
      const current = await credentials().readRecord(CANONICAL);
      const active = isGrant(current) && sameAccount(current, account.record, provider);
      const remaining = accounts.filter(item => item.id !== id);
      if (active) {
        if (remaining.length > 0) {
          await credentials().modifyRecord(CANONICAL, value => {
            if (JSON.stringify(value) !== JSON.stringify(current)) throw new Error('Zeron: the active ChatGPT account changed; try again');
            return remaining[0].record;
          });
        } else {
          await credentials().deleteRecord(CANONICAL);
        }
      }
      if (remaining.length > 0) {
        await credentials().modifyRecord(ARCHIVE, () => ({ kind: 'grant', payload: { version: 1, accounts: remaining } }));
      } else {
        await credentials().deleteRecord(ARCHIVE);
      }
      return { removed: true, active, remaining: remaining.length };
    }),
  };
}
