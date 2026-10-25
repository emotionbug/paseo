import { expect, type BrowserContext, type Page } from "@playwright/test";
import type { CreateAgentRequestMessage, SessionInboundMessage } from "@getpaseo/protocol/messages";
import type { DaemonClient as InternalDaemonClient } from "@getpaseo/client/internal/daemon-client";
import { decodeWorkspaceIdFromPathSegment } from "@/utils/host-routes";
import { connectDaemonClient, loadProtocolSchemas } from "./daemon-client-loader";
import { daemonWsRoutePattern } from "./daemon-port";
import { projectEquivalenceViewKey } from "./project-view-key";
import { expectWorkspaceHeader, waitForWorkspaceInCommandCenter } from "./workspace-ui";
import { withProjectOwnership } from "./project-ownership";

type NewWorkspaceDaemonClient = Pick<
  InternalDaemonClient,
  | "archivePaseoWorktree"
  | "archiveWorkspace"
  | "checkoutRefresh"
  | "close"
  | "connect"
  | "createPaseoWorktree"
  | "createWorkspace"
  | "fetchAgents"
  | "fetchWorkspaces"
  | "getPaseoWorktreeList"
  | "getDaemonConfig"
  | "installDirectoryPlugin"
  | "installPluginSource"
  | "disablePlugin"
  | "enablePlugin"
  | "inspectWorkspaceRecovery"
  | "listProjects"
  | "listTerminals"
  | "on"
  | "patchDaemonConfig"
  | "removeProject"
  | "removePlugin"
  | "reloadPlugin"
  | "setWorkspaceTitle"
>;

type CreateWorkspacePayload = Awaited<ReturnType<NewWorkspaceDaemonClient["createWorkspace"]>>;
type WorkspacePayload = Pick<CreateWorkspacePayload, "error" | "workspace">;
type WorkspaceDescriptor = NonNullable<CreateWorkspacePayload["workspace"]>;

export interface OpenedProject {
  workspaceId: string;
  projectId: string;
  projectKey: string;
  projectDisplayName: string;
  workspaceName: string;
  workspaceDirectory: string;
}

function requireWorkspace(payload: WorkspacePayload) {
  if (payload.error) {
    throw new Error(payload.error);
  }
  if (!payload.workspace) {
    throw new Error("workspace.create returned no workspace.");
  }
  return payload.workspace;
}

async function openedProjectFromWorkspace(
  client: NewWorkspaceDaemonClient,
  workspace: WorkspaceDescriptor,
): Promise<OpenedProject> {
  const payload = await client.listProjects();
  const project = payload.projects.find((candidate) => candidate.projectId === workspace.projectId);
  if (!project?.projectKey) {
    throw new Error(`Project ${workspace.projectId} has no project key`);
  }
  return {
    workspaceId: workspace.id,
    projectId: workspace.projectId,
    projectKey: project.projectKey,
    projectDisplayName: workspace.projectDisplayName,
    workspaceName: workspace.name,
    workspaceDirectory: workspace.workspaceDirectory,
  };
}

