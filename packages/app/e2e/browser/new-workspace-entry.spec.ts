import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { closeCommandCenter, openCommandCenter } from "../support/helpers/command-center";
import {
  expectNewWorkspaceControlsEnabled,
  expectNewWorkspaceProjectSelected,
  expectNewWorkspaceTriggerLabelsAligned,
  openGlobalNewWorkspaceComposer,
  openMissingProjectNewWorkspaceComposer,
  openNewWorkspaceProjectPickerWithShortcut,
} from "../support/helpers/new-workspace";
import { getE2EDaemonPort } from "../support/helpers/daemon-port";
import { expectNoTruncation } from "../support/helpers/no-truncation";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import { seedSavedSettingsHosts } from "../support/helpers/settings";
import { getServerId } from "../support/helpers/server-id";
import { projectEquivalenceViewKey } from "../support/helpers/project-view-key";
import {
  switchWorkspaceViaCommandCenter,
  waitForSidebarHydration,
} from "../support/helpers/workspace-ui";

// Creation stays available from the global entry and host session tree.
// Project selection, host context, and draft state belong to the creation screen.

test.describe("New workspace entry points", () => {
  test.describe.configure({ timeout: 240_000 });

  test("the global new-workspace button opens the New Workspace screen", async ({ page }) => {
    const seeded: SeededWorkspace = await seedWorkspace({ repoPrefix: "entry-global-button-" });

    try {
      await seedSavedSettingsHosts(page, [
        {
          serverId: getServerId(),
          label: "localhost",
          endpoint: `127.0.0.1:${getE2EDaemonPort()}`,
        },
        {
          serverId: "secondary-new-workspace-host",
          label: "Secondary host",
          endpoint: "127.0.0.1:9",
        },
      ]);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId: getServerId(),
        workspaceId: seeded.workspaceId,
      });

      const globalButton = page
        .getByTestId("sidebar-global-new-workspace")
        .filter({ visible: true });
      await expect(globalButton).toBeVisible({ timeout: 30_000 });

      await openGlobalNewWorkspaceComposer(page);
      await expect(page.getByTestId("host-chooser")).toHaveCount(0);

      await expect(page.getByTestId("new-workspace-project-picker-trigger")).toBeVisible({
        timeout: 30_000,
      });
      await expect(page.getByTestId("host-picker-trigger")).toBeVisible({ timeout: 30_000 });
      await expectNewWorkspaceTriggerLabelsAligned(page, {
        projectLabel: seeded.projectDisplayName,
        hostLabel: "localhost",
      });
    } finally {
      await seeded.cleanup();
    }
  });

  test("the New Workspace screen shows the host selector with Add host when there is only one host", async ({
    page,
  }) => {
    const seeded: SeededWorkspace = await seedWorkspace({ repoPrefix: "entry-single-host-" });

    try {
      await seedSavedSettingsHosts(page, [
        {
          serverId: getServerId(),
          label: "localhost",
          endpoint: `127.0.0.1:${getE2EDaemonPort()}`,
        },
      ]);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      await openGlobalNewWorkspaceComposer(page);

      await expect(page.getByTestId("new-workspace-project-picker-trigger")).toBeVisible({
        timeout: 30_000,
      });
      const hostTrigger = page.getByTestId("host-picker-trigger");
      await expect(hostTrigger).toContainText("localhost", { timeout: 30_000 });
      await hostTrigger.click();
      await expect(page.getByTestId("new-workspace-host-picker-add-host")).toBeVisible();
      await expectNoTruncation(page.getByTestId("combobox-desktop-container").last());
      await page.getByTestId("new-workspace-host-picker-add-host").click();
      await expect(page).toHaveURL(/\/settings\/general\?addHost=/);
    } finally {
      await seeded.cleanup();
    }
  });

  test("a stale project route keeps the New Workspace controls enabled", async ({ page }) => {
    await gotoAppShell(page);
    await openMissingProjectNewWorkspaceComposer(page, {
      serverId: getServerId(),
      projectId: "missing-project",
      sourceDirectory: "/tmp/missing-project",
    });

    await expectNewWorkspaceControlsEnabled(page);
  });

  test("Ctrl+P opens the project picker with search focused", async ({ page }) => {
    const seeded: SeededWorkspace = await seedWorkspace({ repoPrefix: "entry-shortcut-" });

    try {
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await openGlobalNewWorkspaceComposer(page);

      await openNewWorkspaceProjectPickerWithShortcut(page);
    } finally {
      await seeded.cleanup();
    }
  });

  test("keeps the in-progress form when the remembered workspace is archived elsewhere", async ({
    page,
  }) => {
    const otherProject: SeededWorkspace = await seedWorkspace({
      repoPrefix: "aa-new-workspace-archive-other-",
    });
    const rememberedProject: SeededWorkspace = await seedWorkspace({
      repoPrefix: "zz-new-workspace-archive-remembered-",
    });
    const serverId = getServerId();
    const draftText = "keep this new workspace draft";

    try {
      await seedSavedSettingsHosts(page, [
        {
          serverId,
          label: "localhost",
          endpoint: `127.0.0.1:${getE2EDaemonPort()}`,
        },
      ]);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: rememberedProject.workspaceId,
      });
      await expect(page).toHaveURL(/\/workspace\//, { timeout: 30_000 });

      await page.goto(`/new?serverId=${encodeURIComponent(serverId)}`);
      await expectNewWorkspaceProjectSelected(page, rememberedProject.projectDisplayName);

      const composer = page.getByRole("textbox", { name: "Message agent..." });
      await expect(composer).toBeEditable({ timeout: 30_000 });
      await composer.fill(draftText);
      await expect(composer).toHaveValue(draftText);

      const panel = await openCommandCenter(page);
      const rememberedWorkspace = panel.getByTestId(
        `command-center-workspace-${serverId}:${rememberedProject.workspaceId}`,
      );
      await expect(rememberedWorkspace).toBeVisible({ timeout: 30_000 });
      const archived = await rememberedProject.client.archiveWorkspace(
        rememberedProject.workspaceId,
      );
      expect(archived.error).toBeNull();
      await expect(rememberedWorkspace).toHaveCount(0, { timeout: 30_000 });
      await closeCommandCenter(page);

      await expect(page).toHaveURL(/\/new(?:\?.*)?$/, { timeout: 30_000 });
      await expect(composer).toHaveValue(draftText);
      await expectNewWorkspaceProjectSelected(page, rememberedProject.projectDisplayName);
    } finally {
      await otherProject.cleanup();
      await rememberedProject.cleanup();
    }
  });

  test("modern hosts show neither isolation nor starting ref controls for git and non-git projects", async ({
    page,
  }) => {
    const gitProject: SeededWorkspace = await seedWorkspace({ repoPrefix: "entry-iso-git-" });
    const nonGitProject: SeededWorkspace = await seedWorkspace({
      repoPrefix: "entry-iso-nongit-",
      git: false,
    });

    try {
      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      // Select the non-git project from the creation screen's project picker.
      await openGlobalNewWorkspaceComposer(page);
      const trigger = page.getByTestId("new-workspace-project-picker-trigger");
      await expect(trigger).toBeVisible({ timeout: 30_000 });
      await trigger.click();
      const nonGitOption = page.getByTestId(
        `new-workspace-project-picker-option-${projectEquivalenceViewKey(nonGitProject.projectKey)}`,
      );
      await expect(nonGitOption).toBeVisible({ timeout: 30_000 });
      await nonGitOption.click();
      await expectNewWorkspaceProjectSelected(page, nonGitProject.projectDisplayName);

      await expect(page.getByTestId("workspace-create-isolation-trigger")).toHaveCount(0);
      await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);

      // Git projects also create another workspace in their existing directory.
      await trigger.click();
      const gitOption = page.getByTestId(
        `new-workspace-project-picker-option-${projectEquivalenceViewKey(gitProject.projectKey)}`,
      );
      await expect(gitOption).toBeVisible({ timeout: 30_000 });
      await gitOption.click();
      await expectNewWorkspaceProjectSelected(page, gitProject.projectDisplayName);

      await expect(page.getByTestId("workspace-create-isolation-trigger")).toHaveCount(0);
      await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
    } finally {
      await gitProject.cleanup();
      await nonGitProject.cleanup();
    }
  });
});
