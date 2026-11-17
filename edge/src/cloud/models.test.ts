import { describe, expect, it } from "vitest";
import { CLOUD_MODELS, type CloudHarness } from "./models";

describe("Cloud model catalog", () => {
  it("offers exactly the DeepSeek harness and the SDK's pinned model ids", () => {
    expect(Object.keys(CLOUD_MODELS)).toEqual(["deepseek"]);
    const harness: CloudHarness = "deepseek";
    expect(CLOUD_MODELS[harness].map((model) => model.id)).toEqual([
      "deepseek-flash",
      "deepseek-v4-pro"
    ]);
    expect(new Set(CLOUD_MODELS[harness].map((model) => model.label)).size).toBe(2);
    for (const model of CLOUD_MODELS[harness]) {
      expect(model.description.length).toBeGreaterThan(0);
      expect(model.reasoningLevels).toEqual(["low", "high", "max"]);
    }
  });

  it("promises no removed provider and no option the SDK does not expose", () => {
    const text = JSON.stringify(CLOUD_MODELS).toLowerCase();
    for (const legacy of ["claude", "codex", "gpt-", "anthropic", "openai"]) {
      expect(text).not.toContain(legacy);
    }
    for (const model of CLOUD_MODELS.deepseek) {
      expect(model).not.toHaveProperty("options");
    }
  });
});
