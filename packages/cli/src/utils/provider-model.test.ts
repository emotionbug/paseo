import { describe, expect, test } from "vitest";

import { resolveProviderAndModel, resolveRunDefaults } from "./provider-model.js";

describe("GUI CLI default profile", () => {
  const profiles = [
    {
      id: "pi-default",
      name: "Pi",
      provider: "pi",
      model: "openai-codex/example",
      modeId: "default",
      thinkingOptionId: "high",
      featureValues: { testFeature: true },
      cliDefault: true,
    },
  ];

  test("uses the saved provider and all launch options when flags are omitted", () => {
    expect(resolveRunDefaults({}, profiles)).toEqual({
      provider: "pi",
      model: "openai-codex/example",
      modeId: "default",
      thinkingOptionId: "high",
      featureValues: { testFeature: true },
    });
  });

  test("explicit options override only their saved values", () => {
    expect(resolveRunDefaults({ mode: "plan", thinking: "low" }, profiles)).toMatchObject({
      provider: "pi",
      model: "openai-codex/example",
      modeId: "plan",
      thinkingOptionId: "low",
    });
  });

  test("switching provider does not inherit another provider's options", () => {
    expect(resolveRunDefaults({ provider: "claude" }, profiles)).toEqual({
      provider: "claude",
      model: undefined,
      modeId: undefined,
      thinkingOptionId: undefined,
      featureValues: undefined,
    });
  });

  test("an explicit model does not inherit an incompatible thinking level", () => {
    expect(resolveRunDefaults({ provider: "pi/other" }, profiles)).toMatchObject({
      provider: "pi",
      model: "other",
      thinkingOptionId: undefined,
    });
  });

  test("removing the default restores the actionable missing-provider error", () => {
    expect(() => resolveRunDefaults({}, [])).toThrow(
      expect.objectContaining({ code: "MISSING_PROVIDER" }),
    );
  });
});

describe("resolveProviderAndModel", () => {
  test("requires an explicit provider when no default is supplied", () => {
    expect(() => resolveProviderAndModel({})).toThrow(
      expect.objectContaining({
        code: "MISSING_PROVIDER",
        message: "Provider is required",
      }),
    );
  });

  test("uses an explicit default provider when supplied by a scoped caller", () => {
    expect(resolveProviderAndModel({ defaultProvider: "claude" })).toEqual({
      provider: "claude",
      model: undefined,
    });
  });

  test("parses provider/model shorthand", () => {
    expect(resolveProviderAndModel({ provider: "codex/gpt-5.4" })).toEqual({
      provider: "codex",
      model: "gpt-5.4",
    });
  });

  test("rejects conflicting model values", () => {
    expect(() =>
      resolveProviderAndModel({ provider: "codex/gpt-5.4", model: "gpt-5.4-mini" }),
    ).toThrow(
      expect.objectContaining({
        code: "CONFLICTING_MODEL_OPTIONS",
      }),
    );
  });
});
