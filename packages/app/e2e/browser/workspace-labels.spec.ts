import type { Locator, Page } from "@playwright/test";
import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { getServerId } from "../support/helpers/server-id";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import { waitForSidebarHydration } from "../support/helpers/workspace-ui";
import { daemonWsRoutePattern } from "../support/helpers/daemon-port";

interface SessionEnvelope {
  type?: string;
  message?: { type?: string; requestId?: string };
}

async function readWorkspaceLabels(seeded: SeededWorkspace) {
  const workspaces = await seeded.client.fetchWorkspaces();
  return workspaces.entries
    .find((workspace) => workspace.id === seeded.workspaceId)
    ?.labels?.slice()
    .sort();
}

async function seedSession(seeded: SeededWorkspace) {
  return seeded.client.createAgent({
    provider: "mock",
    cwd: seeded.repoPath,
    workspaceId: seeded.workspaceId,
    title: seeded.projectDisplayName,
    modeId: "load-test",
    model: "e2e-fast-stream",
  });
}

async function installWorkspaceLabelUpdateFailure(page: Page) {
  let failNextUpdate = false;
  await page.routeWebSocket(daemonWsRoutePattern(), (browser) => {
    const server = browser.connectToServer();
    browser.onMessage((message) => {
      const raw = typeof message === "string" ? message : message.toString("utf8");
      let envelope: SessionEnvelope | null = null;
      try {
        envelope = JSON.parse(raw) as SessionEnvelope;
      } catch {
        server.send(message);
        return;
      }
      const request = envelope?.type === "session" ? envelope.message : null;
      if (
        failNextUpdate &&
        request?.type === "workspace.label.update.request" &&
        request.requestId
      ) {
        failNextUpdate = false;
        browser.send(
          JSON.stringify({
            type: "session",
            message: {
              type: "rpc_error",
              payload: {
                requestId: request.requestId,
                requestType: request.type,
                error: "Injected label mutation failure.",
                code: "handler_error",
              },
            },
          }),
        );
        return;
      }
      server.send(message);
    });
    server.onMessage((message) => browser.send(message));
  });
  return () => {
    failNextUpdate = true;
  };
}

async function size(locator: Locator) {
  let previous = "";
  await expect
    .poll(async () => {
      const current = JSON.stringify(await locator.boundingBox());
      const stable = current !== "null" && current === previous;
      previous = current;
      return stable;
    })
    .toBe(true);
  const measured = await locator.boundingBox();
  if (!measured) throw new Error("Expected a visible label filter");
  return { width: measured.width, height: measured.height };
}

async function openLabelFilter(page: Page) {
  await page.getByTestId("sidebar-display-preferences-menu").click();
  await page.getByTestId("sidebar-display-label-filter").click();
}