async function fetchWorkspaceById(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<WorkspaceDescriptor | null> {
  const payload = await client.fetchWorkspaces();
  return payload.entries.find((entry) => entry.id === workspaceId) ?? null;
}

async function waitForWorkspaceDescriptor(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<WorkspaceDescriptor> {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const workspace = await fetchWorkspaceById(client, workspaceId);
    if (workspace) {
      return workspace;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Workspace descriptor not found: ${workspaceId}`);
}

function parseWorkspaceIdFromPageUrl(page: Page, serverId: string): string | null {
  const pathname = new URL(page.url()).pathname;
  const match = pathname.match(
    new RegExp(`^/h/${serverId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}/workspace/([^/?#]+)`),
  );
  if (!match?.[1]) {
    return null;
  }
  return decodeWorkspaceIdFromPathSegment(match[1]);
}

export async function connectNewWorkspaceDaemonClient(options?: {
  port?: number;
  ownProjects?: boolean;
}): Promise<NewWorkspaceDaemonClient> {
  const client = await connectDaemonClient<NewWorkspaceDaemonClient>({
    clientIdPrefix: "app-e2e-new-workspace",
    port: options?.port,
  });
  if (options?.ownProjects === false) return client;

  return withProjectOwnership(client);
}

export async function openProjectViaDaemon(
  client: NewWorkspaceDaemonClient,
  repoPath: string,
): Promise<OpenedProject> {
  const workspace = requireWorkspace(
    await client.createWorkspace({
      source: { kind: "directory", path: repoPath },
    }),
  );
  return openedProjectFromWorkspace(client, workspace);
}

export async function archiveWorkspaceFromDaemon(
  client: NewWorkspaceDaemonClient,
  workspaceDirectory: string,
  options?: { scope?: "workspace" | "worktree" },
): Promise<void> {
  const payload = await client.archivePaseoWorktree({
    worktreePath: workspaceDirectory,
    ...(options?.scope !== undefined ? { scope: options.scope } : {}),
  });
  if (payload.error) {
    throw new Error(payload.error.message);
  }
  if (!payload.success) {
    throw new Error(`Failed to archive workspace: ${workspaceDirectory}`);
  }
}

export async function archiveLocalWorkspaceFromDaemon(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<void> {
  const payload = await client.archiveWorkspace(workspaceId);
  if (payload.error) {
    throw new Error(payload.error);
  }
  if (!payload.archivedAt) {
    throw new Error(`Failed to archive workspace: ${workspaceId}`);
  }
}

type WorktreeSource = Extract<
  Parameters<NewWorkspaceDaemonClient["createWorkspace"]>[0]["source"],
  { kind: "worktree" }
>;

export async function createWorktreeViaDaemon(
  client: NewWorkspaceDaemonClient,
  input: { cwd: string; slug: string } & Pick<
    WorktreeSource,
    "action" | "refName" | "baseBranch" | "branchName" | "checkoutSource"
  >,
): Promise<OpenedProject> {
  const { slug, ...source } = input;
  const payload = await client.createWorkspace({
    source: { kind: "worktree", ...source, worktreeSlug: slug },
  });
  const workspace = requireWorkspace(payload);
  return openedProjectFromWorkspace(client, workspace);
}

export async function openNewWorkspaceComposer(
  page: Page,
  input: { projectKey: string; projectDisplayName: string },
): Promise<void> {
  await openGlobalNewWorkspaceComposer(page);
  await selectNewWorkspaceProject(page, input);
}

export async function openGlobalNewWorkspaceComposer(page: Page): Promise<void> {
  // The shell keeps a hidden copy of the other layout's sidebar mounted.
  await page.locator('[data-testid="sidebar-global-new-workspace"]:visible').first().click();

  await expect(page).toHaveURL(/\/new(?:\?.*)?$/, {
    timeout: 30_000,
  });
}

export async function openMissingProjectNewWorkspaceComposer(
  page: Page,
  input: { serverId: string; projectId: string; sourceDirectory: string },
): Promise<void> {
  const query = new URLSearchParams({
    serverId: input.serverId,
    projectId: input.projectId,
    dir: input.sourceDirectory,
    name: "Missing project",
  });
  await page.goto(`/new?${query.toString()}`);
  await expect(page).toHaveURL(/\/new\?.*projectId=/u, { timeout: 30_000 });
}

export async function expectNewWorkspaceControlsEnabled(page: Page): Promise<void> {
  await expect(page.getByRole("button", { name: "Workspace project" })).toBeEnabled({
    timeout: 30_000,
  });
  await expect(page.getByRole("textbox", { name: "Message agent..." })).toBeEditable({
    timeout: 30_000,
  });
}

export async function openNewWorkspaceProjectPickerWithShortcut(page: Page): Promise<void> {
  await page.keyboard.press("Control+P");

  const searchInput = page.getByPlaceholder("Search projects");
  await expect(searchInput).toBeVisible({ timeout: 30_000 });
  await expect(searchInput).toBeFocused();
}

export async function expectNewWorkspaceProjectSelected(
  page: Page,
  projectDisplayName: string,
): Promise<void> {
  const projectPicker = page.getByRole("button", { name: "Workspace project" });
  await expect(projectPicker).toBeVisible({ timeout: 30_000 });
  await expect(projectPicker).toContainText(projectDisplayName);
}

export async function expectNewWorkspaceTriggerLabelsAligned(
  page: Page,
  input: { projectLabel: string; hostLabel: string },
): Promise<void> {
  const projectTrigger = page.getByRole("button", { name: "Workspace project" });
  const hostTrigger = page.getByRole("button", { name: "Host", exact: true });
  const projectLabel = projectTrigger.getByText(input.projectLabel, { exact: true });
  const hostLabel = hostTrigger.getByText(input.hostLabel, { exact: true });
  await Promise.all([
    expect(projectLabel).toBeVisible({ timeout: 30_000 }),
    expect(hostLabel).toBeVisible({ timeout: 30_000 }),
  ]);
  const [projectTriggerBox, projectLabelBox, hostTriggerBox, hostLabelBox] = await Promise.all([
    projectTrigger.boundingBox(),
    projectLabel.boundingBox(),
    hostTrigger.boundingBox(),
    hostLabel.boundingBox(),
  ]);
  if (!projectTriggerBox || !projectLabelBox || !hostTriggerBox || !hostLabelBox) {
    throw new Error("New Workspace trigger geometry could not be measured");
  }

  const projectLabelInset = projectLabelBox.x - projectTriggerBox.x;
  const hostLabelInset = hostLabelBox.x - hostTriggerBox.x;
  expect(hostLabelInset).toBeCloseTo(projectLabelInset, 0);
}

export async function fillNewWorkspaceDraft(page: Page, draft: string): Promise<void> {
  const composer = page.getByRole("textbox", { name: "Message agent..." });
  await expect(composer).toBeVisible({ timeout: 30_000 });
  await composer.fill(draft);
}

export async function expectNewWorkspaceDraft(page: Page, draft: string): Promise<void> {
  await expect(page.getByRole("textbox", { name: "Message agent..." })).toHaveValue(draft);
}

export async function selectNewWorkspaceHost(page: Page, hostLabel: string): Promise<void> {
  const trigger = page.getByTestId("host-picker-trigger");
  await trigger.click();
  await page.getByRole("button", { name: hostLabel, exact: true }).click();
  await expect(trigger).toContainText(hostLabel);
}

export async function submitNewWorkspacePrompt(
  page: Page,
  prompt = "Hello from e2e",
): Promise<void> {
  const composer = page.getByRole("textbox", { name: "Message agent..." });
  await expect(composer).toBeVisible({ timeout: 30_000 });
  await composer.fill(prompt);
  const createButton = page
    .getByTestId("message-input-root")
    .getByRole("button", { name: "Create" });
  await expect(createButton).toBeVisible({ timeout: 30_000 });
  await createButton.click();
}

export async function clickNewWorkspaceButton(
  page: Page,
  input: { projectKey: string; projectDisplayName: string; prompt?: string },
): Promise<void> {
  await openNewWorkspaceComposer(page, input);
  await submitNewWorkspacePrompt(page, input.prompt);
}

export async function selectNewWorkspaceProject(
  page: Page,
  input: { projectKey: string; projectDisplayName: string; projectViewKey?: string },
): Promise<void> {
  const trigger = page.getByTestId("new-workspace-project-picker-trigger");
  await expect(trigger).toBeVisible({ timeout: 30_000 });
  await trigger.click();

  const projectViewKey = input.projectViewKey ?? projectEquivalenceViewKey(input.projectKey);
  const option = page.getByTestId(`new-workspace-project-picker-option-${projectViewKey}`);
  await expect(option).toBeVisible({ timeout: 30_000 });
  await option.click();

  await expectNewWorkspaceProjectSelected(page, input.projectDisplayName);
}

export async function submitNewWorkspaceEmpty(page: Page): Promise<void> {
  const createButton = page
    .getByTestId("message-input-root")
    .getByRole("button", { name: "Create" });
  await expect(createButton).toBeVisible({ timeout: 30_000 });
  await createButton.click();
}

export async function expectComposerGithubAttachmentPill(
  page: Page,
  input: { number: number; title: string },
): Promise<void> {
  const pills = page.getByTestId("composer-github-attachment-pill");
  await expect(pills).toHaveCount(1);
  await expect(pills.first()).toContainText(`#${input.number}`);
  await expect(pills.first()).toContainText(input.title);
}

export async function pasteGithubPrUrl(
  page: Page,
  context: BrowserContext,
  url: string,
): Promise<void> {
  const composer = page.getByRole("textbox", { name: "Message agent..." });
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.evaluate((value) => navigator.clipboard.writeText(value), url);
  await composer.focus();
  await page.keyboard.press("Control+V");
}

export async function assertNewWorkspaceNavigationAndHeader(
  page: Page,
  input: {
    serverId: string;
    client: NewWorkspaceDaemonClient;
    previousWorkspaceId: string;
    projectDisplayName: string;
    assertDiscoverable?: boolean;
    assertHeader?: boolean;
  },
): Promise<{ workspaceId: string; workspaceName: string; workspaceDirectory: string }> {
  // URL is the source of truth so another workspace cannot satisfy this.
  await expect
    .poll(
      () => {
        const workspaceId = parseWorkspaceIdFromPageUrl(page, input.serverId);
        return workspaceId && workspaceId !== input.previousWorkspaceId ? workspaceId : null;
      },
      { timeout: 60_000 },
    )
    .not.toBeNull();

  const workspaceId = parseWorkspaceIdFromPageUrl(page, input.serverId);
  if (!workspaceId || workspaceId === input.previousWorkspaceId) {
    throw new Error(`Expected URL to redirect to a new workspace.\nCurrent URL: ${page.url()}`);
  }

  const workspace = await waitForWorkspaceDescriptor(input.client, workspaceId);

  if (input.assertDiscoverable !== false) {
    await waitForWorkspaceInCommandCenter(page, {
      serverId: input.serverId,
      workspaceId: workspace.id,
    });
  }

  if (input.assertHeader !== false) {
    const agents = await input.client.fetchAgents({});
    const agent = agents.entries.find((entry) => entry.agent.workspaceId === workspace.id)?.agent;
    await expectWorkspaceHeader(page, {
      title: agent?.title?.trim() || workspace.name,
      subtitle: input.projectDisplayName,
    });
  }

  return {
    workspaceId: workspace.id,
    workspaceName: workspace.name,
    workspaceDirectory: workspace.workspaceDirectory,
  };
}

type WebSocketMessage = string | Buffer;

function parseWebSocketJson(message: WebSocketMessage): unknown {
  const rawMessage = typeof message === "string" ? message : message.toString("utf8");
  try {
    return JSON.parse(rawMessage);
  } catch {
    return null;
  }
}

export async function loadSessionMessageReaders() {
  // Use the same ESM module instance as the dynamically loaded daemon client.
  const { WSInboundMessageSchema, WSOutboundMessageSchema } = await loadProtocolSchemas();
  return {
    client(message: WebSocketMessage) {
      const parsed = WSInboundMessageSchema.safeParse(parseWebSocketJson(message));
      return parsed.success && parsed.data.type === "session" ? parsed.data.message : null;
    },
    server(message: WebSocketMessage) {
      const parsed = WSOutboundMessageSchema.safeParse(parseWebSocketJson(message));
      return parsed.success && parsed.data.type === "session" ? parsed.data.message : null;
    },
  };
}

export interface AgentCreatedDelayControl {
  release(): void;
  waitForCreateRequest(): Promise<void>;
  waitForDelayedCreatedStatus(): Promise<void>;
  expectSingleWorkspaceIntent(): void;
}

export async function delayBrowserAgentCreatedStatus(
  page: Page,
): Promise<AgentCreatedDelayControl> {
  const frames = await loadSessionMessageReaders();
  const daemonPortPattern = daemonWsRoutePattern();
  const createRequestIds = new Set<string>();
  const creationRequests: SessionInboundMessage[] = [];
  const delayedForwards: Array<() => void> = [];
  let releaseRequested = false;
  let resolveCreateRequest: (() => void) | null = null;
  let resolveDelayedCreatedStatus: (() => void) | null = null;
  const createRequestSeen = new Promise<void>((resolve) => {
    resolveCreateRequest = resolve;
  });
  const delayedCreatedStatusSeen = new Promise<void>((resolve) => {
    resolveDelayedCreatedStatus = resolve;
  });

  await page.routeWebSocket(daemonPortPattern, (ws) => {
    const server = ws.connectToServer();

    ws.onMessage((message) => {
      const sessionMessage = frames.client(message);
      if (sessionMessage?.type === "send_agent_message_request")
        creationRequests.push(sessionMessage);
      if (
        sessionMessage?.type === "create_agent_request" ||
        sessionMessage?.type === "agent.create.request" ||
        sessionMessage?.type === "workspace.create.request"
      ) {
        creationRequests.push(sessionMessage);
        createRequestIds.add(sessionMessage.requestId);
        resolveCreateRequest?.();
      }
      server.send(message);
    });

    server.onMessage((message) => {
      const sessionMessage = frames.server(message);
      const isCreated =
        (sessionMessage?.type === "status" && sessionMessage.payload.status === "agent_created") ||
        sessionMessage?.type === "agent.create.response" ||
        sessionMessage?.type === "workspace.create.response";
      if (
        isCreated &&
        typeof sessionMessage.payload.requestId === "string" &&
        createRequestIds.has(sessionMessage.payload.requestId)
      ) {
        resolveDelayedCreatedStatus?.();
        if (releaseRequested) {
          ws.send(message);
          return;
        }
        delayedForwards.push(() => ws.send(message));
        return;
      }

      ws.send(message);
    });
  });

  return {
    release() {
      releaseRequested = true;
      for (const forward of delayedForwards.splice(0)) {
        forward();
      }
    },

    expectSingleWorkspaceIntent() {
      expect(creationRequests).toHaveLength(1);
      expect(creationRequests[0]).toMatchObject({
        type: "workspace.create.request",
        agent: { initialPrompt: expect.any(String) },
      });
    },
    waitForCreateRequest: () => createRequestSeen,
    waitForDelayedCreatedStatus: () => delayedCreatedStatusSeen,
  };
}

type AgentCreationIntent = Pick<
  CreateAgentRequestMessage,
  "config" | "initialPrompt" | "attachments"
>;

export interface WorkspaceCreatedDelayControl {
  agentRequests: readonly AgentCreationIntent[];
  release(): void;
  waitForCreateRequest(): Promise<void>;
}

/**
 * Holds workspace readiness events and responses so a test can navigate elsewhere before
 * the browser observes creation completion.
 */
export async function delayBrowserWorkspaceCreatedResponse(
  page: Page,
): Promise<WorkspaceCreatedDelayControl> {
  const frames = await loadSessionMessageReaders();
  const daemonPortPattern = daemonWsRoutePattern();
  const agentRequests: AgentCreationIntent[] = [];
  const creationKeys = new Set<string>();
  const createRequestIds = new Set<string>();
  const delayedForwards: Array<() => void> = [];
  let releaseRequested = false;
  let resolveCreateRequest: (() => void) | null = null;
  const createRequestSeen = new Promise<void>((resolve) => {
    resolveCreateRequest = resolve;
  });

  await page.routeWebSocket(daemonPortPattern, (ws) => {
    const server = ws.connectToServer();

    ws.onMessage((message) => {
      const sessionMessage = frames.client(message);
      if (sessionMessage?.type === "create_agent_request") agentRequests.push(sessionMessage);
      if (sessionMessage?.type === "workspace.create.request") {
        createRequestIds.add(sessionMessage.requestId);
        if (sessionMessage.idempotencyKey) creationKeys.add(sessionMessage.idempotencyKey);
        if (sessionMessage.agent) agentRequests.push(sessionMessage.agent);
        resolveCreateRequest?.();
      }
      server.send(message);
    });

    server.onMessage((message) => {
      const sessionMessage = frames.server(message);
      const isCreation =
        (sessionMessage?.type === "workspace.create.response" &&
          createRequestIds.has(sessionMessage.payload.requestId)) ||
        (sessionMessage?.type === "workspace.create.update" &&
          creationKeys.has(sessionMessage.payload.idempotencyKey));
      if (isCreation && !releaseRequested) {
        delayedForwards.push(() => ws.send(message));
        return;
      }

      ws.send(message);
    });
  });

  return {
    agentRequests,
    release() {
      releaseRequested = true;
      for (const forward of delayedForwards.splice(0)) {
        forward();
      }
    },
    waitForCreateRequest: () => createRequestSeen,
  };
}

type WorkspaceEntry = Awaited<
  ReturnType<NewWorkspaceDaemonClient["fetchWorkspaces"]>
>["entries"][number];

/**
 * Waits for a workspace the caller did not already know about, and returns it. Tests that create a
 * workspace without navigating to it have no route to assert on, so the daemon's own list is the
 * signal that creation finished.
 */
export async function waitForCreatedWorkspace(
  client: NewWorkspaceDaemonClient,
  knownWorkspaceIds: ReadonlySet<string>,
  options?: { timeout?: number },
): Promise<WorkspaceEntry> {
  let created: WorkspaceEntry | undefined;
  await expect
    .poll(
      async () => {
        const payload = await client.fetchWorkspaces();
        created = payload.entries.find((entry) => !knownWorkspaceIds.has(entry.id));
        return created !== undefined;
      },
      { timeout: options?.timeout ?? 60_000 },
    )
    .toBe(true);
  if (!created) {
    throw new Error("Workspace list reported a new workspace but it could not be read back");
  }
  return created;
}

export async function countWorkspaceAgents(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<number> {
  const payload = await client.fetchAgents({ filter: { includeArchived: true } });
  return payload.entries.filter((entry) => entry.agent.workspaceId === workspaceId).length;
}

export async function countWorkspaceTerminals(
  client: NewWorkspaceDaemonClient,
  workspaceId: string,
): Promise<number> {
  const payload = await client.listTerminals(undefined, undefined, { workspaceId });
  return payload.terminals.length;
}
