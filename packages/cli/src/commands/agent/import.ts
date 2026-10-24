import type { Command } from "commander";
import { connectToDaemon } from "../../utils/client.js";
import { collectMultiple } from "../../utils/command-options.js";
import type { CommandError, CommandOptions, SingleResult } from "../../output/index.js";
import { agentRunSchema, type AgentRunResult } from "./run.js";
import type { AgentSnapshotPayload } from "@getpaseo/protocol/messages";
import { select, confirm, isCancel } from "@clack/prompts";
import { runChatCommand } from "../chat.js";

export function addImportOptions(cmd: Command): Command {
  return cmd
    .description("Import an existing provider session as a Paseo agent")
    .argument("[id]", "Provider session ID; omit to choose a session in the current directory")
    .option("--provider <provider>", "Limit discovery to a provider; required with an explicit ID")
    .option("--cwd <path>", "Working directory for providers that require it")
    .option(
      "--label <key=value>",
      "Add label(s) to the agent (can be used multiple times)",
      collectMultiple,
      [],
    );
}

export interface AgentImportOptions extends CommandOptions {
  provider?: string;
  cwd?: string;
  label?: string[];
  host?: string;
}

export type AgentImportCommandResult = SingleResult<AgentRunResult>;

function toImportResult(agent: AgentSnapshotPayload): AgentRunResult {
  return {
    agentId: agent.id,
    status: agent.status === "running" ? "running" : "created",
    provider: agent.provider,
    cwd: agent.cwd,
    title: agent.title,
  };
}

function parseImportProvider(provider: string | undefined): string {
  const normalizedProvider = provider?.trim();
  if (!normalizedProvider) {
    throw {
      code: "MISSING_PROVIDER",
      message: "Provider is required",
      details: "Usage: paseo import --provider <provider> <id>",
    } satisfies CommandError;
  }

  return normalizedProvider;
}

function parseImportLabels(labelFlags: string[] | undefined): Record<string, string> {
  const labels: Record<string, string> = {};
  if (!labelFlags) {
    return labels;
  }

  for (const labelFlag of labelFlags) {
    const eqIndex = labelFlag.indexOf("=");
    if (eqIndex === -1) {
      throw {
        code: "INVALID_LABEL",
        message: `Invalid label format: ${labelFlag}`,
        details: "Labels must be in key=value format",
      } satisfies CommandError;
    }

    const key = labelFlag.slice(0, eqIndex).trim();
    if (!key) {
      throw {
        code: "INVALID_LABEL",
        message: `Invalid label format: ${labelFlag}`,
        details: "Labels must include a non-empty key in key=value format",
      } satisfies CommandError;
    }

    labels[key] = labelFlag.slice(eqIndex + 1);
  }

  return labels;
}

export function resolveImportCwd(explicitCwd: string | undefined, defaultCwd: string): string {
  const cwd = explicitCwd?.trim() ?? defaultCwd;
  if (!cwd.trim()) {
    throw {
      code: "INVALID_CWD",
      message: "--cwd cannot be empty",
      details: "Provide a working directory path or omit --cwd",
    } satisfies CommandError;
  }
  return cwd;
}

export async function runImportCommand(
  sessionIdArg: string | undefined,
  options: AgentImportOptions,
  _command: Command,
): Promise<AgentImportCommandResult> {
  if (sessionIdArg === undefined) return runInteractiveImport(options, _command);
  const sessionId = sessionIdArg.trim();
  if (!sessionId) {
    throw {
      code: "MISSING_SESSION_ID",
      message: "Session ID is required",
      details: "Usage: paseo import --provider <provider> <id>",
    } satisfies CommandError;
  }

  const provider = parseImportProvider(options.provider);
  const cwd = resolveImportCwd(options.cwd, process.cwd());

  const labels = parseImportLabels(options.label);
  const client = await connectToDaemon({ target: options.daemonTarget });

  try {
    const agent = await client.importAgent({
      provider,
      sessionId,
      cwd,
      ...(Object.keys(labels).length > 0 ? { labels } : {}),
    });

    await client.close();

    return {
      type: "single",
      data: toImportResult(agent),
      schema: agentRunSchema,
    };
  } catch (err) {
    await client.close().catch(() => {});

    if (err && typeof err === "object" && "code" in err) {
      throw err;
    }

    const message = err instanceof Error ? err.message : String(err);
    throw {
      code: "AGENT_IMPORT_FAILED",
      message: `Failed to import agent: ${message}`,
    } satisfies CommandError;
  }
}

async function runInteractiveImport(
  options: AgentImportOptions,
  command: Command,
): Promise<AgentImportCommandResult> {
  if (!process.stdin.isTTY) {
    throw {
      code: "INTERACTIVE_REQUIRED",
      message: "Run paseo import in a terminal, or specify --provider <provider> <session-id>.",
    } satisfies CommandError;
  }
  const cwd = resolveImportCwd(options.cwd, process.cwd());
  const client = await connectToDaemon({ target: options.daemonTarget });
  let imported: AgentSnapshotPayload;
  try {
    const result = await client.fetchRecentProviderSessions({
      cwd,
      limit: 200,
      ...(options.provider ? { providers: [options.provider] } : {}),
    });
    for (const error of result.providerErrors ?? []) {
      process.stderr.write(`${error.provider}: ${error.message}\n`);
    }
    if (result.entries.length === 0) {
      throw {
        code: "NO_IMPORTABLE_SESSIONS",
        message: `No external sessions found in ${cwd}.`,
        details:
          "Already imported sessions appear in paseo ls; continue them with paseo --resume <id>.",
      } satisfies CommandError;
    }
    const selected = await select({
      message: `Import an external session in ${cwd}`,
      options: result.entries.map((entry, index) => ({
        value: index,
        label: `[${entry.providerLabel}] ${entry.title ?? entry.firstPromptPreview ?? entry.providerHandleId}`,
        hint: `${entry.lastActivityAt} · ${entry.providerHandleId}`,
      })),
    });
    if (isCancel(selected)) throw { code: "CANCELLED", message: "Import cancelled." };
    const entry = result.entries[selected];
    const stopped = await confirm({
      message:
        "Have you closed the original agent session? Import resumes it; do not run two writers on the same session.",
      initialValue: false,
    });
    if (isCancel(stopped) || !stopped)
      throw {
        code: "CANCELLED",
        message: "Close the original session, then run paseo import again.",
      };
    imported = await client.importAgent({
      provider: entry.providerId,
      sessionId: entry.providerHandleId,
      cwd: entry.cwd,
      labels: parseImportLabels(options.label),
    });
  } finally {
    await client.close();
  }
  await runChatCommand({ ...options, resume: imported.id }, command);
  return { type: "single", data: toImportResult(imported), schema: agentRunSchema };
}
