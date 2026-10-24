import type { Command } from "commander";
import { createInterface } from "node:readline";
import type { AgentSnapshotPayload } from "@getpaseo/protocol/messages";
import { connectToDaemon } from "../utils/client.js";
import { resolveRunDefaults } from "../utils/provider-model.js";
import { fetchProjectedTimelineItems } from "../utils/timeline.js";
import { resolveRunWorkspace, type AgentRunOptions } from "./agent/run.js";

export interface ChatOptions extends AgentRunOptions {
  resume?: string;
}

export function addChatOptions(command: Command): Command {
  return command
    .option("--provider <provider>", "Override the host's CLI default provider")
    .option("--model <model>", "Override the saved model")
    .option("--thinking <id>", "Override the saved thinking level")
    .option("--mode <mode>", "Override the saved permission mode")
    .option("--cwd <path>", "Working directory (default: current)")
    .option("--resume <id>", "Continue an existing Paseo session");
}

export async function runChatCommand(options: ChatOptions, _command: Command): Promise<void> {
  const client = await connectToDaemon({ target: options.daemonTarget });
  let agent: AgentSnapshotPayload | undefined;
  let unsubscribe: (() => void) | undefined;
  let input: ReturnType<typeof createInterface> | undefined;
  let running = false;
  try {
    const { config } = await client.getDaemonConfig();
    if (options.resume) {
      const existing = await client.fetchAgent({ agentId: options.resume });
      if (!existing) throw new Error(`Session not found: ${options.resume}`);
      agent = existing.agent;
      await printHistory(client, agent);
    }
    const launch = agent ? undefined : resolveRunDefaults(options, config.agentProfiles ?? []);
    printHeader(agent ?? launch!);
    input = createInterface({
      input: process.stdin,
      output: process.stdout,
      terminal: Boolean(process.stdin.isTTY),
      prompt: "\nYou > ",
    });
    input.on("SIGINT", () => {
      if (running && agent) {
        void client.cancelAgent(agent.id).catch((error: unknown) => {
          process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
        });
      } else input?.close();
    });
    input.prompt();
    for await (const line of input) {
      const message = line.trim();
      if (message === "/exit" || message === "/quit") break;
      if (!message) {
        input.prompt();
        continue;
      }
      if (!agent && launch) {
        const workspace = await resolveRunWorkspace(
          client,
          options,
          options.cwd ?? process.cwd(),
          undefined,
        );
        agent = await client.createAgent({
          ...launch,
          cwd: workspace.cwd,
          workspaceId: workspace.id,
        });
        process.stdout.write(`Session: ${agent.id}\nResume: paseo --resume ${agent.id}\n`);
      }
      if (!agent) throw new Error("Could not create a session");
      if (!unsubscribe) {
        const subscription = subscribeToTools(client, agent.id);
        unsubscribe = subscription;
        await subscription.ready;
      }
      running = true;
      try {
        await client.sendMessage(agent.id, message);
        process.stdout.write("Working…\n");
        const result = await client.waitForFinish(agent.id, 0);
        printResult(agent.provider, result);
      } finally {
        running = false;
      }
      input.prompt();
    }
  } finally {
    input?.close();
    unsubscribe?.();
    await client.close();
  }
}

type ChatClient = Awaited<ReturnType<typeof connectToDaemon>>;

async function printHistory(client: ChatClient, agent: AgentSnapshotPayload) {
  const history = await fetchProjectedTimelineItems({ client, agentId: agent.id });
  for (const item of history) {
    if (item.type === "user_message" || item.type === "assistant_message") {
      process.stdout.write(
        `\n${item.type === "user_message" ? "You" : agent.provider}: ${item.text}\n`,
      );
    }
  }
}

function subscribeToTools(client: ChatClient, agentId: string) {
  return client.subscribeAgentTimeline(agentId, (event) => {
    if (event.type !== "agent_stream") return;
    const update = event.payload.event;
    if (update.type === "timeline" && update.item.type === "tool_call") {
      process.stdout.write(`[${update.item.name}] ${update.item.status ?? "started"}\n`);
    } else if (update.type === "permission_requested") {
      process.stdout.write(`Permission needed: ${update.request.name}. Respond in Paseo.\n`);
    }
  });
}

function printResult(provider: string, result: Awaited<ReturnType<ChatClient["waitForFinish"]>>) {
  if (result.lastMessage) process.stdout.write(`\n${provider}: ${result.lastMessage}\n`);
  if (result.status === "error") process.stderr.write(`${result.error ?? "Agent failed"}\n`);
  if (result.status === "permission")
    process.stdout.write("Respond to the permission request in Paseo.\n");
  if (result.status === "timeout")
    process.stdout.write("Agent is still running; follow it in Paseo.\n");
}

function printHeader(config: { provider: string; model?: string | null }) {
  process.stdout.write(`\nPaseo · ${config.provider} · ${config.model ?? "default model"}\n`);
  process.stdout.write(
    "/exit to detach · Ctrl+C to interrupt · This session is also available in Paseo.\n",
  );
}
