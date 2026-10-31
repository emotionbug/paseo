import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = fileURLToPath(new URL("./seed-worktree-dev-state.mjs", import.meta.url));

test("seeds durable native development metadata, not runtime files or retired server configuration", (t) => {
  const root = mkdtempSync(path.join(tmpdir(), "paseo-worktree-seed-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, "source");
  const target = path.join(root, "target");
  const sourceHome = path.join(source, ".dev/paseo-home");
  const targetHome = path.join(target, ".dev/paseo-home");
  mkdirSync(path.join(sourceHome, "agents/nested"), { recursive: true });
  mkdirSync(path.join(sourceHome, "projects"));
  mkdirSync(path.join(source, "packages/server"), { recursive: true });
  mkdirSync(target);
  writeFileSync(path.join(sourceHome, "agents/nested/agent.json"), '{"id":"agent"}');
  writeFileSync(path.join(sourceHome, "agents/runtime.log"), "not durable");
  writeFileSync(path.join(sourceHome, "projects/project.json"), '{}');
  writeFileSync(path.join(sourceHome, "config.json"), '{"version":1}');
  writeFileSync(path.join(sourceHome, "daemon.pid"), "123");
  writeFileSync(path.join(source, "packages/server/.env"), "RETIRED=1");
  const env = {
    ...process.env,
    PASEO_SOURCE_CHECKOUT_PATH: source,
    PASEO_WORKTREE_PATH: target,
    PASEO_DEV_SEED_HOME: "",
    PASEO_DEV_RESET_HOME: "0",
  };
  execFileSync(process.execPath, [script], { env });
  assert.equal(readFileSync(path.join(targetHome, "agents/nested/agent.json"), "utf8"), '{"id":"agent"}');
  assert.equal(readFileSync(path.join(targetHome, "config.json"), "utf8"), '{"version":1}');
  assert.ok(existsSync(path.join(targetHome, "projects/project.json")));
  assert.ok(!existsSync(path.join(targetHome, "agents/runtime.log")));
  assert.ok(!existsSync(path.join(targetHome, "daemon.pid")));
  assert.ok(!existsSync(path.join(target, "packages/server")));
  writeFileSync(path.join(targetHome, "config.json"), '{"local":true}');
  execFileSync(process.execPath, [script], { env });
  assert.equal(readFileSync(path.join(targetHome, "config.json"), "utf8"), '{"local":true}');
  execFileSync(process.execPath, [script], { env: { ...env, PASEO_DEV_RESET_HOME: "1" } });
  assert.equal(readFileSync(path.join(targetHome, "config.json"), "utf8"), '{"version":1}');
});
