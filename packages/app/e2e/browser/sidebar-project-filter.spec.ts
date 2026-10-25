import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { seedWorkspace } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { projectEquivalenceViewKey } from "../support/helpers/project-view-key";
import {
  closeSidebarDisplayPreferences,
  openSidebarProjectFilter,
  selectAllProjectsFilter,
  toggleProjectFilter,
} from "../support/helpers/sidebar";

test("project filtering narrows sessions and survives reload", async ({ page }) => {
  const alpha = await seedWorkspace({ repoPrefix: "project-filter-alpha-" });
  const beta = await seedWorkspace({ repoPrefix: "project-filter-beta-" });
  try {
    const agents = await Promise.all(
      [alpha, beta].map((workspace, index) =>
        workspace.client.createAgent({
          provider: "mock",
          cwd: workspace.repoPath,
          workspaceId: workspace.workspaceId,
          title: index === 0 ? "Alpha session" : "Beta session",
          modeId: "load-test",
          model: "e2e-fast-stream",
        }),
      ),
    );
    const alphaRow = page.getByTestId(`session-row-${getServerId()}-${agents[0]!.id}`);
    const betaRow = page.getByTestId(`session-row-${getServerId()}-${agents[1]!.id}`);
    await gotoAppShell(page);
    await expect(alphaRow).toBeVisible({ timeout: 30_000 });
    await expect(betaRow).toBeVisible();
    await openSidebarProjectFilter(page);
    await toggleProjectFilter(page, projectEquivalenceViewKey(alpha.projectKey));
    await closeSidebarDisplayPreferences(page);
    await expect(alphaRow).toBeVisible();
    await expect(betaRow).toHaveCount(0);
    await page.reload();
    await expect(alphaRow).toBeVisible({ timeout: 30_000 });
    await expect(betaRow).toHaveCount(0);
    await openSidebarProjectFilter(page);
    await selectAllProjectsFilter(page);
    await closeSidebarDisplayPreferences(page);
    await expect(alphaRow).toBeVisible();
    await expect(betaRow).toBeVisible();
  } finally {
    await beta.cleanup();
    await alpha.cleanup();
  }
});

test("hides the project filter when there is only one project", async ({ page }) => {
  const workspace = await seedWorkspace({ repoPrefix: "project-filter-solo-" });
  try {
    await gotoAppShell(page);
    await expect(page.getByTestId(`session-host-${getServerId()}`)).toBeVisible({
      timeout: 30_000,
    });
    await page.getByTestId("sidebar-display-preferences-menu").click();
    await expect(page.getByTestId("sidebar-display-project-filter")).toHaveCount(0);
  } finally {
    await workspace.cleanup();
  }
});
