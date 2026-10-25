import path from "node:path";
import type { Page, TestInfo } from "@playwright/test";
import { test, expect } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  closeMobileAgentSidebar,
  expectMobileAgentSidebarHidden,
  expectMobileAgentSidebarVisible,
  openMobileAgentSidebar,
} from "../support/helpers/sidebar";
import { seedWorkspace } from "../support/helpers/seed-client";
import {
  expectWorkspaceHeader,
  switchWorkspaceViaCommandCenter,
  waitForWorkspaceInCommandCenter,
} from "../support/helpers/workspace-ui";
import { getServerId } from "../support/helpers/server-id";
import { getE2EDaemonPort } from "../support/helpers/daemon-port";
import { openFilesPanel } from "../support/helpers/workspace-tabs";
import { seedMockAgentWorkspace } from "../support/helpers/mock-agent";

const GITHUB_REMOTE_URL = "https://github.com/test-owner/test-repo.git";

function readSessionListScrollTop(element: HTMLElement) {
  let scroll = element.parentElement;
  while (scroll && !["auto", "scroll"].includes(getComputedStyle(scroll).overflowY)) {
    scroll = scroll.parentElement;
  }
  return scroll?.scrollTop;
}

async function readCachedRowIds(
  page: Page,
  rowKind: "workspace" | "checkpoint",
): Promise<string[]> {
  return page.evaluate(
    async ({ serverId, kind }) => {
      const database = await new Promise<IDBDatabase>((resolve, reject) => {
        const request = indexedDB.open("paseo-replica-row-store");
        request.addEventListener("success", () => resolve(request.result));
        request.addEventListener("error", () => reject(request.error));
      });
      try {
        return await new Promise<string[]>((resolve, reject) => {
          const transaction = database.transaction("rows", "readonly");
          const request = transaction.objectStore("rows").getAll();
          request.onsuccess = () =>
            resolve(
              request.result
                .filter(
                  (row: { serverId: string; kind: string }) =>
                    row.serverId === serverId && row.kind === kind,
                )
                .map((row: { id: string }) => row.id)
                .sort(),
            );
          request.addEventListener("error", () => reject(request.error));
        });
      } finally {
        database.close();
      }
    },
    { serverId: getServerId(), kind: rowKind },
  );
}

async function simulateDamagedLegacyDirectoryCache(
  page: Page,
  workspaceIdToRemove: string,
): Promise<void> {
  // Stop the app before altering storage so no pending app write can repair the fixture.
  const fixtureUrl = new URL("/__directory_cache_fixture", page.url()).href;
  await page.route(fixtureUrl, (route) =>
    route.fulfill({ contentType: "text/html", body: "<html><body>Cache fixture</body></html>" }),
  );
  await page.goto(fixtureUrl);
  await page.evaluate(
    async ({ serverId, missingWorkspaceId }) => {
      const database = await new Promise<IDBDatabase>((resolve, reject) => {
        const request = indexedDB.open("paseo-replica-row-store");
        request.addEventListener("success", () => resolve(request.result));
        request.addEventListener("error", () => reject(request.error));
      });
      try {
        await new Promise<void>((resolve, reject) => {
          const transaction = database.transaction("rows", "readwrite");
          const store = transaction.objectStore("rows");
          const request = store.getAll();
          request.onsuccess = () => {
            const rows = request.result as Array<{
              serverId: string;
              kind: string;
              id: string;
              payload: string;
            }>;
            const checkpoint = rows.find(
              (row) => row.serverId === serverId && row.kind === "checkpoint",
            );
            if (!checkpoint) {
              transaction.abort();
              return;
            }
            const stored = JSON.parse(checkpoint.payload);
            // Accept either writer so this fixture also reproduces on the unfixed client.
            const cursors = stored.version === 1 ? stored.cursors : stored;
            if (!cursors.workspaces?.generation) {
              transaction.abort();
              return;
            }
            store.put({ ...checkpoint, payload: JSON.stringify(cursors) });
            for (const row of rows) {
              if (
                row.serverId === serverId &&
                row.kind === "workspace" &&
                row.id === missingWorkspaceId
              )
                store.delete([serverId, row.kind, row.id]);
            }
          };
          transaction.addEventListener("complete", () => resolve());
          transaction.addEventListener("abort", () =>
            reject(transaction.error ?? new Error("Missing current directory checkpoint")),
          );
          transaction.addEventListener("error", () => reject(transaction.error));
        });
      } finally {
        database.close();
      }
    },
    { serverId: getServerId(), missingWorkspaceId: workspaceIdToRemove },
  );
  expect(await readCachedRowIds(page, "workspace")).not.toContain(workspaceIdToRemove);
}

