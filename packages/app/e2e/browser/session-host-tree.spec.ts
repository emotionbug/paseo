import { mkdir } from "node:fs/promises";
import path from "node:path";
import { test, expect, type Page } from "../support/fixtures";
import { openAgentRoute } from "../support/helpers/mock-agent";
import { connectSeedClient, type SeedDaemonClient } from "../support/helpers/seed-client";
import { createTempDirectory } from "../support/helpers/workspace";
import { getServerId } from "../support/helpers/server-id";
import {
  addConnectedHostAndReload,
  addOfflineHostAndReload,
  reloadPreservingHostRegistry,
} from "../support/helpers/hosts";
import {
  startIsolatedHostDaemon,
  type IsolatedHostDaemon,
} from "../support/helpers/isolated-host-daemon";
import { expectComposerVisible } from "../support/helpers/composer";

async function createDirectorySessions(
  client: SeedDaemonClient,
  directory: string,
  titles: string[],
) {
  const result = await client.createWorkspace({ source: { kind: "directory", path: directory } });
  if (!result.workspace) throw new Error(result.error ?? "Directory workspace was not created");
  const workspace = result.workspace;
  const agents = [];
  for (const title of titles) {
    agents.push(
      await client.createAgent({
        provider: "mock",
        cwd: directory,
        workspaceId: workspace.id,
        title,
        modeId: "load-test",
        model: "e2e-fast-stream",
      }),
    );
  }
  return { workspace, agents };
}

async function showSidebar(page: Page, width: number): Promise<void> {
  if (width < 600) await page.getByTestId("menu-button").filter({ visible: true }).click();
  await expect(page.getByTestId("session-search").filter({ visible: true })).toBeInViewport({
    ratio: 1,
  });
}

async function expectDirectoryCreationRoute(
  page: Page,
  input: { serverId: string; projectId: string; directory: string },
): Promise<void> {
  await page
    .getByTestId(`session-new-directory-${input.serverId}-${input.projectId}`)
    .filter({ visible: true })
    .click();
  await expect(page).toHaveURL(
    (url) =>
      url.pathname === "/new" &&
      url.searchParams.get("serverId") === input.serverId &&
      url.searchParams.get("projectId") === input.projectId &&
      url.searchParams.get("dir") === input.directory,
  );
  await expect(
    page.getByTestId("new-workspace-project-picker-trigger").filter({ visible: true }),
  ).toBeVisible();
}

