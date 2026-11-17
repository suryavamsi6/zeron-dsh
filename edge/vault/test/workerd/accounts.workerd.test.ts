import { env } from "cloudflare:workers";
import { runInDurableObject } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VaultProviderId } from "../../src/api";
import { accountStub } from "../../src/names";
import { runnerCaller, setup, signedGrant, value, vault } from "./helpers";

afterEach(() => vi.restoreAllMocks());

const PROVIDER = "deepseek-key";
const put = async (ctx: Awaited<ReturnType<typeof setup>>, key: string) =>
  value(await vault().putCredential(ctx.caller, PROVIDER, {
    material: { key }, authorizedDevices: [ctx.device.deviceId]
  }));

describe("multiple DeepSeek key accounts", () => {
  it("keeps each key as its own account; a new key becomes active", async () => {
    const ctx = await setup();
    await put(ctx, "sk-deepseek-first-0001");
    await put(ctx, "sk-deepseek-second-0002");
    const accounts = value(await vault().accounts(ctx.caller, PROVIDER));
    expect(accounts.map((a) => [a.account, a.active])).toEqual([["…0001", false], ["…0002", true]]);
    const grant = value(await vault().grant(ctx.caller, await signedGrant(ctx.device, ctx.userId, PROVIDER)));
    expect(grant.accessToken).toBe("sk-deepseek-second-0002");
  });

  it("API keys dedupe by key, never by anything secret in the listing", async () => {
    const ctx = await setup();
    const put = async (key: string) =>
      value(
        await vault().putCredential(ctx.caller, "deepseek-key", {
          material: { key },
          authorizedDevices: [ctx.device.deviceId]
        })
      );
    await put("sk-ant-api03-first-0123456789");
    await put("sk-ant-api03-second-0123456789");
    await put("sk-ant-api03-first-0123456789");
    const accounts = value(await vault().accounts(ctx.caller, "deepseek-key"));
    expect(accounts).toHaveLength(2);
    expect(accounts.every((a) => a.kind === "api-key")).toBe(true);
    expect(JSON.stringify(accounts)).not.toContain("sk-ant-api03");
  });


  it("switching picks the key grants come from; none active issues none", async () => {
    const ctx = await setup();
    await put(ctx, "sk-deepseek-first-0001");
    await put(ctx, "sk-deepseek-second-0002");
    const first = value(await vault().accounts(ctx.caller, PROVIDER))[0]!;
    value(await vault().activateAccount(ctx.caller, PROVIDER, first.slot));
    expect(value(await vault().grant(ctx.caller, await signedGrant(ctx.device, ctx.userId, PROVIDER))).accessToken)
      .toBe("sk-deepseek-first-0001");
    value(await vault().activateAccount(ctx.caller, PROVIDER, null));
    expect(await vault().grant(ctx.caller, await signedGrant(ctx.device, ctx.userId, PROVIDER)))
      .toMatchObject({ ok: false, error: "not_found" });
  });

  it("forgetting the active key leaves none active; other keys stay", async () => {
    const ctx = await setup();
    await put(ctx, "sk-deepseek-first-0001");
    await put(ctx, "sk-deepseek-second-0002");
    const active = value(await vault().accounts(ctx.caller, PROVIDER)).find((a) => a.active)!;
    const remaining = value(await vault().forgetAccount(ctx.caller, PROVIDER, active.slot));
    expect(remaining.map((a) => [a.account, a.active])).toEqual([["…0001", false]]);
    expect(await vault().grant(ctx.caller, await signedGrant(ctx.device, ctx.userId, PROVIDER)))
      .toMatchObject({ ok: false, error: "not_found" });
  });

  it("has no direct LLM usage probe and does not change the account", async () => {
    const ctx = await setup();
    await put(ctx, "sk-deepseek-first-0001");
    const before = value(await vault().accounts(ctx.caller, PROVIDER));
    const fetch = vi.spyOn(globalThis, "fetch");
    expect(await vault().accountUsage(ctx.caller, PROVIDER, before[0]!.slot))
      .toMatchObject({ ok: false, error: "not_found" });
    expect(fetch).not.toHaveBeenCalled();
    expect(value(await vault().accounts(ctx.caller, PROVIDER))).toEqual(before);
  });

  it("only users manage accounts", async () => {
    const ctx = await setup();
    const runner = runnerCaller(ctx.userId, ctx.device.deviceId);
    for (const result of [
      await vault().accounts(runner, PROVIDER),
      await vault().activateAccount(runner, PROVIDER, null),
      await vault().forgetAccount(runner, PROVIDER, "a000000000"),
      await vault().accountUsage(runner, PROVIDER, "a000000000")
    ]) expect(result).toMatchObject({ ok: false, error: "forbidden" });
  });

  it("the single-record layout migrates into one active key that still opens", async () => {
    const ctx = await setup();
    const key = "deepseek-migration-fixture";
    await put(ctx, key);
    await runInDurableObject(accountStub(env, ctx.userId, PROVIDER), (instance, state) => {
      const sql = state.storage.sql;
      const row = sql.exec<Record<string, SqlStorageValue>>("SELECT * FROM accounts").toArray()[0]!;
      sql.exec(
        `CREATE TABLE record (id INTEGER PRIMARY KEY CHECK (id = 1), generation INTEGER NOT NULL, status TEXT NOT NULL,
          account TEXT, authorized_devices TEXT NOT NULL, envelope TEXT NOT NULL, updated_at INTEGER NOT NULL,
          hard_expires_at INTEGER, last_refresh_at INTEGER, last_attempt_at INTEGER, last_error TEXT,
          failures INTEGER NOT NULL DEFAULT 0)`
      );
      sql.exec(
        "INSERT INTO record (id, generation, status, account, authorized_devices, envelope, updated_at) VALUES (1, ?, ?, ?, ?, ?, ?)",
        row.generation, row.status, row.account, row.authorized_devices, row.envelope, row.updated_at
      );
      sql.exec("DELETE FROM accounts");
      sql.exec("DELETE FROM meta WHERE key = 'active'");
      (instance as unknown as { migrateSingleRecord(): void }).migrateSingleRecord();
      expect(sql.exec("SELECT name FROM sqlite_master WHERE name = 'record'").toArray()).toHaveLength(0);
    });
    expect(value(await vault().accounts(ctx.caller, PROVIDER)).map((a) => [a.account, a.active]))
      .toEqual([["…ture", true]]);
    expect(value(await vault().grant(ctx.caller, await signedGrant(ctx.device, ctx.userId, PROVIDER))))
      .toMatchObject({ accessToken: key, generation: 1 });
  });

  it("rejects legacy operations without opening or rewriting legacy storage", async () => {
    const ctx = await setup();
    const legacy = "codex" as VaultProviderId;
    const stub = accountStub(env, ctx.userId, legacy);
    await runInDurableObject(stub, (_instance, state) => {
      const sql = state.storage.sql;
      sql.exec("INSERT OR REPLACE INTO meta (key, value) VALUES ('owner', ?), ('provider', 'codex')", ctx.userId);
      sql.exec("CREATE TABLE IF NOT EXISTS historical (payload TEXT)");
      sql.exec("INSERT INTO historical (payload) VALUES ('keep credentials')");
    });
    const before = await runInDurableObject(stub, (_instance, state) =>
      JSON.stringify(state.storage.sql.exec("SELECT * FROM historical").toArray()));
    const bad = { ok: false, error: "bad_request" };
    expect(await vault().putCredential(ctx.caller, legacy, {
      material: { key: "never-echo-this-key" }, authorizedDevices: []
    })).toMatchObject(bad);
    expect(await vault().authorize(ctx.caller, legacy, [])).toMatchObject(bad);
    expect(await vault().disconnect(ctx.caller, legacy)).toMatchObject(bad);
    expect(await vault().accounts(ctx.caller, legacy)).toMatchObject(bad);
    const valid = await signedGrant(ctx.device, ctx.userId, PROVIDER);
    expect(await vault().grant(ctx.caller, { ...valid, provider: legacy })).toMatchObject(bad);
    expect(await stub.accounts(ctx.userId, legacy)).toMatchObject(bad);
    const after = await runInDurableObject(stub, (_instance, state) =>
      JSON.stringify(state.storage.sql.exec("SELECT * FROM historical").toArray()));
    expect(after).toBe(before);
  });
});