async function holdHostResponsesUntilCacheIsVisible(page: Page): Promise<() => void> {
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.routeWebSocket(new RegExp(`:${getE2EDaemonPort()}(?:/|$)`), (socket) => {
    const server = socket.connectToServer();
    server.onMessage(async (message) => {
      await ready;
      socket.send(message);
    });
  });
  return release;
}

async function createPinnedSiblingWorkspace(
  project: Awaited<ReturnType<typeof seedWorkspace>>,
): Promise<string> {
  const sibling = await project.client.createWorkspace({
    source: { kind: "directory", path: project.repoPath, projectId: project.projectId },
    title: "Pinned sibling",
  });
  if (!sibling.workspace) throw new Error(sibling.error ?? "Sibling workspace was not created");
  await project.client.setWorkspacePinned(sibling.workspace.id, true);
  return sibling.workspace.id;
}

async function expectCommandCenterWorkspaces(page: Page, workspaceIds: string[]): Promise<void> {
  for (const workspaceId of workspaceIds) {
    await waitForWorkspaceInCommandCenter(page, { serverId: getServerId(), workspaceId });
  }
}

async function cacheWorkspaces(page: Page, workspaceIds: string[]): Promise<void> {
  await gotoAppShell(page);
  await expectCommandCenterWorkspaces(page, workspaceIds);
  await expect.poll(() => readCachedRowIds(page, "workspace")).toEqual(workspaceIds);
}

async function reopenDamagedCacheBeforeHostResponds(
  page: Page,
  missingWorkspaceId: string,
): Promise<() => void> {
  await simulateDamagedLegacyDirectoryCache(page, missingWorkspaceId);
  const releaseHost = await holdHostResponsesUntilCacheIsVisible(page);
  await gotoAppShell(page);
  return releaseHost;
}

async function expectDamagedCacheHydrated(
  page: Page,
  retainedWorkspaceId: string,
  missingWorkspaceId: string,
): Promise<void> {
  await waitForWorkspaceInCommandCenter(page, {
    serverId: getServerId(),
    workspaceId: retainedWorkspaceId,
  });
  expect(await readCachedRowIds(page, "workspace")).not.toContain(missingWorkspaceId);
  await expect.poll(() => readCachedRowIds(page, "checkpoint")).toEqual([]);
}

async function expectRecoveredWorkspacesAfterReload(
  page: Page,
  workspaceIds: string[],
  testInfo: TestInfo,
): Promise<void> {
  await expectCommandCenterWorkspaces(page, workspaceIds);
  await expect.poll(() => readCachedRowIds(page, "workspace")).toEqual(workspaceIds);
  await page.reload();
  await expectCommandCenterWorkspaces(page, workspaceIds);
  await page.screenshot({ path: testInfo.outputPath("recovered-workspaces.png") });
  await testInfo.attach("recovered-workspaces", {
    path: testInfo.outputPath("recovered-workspaces.png"),
    contentType: "image/png",
  });
}

test.describe("Workspace navigation", () => {
  test("recovers missing workspaces from a damaged legacy cache after reload", async ({
    page,
  }, testInfo) => {
    const project = await seedWorkspace({ repoPrefix: "directory-cache-recovery-" });
    try {
      const siblingId = await createPinnedSiblingWorkspace(project);
      const ids = [project.workspaceId, siblingId].sort();
      await test.step("cache both workspaces", () => cacheWorkspaces(page, ids));

      const releaseHost = await test.step("reopen damaged cache before host responds", () =>
        reopenDamagedCacheBeforeHostResponds(page, project.workspaceId));
      await test.step("keep cached workspace and discard stale checkpoint", () =>
        expectDamagedCacheHydrated(page, siblingId, project.workspaceId));

      releaseHost();
      await test.step("recover missing workspace and retain both after reload", () =>
        expectRecoveredWorkspacesAfterReload(page, ids, testInfo));
    } finally {
      await project.cleanup();
    }
  });

  test("workspace header uses the selected folder name instead of its GitHub remote", async ({
    page,
  }) => {
    const workspace = await seedWorkspace({
      repoPrefix: "sidebar-header-",
      repo: { withRemote: true, originUrl: GITHUB_REMOTE_URL },
    });

    try {
      const projectName = path.basename(workspace.repoPath);
      await gotoAppShell(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: workspace.workspaceId,
      });

      await expectWorkspaceHeader(page, {
        title: workspace.workspaceName,
        subtitle: projectName,
      });
    } finally {
      await workspace.cleanup();
    }
  });
});

