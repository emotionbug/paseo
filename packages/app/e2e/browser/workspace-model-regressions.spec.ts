import { test, expect } from "../support/fixtures";
import { expectWorkspaceTabVisible } from "../support/helpers/archive-tab";
import {
  expectComposerEditable,
  expectComposerVisible,
  submitMessage,
} from "../support/helpers/composer";
import { clickNewChat, gotoWorkspace } from "../support/helpers/launcher";
import {
  assertNewWorkspaceNavigationAndHeader,
  connectNewWorkspaceDaemonClient,
  expectNewWorkspaceProjectSelected,
  openGlobalNewWorkspaceComposer,
  submitNewWorkspaceEmpty,
  submitNewWorkspacePrompt,
} from "../support/helpers/new-workspace";
import { getServerId } from "../support/helpers/server-id";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import {
  expectSubagentRowVisible,
  openSubagentsTrack,
  seedParentWithCrossWorkspaceSubagent,
} from "../support/helpers/subagents";
import { expectWorkspaceHeader, waitForSidebarHydration } from "../support/helpers/workspace-ui";
import { getVisibleWorkspaceAgentTabIds } from "../support/helpers/workspace-tabs";

type NewWorkspaceDaemonClient = Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>;

interface CreatedAgentAssertion {
  workspaceId: string;
  provider: string;
  cwd: string;
  modeId: string | null;
  model: string | null;
}

async function fetchCreatedAgentForWorkspace(
  seeded: SeededWorkspace,
  workspaceId: string,
): Promise<CreatedAgentAssertion | null> {
  const result = await seeded.client.fetchAgents({ scope: "active" });
  const agents = result.entries
    .map((entry) => entry.agent)
    .filter((agent) => agent.cwd === seeded.repoPath && agent.workspaceId === workspaceId);
  if (agents.length !== 1) {
    return null;
  }
  const [agent] = agents;
  return {
    workspaceId,
    provider: agent.provider,
    cwd: agent.cwd,
    modeId: agent.currentModeId,
    model: agent.model,
  };
}

