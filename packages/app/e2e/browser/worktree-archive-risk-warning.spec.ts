import { execSync } from "node:child_process";
import { existsSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import path from "node:path";
import type { Dialog, Page } from "@playwright/test";
import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  archiveWorkspaceFromDaemon,
  connectNewWorkspaceDaemonClient,
  createWorktreeViaDaemon,
  openProjectViaDaemon,
} from "../support/helpers/new-workspace";
import { getServerId } from "../support/helpers/server-id";
import {
  clickArchiveWorkspaceCommand,
  expectWorkspaceAbsentFromCommandCenter,
} from "../support/helpers/sidebar";
import { createTempGitRepo } from "../support/helpers/workspace";
import {
  waitForSidebarHydration,
  waitForWorkspaceInCommandCenter,
} from "../support/helpers/workspace-ui";

async function seedRiskyWorktree(
  client: Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>,
  worktreeDirectory: string,
  risk: "uncommitted" | "unpushed",
): Promise<void> {
  const branch = execSync("git rev-parse --abbrev-ref HEAD", {
    cwd: worktreeDirectory,
    stdio: "pipe",
  })
    .toString()
    .trim();
  execSync(`git push -u origin ${JSON.stringify(branch)}`, {
    cwd: worktreeDirectory,
    stdio: "ignore",
  });

  if (risk === "unpushed") {
    const committedFile = path.join(worktreeDirectory, "UNPUSHED.md");
    await writeFile(committedFile, "# unpushed\n");
    execSync(`git add ${JSON.stringify(path.basename(committedFile))}`, {
      cwd: worktreeDirectory,
      stdio: "ignore",
    });
    execSync('git commit -m "Add unpushed change"', {
      cwd: worktreeDirectory,
      stdio: "ignore",
    });
  } else {
    await writeFile(path.join(worktreeDirectory, "DIRTY.md"), "# dirty\n");
  }

  // Refresh is throttled/coalesced with watcher reads. Wait until the daemon sees
  // the externally seeded Git state before exercising its archive confirmation.
  await expect
    .poll(async () => {
      const refreshed = await client.checkoutRefresh(worktreeDirectory);
      if (!refreshed.success) {
        throw new Error(`Failed to refresh checkout for ${worktreeDirectory}`);
      }
      const status = await client.getCheckoutStatus(worktreeDirectory);
      return { isDirty: status.isDirty, aheadOfOrigin: status.aheadOfOrigin };
    })
    .toEqual({ isDirty: risk === "uncommitted", aheadOfOrigin: risk === "unpushed" ? 1 : 0 });
}

// The archive confirmation is a synchronous web `window.confirm()`. The click that
// opens it does not resolve until the dialog is answered, so the handler must
// accept/dismiss inline — awaiting the dialog only *after* the click deadlocks, as
// the click waits for an answer that is gated behind that same click.
async function clickArchiveAndAnswerWarning(
  page: Page,
  workspaceId: string,
  answer: "accept" | "dismiss",
): Promise<Dialog> {
  let warning: Dialog | undefined;
  page.once("dialog", (dialog) => {
    warning = dialog;
    void (answer === "accept" ? dialog.accept() : dialog.dismiss());
  });
  await clickArchiveWorkspaceCommand(page, workspaceId);
  if (!warning) {
    throw new Error("Expected an archive confirmation dialog, but none was shown.");
  }
  return warning;
}

test.describe("Workspace archive risk warning for worktree backing", () => {
  let client: Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>;
  let tempRepo: { path: string; cleanup: () => Promise<void> };
  const createdWorktreeDirectories = new Set<string>();

  test.describe.configure({ timeout: 120_000 });

  test.beforeEach(async () => {
    client = await connectNewWorkspaceDaemonClient();
    tempRepo = await createTempGitRepo("wt-archive-risk-", { withRemote: true });
  });

  test.afterEach(async () => {
    for (const directory of createdWorktreeDirectories) {
      await archiveWorkspaceFromDaemon(client, directory).catch(() => undefined);
    }
    createdWorktreeDirectories.clear();
    await client?.close().catch(() => undefined);
    await tempRepo?.cleanup().catch(() => undefined);
  });

  for (const risk of ["uncommitted", "unpushed"] as const) {
    test(`${risk} changes require confirmation before the worktree is removed`, async ({
      page,
    }) => {
      const serverId = getServerId();
      await openProjectViaDaemon(client, tempRepo.path);
      const worktree = await createWorktreeViaDaemon(client, {
        cwd: tempRepo.path,
        slug: `archive-risk-${Date.now()}`,
      });
      createdWorktreeDirectories.add(worktree.workspaceDirectory);
      expect(existsSync(worktree.workspaceDirectory)).toBe(true);

      await seedRiskyWorktree(client, worktree.workspaceDirectory, risk);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await waitForWorkspaceInCommandCenter(page, { serverId, workspaceId: worktree.workspaceId });

      const warning = await clickArchiveAndAnswerWarning(page, worktree.workspaceId, "dismiss");
      expect(warning.type()).toBe("confirm");
      await waitForWorkspaceInCommandCenter(page, { serverId, workspaceId: worktree.workspaceId });
      expect(existsSync(worktree.workspaceDirectory)).toBe(true);

      await clickArchiveAndAnswerWarning(page, worktree.workspaceId, "accept");
      await expectWorkspaceAbsentFromCommandCenter(page, worktree.workspaceId);
      await expect
        .poll(() => existsSync(worktree.workspaceDirectory), { timeout: 30_000 })
        .toBe(false);

      createdWorktreeDirectories.delete(worktree.workspaceDirectory);
    });
  }
});