test.describe("Workspace labels", () => {
  test.describe.configure({ timeout: 180_000 });

  test("keeps label filters reachable when no sessions match", async ({ page }) => {
    const seeded = await seedWorkspace({ repoPrefix: "workspace-labels-no-matches-" });
    try {
      const agent = await seedSession(seeded);
      await seeded.client.setWorkspaceLabel({
        workspaceId: seeded.workspaceId,
        label: { name: "Unused", color: "red" },
        assigned: true,
      });
      await seeded.client.setWorkspaceLabel({
        workspaceId: seeded.workspaceId,
        label: { name: "Unused", color: "red" },
        assigned: false,
      });
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      const row = page.getByTestId(`session-row-${getServerId()}-${agent.id}`);
      await expect(row).toBeVisible();
      await openLabelFilter(page);
      await expect(page.getByTestId("sidebar-label-filter-clear")).toBeHidden();
      await page.getByTestId("sidebar-label-filter-option-Unused").click();
      await expect(row).toBeHidden();
      await expect(page.getByTestId("sidebar-display-preferences-menu")).toBeVisible();
      await expect(page.getByTestId("sidebar-label-filter-clear")).toBeVisible();
      await page.getByTestId("sidebar-label-filter-clear").click();
      await expect(row).toBeVisible();
    } finally {
      await seeded.cleanup();
    }
  });

  test("filters sessions by workspace labels and edits labels against the daemon", async ({
    page,
  }) => {
    const seeded = await seedWorkspace({ repoPrefix: "workspace-labels-" });
    const unlabelled = await seedWorkspace({ repoPrefix: "workspace-labels-unlabelled-" });
    try {
      const labelledAgent = await seedSession(seeded);
      const unlabelledAgent = await seedSession(unlabelled);
      for (const label of [
        { name: "Urgent", color: "red" },
        { name: "Frontend", color: "sky" },
      ] as const) {
        await seeded.client.setWorkspaceLabel({
          workspaceId: seeded.workspaceId,
          label,
          assigned: true,
        });
      }
      const failNextUpdate = await installWorkspaceLabelUpdateFailure(page);
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      const labelledRow = page.getByTestId(`session-row-${getServerId()}-${labelledAgent.id}`);
      const unlabelledRow = page.getByTestId(`session-row-${getServerId()}-${unlabelledAgent.id}`);
      await expect(labelledRow).toBeVisible();
      await expect(unlabelledRow).toBeVisible();

      await test.step("a label filters on one press and clears on the next", async () => {
        await openLabelFilter(page);
        const urgent = page.getByTestId("sidebar-label-filter-option-Urgent");
        const frontend = page.getByTestId("sidebar-label-filter-option-Frontend");
        const unlabelledLabel = page.getByTestId("sidebar-label-filter-option-unlabelled");
        const atRest = await size(urgent);
        await urgent.click();
        await expect(urgent).toHaveAttribute("aria-checked", "true");
        await expect(labelledRow).toBeVisible();
        await expect(unlabelledRow).toBeHidden();
        expect(await size(urgent)).toEqual(atRest);
        await urgent.click();
        await expect(urgent).toHaveAttribute("aria-checked", "false");
        await expect(labelledRow).toBeVisible();
        await expect(unlabelledRow).toBeVisible();
        expect(await size(urgent)).toEqual(atRest);
        await unlabelledLabel.click();
        await expect(labelledRow).toBeHidden();
        await expect(unlabelledRow).toBeVisible();
        await urgent.click();
        await expect(labelledRow).toBeVisible();
        await expect(unlabelledRow).toBeVisible();
        await page.getByTestId("sidebar-label-filter-clear").click();
        await expect(page.getByTestId("sidebar-label-filter-clear")).toBeHidden();
        await expect(labelledRow).toBeVisible();
        await expect(unlabelledRow).toBeVisible();
        await expect(urgent).toHaveAttribute("aria-checked", "false");
        await expect(frontend).toHaveAttribute("aria-checked", "false");
        await expect(unlabelledLabel).toHaveAttribute("aria-checked", "false");
        await page.keyboard.press("Escape");
      });

      await test.step("editing preserves assignments and retains the draft on failure", async () => {
        await openLabelFilter(page);
        await page.getByTestId("sidebar-label-manage").click();
        await expect(page.getByText("Manage labels", { exact: true })).toBeVisible();
        await page.getByTestId("workspace-label-manager-search").fill("urg");
        await expect(page.getByTestId("workspace-label-manager-label-Frontend")).toBeHidden();
        await page.getByTestId("workspace-label-manager-search").fill("");
        await expect(page.getByTestId("workspace-label-manager-label-Frontend")).toBeVisible();
        await page.getByTestId("workspace-label-manager-label-Urgent").hover();
        await page.getByTestId("workspace-label-manager-edit-Urgent").click();
        await expect(page.getByRole("radio", { name: "Red" })).toBeChecked();
        await expect(page.getByRole("radio", { name: "Sky" })).not.toBeChecked();
        await page.getByTestId("workspace-label-manager-name").fill("Priority");
        await page.getByRole("radio", { name: "Amber" }).click();
        await expect(page.getByTestId("workspace-label-manager-save")).toBeVisible();
        failNextUpdate();
        await page.getByTestId("workspace-label-manager-save").click();
        await expect(page.getByTestId("workspace-label-manager-error")).toContainText(
          "Injected label mutation failure.",
        );
        await expect(page.getByTestId("workspace-label-manager-name")).toHaveValue("Priority");
        await expect(page.getByRole("radio", { name: "Amber" })).toBeChecked();
        await page.getByTestId("workspace-label-manager-save").click();
        await expect(page.getByTestId("workspace-label-manager-label-Priority")).toBeVisible();
        await expect(page.getByTestId("workspace-label-manager-label-Urgent")).toBeHidden();
      });
      await expect.poll(() => readWorkspaceLabels(seeded)).toEqual(["Frontend", "Priority"]);
    } finally {
      await unlabelled.cleanup();
      await seeded.cleanup();
    }
  });
});
