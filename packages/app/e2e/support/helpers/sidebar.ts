import { expect, type Page } from "@playwright/test";
import { getServerId } from "./server-id";
import { openCommandCenter, closeCommandCenter } from "./command-center";
import { switchWorkspaceViaCommandCenter } from "./workspace-ui";

export async function clickArchiveWorkspaceCommand(page: Page, workspaceId: string): Promise<void> {
  await switchWorkspaceViaCommandCenter({ page, serverId: getServerId(), workspaceId });
  const panel = await openCommandCenter(page);
  await panel.getByTestId("command-center-input").fill("archive");
  await panel.getByRole("button", { name: /^Archive workspace(?:\s|$)/ }).click();
}

export async function archiveWorkspaceFromCommandCenter(
  page: Page,
  workspaceId: string,
): Promise<void> {
  page.once("dialog", (dialog) => void dialog.accept());
  await clickArchiveWorkspaceCommand(page, workspaceId);
}

export async function expectWorkspaceAbsentFromCommandCenter(
  page: Page,
  workspaceId: string,
): Promise<void> {
  const panel = await openCommandCenter(page);
  await expect(
    panel.getByTestId(`command-center-workspace-${getServerId()}:${workspaceId}`),
  ).toHaveCount(0, { timeout: 30_000 });
  await closeCommandCenter(page);
}

// The display-preferences menu is one row per decision, and the options sit a page below that
// row. Every caller has to walk the same path, so it lives here: when the menu's shape moves
// again, this is the only place that has to follow.
export async function openSidebarDisplayPage(page: Page, branchTestID: string): Promise<void> {
  await page.getByTestId("sidebar-display-preferences-menu").click();
  await page.getByTestId(branchTestID).click();
}

// Project filters live a page below the display-preferences root, like the host filters.
export async function openSidebarProjectFilter(page: Page): Promise<void> {
  await openSidebarDisplayPage(page, "sidebar-display-project-filter");
}

// The filter rows are keyed by the project's sidebar view key. A project seeded from its own temp
// repo has a `projectKey` unique on the host, so the view key IS that key — see
// `buildWorkspaceStructureProjects`.
export async function toggleProjectFilter(page: Page, projectViewKey: string): Promise<void> {
  await page.getByTestId(`sidebar-project-filter-${projectViewKey}`).click();
}

export async function selectAllProjectsFilter(page: Page): Promise<void> {
  await page.getByTestId("sidebar-project-filter-all").click();
}

export async function closeSidebarDisplayPreferences(page: Page): Promise<void> {
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("sidebar-display-preferences-content")).toHaveCount(0);
}

export async function openMobileAgentSidebar(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Open menu" }).click();
}

export async function closeMobileAgentSidebar(page: Page): Promise<void> {
  const closeButton = page.getByTestId("sidebar-close");
  await expect(closeButton).toBeInViewport({ ratio: 1, timeout: 5_000 });
  await closeButton.click();
}

// The mobile sidebar panel animates via translateX. Waiting for its header to be fully visible
// prevents a close click from targeting a button while the panel is still moving.
export async function expectMobileAgentSidebarVisible(page: Page): Promise<void> {
  await expect(page.getByTestId("sidebar-sessions")).toBeInViewport({ ratio: 1, timeout: 5_000 });
}

export async function expectMobileAgentSidebarHidden(page: Page): Promise<void> {
  await expect(page.getByTestId("sidebar-sessions")).not.toBeInViewport({ timeout: 5_000 });
}

/** Drag from the left edge: the only way to open the list while fullscreen hides the header. */
export async function swipeOpenMobileAgentSidebar(page: Page): Promise<void> {
  const viewport = page.viewportSize();
  if (!viewport) throw new Error("viewport size is required");
  const y = Math.round(viewport.height / 2);
  await page.mouse.move(8, y);
  await page.mouse.down();
  await page.mouse.move(40, y, { steps: 4 });
  await page.mouse.move(Math.round(viewport.width * 0.7), y, { steps: 12 });
  await page.mouse.up();
}

/** Drag from the right edge to open the compact Explorer over the agent. */
export async function swipeOpenMobileExplorer(page: Page): Promise<void> {
  const viewport = page.viewportSize();
  if (!viewport) throw new Error("viewport size is required");
  const y = Math.round(viewport.height / 2);
  await page.mouse.move(viewport.width - 8, y);
  await page.mouse.down();
  await page.mouse.move(viewport.width - 40, y, { steps: 4 });
  await page.mouse.move(Math.round(viewport.width * 0.3), y, { steps: 12 });
  await page.mouse.up();
}

export async function closeMobileExplorer(page: Page): Promise<void> {
  const closeButton = page
    .getByTestId("workspace-explorer-sidebar")
    .getByRole("button", { name: "Close Explorer sidebar", exact: true });
  await expect(closeButton).toBeInViewport({ ratio: 1, timeout: 5_000 });
  await closeButton.click();
}

export async function expectMobileExplorerVisible(page: Page): Promise<void> {
  await expect(page.getByTestId("workspace-explorer-sidebar")).toBeInViewport({
    ratio: 1,
    timeout: 5_000,
  });
}

export async function expectMobileExplorerHidden(page: Page): Promise<void> {
  await expect(page.getByTestId("workspace-explorer-sidebar")).not.toBeInViewport({
    timeout: 5_000,
  });
}
