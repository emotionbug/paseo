import { test as base } from "../support/fixtures";
import {
  beginWorkspaceFromProject,
  createWorkspaceWithoutAgent,
  expectProjectSettingsName,
  expectProjectWorkspaceCountForHost,
  openGroupedProjectSettings,
  openGroupedProjectWorkspace,
  expectWorkspaceHost,
  openProjectsForSettingsHost,
  openProjectDirectory,
  openProjectDirectoryWithHosts,
  renameProject,
  selectWorkspaceHost,
} from "../support/helpers/project-grouping";
import { startIsolatedHostDaemon } from "../support/helpers/isolated-host-daemon";
import { connectSeedClient, type SeedDaemonClient } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { createTempGitRepo } from "../support/helpers/workspace";

const PRIMARY_HOST_LABEL = "Primary Host";
const SECONDARY_HOST_LABEL = "Secondary Host";
const GROUPED_PROJECT_NAME = "paseo-e2e/grouped-project";
const SHARED_REMOTE_URL = "https://github.com/paseo-e2e/grouped-project.git";

interface HostConnection {
  serverId: string;
  label: string;
  port: number;
}

interface ProjectDirectoryScenario {
  hosts: HostConnection[];
  primaryLabel?: string;
}

interface CreatedProject {
  projectId: string;
}

async function createProject(
  client: SeedDaemonClient,
  input: {
    projectPath: string;
    serverId: string;
    workspaceName: string;
    projectName?: string;
  },
): Promise<CreatedProject> {
  const created = await client.createWorkspace({
    source: { kind: "directory", path: input.projectPath },
    title: input.workspaceName,
  });
  if (!created.workspace) {
    throw new Error(created.error ?? `Failed to create project on ${input.serverId}`);
  }
  if (input.projectName) {
    await client.renameProject(created.workspace.projectId, input.projectName);
  }
  return { projectId: created.workspace.projectId };
}

const test = base.extend<{
  crossHostProject: ProjectDirectoryScenario;
}>({
  crossHostProject: async ({ page: _page }, provide) => {
    const secondaryHost = await startIsolatedHostDaemon("project-grouping-secondary");
    const primaryRepo = await createTempGitRepo("grouped-primary-", {
      originUrl: SHARED_REMOTE_URL,
    });
    const secondaryRepo = await createTempGitRepo("grouped-secondary-", {
      originUrl: SHARED_REMOTE_URL,
    });
    const primaryClient = await connectSeedClient();
    const secondaryClient = await connectSeedClient({ port: secondaryHost.port });
    let primary: CreatedProject | null = null;
    let secondary: CreatedProject | null = null;

    try {
      primary = await createProject(primaryClient, {
        projectPath: primaryRepo.path,
        serverId: getServerId(),
        workspaceName: "Primary workspace",
        projectName: GROUPED_PROJECT_NAME,
      });
      secondary = await createProject(secondaryClient, {
        projectPath: secondaryRepo.path,
        serverId: secondaryHost.serverId,
        workspaceName: "Secondary workspace",
        projectName: GROUPED_PROJECT_NAME,
      });
      await provide({
        primaryLabel: PRIMARY_HOST_LABEL,
        hosts: [
          {
            serverId: secondaryHost.serverId,
            label: SECONDARY_HOST_LABEL,
            port: secondaryHost.port,
          },
        ],
      });
    } finally {
      if (primary) await primaryClient.removeProject(primary.projectId).catch(() => undefined);
      if (secondary)
        await secondaryClient.removeProject(secondary.projectId).catch(() => undefined);
      await primaryClient.close().catch(() => undefined);
      await secondaryClient.close().catch(() => undefined);
      await primaryRepo.cleanup().catch(() => undefined);
      await secondaryRepo.cleanup().catch(() => undefined);
      await secondaryHost.close().catch(() => undefined);
    }
  },
});

async function openScenario(
  page: Parameters<typeof openProjectDirectory>[0],
  scenario: ProjectDirectoryScenario,
): Promise<void> {
  if (scenario.hosts.length === 0) {
    await openProjectDirectory(page);
    return;
  }
  await openProjectDirectoryWithHosts(page, scenario);
}

test.describe("Cross-host project settings and creation", () => {
  test.describe.configure({ timeout: 120_000 });

  test("renames only the selected host's grouped project", async ({ page, crossHostProject }) => {
    await openScenario(page, crossHostProject);
    await openGroupedProjectSettings(page, {
      serverId: getServerId(),
      projectName: GROUPED_PROJECT_NAME,
    });
    await renameProject(page, "Primary-only project name");
    await expectProjectSettingsName(page, "Primary-only project name");
    const secondaryHost = crossHostProject.hosts[0];
    if (!secondaryHost) throw new Error("Expected a secondary host");
    await openProjectsForSettingsHost(page, {
      serverId: secondaryHost.serverId,
      projectName: GROUPED_PROJECT_NAME,
    });
    await expectProjectSettingsName(page, GROUPED_PROJECT_NAME);
  });

  test("creates a workspace on the selected host from a grouped project", async ({
    page,
    crossHostProject,
  }) => {
    await openScenario(page, crossHostProject);
    await beginWorkspaceFromProject(page, GROUPED_PROJECT_NAME);
    await selectWorkspaceHost(page, SECONDARY_HOST_LABEL);
    await createWorkspaceWithoutAgent(page);
    await expectProjectWorkspaceCountForHost(page, {
      projectName: GROUPED_PROJECT_NAME,
      hostName: SECONDARY_HOST_LABEL,
      count: 2,
    });
  });
  test("creates grouped project workspaces on the active workspace host", async ({
    page,
    crossHostProject,
  }) => {
    await openScenario(page, crossHostProject);
    await openGroupedProjectWorkspace(page, GROUPED_PROJECT_NAME, "Secondary workspace");
    await beginWorkspaceFromProject(page, GROUPED_PROJECT_NAME);
    await expectWorkspaceHost(page, SECONDARY_HOST_LABEL);
    await createWorkspaceWithoutAgent(page);
    await expectProjectWorkspaceCountForHost(page, {
      projectName: GROUPED_PROJECT_NAME,
      hostName: SECONDARY_HOST_LABEL,
      count: 2,
    });

    await openGroupedProjectWorkspace(page, GROUPED_PROJECT_NAME, "Primary workspace");
    await beginWorkspaceFromProject(page, GROUPED_PROJECT_NAME);
    await expectWorkspaceHost(page, PRIMARY_HOST_LABEL);
    await createWorkspaceWithoutAgent(page);
    await expectProjectWorkspaceCountForHost(page, {
      projectName: GROUPED_PROJECT_NAME,
      hostName: PRIMARY_HOST_LABEL,
      count: 2,
    });
  });
});
