/**
 * The models Cloud offers: what the phone (or any client without an engine of
 * its own) shows in a Cloud chat's model picker. Kept by hand, so a new model
 * ships with an edge deploy, not an app release.
 *
 * Cloud runs only the DeepSeek harness through its official JSON-RPC SDK, so
 * these are the SDK's own model ids and the reasoning levels it honours
 * (`low`, `high`, `max`). The first entry is the default. Only the ids the
 * pinned SDK declares belong here — a made-up or adapter-level id would make
 * the picker offer a model the harness cannot run.
 */

export interface CloudModel {
  readonly id: string;
  readonly label: string;
  readonly description: string;
  readonly reasoningLevels: readonly string[];
}

/** The one agent Cloud machines run. */
export type CloudHarness = "deepseek";

const DEEPSEEK_REASONING = ["low", "high", "max"] as const;

export const CLOUD_MODELS: Readonly<Record<CloudHarness, readonly CloudModel[]>> = {
  deepseek: [
    {
      id: "deepseek-flash",
      label: "DeepSeek Flash",
      description: "Fast, low-cost model for everyday coding and quick questions.",
      reasoningLevels: DEEPSEEK_REASONING
    },
    {
      id: "deepseek-v4-pro",
      label: "DeepSeek V4 Pro",
      description: "Stronger model for hard, multi-step engineering work.",
      reasoningLevels: DEEPSEEK_REASONING
    }
  ]
};
