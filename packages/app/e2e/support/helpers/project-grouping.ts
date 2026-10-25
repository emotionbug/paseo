import { expect, type Page } from "@playwright/test";
import { gotoAppShell } from "./app";
import { addConnectedHostsAndReload, waitForConnectedHost } from "./hosts";
import { openProjectSettings } from "./project-settings";
import { selectSettingsHost } from "./settings";
import { waitForSidebarHydration } from "./workspace-ui";
import { buildProjectsSettingsRoute } from "@/utils/host-routes";
import { openCommandCenter, closeCommandCenter } from "./command-center";
import { openGlobalNewWorkspaceComposer, submitNewWorkspaceEmpty } from "./new-workspace";

const PROJECT_VISIBILITY_TIMEOUT = 30_000;

export async function openProjectDirectory(page: Page): Promise<void> {
  await gotoAppShell(page);
  await waitForSidebarHydration(page);
}

export async function openProjectDirectoryWithHosts(
  page: Page,
  input: {
    hosts: Array<{ serverId: string; label: string; port: number }>;
    primaryLabel?: string;
  },
): Promise<void> {
  await gotoAppShell(page);
  await addConnectedHostsAndReload(page, input.hosts, { primaryLabel: input.primaryLabel });
  for (const host of input.hosts) {
    await waitForConnectedHost(page, {
      serverId: host.serverId,
      endpoint: `localhost:${host.port}`,
    });
  }
  await waitForSidebarHydration(page);
}

export async function beginWorkspaceFromProject(page: Page, projectName: string): Promise<void> {
  await openGlobalNewWorkspaceComposer(page);
  await page.getByTestId("new-workspace-project-picker-trigger").click();
  await page
    .locator('[data-testid^="new-workspace-project-picker-option-"]')
    .filter({ hasText: projectName })
    .click();
  await expect(page.getByRole("button", { name: "Workspace project", exact: true })).toContainText(
    projectName,
    { timeout: PROJECT_VISIBILITY_TIMEOUT },
  );
}

export async function selectWorkspaceHost(page: Page, hostName: string): Promise<void> {
  await page.getByRole("button", { name: "Host", exact: true }).click();
  await page.getByRole("button", { name: hostName, exact: true }).click();
  await expect(page.getByRole("button", { name: "Host", exact: true })).toContainText(hostName);
}

export async function createWorkspaceWithoutAgent(page: Page): Promise<void> {
  await submitNewWorkspaceEmpty(page);
  await expect(page).toHaveURL(/\/workspace\//, { timeout: 30_000 });
}

export async function expectProjectWorkspaceCountForHost(
  page: Page,
  input: { projectName: string; hostName: string; count: number },
): Promise<void> {
  const panel = await openCommandCenter(page);
  await panel.getByTestId("command-center-input").fill(input.projectName);
  await expect(
    panel
      .getByTestId(/^command-center-workspace-.*:/)
      .filter({ hasText: input.projectName })
      .filter({ hasText: input.hostName }),
  ).toHaveCount(input.count, { timeout: PROJECT_VISIBILITY_TIMEOUT });
  await closeCommandCenter(page);
}

export async function renameProject(page: Page, name: string): Promise<void> {
  await page.getByRole("button", { name: "Edit project", exact: true }).click();
  await page.getByRole("textbox", { name: "Project name", exact: true }).fill(name);
  await page.getByRole("button", { name: "Save changes", exact: true }).click();
  await expect(page.getByText("Project updated", { exact: true })).toBeVisible({
    timeout: PROJECT_VISIBILITY_TIMEOUT,
  });
}

export async function openGroupedProjectSettings(
  page: Page,
  input: { serverId: string; projectName: string },
): Promise<void> {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/general$/);
  await selectSettingsHost(page, input.serverId);
  await page.locator('[data-testid="settings-host-section-projects"]:visible').click();
  await expect(page).toHaveURL(/\/settings\/hosts\/[^/]+\/projects$/);
  await openProjectSettings(page, input.projectName);
}

export async function openProjectsForSettingsHost(
  page: Page,
  input: { serverId: string; projectName: string },
): Promise<void> {
  await selectSettingsHost(page, input.serverId);
  await expect(page).toHaveURL(buildProjectsSettingsRoute(input.serverId));
  await openProjectSettings(page, input.projectName);
}

export async function expectProjectSettingsName(page: Page, projectName: string): Promise<void> {
  await expect(page.getByRole("main").getByText(projectName, { exact: true })).toBeVisible({
    timeout: PROJECT_VISIBILITY_TIMEOUT,
  });
}

export async function openGroupedProjectWorkspace(
  page: Page,
  projectName: string,
  workspaceName: string,
): Promise<void> {
  const panel = await openCommandCenter(page);
  await panel.getByTestId("command-center-input").fill(workspaceName);
  await panel
    .getByTestId(/^command-center-workspace-.*:/)
    .filter({ hasText: projectName })
    .filter({ hasText: workspaceName })
    .click();
  await expect(page).toHaveURL(/\/workspace\//);
}

export async function expectWorkspaceHost(page: Page, hostName: string): Promise<void> {
  await expect(page.getByRole("button", { name: "Host", exact: true })).toContainText(hostName);
}
