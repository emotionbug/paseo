import { test, expect } from "../support/fixtures";
import { seedMockAgentWorkspace, openAgentRoute } from "../support/helpers/mock-agent";
import { getServerId } from "../support/helpers/server-id";
import { addOfflineHostAndReload } from "../support/helpers/hosts";
import { expectComposerVisible } from "../support/helpers/composer";

for (const width of [390, 1280]) {
  test(`host session tree at ${width}px`, async ({ page }, info) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width, height: 844 });
    const seeded = await seedMockAgentWorkspace({
      repoPrefix: "session-host-tree-",
      title: "Navigation regression session",
    });
    const serverId = getServerId();
    const host = page.getByTestId(`session-host-${serverId}`).filter({ visible: true });
    const session = page.getByTestId(`session-row-${serverId}-${seeded.agentId}`);
    const search = page.getByTestId("session-search").filter({ visible: true });
    try {
      await openAgentRoute(page, seeded);
      await expectComposerVisible(page);
      if (width < 600) await page.getByTestId("menu-button").filter({ visible: true }).click();
      await expect(host).toBeVisible({ timeout: 30_000 });
      await expect(host).toHaveAttribute("aria-expanded", "true");
      await expect(session).toBeVisible();

      await host.click();
      await expect(host).toHaveAttribute("aria-expanded", "false");
      await expect(session).toHaveCount(0);
      await search.fill("Navigation regression");
      await expect(host).toHaveAttribute("aria-expanded", "true");
      await expect(session).toBeVisible();
      await search.fill("");
      await expect(host).toHaveAttribute("aria-expanded", "false");
      await expect(session).toHaveCount(0);
      await host.click();
      await expect(session).toBeVisible();
      await page.screenshot({ path: info.outputPath("host-session-tree.png") });
      await session.click();
      if (width < 600) {
        await expect(host).not.toBeVisible();
        await page.getByTestId("menu-button").filter({ visible: true }).click();
      }
      await expect(session).toHaveAttribute("aria-selected", "true");

      await addOfflineHostAndReload(page, {
        serverId: "tree-offline-host",
        label: "Offline build host",
      });
      await expectComposerVisible(page);
      if (width < 600) await page.getByTestId("menu-button").filter({ visible: true }).click();
      const offlineHost = page
        .getByTestId("session-host-tree-offline-host")
        .filter({ visible: true });
      await expect(offlineHost).toHaveText("Offline build host");
      await expect(host).toBeVisible();
      await expect(session).toBeVisible();
      await offlineHost.click();
      await expect(offlineHost).toHaveAttribute("aria-expanded", "false");
      await expect(session).toBeVisible();
      await page.screenshot({ path: info.outputPath("multiple-hosts.png") });
    } finally {
      await seeded.cleanup();
    }
  });
}
