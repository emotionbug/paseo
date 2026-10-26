import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { beforeEach, afterEach, describe, expect, it } from "vitest";
import type { ParsedDiffFile } from "@getpaseo/protocol/messages";
import { startGitCommandMetrics, stopGitCommandMetrics } from "@server/utils/run-git-command.js";

import { getCheckoutDiff } from "./checkout-git.js";

function initRepoWithTrackedChanges(
  fileCount: number,
  extension = "txt",
): { tempDir: string; repoDir: string } {
  const tempDir = realpathSync(mkdtempSync(join(tmpdir(), "checkout-git-batch-test-")));
  const repoDir = join(tempDir, "repo");

  mkdirSync(repoDir, { recursive: true });
  execFileSync("git", ["init", "-b", "main"], { cwd: repoDir });
  execFileSync("git", ["config", "user.email", "test@test.com"], { cwd: repoDir });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: repoDir });

  for (let i = 0; i < fileCount; i += 1) {
    const content = extension === "ts" ? `export const value_${i} = 0;\n` : `before-${i}\n`;
    writeFileSync(join(repoDir, `file-${i}.${extension}`), content);
  }
  execFileSync("git", ["add", "."], { cwd: repoDir });
  execFileSync("git", ["-c", "commit.gpgsign=false", "commit", "-m", "initial"], {
    cwd: repoDir,
  });

  for (let i = 0; i < fileCount; i += 1) {
    const content = extension === "ts" ? `export const value_${i} = 1;\n` : `after-${i}\n`;
    writeFileSync(join(repoDir, `file-${i}.${extension}`), content);
  }

  return { tempDir, repoDir };
}

function hasHighlightTokens(file: ParsedDiffFile): boolean {
  for (const hunk of file.hunks) {
    for (const line of hunk.lines) {
      if ((line.tokens?.length ?? 0) > 0) return true;
    }
  }
  return false;
}

describe("checkout git diff batching", () => {
  let tempDir: string;
  let repoDir: string;

  beforeEach(() => {
    const setup = initRepoWithTrackedChanges(20);
    tempDir = setup.tempDir;
    repoDir = setup.repoDir;
  });

  afterEach(() => {
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("collects ordinary tracked patches with one bounded Git command", async () => {
    startGitCommandMetrics();
    const result = await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      includeStructured: false,
    });

    expect(result.diff).toContain("file-0.txt");
    expect(result.diff).toContain("file-19.txt");
    const metrics = stopGitCommandMetrics();
    expect(
      metrics.commands.filter(({ args }) => args[0] === "diff" && args.includes("--")),
    ).toHaveLength(1);
  });

  it("returns tracked file summaries without building patch bodies", async () => {
    startGitCommandMetrics();
    const result = await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      detail: "summary",
      includeStructured: true,
    });
    const metrics = stopGitCommandMetrics();

    expect(result.diff).toBe("");
    expect(result.structured).toHaveLength(20);
    expect(
      result.structured?.every(
        (file) => file.hunks.length === 0 && file.additions === 1 && file.deletions === 1,
      ),
    ).toBe(true);
    expect(
      metrics.commands.filter(({ args }) => args[0] === "diff" && args.includes("--")),
    ).toHaveLength(0);
  });

  it("batches committed file contents and preserves the highlighted diff", async () => {
    execFileSync("git", ["checkout", "-b", "feature"], { cwd: repoDir });
    execFileSync("git", ["add", "."], { cwd: repoDir });
    execFileSync("git", ["-c", "commit.gpgsign=false", "commit", "-m", "change"], { cwd: repoDir });
    startGitCommandMetrics();
    const result = await getCheckoutDiff(repoDir, {
      mode: "base",
      baseRef: "main",
      includeStructured: true,
    });
    const metrics = stopGitCommandMetrics();
    expect(result.structured).toHaveLength(20);
    expect(result.structured?.every((file) => file.additions === 1 && file.deletions === 1)).toBe(
      true,
    );
    expect(metrics.commands.filter(({ args }) => args[0] === "show")).toHaveLength(0);
    expect(metrics.commands.filter(({ args }) => args[0] === "cat-file")).toHaveLength(1);
  });

  it("bounds initial syntax highlighting to the visible file batch", async () => {
    rmSync(tempDir, { recursive: true, force: true });
    const setup = initRepoWithTrackedChanges(20, "ts");
    tempDir = setup.tempDir;
    repoDir = setup.repoDir;

    const result = await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      includeStructured: true,
    });
    const highlighted = result.structured?.filter(hasHighlightTokens);

    expect(result.structured).toHaveLength(20);
    expect(highlighted).toHaveLength(16);
  });

  it("keeps small neighbors when a patch exceeds the entire batch budget", async () => {
    writeFileSync(join(repoDir, "file-0.txt"), "x".repeat(9 * 1024 * 1024) + "\n");
    const result = await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      includeStructured: true,
    });
    expect(result.structured).toHaveLength(20);
    expect(result.structured?.find((file) => file.path === "file-0.txt")).toMatchObject({
      status: "too_large",
      hunks: [],
    });
    expect(result.structured?.filter((file) => file.status === "ok")).toHaveLength(19);
    expect(result.diff).toContain("+after-19");
    expect(result.diff).not.toContain("x".repeat(1_000));
  });

  it("does not launch a Git process for each small untracked file", async () => {
    writeFileSync(join(repoDir, "untracked-0.txt"), "new-0\n");
    startGitCommandMetrics();
    await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      includeStructured: true,
    });
    const oneFileMetrics = stopGitCommandMetrics();

    for (let i = 1; i < 250; i += 1) {
      writeFileSync(join(repoDir, `untracked-${i}.txt`), `new-${i}\n`);
    }
    startGitCommandMetrics();
    const result = await getCheckoutDiff(repoDir, {
      mode: "uncommitted",
      includeStructured: true,
    });
    const manyFileMetrics = stopGitCommandMetrics();

    expect(result.structured).toHaveLength(270);
    expect(manyFileMetrics.commands.filter(({ args }) => args.includes("--no-index"))).toHaveLength(
      0,
    );
    expect(manyFileMetrics.total).toBe(oneFileMetrics.total);
  });
});