async function fetchWorkspaceName(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<string | null> {
  const result = await client.fetchWorkspaces();
  return result.entries.find((entry) => entry.id === workspaceId)?.name ?? null;
}

async function fetchWorkspaceStatuses(
  client: NewWorkspaceDaemonClient,
  workspaceIds: string[],
): Promise<Record<string, string | null>> {
  const result = await client.fetchWorkspaces();
  const entriesById = new Map(result.entries.map((entry) => [entry.id, entry.status]));
  return Object.fromEntries(
    workspaceIds.map((workspaceId) => [workspaceId, entriesById.get(workspaceId) ?? null]),
  );
}

async function fetchAgentStatus(seeded: SeededWorkspace, agentId: string): Promise<string | null> {
  const result = await seeded.client.fetchAgents({ scope: "active" });
  return result.entries.find((entry) => entry.agent.id === agentId)?.agent.status ?? null;
}

test.describe("Workspace model regressions", () => {
  let client: NewWorkspaceDaemonClient;

  test.describe.configure({ timeout: 240_000 });

  test.beforeEach(async () => {
    client = await connectNewWorkspaceDaemonClient();
  });

  test.afterEach(async () => {
    await client?.close().catch(() => undefined);
  });

  test("same-directory workspace does not show agents owned by another workspace", async ({
    page,
  }) => {
    const seeded = await seedWorkspace({ repoPrefix: "workspace-legacy-agents-" });

    try {
      const ownedAgent = await seeded.client.createAgent({
        provider: "mock",
        cwd: seeded.repoPath,
        workspaceId: seeded.workspaceId,
        title: "Agent owned by original workspace",
        modeId: "load-test",
        model: "ten-second-stream",
      });
      const secondWorkspace = await seeded.client.createWorkspace({
        source: { kind: "directory", path: seeded.repoPath, projectId: seeded.projectId },
        title: "Fresh workspace",
      });
      if (!secondWorkspace.workspace) {
        throw new Error(secondWorkspace.error ?? "Failed to create same-directory workspace");
      }

      await gotoWorkspace(page, secondWorkspace.workspace.id);

      await expect
        .poll(() => getVisibleWorkspaceAgentTabIds(page), { timeout: 30_000 })
        .toEqual([]);

      await gotoWorkspace(page, seeded.workspaceId);
      await expect
        .poll(() => getVisibleWorkspaceAgentTabIds(page), { timeout: 30_000 })
        .toContain(`workspace-tab-agent_${ownedAgent.id}`);
    } finally {
      await seeded.cleanup();
    }
  });

  test("new agent tab in a same-directory workspace picks a default model for the saved provider", async ({
    page,
  }) => {
    const seeded: SeededWorkspace = await seedWorkspace({
      repoPrefix: "workspace-new-agent-model-",
    });

    try {
      const secondWorkspace = await seeded.client.createWorkspace({
        source: { kind: "directory", path: seeded.repoPath, projectId: seeded.projectId },
        title: "Fresh workspace",
      });
      if (!secondWorkspace.workspace) {
        throw new Error(secondWorkspace.error ?? "Failed to create same-directory workspace");
      }
      const workspace = secondWorkspace.workspace;

      await page.addInitScript(() => {
        localStorage.setItem(
          "@paseo:create-agent-preferences",
          JSON.stringify({
            provider: "mock",
            providerPreferences: {
              mock: { mode: "load-test" },
            },
          }),
        );
      });
      await gotoWorkspace(page, workspace.id);
      await clickNewChat(page);

      await expectComposerVisible(page);
      await expectComposerEditable(page);
      const prompt = `Create agent default model ${Date.now()}`;
      await submitMessage(page, prompt);
      await expect(page.getByText("No model is available for the selected provider")).toHaveCount(
        0,
      );
      await expect
        .poll(() => fetchCreatedAgentForWorkspace(seeded, workspace.id), {
          timeout: 10_000,
        })
        .toEqual({
          workspaceId: workspace.id,
          provider: "mock",
          cwd: seeded.repoPath,
          modeId: "load-test",
          model: "five-minute-stream",
        });
    } finally {
      await seeded.cleanup();
    }
  });

  test("local same-directory workspace with an initial prompt shows a generated title", async ({
    page,
  }) => {
    const serverId = getServerId();
    const seeded: SeededWorkspace = await seedWorkspace({
      repoPrefix: "workspace-generated-local-title-",
    });
    const previousConfig = await client.getDaemonConfig();

    try {
      await client.patchDaemonConfig({
        metadataGeneration: {
          providers: [{ provider: "mock", model: "ten-second-stream" }],
        },
      });

      await gotoWorkspace(page, seeded.workspaceId);
      await waitForSidebarHydration(page);
      await openGlobalNewWorkspaceComposer(page);
      await expectNewWorkspaceProjectSelected(page, seeded.projectDisplayName);
      await submitNewWorkspacePrompt(page, "Fix login bug");

      const createdWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
        serverId,
        client,
        previousWorkspaceId: seeded.workspaceId,
        projectDisplayName: seeded.projectDisplayName,
      });
      const createdAgent = (await seeded.client.fetchAgents({ scope: "active" })).entries.find(
        (entry) => entry.agent.workspaceId === createdWorkspace.workspaceId,
      )?.agent;
      expect(createdAgent).not.toBeUndefined();
      const createdRow = page.getByTestId(`session-row-${serverId}-${createdAgent!.id}`);

      await expect
        .poll(() => fetchWorkspaceName(client, createdWorkspace.workspaceId), {
          timeout: 30_000,
        })
        .toBe("Fix login bug");
      await expectWorkspaceHeader(page, {
        title: "Fix login bug",
        subtitle: seeded.projectDisplayName,
      });
      await expect(createdRow).toContainText("Fix login bug", { timeout: 30_000 });
      expect(createdWorkspace.workspaceDirectory).toBe(seeded.workspaceDirectory);
    } finally {
      await client
        .patchDaemonConfig({
          metadataGeneration: {
            providers: previousConfig.config.metadataGeneration.providers,
          },
        })
        .catch(() => undefined);
      await seeded.cleanup();
    }
  });

  test("running agent status remains confined to its owning same-directory workspace", async ({
    page,
  }) => {
    const serverId = getServerId();
    const seeded = await seedWorkspace({
      repoPrefix: "workspace-same-cwd-running-",
    });

    try {
      const secondWorkspace = await seeded.client.createWorkspace({
        source: { kind: "directory", path: seeded.repoPath, projectId: seeded.projectId },
        title: "Existing sibling workspace",
      });
      if (!secondWorkspace.workspace) {
        throw new Error(secondWorkspace.error ?? "Failed to create same-directory workspace");
      }
      const secondWorkspaceId = secondWorkspace.workspace.id;

      const runningAgent = await seeded.client.createAgent({
        provider: "mock",
        cwd: seeded.repoPath,
        workspaceId: seeded.workspaceId,
        title: "Running agent",
        modeId: "load-test",
        model: "five-minute-stream",
        initialPrompt: "stay running",
      });
      await seeded.client.waitForAgentUpsert(
        runningAgent.id,
        (snapshot) => snapshot.status === "running",
        15_000,
      );

      await gotoWorkspace(page, seeded.workspaceId);
      await waitForSidebarHydration(page);

      await openGlobalNewWorkspaceComposer(page);
      await expectNewWorkspaceProjectSelected(page, seeded.projectDisplayName);
      await submitNewWorkspaceEmpty(page);

      const createdWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
        serverId,
        client,
        previousWorkspaceId: seeded.workspaceId,
        projectDisplayName: seeded.projectDisplayName,
      });

      await expect
        .poll(() => fetchAgentStatus(seeded, runningAgent.id), { timeout: 10_000 })
        .toBe("running");

      await expect
        .poll(
          () =>
            fetchWorkspaceStatuses(client, [
              seeded.workspaceId,
              secondWorkspaceId,
              createdWorkspace.workspaceId,
            ]),
          { timeout: 10_000 },
        )
        .toEqual({
          [seeded.workspaceId]: "running",
          [secondWorkspaceId]: "done",
          [createdWorkspace.workspaceId]: "done",
        });

      await expect(page.getByTestId(`session-row-${serverId}-${runningAgent.id}`)).toContainText(
        "Running",
      );
    } finally {
      await seeded.cleanup();
    }
  });

  test("pending permission remains confined to its owning same-directory workspace", async ({
    page,
  }) => {
    const serverId = getServerId();
    const seeded = await seedWorkspace({
      repoPrefix: "workspace-same-cwd-permission-",
    });

    try {
      const secondWorkspace = await seeded.client.createWorkspace({
        source: { kind: "directory", path: seeded.repoPath, projectId: seeded.projectId },
        title: "Permission sibling workspace",
      });
      if (!secondWorkspace.workspace) {
        throw new Error(secondWorkspace.error ?? "Failed to create same-directory workspace");
      }
      const secondWorkspaceId = secondWorkspace.workspace.id;

      const agent = await seeded.client.createAgent({
        provider: "mock",
        cwd: seeded.repoPath,
        workspaceId: seeded.workspaceId,
        title: "Permission agent",
        modeId: "load-test",
        model: "ten-second-stream",
      });
      await seeded.client.sendAgentMessage(agent.id, "Emit synthetic plan approval.");
      const parked = await seeded.client.waitForFinish(agent.id, 15_000);
      expect(parked.status).toBe("permission");

      await gotoWorkspace(page, seeded.workspaceId);
      await waitForSidebarHydration(page);

      await openGlobalNewWorkspaceComposer(page);
      await expectNewWorkspaceProjectSelected(page, seeded.projectDisplayName);
      await submitNewWorkspaceEmpty(page);

      const createdWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
        serverId,
        client,
        previousWorkspaceId: seeded.workspaceId,
        projectDisplayName: seeded.projectDisplayName,
      });

      await expect
        .poll(
          () =>
            fetchWorkspaceStatuses(client, [
              seeded.workspaceId,
              secondWorkspaceId,
              createdWorkspace.workspaceId,
            ]),
          { timeout: 10_000 },
        )
        .toEqual({
          [seeded.workspaceId]: "needs_input",
          [secondWorkspaceId]: "done",
          [createdWorkspace.workspaceId]: "done",
        });

      await expect(page.getByTestId(`session-row-${serverId}-${agent.id}`)).toBeVisible();
    } finally {
      await seeded.cleanup();
    }
  });

  test("cross-workspace subagent opens in its workspace and keeps its parent relationship", async ({
    page,
  }) => {
    const serverId = getServerId();
    const seeded = await seedWorkspace({ repoPrefix: "workspace-cross-subagent-" });

    try {
      const agents = await seedParentWithCrossWorkspaceSubagent(seeded, {
        parentTitle: "Parent agent",
        childTitle: "Cross-workspace subagent",
      });

      await gotoWorkspace(page, agents.child.workspaceId);
      await waitForSidebarHydration(page);
      await expectWorkspaceTabVisible(page, agents.child.id);

      await expect(page.getByTestId(`session-row-${serverId}-${agents.child.id}`)).toHaveCount(0);
      await expect(page.getByTestId(`session-row-${serverId}-${agents.parent.id}`)).toBeVisible();

      await gotoWorkspace(page, agents.parent.workspaceId);
      await openSubagentsTrack(page);
      await expectSubagentRowVisible(page, agents.child.id);
    } finally {
      await seeded.cleanup();
    }
  });
});
