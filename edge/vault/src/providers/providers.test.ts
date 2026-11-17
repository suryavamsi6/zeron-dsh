import { describe, expect, it, vi } from "vitest";
import {
  DEVICE_GRANT_TYPE,
  GITHUB_TOKEN_URL,
  githubAdapter,
  githubRefreshDue,
  parseDeviceCode,
  parseDevicePoll,
  parseGithubRefresh,
  parseGithubToken,
  refreshGithub,
  type GithubSecret
} from "./github";
import { deepseekKeyAdapter } from "./keys";
import { adapterFor } from "./index";
import { isProvider } from "../validate";
import type { VaultProviderId } from "../api";
import type { FetchFn } from "./types";

const NOW = Date.parse("2026-10-01T12:00:00Z");
const HOUR = 60 * 60_000;
const DAY = 24 * HOUR;
const jsonResponse = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

describe("github", () => {
  const secret = (overrides: Partial<GithubSecret> = {}): GithubSecret => ({
    accessToken: "ghu_1",
    expiresAt: NOW + 8 * HOUR,
    refreshToken: "ghr_1",
    refreshTokenExpiresAt: NOW + 180 * DAY,
    login: "octocat",
    lastRefresh: NOW,
    ...overrides
  });

  it("parses token responses (expiring and non-expiring)", () => {
    expect(
      parseGithubToken(
        { access_token: "ghu_1", expires_in: 28800, refresh_token: "ghr_1", refresh_token_expires_in: 15897600 },
        NOW,
        "octocat"
      )
    ).toEqual(secret({ refreshTokenExpiresAt: NOW + 15897600 * 1000 }));
    expect(parseGithubToken({ access_token: "gho_1", token_type: "bearer" }, NOW, "octocat")).toEqual({
      accessToken: "gho_1",
      expiresAt: undefined,
      refreshToken: undefined,
      refreshTokenExpiresAt: undefined,
      login: "octocat",
      lastRefresh: NOW
    });
    expect(parseGithubToken({ error: "bad" }, NOW, undefined)).toBeUndefined();
  });

  it("refreshes only expiring tokens under 10 minutes from expiry", () => {
    expect(githubRefreshDue(secret({ expiresAt: NOW + 11 * 60_000 }), NOW)).toBe(false);
    expect(githubRefreshDue(secret({ expiresAt: NOW + 9 * 60_000 }), NOW)).toBe(true);
    expect(githubRefreshDue(secret({ expiresAt: undefined, refreshToken: undefined }), NOW)).toBe(false);
    expect(githubAdapter.refreshable(secret({ expiresAt: undefined, refreshToken: undefined }))).toBe(false);
  });

  it("non-expiring grants still expire hourly", () => {
    expect(githubAdapter.grant(secret({ expiresAt: undefined }), NOW)).toEqual({
      accessToken: "ghu_1",
      expiresAt: NOW + HOUR,
      account: "octocat"
    });
  });

  it("classifies refresh responses (errors arrive as HTTP 200)", () => {
    const previous = secret();
    expect(parseGithubRefresh(200, { error: "bad_refresh_token" }, previous, NOW).kind).toBe("reconnect");
    expect(parseGithubRefresh(200, { error: "incorrect_client_credentials" }, previous, NOW).kind).toBe("transient");
    expect(parseGithubRefresh(502, undefined, previous, NOW).kind).toBe("transient");
    const refreshed = parseGithubRefresh(200, { access_token: "ghu_2", expires_in: 28800 }, previous, NOW);
    expect(refreshed.kind === "ok" && refreshed.secret).toMatchObject({
      accessToken: "ghu_2",
      refreshToken: "ghr_1",
      login: "octocat"
    });
  });

  it("refreshes with the App's client credentials", async () => {
    const fetchMock = vi.fn<FetchFn>(async () =>
      jsonResponse(200, { access_token: "ghu_2", expires_in: 28800, refresh_token: "ghr_2" })
    );
    const outcome = await refreshGithub(secret(), {
      fetch: fetchMock,
      now: NOW,
      githubClientId: "Iv1.client",
      githubClientSecret: "shh"
    });
    expect(outcome.kind === "ok" && outcome.secret.refreshToken).toBe("ghr_2");
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe(GITHUB_TOKEN_URL);
    expect(Object.fromEntries(new URLSearchParams(init?.body as string))).toEqual({
      client_id: "Iv1.client",
      client_secret: "shh",
      grant_type: "refresh_token",
      refresh_token: "ghr_1"
    });
  });

  it("without App secrets a refresh is transient, never a reconnect", async () => {
    const outcome = await refreshGithub(secret(), { fetch: vi.fn<FetchFn>(), now: NOW });
    expect(outcome.kind).toBe("transient");
  });

  it("parses the device code response", () => {
    expect(
      parseDeviceCode(200, {
        device_code: "dc",
        user_code: "ABCD-1234",
        verification_uri: "https://github.com/login/device",
        expires_in: 899,
        interval: 5
      })
    ).toEqual({
      ok: true,
      start: {
        deviceCode: "dc",
        userCode: "ABCD-1234",
        verificationUri: "https://github.com/login/device",
        expiresInSecs: 899,
        intervalSecs: 5
      }
    });
    expect(parseDeviceCode(200, { error: "device_flow_disabled" })).toEqual({ ok: false, reason: "device_flow_disabled" });
  });

  it("parses device poll answers", () => {
    expect(parseDevicePoll(200, { error: "authorization_pending" }, 5)).toEqual({ kind: "pending" });
    expect(parseDevicePoll(200, { error: "slow_down" }, 5)).toEqual({ kind: "slow_down", intervalSecs: 10 });
    expect(parseDevicePoll(200, { error: "slow_down", interval: 15 }, 5)).toEqual({ kind: "slow_down", intervalSecs: 15 });
    expect(parseDevicePoll(200, { error: "expired_token" }, 5)).toEqual({ kind: "expired" });
    expect(parseDevicePoll(200, { error: "access_denied" }, 5)).toEqual({ kind: "denied" });
    expect(parseDevicePoll(200, { error: "incorrect_device_code" }, 5)).toEqual({
      kind: "failed",
      error: "incorrect_device_code"
    });
    expect(parseDevicePoll(503, undefined, 5).kind).toBe("transient");
    expect(parseDevicePoll(200, { access_token: "ghu_1" }, 5)).toEqual({ kind: "token", body: { access_token: "ghu_1" } });
    expect(DEVICE_GRANT_TYPE).toBe("urn:ietf:params:oauth:grant-type:device_code");
  });
});