test.describe("Mobile sidebar panelState transition", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("showMobileAgent open and close transition", async ({ page }) => {
    await gotoAppShell(page);
    await expectMobileAgentSidebarHidden(page);
    await openMobileAgentSidebar(page);
    await expectMobileAgentSidebarVisible(page);
    await closeMobileAgentSidebar(page);
    await expectMobileAgentSidebarHidden(page);
  });

  test("places the close button beside the first nav row instead of over it", async ({ page }) => {
    await gotoAppShell(page);
    await openMobileAgentSidebar(page);
    await expectMobileAgentSidebarVisible(page);

    const firstRow = await page.getByTestId("sidebar-global-new-workspace").boundingBox();
    const closeButton = await page.getByTestId("sidebar-close").boundingBox();
    if (!firstRow || !closeButton) throw new Error("first nav row and close button must render");
    expect(closeButton.x).toBeGreaterThanOrEqual(firstRow.x + firstRow.width);
  });

  test("keeps a session rendered while the retained sidebar is closed", async ({ page }) => {
    const workspace = await seedMockAgentWorkspace({
      repoPrefix: "sidebar-retained-session-",
      title: "Retained session",
    });

    try {
      await gotoAppShell(page);
      await openMobileAgentSidebar(page);
      await expectMobileAgentSidebarVisible(page);

      const row = page.getByTestId(`session-row-${getServerId()}-${workspace.agentId}`);
      await expect(row).toBeVisible({ timeout: 30_000 });

      await closeMobileAgentSidebar(page);
      await expectMobileAgentSidebarHidden(page);

      await expect(row).toHaveCount(1);
    } finally {
      await workspace.cleanup();
    }
  });
});

test.describe("Half-screen desktop layout", () => {
  test.use({ viewport: { width: 751, height: 982 } });

  test("keeps the sidebar scroll position across close and reopen", async ({ page }) => {
    const workspace = await seedMockAgentWorkspace({
      repoPrefix: "sidebar-retained-scroll-",
      title: "Retained scroll session",
    });

    try {
      for (let index = 0; index < 24; index += 1) {
        await workspace.client.createAgent({
          provider: "mock",
          cwd: workspace.cwd,
          workspaceId: workspace.workspaceId,
          title: `Retained sidebar ${index + 1}`,
          modeId: "load-test",
          model: "e2e-fast-stream",
        });
      }

      await gotoAppShell(page);
      const row = page.getByTestId(`session-row-${getServerId()}-${workspace.agentId}`);
      await expect(row).toBeVisible({ timeout: 30_000 });
      const scrollTop = await row.evaluate((element) => {
        let scroll = element.parentElement;
        while (scroll && !["auto", "scroll"].includes(getComputedStyle(scroll).overflowY)) {
          scroll = scroll.parentElement;
        }
        if (!scroll) throw new Error("Session list scroll container is missing");
        scroll.scrollTop = 160;
        return scroll.scrollTop;
      });
      expect(scrollTop).toBe(160);

      await page.getByTestId("menu-button").click();
      await expect(page.getByTestId("sidebar-global-new-workspace")).not.toBeVisible();

      await page.getByTestId("menu-button").click();
      await expect(page.getByTestId("sidebar-global-new-workspace")).toBeVisible();
      await expect.poll(() => row.evaluate(readSessionListScrollTop)).toBe(scrollTop);
    } finally {
      await workspace.cleanup();
    }
  });

  test("keeps the pinned sidebar at half of a 14-inch Mac display", async ({ page }) => {
    await gotoAppShell(page);
    await expect(page.getByTestId("sidebar-global-new-workspace")).toBeVisible();
    await expect(page.getByTestId("agent-list-backdrop")).not.toBeVisible();
  });

  test("keeps the left toggle center-owned without left window controls", async ({ page }) => {
    await gotoAppShell(page);

    const openToggle = page.getByTestId("menu-button");
    const openIcon = openToggle.locator("svg").first();
    await expect(openIcon).toBeVisible();
    const openBounds = await openIcon.boundingBox();
    expect(openBounds).not.toBeNull();
    expect(openBounds?.x).toBeGreaterThan(12);

    await openToggle.click();
    await expect(page.getByTestId("sidebar-global-new-workspace")).not.toBeVisible();

    const closedToggle = page.getByTestId("menu-button");
    const closedIcon = closedToggle.locator("svg").first();
    await expect(closedIcon).toBeVisible();
    const closedBounds = await closedIcon.boundingBox();
    expect(closedBounds).not.toBeNull();
    expect(closedBounds?.x).toBeCloseTo(9, 0);
    expect(closedBounds?.y).toBe(openBounds?.y);
  });

  test("yields app navigation to the settings split", async ({ page }) => {
    await gotoAppShell(page);
    await page.getByTestId("sidebar-settings").click();

    await expect(page.getByTestId("settings-sidebar")).toBeVisible();
    await expect(page.getByTestId("settings-detail-pane")).toBeVisible();
    await expect(page.getByTestId("sidebar-settings")).not.toBeVisible();
  });

  test("yields app navigation to Explorer and restores it when Explorer closes", async ({
    page,
  }) => {
    const workspace = await seedWorkspace({ repoPrefix: "sidebar-half-screen-explorer-" });

    try {
      await gotoAppShell(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: workspace.workspaceId,
      });

      await openFilesPanel(page);
      const explorerToggle = page.getByTestId("workspace-explorer-toggle").first();
      await expect(page.getByTestId("workspace-tab-files").filter({ visible: true })).toBeVisible();
      await expect(explorerToggle).toHaveAccessibleName("Close Explorer sidebar");
      await expect(page.getByTestId("sidebar-global-new-workspace")).not.toBeVisible();
      await expect(page.getByTestId("workspace-explorer-sidebar")).toBeVisible();
      await expect(
        page
          .locator('[data-testid^="workspace-pane-"]')
          .getByTestId("workspace-tabs-row")
          .filter({ visible: true }),
      ).toHaveCount(1);

      await explorerToggle.click();
      await expect(page.getByTestId("workspace-tab-files").filter({ visible: true })).toHaveCount(
        0,
      );
      await expect(explorerToggle).toHaveAccessibleName("Open Explorer sidebar");
      await expect(page.getByTestId("sidebar-global-new-workspace")).toBeVisible();
    } finally {
      await workspace.cleanup();
    }
  });
});

