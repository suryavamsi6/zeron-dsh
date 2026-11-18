import test from 'node:test';
import assert from 'node:assert/strict';
import { createAccountStore } from './chatgpt-accounts.mjs';

const canonical = 'llm-pi-ai/openai';
const archive = 'zeron-chatgpt/accounts';
function grant(user, accessSuffix = '') {
  const claims = { 'https://api.openai.com/auth': { chatgpt_user_id: user } };
  return { kind: 'grant', payload: { type: 'oauth', accountId: 'workspace', clientId: user, access: `x.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.${accessSuffix}`, refresh: `refresh-${user}`, expires: 1 } };
}
function fixture(initial) {
  const data = new Map(initial ? [[canonical, initial]] : []);
  let fail = null;
  const credentials = {
    async readRecord(key) { return data.get(key); },
    async modifyRecord(key, mutate) {
      const next = await mutate(data.get(key));
      if (fail === key) { fail = null; throw new Error('synthetic write failure'); }
      if (next !== undefined) data.set(key, next);
      return next;
    },
  };
  return { data, store: createAccountStore({ get: () => credentials }), fail: key => { fail = key; } };
}

test('migrates legacy active grant, separates seats, switches and preserves refreshes', async () => {
  const a = grant('a'), b = grant('b');
  const f = fixture(a);
  const [legacy] = await f.store.list();
  assert.equal(legacy.active, true);
  await f.store.add(b);
  let accounts = await f.store.list();
  assert.equal(accounts.length, 2);
  assert.equal(accounts.find(x => x.id === legacy.id).active, false);
  assert.equal(accounts.find(x => x.active).record, b);
  const refreshed = grant('b', 'refreshed');
  f.data.set(canonical, refreshed);
  await f.store.select(legacy.id);
  accounts = await f.store.list();
  assert.deepEqual(f.data.get(canonical), a);
  assert.deepEqual(accounts.find(x => !x.active).record, refreshed);
  await f.store.select(accounts.find(x => !x.active).id);
  assert.deepEqual(f.data.get(canonical), refreshed);
});

test('write failures leave canonical grant intact and archived', async () => {
  const a = grant('a'), b = grant('b');
  for (const failedKey of [archive, canonical]) {
    const f = fixture(a);
    await f.store.list();
    f.fail(failedKey);
    await assert.rejects(f.store.add(b));
    assert.deepEqual(f.data.get(canonical), a);
    assert.deepEqual((await f.store.list()).find(x => x.active).record, a);
  }
});

test('cancel and API key do not replace canonical record', async () => {
  const a = grant('a'), controller = new AbortController();
  const f = fixture(a);
  controller.abort();
  await assert.rejects(f.store.add(grant('b'), controller.signal));
  assert.deepEqual(f.data.get(canonical), a);
  const key = { kind: 'api-key', key: 'synthetic' };
  const other = fixture(key);
  await assert.rejects(other.store.add(grant('b')));
  assert.deepEqual(other.data.get(canonical), key);
});

test('retries when another writer refreshes canonical during a switch', async () => {
  const a = grant('a'), fresh = grant('a', 'new'), b = grant('b');
  const data = new Map([[canonical, a]]);
  let race = true;
  const credentials = {
    async readRecord(key) { return data.get(key); },
    async modifyRecord(key, mutate) {
      if (key === canonical && race) { data.set(key, fresh); race = false; }
      const next = await mutate(data.get(key));
      if (next !== undefined) data.set(key, next);
      return next;
    },
  };
  const store = createAccountStore({ get: () => credentials });
  await store.add(b);
  assert.deepEqual(data.get(canonical), b);
  const prior = (await store.list()).find(x => !x.active);
  assert.deepEqual(prior.record, fresh);
  await store.select(prior.id);
  assert.deepEqual(data.get(canonical), fresh);
});

test('providers use isolated canonical credentials and archives', async () => {
  const data = new Map();
  const credentials = {
    async readRecord(key) { return data.get(key); },
    async modifyRecord(key, mutate) {
      const next = await mutate(data.get(key));
      if (next !== undefined) data.set(key, next);
      return next;
    },
  };
  const ctx = { get: () => credentials };
  const direct = createAccountStore(ctx, 'openai');
  const codex = createAccountStore(ctx, 'openai-codex');
  const a = grant('a'), b = grant('b');
  await direct.add(a);
  await codex.add(b);
  assert.deepEqual(data.get('llm-pi-ai/openai'), a);
  assert.deepEqual(data.get('llm-pi-ai/openai-codex'), b);
  assert.equal((await direct.list()).length, 1);
  assert.equal((await codex.list()).length, 1);
  assert.notEqual((await direct.list())[0].id, (await codex.list())[0].id);
  assert.throws(() => createAccountStore(ctx, 'other'));
});

test('Codex workspace without user identity does not conflate distinct grants', async () => {
  const records = new Map();
  const credentials = {
    async readRecord(key) { return records.get(key); },
    async modifyRecord(key, mutate) {
      const next = await mutate(records.get(key));
      if (next !== undefined) records.set(key, next);
      return next;
    },
  };
  const store = createAccountStore({ get: () => credentials }, 'openai-codex');
  const a = { kind: 'grant', payload: { type: 'oauth', accountId: 'shared', access: 'opaque-a' } };
  const b = { kind: 'grant', payload: { type: 'oauth', accountId: 'shared', access: 'opaque-b' } };
  await store.add(a);
  await store.add(b);
  assert.equal((await store.list()).length, 2);
});
