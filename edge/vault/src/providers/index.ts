/** Provider registry. Secrets are opaque JSON to everything outside the
 * adapter that owns them, hence the one `unknown` cast below. */
import type { VaultProviderId } from "../api";
import { githubAdapter } from "./github";
import { deepseekKeyAdapter } from "./keys";
import type { ProviderAdapter } from "./types";

const ADAPTERS = {
  github: githubAdapter,
  "deepseek-key": deepseekKeyAdapter
} satisfies Record<VaultProviderId, { readonly id: VaultProviderId }>;

export const adapterFor = (provider: VaultProviderId): ProviderAdapter<unknown> => {
  if (!Object.hasOwn(ADAPTERS, provider)) throw new Error("unsupported credential provider");
  return ADAPTERS[provider] as unknown as ProviderAdapter<unknown>;
};

/** Providers the daily canary exercises: the ones with a refresh contract
 * (or, for Claude setup tokens, an expiry worth warning about). */
export const CANARY_PROVIDERS: readonly VaultProviderId[] = ["github"];