test.describe("session-first navigation", () => {
  test("opens individual conversations and ignores the removed workspace preference", async ({
    page,
  }, testInfo) => {
    const first = await seedMockAgentWorkspace({ repoPrefix: "session-nav-", title: "Fix login" });
    try {
      const second = await first.client.createAgent({
        provider: "mock",
        cwd: first.cwd,
        workspaceId: first.workspaceId,
        title: "Review login",
        modeId: "load-test",
        model: "e2e-fast-stream",
      });
      await gotoAppShell(page);
      const firstRow = page.getByTestId(`session-row-${getServerId()}-${first.agentId}`);
      const secondRow = page.getByTestId(`session-row-${getServerId()}-${second.id}`);
      await expect(firstRow).toBeVisible({ timeout: 30_000 });
      await expect(secondRow).toBeVisible();
      await secondRow.click();
      await expect(secondRow).toHaveAttribute("aria-selected", "true");
      await expect(page.getByTestId("workspace-header-title")).toHaveText("Review login");
      await firstRow.click();
      await expect(firstRow).toHaveAttribute("aria-selected", "true");
      await expect(secondRow).toHaveAttribute("aria-selected", "false");
      await page.getByTestId("session-search").fill("Review");
      await expect(firstRow).toHaveCount(0);
      await expect(secondRow).toBeVisible();
      await page.getByTestId("session-search").fill("");
      await page.getByTestId(`session-rename-${getServerId()}-${second.id}`).click();
      await page.getByTestId("session-rename-modal-input").fill("Review authentication");
      await page.getByTestId("session-rename-modal-submit").click();
      await expect(secondRow).toContainText("Review authentication");
      await secondRow.click();
      await expect(page.getByTestId("workspace-header-title")).toHaveText("Review authentication");
      await page.getByTestId("sidebar-display-preferences-menu").click();
      await expect(page.getByTestId("sidebar-display-preferences-content")).toBeVisible();
      await expect(page.getByTestId("sidebar-display-grouping")).toHaveCount(0);
      await expect(page.getByTestId("sidebar-display-title-source")).toHaveCount(0);
      await page.keyboard.press("Escape");
      await page.screenshot({ path: testInfo.outputPath("sessions-desktop.png") });
      await page.evaluate(() =>
        localStorage.setItem(
          "paseo-session-navigation",
          JSON.stringify({ state: { sessionFirst: false }, version: 0 }),
        ),
      );
      await page.reload();
      await expect(firstRow).toBeVisible();
      await expect(page.getByTestId("session-navigation-toggle")).toHaveCount(0);
      await page.getByRole("button", { name: "New session", exact: true }).first().click();
      await expect(page.getByTestId("workspace-create-isolation-trigger")).toHaveCount(0);
      await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
    } finally {
      await first.cleanup();
    }
  });

  test("opens a conversation from the compact sidebar", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 412, height: 915 });
    const agent = await seedMockAgentWorkspace({
      repoPrefix: "session-mobile-",
      title: "Mobile session",
    });
    try {
      await gotoAppShell(page);
      await openMobileAgentSidebar(page);
      const row = page.getByTestId(`session-row-${getServerId()}-${agent.agentId}`);
      await expect(row).toBeVisible({ timeout: 30_000 });
      await page.screenshot({ path: testInfo.outputPath("sessions-mobile.png") });
      await row.click();
      await expectMobileAgentSidebarHidden(page);
      await expect(page.getByTestId("workspace-header-title")).toHaveText("Mobile session");
      await expect(page).toHaveURL(/\/workspace\//);
      await openMobileAgentSidebar(page);
      await expect(row).toHaveAttribute("aria-selected", "true");
    } finally {
      await agent.cleanup();
    }
  });
});
