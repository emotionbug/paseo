import { existsSync } from "node:fs";
import path from "node:path";
import { buildNewWorkspaceRoute } from "@/utils/host-routes";
import { expect, test } from "../support/fixtures";
import { openAgentRoute } from "../support/helpers/mock-agent";
import { expectAppRoute } from "../support/helpers/route-assertions";
import { seedWorkspace } from "../support/helpers/seed-client";
import { expectWorkspaceAbsentFromCommandCenter } from "../support/helpers/sidebar";
import { getServerId } from "../support/helpers/server-id";

test.describe("Workspace archive shortcut", () => {
  for (const git of [true, false]) {
    const backing = git ? "checkout" : "non-Git directory";
    test(`archives the selected workspace without removing its local ${backing}`, async ({
      page,
    }) => {
      const workspace = await seedWorkspace({ repoPrefix: "archive-shortcut-", git });

      try {
        const agent = await workspace.client.createAgent({
          provider: "mock",
          cwd: workspace.repoPath,
          workspaceId: workspace.workspaceId,
          modeId: "load-test",
          model: "e2e-fast-stream",
        });
        await openAgentRoute(page, {
          workspaceId: workspace.workspaceId,
          agentId: agent.id,
        });

        // This shortcut is disabled while the palette is open or a non-Mac terminal has focus.
        await expect(page.getByTestId("command-center-panel")).not.toBeVisible();
        const composer = page
          .getByRole("textbox", { name: "Message agent...", exact: true })
          .filter({ visible: true });
        await expect(composer).toBeEditable();
        await composer.focus();
        await expect(composer).toBeFocused();
        await page.keyboard.press("ControlOrMeta+Shift+Backspace");

        await expectAppRoute(
          page,
          buildNewWorkspaceRoute({
            serverId: getServerId(),
            sourceDirectory: workspace.repoPath,
            displayName: workspace.projectDisplayName,
            projectId: workspace.projectId,
          }),
        );
        // The UI removes archives optimistically; verify the daemon accepted the real shortcut.
        await expect
          .poll(async () => {
            const { entries } = await workspace.client.fetchWorkspaces();
            for (const entry of entries) {
              if (entry.id === workspace.workspaceId) return true;
            }
            return false;
          })
          .toBe(false);

        await expectWorkspaceAbsentFromCommandCenter(page, workspace.workspaceId);
        expect(existsSync(workspace.repoPath)).toBe(true);
        if (git) {
          expect(existsSync(path.join(workspace.repoPath, ".git"))).toBe(true);
        }
      } finally {
        await workspace.cleanup();
      }
    });
  }
});
