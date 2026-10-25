import { expect, test as base } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  addConnectedHostAndReload,
  chooseHostBadgeDisplay,
  chooseHostColor,
  expectHostAppearancePreview,
  leaveHostAppearanceSettings,
  openHostAppearanceSettings,
  reloadPreservingHostRegistry,
  renameHostFromSettings,
  waitForConnectedHost,
} from "../support/helpers/hosts";
import {
  type IsolatedHostDaemon,
  startIsolatedHostDaemon,
} from "../support/helpers/isolated-host-daemon";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { waitForSidebarHydration } from "../support/helpers/workspace-ui";

const PRIMARY_HOST_LABEL = "Primary Host";
const SECONDARY_HOST_LABEL = "Secondary Host";

interface TwoHostSidebar {
  primaryServerId: string;
  primaryWorkspaceId: string;
  secondaryServerId: string;
  secondaryWorkspaceId: string;
}

// Two connected hosts prove that changing one host's appearance does not rename the other.
const test = base.extend<{ twoHostSidebar: TwoHostSidebar }>({
  twoHostSidebar: async ({ page }, provide) => {
    const secondaryHost: IsolatedHostDaemon = await startIsolatedHostDaemon(
      "host-appearance-secondary",
    );
    let primary: SeededWorkspace | null = null;
    let secondary: SeededWorkspace | null = null;

    try {
      primary = await seedWorkspace({
        repoPrefix: "host-appearance-primary-",
        title: "Primary workspace",
      });
      secondary = await seedWorkspace({
        repoPrefix: "host-appearance-secondary-",
        title: "Secondary workspace",
        port: secondaryHost.port,
      });

      await gotoAppShell(page);
      await addConnectedHostAndReload(page, {
        serverId: secondaryHost.serverId,
        label: SECONDARY_HOST_LABEL,
        port: secondaryHost.port,
        primaryLabel: PRIMARY_HOST_LABEL,
      });
      await waitForConnectedHost(page, {
        serverId: secondaryHost.serverId,
        endpoint: `localhost:${secondaryHost.port}`,
      });
      await waitForSidebarHydration(page);

      await provide({
        primaryServerId: getServerId(),
        primaryWorkspaceId: primary.workspaceId,
        secondaryServerId: secondaryHost.serverId,
        secondaryWorkspaceId: secondary.workspaceId,
      });
    } finally {
      await primary?.cleanup().catch(() => undefined);
      await secondary?.cleanup().catch(() => undefined);
      await secondaryHost.close().catch(() => undefined);
    }
  },
});

test.describe.configure({ timeout: 180_000 });

test("customizes host appearance and preserves it after reload", async ({
  page,
  twoHostSidebar,
}) => {
  const badge = {
    serverId: twoHostSidebar.secondaryServerId,
    workspaceId: twoHostSidebar.secondaryWorkspaceId,
  };
  const hostName = "Developer MacBook Pro.local";

  await test.step("rename the host in the session tree", async () => {
    const host = page.getByTestId(`session-host-${badge.serverId}`).filter({ visible: true });
    await expect(host).toHaveText(SECONDARY_HOST_LABEL);
    await openHostAppearanceSettings(page, badge.serverId);
    await renameHostFromSettings(page, hostName);
    await leaveHostAppearanceSettings(page);
    await expect(host).toHaveText(hostName);
    await expect(
      page.getByTestId(`session-host-${twoHostSidebar.primaryServerId}`).filter({ visible: true }),
    ).toHaveText(PRIMARY_HOST_LABEL);
  });

  await test.step("preview a color and preserve it after reload", async () => {
    await openHostAppearanceSettings(page, badge.serverId);
    await chooseHostColor(page, "Teal");
    await expectHostAppearancePreview(page, { serverId: badge.serverId, hostName, color: "teal" });
    await reloadPreservingHostRegistry(page);
    await expectHostAppearancePreview(page, { serverId: badge.serverId, hostName, color: "teal" });
  });

  await test.step("keep the icon-only preference through reload", async () => {
    await chooseHostBadgeDisplay(page, "Icon only");
    const previewBadge = page
      .getByTestId("host-appearance-preview")
      .getByTestId(`host-badge-${badge.serverId}`);
    await expect(previewBadge).toBeVisible();
    await expect(previewBadge).not.toHaveText(hostName);
    await reloadPreservingHostRegistry(page);
    await expect(page.getByRole("button", { name: "Sidebar badge, Icon only" })).toBeVisible();
    await expect(previewBadge).toBeVisible();
    await expect(previewBadge).not.toHaveText(hostName);
  });

  await test.step("hide the optional badge without hiding host navigation", async () => {
    await chooseHostBadgeDisplay(page, "Hidden");
    await expect(
      page.getByTestId("host-appearance-preview").getByTestId(`host-badge-${badge.serverId}`),
    ).toHaveCount(0);
    await leaveHostAppearanceSettings(page);
    await expect(
      page.getByTestId(`session-host-${badge.serverId}`).filter({ visible: true }),
    ).toHaveText(hostName);
    await expect(
      page.getByTestId(`session-host-${twoHostSidebar.primaryServerId}`).filter({ visible: true }),
    ).toHaveText(PRIMARY_HOST_LABEL);
  });
});
