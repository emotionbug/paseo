import { watch } from "node:fs";
import { readdir, readFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawn } from "node:child_process";
import { readTitleIndex } from "./sync-codex-session-titles.mjs";

export function titleFingerprint(agents, index) {
  const titles = agents
    .filter((agent) => agent.provider === "codex" && agent.persistence?.sessionId)
    .map((agent) => [agent.id, agent.persistence.sessionId, agent.title ?? ""])
    .sort((a, b) => a[0].localeCompare(b[0]));
  const codex = [...readTitleIndex(index)].sort((a, b) => a[0].localeCompare(b[0]));
  return JSON.stringify({ titles, codex });
}

async function readSnapshot(paseoHome, codexHome) {
  const root = path.join(paseoHome, "agents");
  const entries = await readdir(root, { recursive: true, withFileTypes: true });
  const agents = [];
  for (const entry of entries) {
    if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
    try {
      agents.push(JSON.parse(await readFile(path.join(entry.parentPath, entry.name), "utf8")));
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  let index = "";
  try {
    index = await readFile(path.join(codexHome, "session_index.jsonl"), "utf8");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  return titleFingerprint(agents, index);
}

export function watchTitles() {
  const paseoHome = process.env.PASEO_HOME ?? path.join(homedir(), ".paseo");
  const codexHome = process.env.CODEX_HOME ?? path.join(homedir(), ".codex");
  let previous = null;
  let timer;
  let running = false;
  let dirty = false;
  let stopped = false;
  let child;

  function schedule(delay = 500) {
    if (stopped) return;
    dirty = true;
    if (running) return;
    if (timer) return;
    timer = setTimeout(check, delay);
  }

  async function check() {
    timer = undefined;
    if (running || stopped) return;
    running = true;
    dirty = false;
    let retry = false;
    try {
      const snapshot = await readSnapshot(paseoHome, codexHome);
      if (snapshot === previous) return;
      await new Promise((resolve, reject) => {
        child = spawn(
          process.execPath,
          [path.join(import.meta.dirname, "sync-codex-session-titles.mjs")],
          {
            env: process.env,
            stdio: ["ignore", "inherit", "inherit"],
          },
        );
        child.once("error", reject);
        child.once("exit", (code) => {
          if (code === 0) {
            resolve();
            return;
          }
          reject(new Error(`Title sync exited: ${code}`));
        });
      });
      // Keep the pre-sync snapshot so a concurrent rename is checked again.
      previous = snapshot;
      dirty = true;
    } catch (error) {
      console.error(`Session title synchronization: ${error.message}`);
      retry = true;
    } finally {
      running = false;
      if (retry) schedule(30_000);
      else if (dirty) schedule();
    }
  }

  const agentsWatcher = watch(path.join(paseoHome, "agents"), { recursive: true }, () =>
    schedule(),
  );
  const codexWatcher = watch(codexHome, (_event, file) => {
    if (file?.toString() === "session_index.jsonl") schedule();
  });
  // Let systemd restart on a watcher failure rather than silently losing synchronization.
  agentsWatcher.on("error", (error) => {
    throw error;
  });
  codexWatcher.on("error", (error) => {
    throw error;
  });
  function stop() {
    stopped = true;
    clearTimeout(timer);
    agentsWatcher.close();
    codexWatcher.close();
    child?.kill();
  }
  process.once("SIGTERM", stop);
  process.once("SIGINT", stop);
  schedule(0);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) watchTitles();
