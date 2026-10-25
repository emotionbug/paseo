import type { Page } from "@playwright/test";
import { buildHostWorkspaceRoute, decodeWorkspaceIdFromPathSegment } from "@/utils/host-routes";
import { test, expect } from "../support/fixtures";
import { gotoAppShell, openSettings } from "../support/helpers/app";
import { seedWorkspace } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { waitForWorkspaceInCommandCenter } from "../support/helpers/workspace-ui";

async function selectedWorkspace(page: Page): Promise<string> {
  await expect(page).toHaveURL(/\/workspace\/[^/?#]+$/);
  const workspaceId = decodeWorkspaceIdFromPathSegment(
    new URL(page.url()).pathname.split("/").at(-1)!,
  );
  if (!workspaceId) throw new Error(`Expected a workspace route, got ${page.url()}`);
  return workspaceId;
}

test("previous and next traverse every workspace while digit shortcuts stop at nine", async ({
  page,
}) => {
  test.setTimeout(120_000);
  const project = await seedWorkspace({
    repoPrefix: "relative-navigation-",
    title: "Workspace 01",
  });
  try {
    const workspaceIds = [project.workspaceId];
    for (let index = 2; index <= 12; index += 1) {
      const created = await project.client.createWorkspace({
        source: { kind: "directory", path: project.repoPath },
        title: `Workspace ${String(index).padStart(2, "0")}`,
      });
      expect(created.error).toBeNull();
      if (!created.workspace) throw new Error("Workspace creation returned no workspace");
      workspaceIds.push(created.workspace.id);
    }
    await gotoAppShell(page);
    for (const workspaceId of workspaceIds) {
      await waitForWorkspaceInCommandCenter(page, {
        serverId: getServerId(),
        workspaceId,
        query: "Workspace",
      });
    }

    await openSettings(page);
    await page.keyboard.press("Alt+1");
    const first = await selectedWorkspace(page);
    const order = [first];
    for (let index = 1; index < workspaceIds.length; index += 1) {
      const previousUrl = page.url();
      await page.keyboard.press("Alt+]");
      await expect(page).not.toHaveURL(previousUrl);
      order.push(await selectedWorkspace(page));
    }
    expect([...order].sort()).toEqual([...workspaceIds].sort());
    await page.keyboard.press("Alt+]");
    await expect(page).toHaveURL(buildHostWorkspaceRoute(getServerId(), first));

    for (const workspaceId of order.toReversed()) {
      await page.keyboard.press("Alt+[");
      await expect(page).toHaveURL(buildHostWorkspaceRoute(getServerId(), workspaceId));
    }

    await page.keyboard.press("Alt+9");
    await expect(page).toHaveURL(buildHostWorkspaceRoute(getServerId(), order[8]));
    await page.keyboard.press("Alt+]");
    await expect(page).toHaveURL(buildHostWorkspaceRoute(getServerId(), order[9]));
    await page.keyboard.press("Alt+[");
    await expect(page).toHaveURL(buildHostWorkspaceRoute(getServerId(), order[8]));
  } finally {
    await project.cleanup();
  }
});
