import { existsSync } from "node:fs";
import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { seedWorkspace } from "../support/helpers/seed-client";
import { expectWorkspaceAbsentFromCommandCenter } from "../support/helpers/sidebar";
import {
  switchWorkspaceViaCommandCenter,
  waitForSidebarHydration,
} from "../support/helpers/workspace-ui";
import { getServerId } from "../support/helpers/server-id";

test.describe("Workspace archive shortcut", () => {
  test("archives the selected workspace without removing its local checkout", async ({ page }) => {
    const workspace = await seedWorkspace({ repoPrefix: "archive-shortcut-" });

    try {
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: workspace.workspaceId,
      });

      const modifier = process.platform === "darwin" ? "Meta" : "Control";
      await page.keyboard.press(`${modifier}+Shift+Backspace`);

      await expectWorkspaceAbsentFromCommandCenter(page, workspace.workspaceId);
      expect(existsSync(workspace.repoPath)).toBe(true);
    } finally {
      await workspace.cleanup();
    }
  });
});
