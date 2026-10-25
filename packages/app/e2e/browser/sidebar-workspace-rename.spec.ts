import { test, expect, type Page } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { seedWorkspace } from "../support/helpers/seed-client";
import { openAgentRoute, seedMockAgentWorkspace } from "../support/helpers/mock-agent";
import { getServerId } from "../support/helpers/server-id";

test("renaming a session survives reload", async ({ page }) => {
  const workspace = await seedMockAgentWorkspace({
    repoPrefix: "sidebar-session-rename-",
    title: "Original session",
  });
  try {
    await gotoAppShell(page);
    const row = page.getByTestId(`session-row-${getServerId()}-${workspace.agentId}`);
    await expect(row).toBeVisible({ timeout: 30_000 });
    await page.getByTestId(`session-rename-${getServerId()}-${workspace.agentId}`).click();
    const input = page.getByTestId("session-rename-modal-input");
    await expect(input).toHaveValue("Original session");
    await input.fill("Payments Refactor");
    await page.getByTestId("session-rename-modal-submit").click();
    await expect(input).toHaveCount(0);
    await expect(row).toContainText("Payments Refactor");
    await page.reload();
    await expect(row).toContainText("Payments Refactor", { timeout: 30_000 });
  } finally {
    await workspace.cleanup();
  }
});

async function toggleBackgroundWorkspaces(page: Page): Promise<void> {
  await page.getByLabel("Display preferences", { exact: true }).click();
  await page.getByRole("menuitem", { name: "Show background", exact: true }).click();
  await page.keyboard.press("Escape");
}

async function expectWorkspaceInSidebar(page: Page, agentId: string): Promise<void> {
  await expect(page.getByTestId(`session-row-${getServerId()}-${agentId}`)).toBeVisible({
    timeout: 30_000,
  });
}

async function expectWorkspaceHidden(page: Page, agentId: string): Promise<void> {
  await expect(page.getByTestId(`session-row-${getServerId()}-${agentId}`)).toHaveCount(0);
}

async function reconnectApp(page: Page): Promise<void> {
  await page.reload();
  await expect(page.getByLabel("Display preferences", { exact: true })).toBeVisible();
}

async function expectWorkerOpen(page: Page): Promise<void> {
  await expect(page.getByRole("textbox", { name: "Message agent..." })).toBeEditable({
    timeout: 30_000,
  });
  await expect(page.getByText("Background reviewer", { exact: true }).first()).toBeVisible();
  await expect(page.getByText("Background review prompt", { exact: true }).first()).toBeVisible();
}

test("background workspace stays directly accessible across sidebar toggles and reconnect", async ({
  page,
}, testInfo) => {
  const workspace = await seedWorkspace({
    repoPrefix: "sidebar-background-",
    title: "Background review",
    background: true,
  });
  try {
    const worker = await workspace.client.createAgent({
      provider: "mock",
      cwd: workspace.repoPath,
      workspaceId: workspace.workspaceId,
      title: "Background reviewer",
      initialPrompt: "Background review prompt",
      model: "e2e-fast-stream",
      modeId: "load-test",
    });
    await test.step("Default sidebar hides background work", async () => {
      await gotoAppShell(page);
      await expectWorkspaceHidden(page, worker.id);
    });
    await test.step("Show background exposes the fully replicated workspace", async () => {
      await toggleBackgroundWorkspaces(page);
      await expectWorkspaceInSidebar(page, worker.id);
      await reconnectApp(page);
      await expectWorkspaceInSidebar(page, worker.id);
    });
    await test.step("Direct agent navigation survives hiding and reconnect", async () => {
      await toggleBackgroundWorkspaces(page);
      await expectWorkspaceHidden(page, worker.id);
      await openAgentRoute(page, { workspaceId: workspace.workspaceId, agentId: worker.id });
      await expectWorkerOpen(page);
      await expectWorkspaceHidden(page, worker.id);
      await reconnectApp(page);
      await expectWorkerOpen(page);
      await expectWorkspaceHidden(page, worker.id);
      await page.screenshot({ path: testInfo.outputPath("background-direct-open.png") });
    });
  } finally {
    await workspace.cleanup();
  }
});