for (const width of [390, 1280]) {
  test(`host directory session tree at ${width}px`, async ({ page }, info) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 844 });
    const root = await createTempDirectory("session-directory-tree-");
    const client = await connectSeedClient();
    try {
      const firstPath = path.join(root.path, "one", "shared");
      const secondPath = path.join(root.path, "two", "shared");
      const emptyPath = path.join(root.path, "empty");
      await Promise.all(
        [firstPath, secondPath, emptyPath].map((directory) =>
          mkdir(directory, { recursive: true }),
        ),
      );
      const first = await createDirectorySessions(client, firstPath, [
        "Navigation regression session",
        "Review directory grouping",
      ]);
      const second = await createDirectorySessions(client, secondPath, ["Other directory session"]);
      const added = await client.addProject(emptyPath);
      if (!added.project) throw new Error(added.error ?? "Empty directory was not added");
      const emptyProjectId = added.project.projectId;
      const serverId = getServerId();
      const host = page.getByTestId(`session-host-${serverId}`).filter({ visible: true });
      const firstDirectory = page.getByTestId(
        `session-directory-${serverId}-${first.workspace.projectId}`,
      );
      const secondDirectory = page.getByTestId(
        `session-directory-${serverId}-${second.workspace.projectId}`,
      );
      const emptyDirectory = page.getByTestId(`session-directory-${serverId}-${emptyProjectId}`);
      const firstSession = page.getByTestId(`session-row-${serverId}-${first.agents[0]!.id}`);
      const siblingSession = page.getByTestId(`session-row-${serverId}-${first.agents[1]!.id}`);
      const otherSession = page.getByTestId(`session-row-${serverId}-${second.agents[0]!.id}`);
      const search = page.getByTestId("session-search").filter({ visible: true });
      const selected = { workspaceId: first.workspace.id, agentId: first.agents[0]!.id };

      await openAgentRoute(page, selected);
      await expectComposerVisible(page);
      await showSidebar(page, width);
      await expect(host).toBeVisible({ timeout: 30_000 });
      await expect(host).toHaveAttribute("aria-expanded", "true");
      await expect(firstDirectory).toHaveAccessibleName(`shared, ${firstPath}`);
      await expect(secondDirectory).toHaveAccessibleName(`shared, ${secondPath}`);
      await expect(emptyDirectory).toHaveAccessibleName(`empty, ${emptyPath}`);
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "true");
      await expect(firstSession).toBeVisible();
      await expect(siblingSession).toBeVisible();
      await expect(otherSession).toBeVisible();
      const hostBox = await host.boundingBox();
      const directoryBox = await firstDirectory.boundingBox();
      const sessionBox = await firstSession.boundingBox();
      expect(hostBox).not.toBeNull();
      expect(directoryBox).not.toBeNull();
      expect(sessionBox).not.toBeNull();
      expect(hostBox!.y).toBeLessThan(directoryBox!.y);
      expect(directoryBox!.y).toBeLessThan(sessionBox!.y);

      await firstDirectory.click();
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "false");
      await expect(firstSession).toHaveCount(0);
      await expect(siblingSession).toHaveCount(0);
      await expect(otherSession).toBeVisible();
      await expect(emptyDirectory).toBeVisible();
      await search.fill("Navigation regression");
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "true");
      await expect(firstSession).toBeVisible();
      await expect(siblingSession).toHaveCount(0);
      await expect(secondDirectory).toHaveCount(0);
      await search.fill(firstPath);
      await expect(firstSession).toBeVisible();
      await expect(siblingSession).toBeVisible();
      await expect(secondDirectory).toHaveCount(0);
      await search.fill(emptyPath);
      await expect(emptyDirectory).toBeVisible();
      await expect(firstDirectory).toHaveCount(0);
      await search.fill("");
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "false");
      await expect(firstSession).toHaveCount(0);
      await expect(otherSession).toBeVisible();

      await host.click();
      await expect(host).toHaveAttribute("aria-expanded", "false");
      await expect(firstDirectory).toHaveCount(0);
      await expect(secondDirectory).toHaveCount(0);
      await expect(emptyDirectory).toHaveCount(0);
      await search.fill("Navigation regression");
      await expect(host).toHaveAttribute("aria-expanded", "true");
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "true");
      await expect(firstSession).toBeVisible();
      await search.fill("");
      await expect(host).toHaveAttribute("aria-expanded", "false");
      await expect(firstDirectory).toHaveCount(0);
      await host.click();
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "false");
      await expect(otherSession).toBeVisible();

      await reloadPreservingHostRegistry(page);
      await expectComposerVisible(page);
      await showSidebar(page, width);
      await expect(firstDirectory).toHaveAttribute("aria-expanded", "false");
      await expect(firstSession).toHaveCount(0);
      await expect(secondDirectory).toHaveAttribute("aria-expanded", "true");
      await firstDirectory.click();
      await siblingSession.click();
      if (width < 600) {
        await expect(host).not.toBeVisible();
        await showSidebar(page, width);
      }
      await expect(siblingSession).toHaveAttribute("aria-selected", "true");
      await expect(firstSession).toHaveAttribute("aria-selected", "false");
      await page.screenshot({ path: info.outputPath("host-directory-session-tree.png") });
      await expectDirectoryCreationRoute(page, {
        serverId,
        projectId: second.workspace.projectId,
        directory: secondPath,
      });

      await openAgentRoute(page, selected);
      await expectComposerVisible(page);
      await showSidebar(page, width);
      await expectDirectoryCreationRoute(page, {
        serverId,
        projectId: emptyProjectId,
        directory: emptyPath,
      });
    } finally {
      await client.close();
      await root.cleanup();
    }
  });

  test(`directories stay host-local with an offline host at ${width}px`, async ({ page }, info) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 844 });
    const root = await createTempDirectory("host-directory-isolation-");
    const client = await connectSeedClient();
    let remote: IsolatedHostDaemon | undefined;
    let remoteClient: SeedDaemonClient | undefined;
    try {
      const directory = path.join(root.path, "shared");
      await mkdir(directory);
      const local = await createDirectorySessions(client, directory, ["Local directory session"]);
      const serverId = getServerId();
      const host = page.getByTestId(`session-host-${serverId}`).filter({ visible: true });
      const localDirectory = page.getByTestId(
        `session-directory-${serverId}-${local.workspace.projectId}`,
      );
      const localSession = page.getByTestId(`session-row-${serverId}-${local.agents[0]!.id}`);
      remote = await startIsolatedHostDaemon(`directory-tree-remote-${width}`);
      remoteClient = await connectSeedClient({ port: remote.port });
      const remoteSeed = await createDirectorySessions(remoteClient, directory, [
        "Remote directory session",
      ]);
      await openAgentRoute(page, {
        workspaceId: local.workspace.id,
        agentId: local.agents[0]!.id,
      });
      await expectComposerVisible(page);
      await addConnectedHostAndReload(page, {
        serverId: remote.serverId,
        label: "Remote build host",
        port: remote.port,
      });
      await expectComposerVisible(page);
      await showSidebar(page, width);
      const remoteHost = page
        .getByTestId(`session-host-${remote.serverId}`)
        .filter({ visible: true });
      const remoteDirectory = page.getByTestId(
        `session-directory-${remote.serverId}-${remoteSeed.workspace.projectId}`,
      );
      const remoteSession = page.getByTestId(
        `session-row-${remote.serverId}-${remoteSeed.agents[0]!.id}`,
      );
      await expect(remoteHost).toHaveText("Remote build host");
      await expect(localDirectory).toHaveAccessibleName(`shared, ${directory}`);
      await expect(remoteDirectory).toHaveAccessibleName(`shared, ${directory}`);
      await expect(localSession).toBeVisible();
      await expect(remoteSession).toBeVisible();
      await localDirectory.click();
      await expect(localSession).toHaveCount(0);
      await expect(remoteDirectory).toHaveAttribute("aria-expanded", "true");
      await expect(remoteSession).toBeVisible();
      await page.screenshot({ path: info.outputPath("host-local-directories.png") });
      await host.click();
      await expect(localDirectory).toHaveCount(0);
      await expect(remoteSession).toBeVisible();
      await expectDirectoryCreationRoute(page, {
        serverId: remote.serverId,
        projectId: remoteSeed.workspace.projectId,
        directory,
      });
      await addOfflineHostAndReload(page, {
        serverId: "tree-offline-host",
        label: "Offline build host",
      });
      await expect(
        page.getByTestId("new-workspace-project-picker-trigger").filter({ visible: true }),
      ).toBeVisible();
      await showSidebar(page, width);
      const offlineHost = page
        .getByTestId("session-host-tree-offline-host")
        .filter({ visible: true });
      await expect(offlineHost).toHaveText("Offline build host");
      await expect(localDirectory).toHaveAttribute("aria-expanded", "false");
      await expect(remoteDirectory).toHaveAttribute("aria-expanded", "true");
      await offlineHost.click();
      await expect(offlineHost).toHaveAttribute("aria-expanded", "false");
      await expect(remoteSession).toBeVisible();
    } finally {
      await remoteClient?.close();
      await remote?.close();
      await client.close();
      await root.cleanup();
    }
  });
}
