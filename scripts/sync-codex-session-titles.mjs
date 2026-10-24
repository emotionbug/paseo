import { readFile, writeFile, rename } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { z } from "zod";
import { DaemonClient } from "@getpaseo/client/internal/daemon-client";

const indexEntry = z.object({ id: z.string(), thread_name: z.string() });
const savedState = z.record(z.string(), z.object({ paseo: z.string(), codex: z.string() }));

export function readTitleIndex(text) {
  const names = new Map();
  const lines = text.split("\n");
  for (const [index, line] of lines.entries()) {
    if (!line.trim()) continue;
    let value;
    try {
      value = JSON.parse(line);
    } catch (error) {
      // Codex may be appending the last record while this snapshot is read.
      if (index === lines.length - 1) break;
      throw error;
    }
    const entry = indexEntry.parse(value);
    names.set(entry.id, entry.thread_name.trim());
  }
  return names;
}

export function resolveTitleSync({ paseo, codex, previous }) {
  if (paseo === codex) return null;
  if (!previous) return codex ? { target: "paseo", title: codex } : null;
  // A changed Codex name wins a simultaneous edit; ordinary chat activity is not an edit.
  if (codex && codex !== previous.codex) return { target: "paseo", title: codex };
  if (paseo && paseo !== previous.paseo) return { target: "codex", title: paseo };
  return null;
}

async function optionalFile(file, fallback) {
  try {
    return await readFile(file, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return fallback;
    throw error;
  }
}

export async function setCodexNames(updates, { codexHome, executable = "codex" }) {
  if (updates.length === 0) return;
  const child = spawn(executable, ["app-server", "--listen", "stdio://"], {
    env: { ...process.env, CODEX_HOME: codexHome },
    stdio: ["pipe", "pipe", "ignore"],
  });
  const lines = createInterface({ input: child.stdout });
  const pending = new Map();
  let id = 0;
  function fail(error) {
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  }
  child.on("error", fail);
  child.on("exit", () => fail(new Error("Codex title service exited")));
  lines.on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      fail(error);
      return;
    }
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    if (message.error) request.reject(new Error(message.error.message));
    else request.resolve(message.result);
  });
  function rpc(method, params) {
    const requestId = ++id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(requestId);
        reject(new Error(`Codex ${method} timed out`));
      }, 15_000);
      pending.set(requestId, {
        resolve(value) {
          clearTimeout(timer);
          resolve(value);
        },
        reject(error) {
          clearTimeout(timer);
          reject(error);
        },
      });
      child.stdin.write(JSON.stringify({ id: requestId, method, params }) + "\n");
    });
  }
  try {
    await rpc("initialize", {
      clientInfo: { name: "paseo_sessions_titles", version: "1.0.0" },
    });
    child.stdin.write(JSON.stringify({ method: "initialized", params: {} }) + "\n");
    for (const update of updates) {
      await rpc("thread/name/set", { threadId: update.threadId, name: update.title });
    }
  } finally {
    lines.close();
    child.stdin.end();
    child.kill();
  }
}

export async function syncTitles() {
  const paseoHome = process.env.PASEO_HOME ?? path.join(homedir(), ".paseo");
  const codexHome = process.env.CODEX_HOME ?? path.join(homedir(), ".codex");
  const stateFile = path.join(paseoHome, "session-title-sync.json");
  const names = readTitleIndex(await optionalFile(path.join(codexHome, "session_index.jsonl"), ""));
  const previous = savedState.parse(JSON.parse(await optionalFile(stateFile, "{}")));
  const next = {};
  const config = JSON.parse(await readFile(path.join(paseoHome, "config.json"), "utf8"));
  const endpoint = process.env.PASEO_TITLE_SYNC_URL ?? `ws://${config.daemon.listen}/ws`;
  const password =
    process.env.PASEO_PASSWORD ??
    (await readFile(path.join(paseoHome, "connection-password.txt"), "utf8")).trim();
  const client = new DaemonClient({
    url: endpoint,
    password,
    clientId: "paseo-session-title-sync",
    clientType: "cli",
    connectTimeoutMs: 10_000,
    reconnect: { enabled: false },
  });
  let updated = 0;
  const codexUpdates = [];
  try {
    await client.connect();
    let cursor;
    do {
      const page = await client.fetchAgents({ page: { cursor, limit: 100 } });
      for (const { agent } of page.entries) {
        if (agent.provider !== "codex" || !agent.persistence?.sessionId || agent.parentAgentId)
          continue;
        const threadId = agent.persistence.sessionId;
        const key = `${agent.id}:${threadId}`;
        const paseo = agent.title?.trim() ?? "";
        const codex = names.get(threadId) ?? "";
        const action = resolveTitleSync({ paseo, codex, previous: previous[key] });
        next[key] = { paseo, codex };
        if (!action) continue;
        if (action.target === "paseo") {
          await client.updateAgent(agent.id, { name: action.title });
        } else {
          codexUpdates.push({ threadId, title: action.title });
        }
        next[key] = { paseo: action.title, codex: action.title };
        updated++;
      }
      cursor = page.pageInfo.nextCursor ?? undefined;
    } while (cursor);
    await setCodexNames(codexUpdates, { codexHome, executable: process.env.CODEX_BIN ?? "codex" });
    await writeFile(stateFile + ".tmp", JSON.stringify(next, null, 2) + "\n", { mode: 0o600 });
    await rename(stateFile + ".tmp", stateFile);
    if (updated) console.log(`Synchronized ${updated} session title(s).`);
  } finally {
    await client.close();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await syncTitles();
}
