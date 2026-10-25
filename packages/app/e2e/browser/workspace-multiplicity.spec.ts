import { writeFile } from "node:fs/promises";
import path from "node:path";
import { test, expect, type Page } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  assertNewWorkspaceNavigationAndHeader,
  connectNewWorkspaceDaemonClient,
  openGlobalNewWorkspaceComposer,
  selectNewWorkspaceProject,
  submitNewWorkspaceEmpty,
} from "../support/helpers/new-workspace";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import {
  expectExplorerEntryHidden,
  expectExplorerEntryVisible,
} from "../support/helpers/file-explorer";
import { getServerId } from "../support/helpers/server-id";
import { openFilesPanel } from "../support/helpers/workspace-tabs";
import {
  switchWorkspaceViaCommandCenter,
  waitForSidebarHydration,
  waitForWorkspaceInCommandCenter,
} from "../support/helpers/workspace-ui";

// Modern creation always adds a workspace in the selected project's directory.
// Worktree-backed workspaces created through the daemon remain independently
// selectable alongside workspaces sharing the project's original checkout.

async function openFilesTab(page: Page): Promise<void> {
  await openFilesPanel(page);
  await expect(page.getByTestId("file-explorer-tree-scroll")).toBeVisible({ timeout: 30_000 });
}

async function createWorkspaceViaUi(
  page: Page,
  input: {
    project: { projectKey: string; projectDisplayName: string };
    previousWorkspaceId: string;
    client: Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>;
  },
): Promise<{ workspaceId: string; workspaceName: string; workspaceDirectory: string }> {
  await openGlobalNewWorkspaceComposer(page);
  await selectNewWorkspaceProject(page, input.project);
  await submitNewWorkspaceEmpty(page);

  return assertNewWorkspaceNavigationAndHeader(page, {
    serverId: getServerId(),
    client: input.client,
    previousWorkspaceId: input.previousWorkspaceId,
    projectDisplayName: input.project.projectDisplayName,
    assertDiscoverable: false,
    assertHeader: false,
  });
}

test.describe("Workspace multiplicity creation flow", () => {
  let client: Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>;

  test.describe.configure({ timeout: 240_000 });

  test.beforeEach(async () => {
    client = await connectNewWorkspaceDaemonClient();
  });

  test.afterEach(async () => {
    await client?.close().catch(() => undefined);
  });

  test("two Local workspaces share one git checkout and both are independently selectable", async ({
    page,
  }) => {
    const seeded: SeededWorkspace = await seedWorkspace({
      repoPrefix: "multiplicity-local-git-",
    });

    try {
      const project = {
        projectKey: seeded.projectKey,
        projectDisplayName: seeded.projectDisplayName,
      };

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });

      const second = await createWorkspaceViaUi(page, {
        project,
        previousWorkspaceId: seeded.workspaceId,
        client,
      });

      // A second workspace was minted on the SAME checkout — creation did not
      // dedupe the directory away.
      expect(second.workspaceId).not.toBe(seeded.workspaceId);
      expect(second.workspaceDirectory).toBe(seeded.workspaceDirectory);

      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: second.workspaceId,
      });

      // Selecting the second workspace shows the shared checkout's files.
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: second.workspaceId,
      });
      await openFilesTab(page);
      await expectExplorerEntryVisible(page, "README.md");

      // Selecting the first workspace shows the SAME shared directory data.
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });
      await openFilesTab(page);
      await expectExplorerEntryVisible(page, "README.md");
    } finally {
      await seeded.cleanup();
    }
  });

  test("a daemon-created worktree has a distinct directory and independently selectable files", async ({
    page,
  }) => {
    const seeded: SeededWorkspace = await seedWorkspace({
      repoPrefix: "multiplicity-worktree-",
    });
    let worktreeDirectory: string | undefined;

    try {
      const created = await client.createWorkspace({
        source: {
          kind: "worktree",
          cwd: seeded.repoPath,
          action: "branch-off",
          worktreeSlug: "multiplicity-worktree",
        },
      });
      expect(created.error).toBeNull();
      if (!created.workspace) throw new Error("Expected a worktree-backed workspace");
      const worktree = created.workspace;
      worktreeDirectory = worktree.workspaceDirectory;
      expect(worktree.id).not.toBe(seeded.workspaceId);
      if (!worktreeDirectory) throw new Error("Expected a worktree directory");
      expect(worktreeDirectory).not.toBe(seeded.workspaceDirectory);
      expect(worktree.workspaceKind).toBe("worktree");

      await writeFile(path.join(seeded.workspaceDirectory, "checkout-only.txt"), "checkout\n");
      await writeFile(path.join(worktreeDirectory, "worktree-only.txt"), "worktree\n");

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: worktree.id,
      });
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: worktree.id,
      });
      await openFilesTab(page);
      await expectExplorerEntryVisible(page, "README.md");
      await expectExplorerEntryVisible(page, "worktree-only.txt");
      await expectExplorerEntryHidden(page, "checkout-only.txt");

      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });
      await openFilesTab(page);
      await expectExplorerEntryVisible(page, "README.md");
      await expectExplorerEntryVisible(page, "checkout-only.txt");
      await expectExplorerEntryHidden(page, "worktree-only.txt");
    } finally {
      if (worktreeDirectory) {
        await client
          .archivePaseoWorktree({ worktreePath: worktreeDirectory })
          .catch(() => undefined);
      }
      await seeded.cleanup();
    }
  });

  test("two Local workspaces remain selectable in the same non-git project", async ({ page }) => {
    const seeded: SeededWorkspace = await seedWorkspace({
      repoPrefix: "multiplicity-local-nongit-",
      git: false,
    });

    try {
      const project = {
        projectKey: seeded.projectKey,
        projectDisplayName: seeded.projectDisplayName,
      };

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });

      const second = await createWorkspaceViaUi(page, {
        project,
        previousWorkspaceId: seeded.workspaceId,
        client,
      });

      expect(second.workspaceId).not.toBe(seeded.workspaceId);
      expect(second.workspaceDirectory).toBe(seeded.workspaceDirectory);

      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: second.workspaceId,
      });
    } finally {
      await seeded.cleanup();
    }
  });
});
