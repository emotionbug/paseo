import path from "node:path";
import { existsSync } from "node:fs";
import { test, expect, type Page } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  addProjectFlowInput,
  chooseAddProjectMethod,
  openAddProjectFlow,
} from "../support/helpers/add-project-flow";
import { expectOpenedProject } from "../support/helpers/project-picker-ui";
import { connectSeedClient, seedWorkspace } from "../support/helpers/seed-client";
import {
  openNewWorkspaceComposer,
  expectNewWorkspaceProjectSelected,
} from "../support/helpers/new-workspace";
import { archiveWorkspaceFromCommandCenter } from "../support/helpers/sidebar";
import { switchWorkspaceViaCommandCenter } from "../support/helpers/workspace-ui";
import { getServerId } from "../support/helpers/server-id";
import { createTempGitRepo } from "../support/helpers/workspace";
import { waitForSidebarHydration } from "../support/helpers/workspace-ui";

async function addProjectFromPicker(page: Page, projectPath: string): Promise<string> {
  await openAddProjectFlow(page);
  await chooseAddProjectMethod(page, "directory-search");

  const input = addProjectFlowInput(page);
  await input.fill(projectPath);
  await page.keyboard.press("Enter");

  return expectOpenedProject(page, path.basename(projectPath));
}

test.describe("Project picker search", () => {
  test("opens a project from a fuzzy directory-name search", async ({
    page,
    projectPickerFixture,
  }) => {
    await gotoAppShell(page);
    await waitForSidebarHydration(page);
    await openAddProjectFlow(page);
    await chooseAddProjectMethod(page, "directory-search");

    const input = addProjectFlowInput(page);
    await input.fill(projectPickerFixture.fuzzyQuery);

    const suggestion = page.getByText(projectPickerFixture.projectName, { exact: false }).first();
    await expect(suggestion).toBeVisible({ timeout: 30_000 });
    await suggestion.click();

    const projectId = await expectOpenedProject(page, projectPickerFixture.projectName);
    projectPickerFixture.rememberProjectId(projectId);
  });

  test("shows a loading state after typing while directory suggestions are pending", async ({
    page,
  }) => {
    await gotoAppShell(page);
    await waitForSidebarHydration(page);
    await openAddProjectFlow(page);
    await chooseAddProjectMethod(page, "directory-search");

    const input = addProjectFlowInput(page);
    await input.fill("paseo-loading-state-no-match");

    await expect(page.getByText("Start typing a path", { exact: true })).toHaveCount(0);
    await expect(page.getByText("Loading...", { exact: true })).toBeVisible();
  });
});

test.describe("Project with no workspaces persists", () => {
  test("adding a project opens creation without creating an empty workspace", async ({ page }) => {
    const repo = await createTempGitRepo("empty-project-add-");
    const client = await connectSeedClient();
    let projectId: string | null = null;

    try {
      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      projectId = await addProjectFromPicker(page, repo.path);
      await expectNewWorkspaceProjectSelected(page, path.basename(repo.path));
      await page.reload();
      await expectNewWorkspaceProjectSelected(page, path.basename(repo.path));

      const workspaces = await client.fetchWorkspaces({ filter: { projectId } });
      expect(workspaces.entries).toEqual([]);
    } finally {
      if (projectId) {
        await client.removeProject(projectId).catch(() => undefined);
      }
      await client.close().catch(() => undefined);
      await repo.cleanup().catch(() => undefined);
    }
  });

  test("archiving the only workspace keeps its project available for creation", async ({
    page,
  }) => {
    const workspace = await seedWorkspace({ repoPrefix: "empty-project-persists-" });

    try {
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: workspace.workspaceId,
      });
      await expect(page.getByTestId("changes-primary-cta")).toHaveCount(0);
      await archiveWorkspaceFromCommandCenter(page, workspace.workspaceId);
      expect(existsSync(workspace.repoPath)).toBe(true);
      await openNewWorkspaceComposer(page, workspace);
      await expectNewWorkspaceProjectSelected(page, workspace.projectDisplayName);
      await page.reload();
      await expectNewWorkspaceProjectSelected(page, workspace.projectDisplayName);
    } finally {
      await workspace.cleanup();
    }
  });
});
