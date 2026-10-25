import { expect, type Page } from "@playwright/test";
import { buildHostWorkspaceRoute } from "@/utils/host-routes";
import { gotoHome } from "./app";
import { expectAppRoute } from "./route-assertions";
import { openCommandCenter, closeCommandCenter } from "./command-center";

export async function openNewAgentComposer(page: Page): Promise<void> {
  await gotoHome(page);
}

/** Wait for session host navigation to hydrate, including empty and dedicated test hosts. */
export async function waitForSidebarHydration(page: Page, timeout = 60_000): Promise<void> {
  await page
    .getByTestId(/^session-host-/)
    .filter({ visible: true })
    .first()
    .waitFor({ state: "visible", timeout });
}

export async function expectWorkspaceSelected(input: {
  page: Page;
  serverId: string;
  workspaceId: string;
  selected?: boolean;
}): Promise<void> {
  const route = buildHostWorkspaceRoute(input.serverId, input.workspaceId);
  const selected = expect.poll(() => new URL(input.page.url()).pathname, { timeout: 30_000 });
  if (input.selected === false) {
    await selected.not.toBe(route);
  } else {
    await selected.toBe(route);
  }
}

export async function switchWorkspaceViaCommandCenter(input: {
  page: Page;
  serverId: string;
  workspaceId: string;
  query?: string;
}): Promise<void> {
  const panel = await openCommandCenter(input.page);
  if (input.query) await panel.getByTestId("command-center-input").fill(input.query);
  const row = panel.getByTestId(`command-center-workspace-${input.serverId}:${input.workspaceId}`);
  await expect(row).toBeVisible({ timeout: 30_000 });
  await row.click();

  const targetWorkspaceRoute = buildHostWorkspaceRoute(input.serverId, input.workspaceId);
  await expectAppRoute(input.page, targetWorkspaceRoute, { timeout: 30_000 });
}

/** Assert that a workspace is discoverable, including workspaces without an agent. */
export async function waitForWorkspaceInCommandCenter(
  page: Page,
  input: { serverId: string; workspaceId: string; query?: string },
): Promise<void> {
  const panel = await openCommandCenter(page);
  if (input.query) await panel.getByTestId("command-center-input").fill(input.query);
  await expect(
    panel.getByTestId(`command-center-workspace-${input.serverId}:${input.workspaceId}`),
  ).toBeVisible({ timeout: 60_000 });
  await closeCommandCenter(page);
}

export async function expectWorkspaceHeader(
  page: Page,
  input: { title: string; subtitle: string },
): Promise<void> {
  const titleLocator = page.getByTestId("workspace-header-title").filter({ visible: true });
  const subtitleLocator = page.getByTestId("workspace-header-subtitle").filter({ visible: true });

  await expect(titleLocator.first()).toHaveText(input.title, {
    timeout: 30_000,
  });
  await expect(subtitleLocator.first()).toHaveText(input.subtitle, {
    timeout: 30_000,
  });
}

export async function expectReconnectingToastVisible(
  page: Page,
  options?: { timeout?: number },
): Promise<void> {
  const toast = page.getByRole("alert").filter({ hasText: "Reconnecting to host" });
  await expect(toast).toBeVisible({
    timeout: options?.timeout ?? 30_000,
  });
  await expect(toast).toHaveText("Reconnecting to host");
}

export async function expectReconnectingToastGone(
  page: Page,
  options?: { timeout?: number },
): Promise<void> {
  await expect(page.getByRole("alert").filter({ hasText: "Reconnecting to host" })).toHaveCount(0, {
    timeout: options?.timeout ?? 30_000,
  });
}

declare global {
  interface Window {
    __shownPanelToasts?: string[];
  }
}

export interface PanelToastLog {
  /** Test ids of every toast that entered the DOM since recording started, in order. */
  shown(): Promise<string[]>;
  expectNeverShown(testId: string): Promise<void>;
}

/**
 * Record every toast that enters the DOM from the next navigation onward. A toast can appear
 * and disappear within a few frames, so a settled `toHaveCount(0)` cannot prove one never showed.
 */
export async function recordPanelToasts(page: Page): Promise<PanelToastLog> {
  await page.addInitScript(() => {
    const shown: string[] = [];
    window.__shownPanelToasts = shown;
    const record = (node: Element) => {
      if (node.getAttribute("role") !== "alert") return;
      shown.push(node.getAttribute("data-testid") ?? "");
    };
    new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (!(node instanceof Element)) continue;
          record(node);
          for (const descendant of node.querySelectorAll('[role="alert"]')) record(descendant);
        }
      }
    }).observe(document, { childList: true, subtree: true });
  });

  const shown = () => page.evaluate(() => window.__shownPanelToasts ?? []);
  return {
    shown,
    async expectNeverShown(testId: string): Promise<void> {
      expect(await shown()).not.toContain(testId);
    },
  };
}

export async function expectHostConnectingOrOffline(
  page: Page,
  options?: { timeout?: number },
): Promise<void> {
  await expect(
    page.getByText(/^Connecting$|localhost is offline|Cannot reach localhost/i),
  ).toBeVisible({ timeout: options?.timeout ?? 30_000 });
}

export async function expectMenuButtonVisible(page: Page): Promise<void> {
  await expect(page.getByTestId("menu-button")).toBeVisible();
}

export async function expectWorkspaceHeaderAbsent(page: Page): Promise<void> {
  await expect(page.getByTestId("workspace-header-title")).toHaveCount(0);
}

export function workspaceDeckEntryLocator(page: Page, serverId: string, workspaceId: string) {
  return page.getByTestId(`workspace-deck-entry-${serverId}:${workspaceId}`);
}

export async function expectWorkspaceDeckEntryCount(page: Page, count: number): Promise<void> {
  await expect(page.locator('[data-testid^="workspace-deck-entry-"]')).toHaveCount(count);
}

export async function seedWorkspaceActivity(page: Page, marker: string): Promise<void> {
  const input = page.getByRole("textbox", { name: "Message agent..." });
  await expect(input).toBeEditable({ timeout: 30_000 });
  await input.fill(marker);
  await input.press("Enter");
  await expect(page).toHaveURL(/\/workspace\//, { timeout: 30_000 });
}