describe("DeepSeek API keys", () => {
  it("accepts only DeepSeek and GitHub operational providers", () => {
    expect(isProvider("deepseek-key")).toBe(true);
    expect(isProvider("github")).toBe(true);
    for (const legacy of ["codex", "claude", "anthropic-key", "openai-key"]) {
      expect(isProvider(legacy)).toBe(false);
      expect(() => adapterFor(legacy as VaultProviderId)).toThrow();
    }
  });

  it("validates locally and exposes no refresh or HTTP probe", () => {
    const parsed = deepseekKeyAdapter.parseUpload!({ key: "sk-deepseek-test-0001" }, NOW);
    expect(parsed).toEqual({ ok: true, secret: { key: "sk-deepseek-test-0001" } });
    if (!parsed.ok) throw new Error("expected key");
    expect(deepseekKeyAdapter.grant(parsed.secret, NOW)).toEqual({
      accessToken: "sk-deepseek-test-0001", expiresAt: NOW + HOUR, account: "…0001"
    });
    expect(deepseekKeyAdapter.refreshable(parsed.secret)).toBe(false);
    expect(deepseekKeyAdapter.refreshDue(parsed.secret, NOW)).toBe(false);
    expect(deepseekKeyAdapter.refresh).toBeUndefined();
    for (const key of ["", "bad key", "key\ncontrol"]) {
      expect(deepseekKeyAdapter.parseUpload!({ key }, NOW).ok).toBe(false);
    }
    // A pasted key keeps its accidental surrounding whitespace out of the secret.
    expect(deepseekKeyAdapter.parseUpload!({ key: "  sk-deepseek-test-0001\n" }, NOW)).toEqual({
      ok: true, secret: { key: "sk-deepseek-test-0001" }
    });
  });
});
