import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readAccount, redeemReset, accountIdForRecord } from '../../src/deepseek/chatgpt.mjs';

const record = { kind: 'grant', payload: { access: 'synthetic-token', accountId: 'synthetic-account' } };
const response = value => ({ ok: true, text: async () => JSON.stringify(value) });
const usage = { account_id: 'synthetic-account', plan_type: 'plus', rate_limit: {
  primary_window: { used_percent: 40, limit_window_seconds: 18000, reset_at: 123 },
  secondary_window: { used_percent: 7, limit_window_seconds: 604800, reset_at: 456 },
}, rate_limit_reset_credits: { available_count: 1 } };

test('reads account-scoped usage and eligible resets without exposing auth', async () => {
  const calls = [];
  const result = await readAccount(record, async (url, options) => {
    calls.push([url, options]);
    return response(url.endsWith('/usage') ? usage : { credits: [
      { id: 'one', status: 'available', reset_type: 'codex_rate_limits', expires_at: '2026-12-01T00:00:00Z' },
      { id: 'two', status: 'redeemed', reset_type: 'codex_rate_limits' },
    ] });
  });
  assert.equal(result.session, 0.4); assert.equal(result.week, 0.07);
  assert.equal(result.sessionResets, 123); assert.equal(result.resetCount, 1);
  assert.deepEqual(result.resets.map(c => c.id), ['one']);
  assert.equal(calls[0][1].redirect, 'error');
  assert.equal(calls[0][1].headers['chatgpt-account-id'], 'synthetic-account');
  assert(!JSON.stringify(result).includes('synthetic-token'));
});

test('classifies monthly and weekly windows by duration', async () => {
  const result = await readAccount(record, async () => response({
    rate_limit: { primary_window: { used_percent: 50, limit_window_seconds: 30 * 86400 } },
    rate_limit_reset_credits: { available_count: 0 } }));
  assert.equal(result.month, 0.5); assert.equal(result.session, undefined);
});

test('account mismatch and failed probes do not fabricate usage or credits', async () => {
  const result = await readAccount(record, async () => response({ ...usage, account_id: 'different' }));
  assert.equal(result.usageError, true); assert.equal(result.session, undefined); assert.equal(result.resetCount, undefined);
  assert.equal(await readAccount({ kind: 'api-key', key: 'test' }), undefined);
});

test('redemption verifies account and sends the idempotency key', async () => {
  const id = await accountIdForRecord(record); const calls = [];
  const result = await redeemReset(record, id, 'credit-1', 'request-1', async (url, options) => {
    calls.push([url, options]); return response(url.endsWith('/usage') ? usage : { code: 'reset' });
  });
  assert.deepEqual(result, { outcome: 'reset' });
  const body = JSON.parse(calls[1][1].body);
  assert.equal(body.credit_id, 'credit-1');
  assert.match(body.redeem_request_id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  const replay = [];
  await redeemReset(record, id, 'credit-1', 'request-1', async (url, options) => { replay.push(options); return response(url.endsWith('/usage') ? usage : { code: 'already_redeemed' }); });
  assert.equal(JSON.parse(replay[1].body).redeem_request_id, body.redeem_request_id);
  let sent = false;
  await assert.rejects(redeemReset(record, id, 'credit-1', 'request-1', async (url) => {
    if (!url.endsWith('/usage')) sent = true;
    return response({ account_id: 'different' });
  }));
  assert.equal(sent, false);
  await assert.rejects(redeemReset(record, 'other', 'credit-1', 'request-1', async () => { throw Error('should not call'); }));
});

test('provider accounts stay separate and direct grants do not need Codex metadata', async () => {
  const { apply } = await import('../../src/deepseek/chatgpt.mjs');
  const entries = new Map([
    ['llm-pi-ai/openai', { kind: 'grant', payload: { type: 'oauth', clientId: 'synthetic-client', access: 'opaque' } }],
  ]);
  let service;
  const credentials = {
    readRecord: async key => entries.get(key),
    modifyRecord: async (key, fn) => { const next = await fn(entries.get(key)); if (next !== undefined) entries.set(key, next); return entries.get(key); },
  };
  const ctx = { get: () => credentials, provide: (_, value) => { service = value; }, typert: { register() {} } };
  apply(ctx, { home: '/tmp/synthetic', sdkRoot: '/tmp/synthetic' });
  const direct = await service.accounts('openai');
  assert.equal(direct.accounts.length, 1); assert.equal(direct.accounts[0].active, true);
  assert.equal(direct.accounts[0].session, undefined);
  assert.equal((await service.status('openai')).authorized, true);
  assert.equal((await service.status('openai-codex')).authorized, false);
  assert.deepEqual((await service.accounts('openai-codex')).accounts, []);
  await assert.rejects(service.useReset(direct.accounts[0].id, 'credit', 'key', 'openai'));
  await assert.rejects(service.status('unknown'));
});
