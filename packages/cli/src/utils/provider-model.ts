import type { CommandError } from "../output/index.js";
import type { AgentProfile } from "@getpaseo/protocol/agent-profile";

export function resolveRunDefaults(
  options: { provider?: string; model?: string; mode?: string; thinking?: string },
  profiles: readonly AgentProfile[],
) {
  const profile = profiles.find((entry) => entry.cliDefault === true);
  const resolved = resolveProviderAndModel({ ...options, defaultProvider: profile?.provider });
  const matchingProfile = profile?.provider === resolved.provider ? profile : undefined;
  const model = resolved.model ?? matchingProfile?.model;
  const sameModel = !resolved.model || resolved.model === matchingProfile?.model;
  return {
    provider: resolved.provider,
    model,
    modeId: options.mode ?? matchingProfile?.modeId,
    thinkingOptionId:
      options.thinking ?? (sameModel ? matchingProfile?.thinkingOptionId : undefined),
    featureValues: matchingProfile?.featureValues,
  };
}

export interface ResolveProviderAndModelOptions {
  provider?: string;
  model?: string;
  defaultProvider?: string;
}

export interface ResolvedProviderModel {
  provider: string;
  model: string | undefined;
}

export function resolveProviderAndModel(
  options: ResolveProviderAndModelOptions,
): ResolvedProviderModel {
  const providerInput = options.provider?.trim() || options.defaultProvider;
  const modelInput = options.model?.trim();

  if (!providerInput) {
    const error: CommandError = {
      code: "MISSING_PROVIDER",
      message: "Provider is required",
      details:
        "Set a CLI default profile in Paseo host settings, or pass --provider <provider>. Use `paseo provider ls` to see providers.",
    };
    throw error;
  }

  if (options.model !== undefined && !modelInput) {
    const error: CommandError = {
      code: "INVALID_MODEL",
      message: "--model cannot be empty",
    };
    throw error;
  }

  const slashIndex = providerInput.indexOf("/");
  if (slashIndex === -1) {
    return {
      provider: providerInput,
      model: modelInput,
    };
  }

  const provider = providerInput.slice(0, slashIndex).trim();
  const modelFromProvider = providerInput.slice(slashIndex + 1).trim();
  if (!provider || !modelFromProvider) {
    const error: CommandError = {
      code: "INVALID_PROVIDER",
      message: "Invalid --provider value",
      details: "Use --provider <provider> or --provider <provider>/<model>",
    };
    throw error;
  }

  if (modelInput && modelInput !== modelFromProvider) {
    const error: CommandError = {
      code: "CONFLICTING_MODEL_OPTIONS",
      message: "Conflicting model values provided",
      details: `--provider specifies model ${modelFromProvider}, but --model specifies ${modelInput}`,
    };
    throw error;
  }

  return {
    provider,
    model: modelInput ?? modelFromProvider,
  };
}
