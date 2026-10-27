import { describe, test, expect } from "vitest";
import { execSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { createDaemonTestContext } from "../test-utils/index.js";

const RUN = process.env.PASEO_GIT_DIFF_BOTTLENECK_E2E === "1";
const LARGE_CHANGESET_SIZE = Number.parseInt(
  process.env.PASEO_GIT_DIFF_BOTTLENECK_FILE_COUNT ?? "1200",
  10,
);

function tmpRepo(): string {
  return mkdtempSync(path.join(tmpdir(), "paseo-git-diff-bottleneck-"));
}

function initGitRepo(cwd: string): void {
  execSync("git init -b main", { cwd, stdio: "pipe" });
  execSync("git config user.email 'test@test.com'", { cwd, stdio: "pipe" });
  execSync("git config user.name 'Test'", { cwd, stdio: "pipe" });
}

function seedLargeDirtyRepo(cwd: string, fileCount: number): void {
  mkdirSync(path.join(cwd, "files"), { recursive: true });
  for (let i = 0; i < fileCount; i += 1) {
    writeFileSync(path.join(cwd, "files", `f-${i}.txt`), `line ${i}\n`);
  }
  execSync("git add .", { cwd, stdio: "pipe" });
  execSync("git -c commit.gpgsign=false commit -m 'init'", {
    cwd,
    stdio: "pipe",
  });
  for (let i = 0; i < fileCount; i += 1) {
    writeFileSync(path.join(cwd, "files", `f-${i}.txt`), `line ${i} changed\n`);
  }

  // Explicit binary artifact to verify we do not diff binary contents.
  writeFileSync(path.join(cwd, "blob.bin"), Buffer.from([0x00, 0xff, 0x10, 0x80, 0x00, 0x7f]));
}

const runDescribe = RUN ? describe : describe.skip;

runDescribe("daemon E2E git diff bottleneck profiling", () => {
  test("shows per-file git diff subprocess fanout and timeout pressure", async () => {
    const cwd = tmpRepo();

    try {
      initGitRepo(cwd);
      seedLargeDirtyRepo(cwd, LARGE_CHANGESET_SIZE);

      const cliStart = performance.now();
      const cliDiff = execSync("git diff HEAD", { cwd, stdio: "pipe" }).toString();
      const cliMs = performance.now() - cliStart;

      const ctx = await createDaemonTestContext();
      try {
        const measure = async (
          mode: "uncommitted" | "all",
          detail?: "summary",
        ): Promise<{ elapsedMs: number; fileCount: number }> => {
          const startedAt = performance.now();
          const payload = await ctx.client.getCheckoutDiff(cwd, {
            mode,
            ...(mode === "all" ? { baseRef: "main" } : {}),
            ...(detail ? { detail } : {}),
          });
          expect(payload.error).toBeNull();
          expect(payload.files.length).toBeGreaterThanOrEqual(LARGE_CHANGESET_SIZE);
          return { elapsedMs: performance.now() - startedAt, fileCount: payload.files.length };
        };

        const observedAllSummaryStartedAt = performance.now();
        const observedAllSummarySubscription = ctx.client.observeCheckoutDiff(cwd, {
          mode: "all",
          baseRef: "main",
          detail: "summary",
        });
        const observedAllSummaryPayload = await observedAllSummarySubscription.ready;
        const observedAllSummaryMs = performance.now() - observedAllSummaryStartedAt;
        await observedAllSummarySubscription.release();
        expect(observedAllSummaryPayload.error).toBeNull();
        expect(observedAllSummaryPayload.files.length).toBeGreaterThanOrEqual(LARGE_CHANGESET_SIZE);

        const uncommittedSummary = await measure("uncommitted", "summary");
        const allSummary = await measure("all", "summary");
        const uncommittedFull = await measure("uncommitted");

        const binaryEntry = (
          await ctx.client.getCheckoutDiff(cwd, { mode: "uncommitted" })
        ).files.find((file) => file.path === "blob.bin");
        expect(binaryEntry).toBeTruthy();
        expect(binaryEntry?.status).toBe("binary");

        // Keep this visible in test output for local bottleneck analysis.
        console.info(
          "[git-diff-bottleneck]",
          JSON.stringify(
            {
              fileCount: LARGE_CHANGESET_SIZE,
              cliMs: Math.round(cliMs),
              cliDiffBytes: cliDiff.length,
              uncommittedSummaryMs: Math.round(uncommittedSummary.elapsedMs),
              observedAllSummaryMs: Math.round(observedAllSummaryMs),
              allSummaryMs: Math.round(allSummary.elapsedMs),
              fullMs: Math.round(uncommittedFull.elapsedMs),
              checkoutFiles: uncommittedFull.fileCount,
              speedRatio: Number((uncommittedFull.elapsedMs / Math.max(cliMs, 1)).toFixed(2)),
            },
            null,
            2,
          ),
        );

        expect(uncommittedFull.elapsedMs).toBeLessThan(cliMs * 10);
      } finally {
        await ctx.cleanup();
      }
    } finally {
      rmSync(cwd, { recursive: true, force: true });
    }
  }, 240000);
});
