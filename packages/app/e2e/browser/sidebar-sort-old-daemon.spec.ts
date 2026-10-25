import type { Page } from "@playwright/test";
import { buildHostWorkspaceRoute } from "../../src/utils/host-routes";
import { randomUUID } from "node:crypto";
import { metroTest as test, expect } from "../support/fixtures";
import { startIsolatedHostDaemon } from "../support/helpers/isolated-host-daemon";
import { seedWorkspace } from "../support/helpers/seed-client";
import { buildSeededHost } from "../support/helpers/daemon-registry";
import { switchWorkspaceViaCommandCenter } from "../support/helpers/workspace-ui";

test("published old daemon keeps workspace navigation without removed sorting controls", async ({
  page,
}) => {
  test.setTimeout(180_000);
  const daemon = await startIsolatedHostDaemon(`srv_sort_old_${randomUUID()}`, {
    publishedVersion: "0.8.0",
  });
  try {
    const project = await seedWorkspace({ repoPrefix: "sidebar-old-host-", port: daemon.port });
    try {
      const second = await project.client.createWorkspace({
        source: { kind: "directory", path: project.repoPath, projectId: project.projectId },
        title: "Second old workspace",
      });
      if (!second.workspace) throw new Error(second.error ?? "Failed to create old-host workspace");
      await openPublishedHost(page, daemon, project.workspaceId);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: daemon.serverId,
        workspaceId: second.workspace.id,
      });
      await page.getByLabel("Display preferences", { exact: true }).click();
      await expect(page.getByRole("menuitem", { name: /^Sort/ })).toHaveCount(0);
      await expect(page.getByTestId("sidebar-display-grouping")).toHaveCount(0);
    } finally {
      await project.cleanup();
    }
  } finally {
    await daemon.close();
  }
});

async function openPublishedHost(
  page: Page,
  daemon: { serverId: string; port: number },
  workspaceId: string,
) {
  const host = buildSeededHost({
    serverId: daemon.serverId,
    endpoint: `127.0.0.1:${daemon.port}`,
    nowIso: new Date().toISOString(),
  });
  await page.addInitScript((seededHost) => {
    localStorage.setItem("@paseo:e2e", "1");
    localStorage.setItem("@paseo:daemon-registry", JSON.stringify([seededHost]));
    localStorage.setItem(
      "sidebar-view",
      JSON.stringify({ version: 8, state: { groupMode: "project", workspaceSort: "lastPrompt" } }),
    );
  }, host);
  await page.goto(buildHostWorkspaceRoute(daemon.serverId, workspaceId));
}
