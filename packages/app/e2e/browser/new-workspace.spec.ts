import { existsSync } from "node:fs";
import path from "node:path";
import type { Page } from "@playwright/test";
import { buildHostWorkspaceRoute } from "@/utils/host-routes";
import { expect, test } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import {
  archiveWorkspaceFromDaemon,
  archiveLocalWorkspaceFromDaemon,
  assertNewWorkspaceNavigationAndHeader,
  connectNewWorkspaceDaemonClient,
  createWorktreeViaDaemon,
  delayBrowserAgentCreatedStatus,
  expectComposerGithubAttachmentPill,
  expectNewWorkspaceProjectSelected,
  fillNewWorkspaceDraft,
  openGlobalNewWorkspaceComposer,
  openNewWorkspaceComposer,
  openProjectViaDaemon,
  pasteGithubPrUrl,
  submitNewWorkspacePrompt,
} from "../support/helpers/new-workspace";
import {
  commitLocalOnly,
  createTempGitRepo,
  readRepoRef,
  readWorktreeBaseMetadata,
  readWorktreeBranchInfo,
  trackForkUpstream,
} from "../support/helpers/workspace";
import {
  createLocalGithubPrFixture,
  cloneGithubRepoDefaultBranchOnly,
  createTempGithubRepo,
  hasGithubAuth,
  type LocalGhPrFixture,
} from "../support/helpers/github-fixtures";
import { getServerId } from "../support/helpers/server-id";
import { getE2EDaemonPort } from "../support/helpers/daemon-port";
import { chooseAddProjectMethod, expectAddProjectPage } from "../support/helpers/add-project-flow";
import { seedSavedSettingsHosts } from "../support/helpers/settings";
import {
  expectWorkspaceSelected,
  expectWorkspaceHeader,
  switchWorkspaceViaCommandCenter,
  waitForSidebarHydration,
  waitForWorkspaceInCommandCenter,
} from "../support/helpers/workspace-ui";
import { dropFileOnComposer, expectAttachmentPill } from "../support/helpers/composer";

const BACKGROUND_RESOLUTION_FILE = {
  name: "background-context.json",
  mimeType: "application/json",
  buffer: Buffer.from(JSON.stringify({ composer: "background-resolution" })),
};

async function submitNewWorkspaceWithoutPrompt(page: Page) {
  const createButton = page
    .getByTestId("message-input-root")
    .getByRole("button", { name: "Create" });
  await expect(createButton).toBeVisible({ timeout: 30_000 });
  await createButton.click();
}

test.describe("New workspace flow", () => {
  let client: Awaited<ReturnType<typeof connectNewWorkspaceDaemonClient>>;
  const localWorkspaceIds = new Set<string>();
  const localProjectIds = new Set<string>();
  const createdWorktreeDirectories = new Set<string>();
  const localGithubFixtures = new Set<LocalGhPrFixture>();

  test.describe.configure({ timeout: 240_000 });

  test.beforeEach(async () => {
    client = await connectNewWorkspaceDaemonClient();
  });

  test.afterEach(async () => {
    if (client) {
      for (const workspaceDirectory of createdWorktreeDirectories) {
        await archiveWorkspaceFromDaemon(client, workspaceDirectory).catch(() => undefined);
      }
      for (const workspaceId of localWorkspaceIds) {
        await archiveLocalWorkspaceFromDaemon(client, workspaceId).catch(() => undefined);
      }
      for (const projectId of localProjectIds) {
        await client.removeProject(projectId).catch(() => undefined);
      }
    }
    createdWorktreeDirectories.clear();
    localWorkspaceIds.clear();
    localProjectIds.clear();
    await client?.close().catch(() => undefined);
  });

  test.afterAll(async () => {
    for (const fixture of localGithubFixtures) {
      await fixture.cleanup();
    }
    localGithubFixtures.clear();
  });

  test("adds a project from the selected empty host", async ({ page }) => {
    const repo = await createTempGitRepo("new-workspace-project-picker-");
    const primaryServerId = getServerId();
    const emptyServerId = "empty-new-workspace-host";

    try {
      const openedProject = await openProjectViaDaemon(client, repo.path);
      localWorkspaceIds.add(openedProject.workspaceId);
      await seedSavedSettingsHosts(page, [
        {
          serverId: primaryServerId,
          label: "Primary host",
          endpoint: `127.0.0.1:${getE2EDaemonPort()}`,
        },
        {
          serverId: emptyServerId,
          label: "Empty host",
          endpoint: "127.0.0.1:9",
        },
      ]);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await openGlobalNewWorkspaceComposer(page);

      const projectTrigger = page.getByTestId("new-workspace-project-picker-trigger");
      await projectTrigger.click();
      await page.getByPlaceholder("Search projects").fill("no matching project");
      await expect(page.getByTestId("new-workspace-project-picker-add-project")).toBeVisible();
      await page.keyboard.press("Escape");

      await page.getByTestId("host-picker-trigger").click();
      await page.getByTestId(`new-workspace-host-picker-option-${emptyServerId}`).click();
      await expect(projectTrigger).toContainText("Choose project");
      await projectTrigger.click();

      const addProject = page.getByTestId("new-workspace-project-picker-add-project");
      await expect(addProject).toContainText("Add project");
      await expect(addProject).toContainText(/(?:⌘|Ctrl\+)O/);
      await addProject.click();

      await expectAddProjectPage(page, "method");
      await chooseAddProjectMethod(page, "directory-search");
    } finally {
      await repo.cleanup();
    }
  });

  test("command center workspace navigation updates URL and header", async ({ page }) => {
    const serverId = getServerId();

    const firstRepo = await createTempGitRepo("workspace-nav-a-");
    const secondRepo = await createTempGitRepo("workspace-nav-b-");

    try {
      const firstWorkspace = await openProjectViaDaemon(client, firstRepo.path);
      const secondWorkspace = await openProjectViaDaemon(client, secondRepo.path);
      localWorkspaceIds.add(firstWorkspace.workspaceId);
      localWorkspaceIds.add(secondWorkspace.workspaceId);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: firstWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: firstWorkspace.workspaceName,
        subtitle: firstWorkspace.projectDisplayName,
      });

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: secondWorkspace.workspaceId,
      });
      await waitForWorkspaceInCommandCenter(page, {
        serverId,
        workspaceId: secondWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: secondWorkspace.workspaceName,
        subtitle: secondWorkspace.projectDisplayName,
      });

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: firstWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: firstWorkspace.workspaceName,
        subtitle: firstWorkspace.projectDisplayName,
      });
    } finally {
      await secondRepo.cleanup();
      await firstRepo.cleanup();
    }
  });

  test("same-project workspaces switch content without requiring refresh", async ({ page }) => {
    const serverId = getServerId();

    const repo = await createTempGitRepo("workspace-nav-same-project-");

    try {
      const rootWorkspace = await openProjectViaDaemon(client, repo.path);
      const worktreeWorkspace = await createWorktreeViaDaemon(client, {
        cwd: repo.path,
        slug: `nav-${Date.now()}`,
      });
      localWorkspaceIds.add(rootWorkspace.workspaceId);
      createdWorktreeDirectories.add(worktreeWorkspace.workspaceDirectory);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: rootWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: rootWorkspace.workspaceName,
        subtitle: rootWorkspace.projectDisplayName,
      });
      await expectWorkspaceSelected({
        page,
        serverId,
        workspaceId: rootWorkspace.workspaceId,
      });

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: worktreeWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: worktreeWorkspace.workspaceName,
        subtitle: worktreeWorkspace.projectDisplayName,
      });
      await expectWorkspaceSelected({
        page,
        serverId,
        workspaceId: worktreeWorkspace.workspaceId,
      });
      await expectWorkspaceSelected({
        page,
        serverId,
        workspaceId: rootWorkspace.workspaceId,
        selected: false,
      });

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: rootWorkspace.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: rootWorkspace.workspaceName,
        subtitle: rootWorkspace.projectDisplayName,
      });
      await expectWorkspaceSelected({
        page,
        serverId,
        workspaceId: rootWorkspace.workspaceId,
      });
      await expectWorkspaceSelected({
        page,
        serverId,
        workspaceId: worktreeWorkspace.workspaceId,
        selected: false,
      });
    } finally {
      await repo.cleanup();
    }
  });

  test("global new workspace uses the last active project and creates one agent tab", async ({
    page,
  }) => {
    const serverId = getServerId();

    const tempRepo = await createTempGitRepo("new-workspace-");

    try {
      const openedProject = await openProjectViaDaemon(client, tempRepo.path);
      localWorkspaceIds.add(openedProject.workspaceId);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: openedProject.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: openedProject.workspaceName,
        subtitle: openedProject.projectDisplayName,
      });

      await openGlobalNewWorkspaceComposer(page);
      await expectNewWorkspaceProjectSelected(page, openedProject.projectDisplayName);
      await expect(page.getByTestId("workspace-create-isolation-trigger")).toHaveCount(0);
      await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
      const prompt = "Hello from e2e";
      await submitNewWorkspacePrompt(page, prompt);

      const createdWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
        serverId,
        client,
        previousWorkspaceId: openedProject.workspaceId,
        projectDisplayName: openedProject.projectDisplayName,
        assertDiscoverable: false,
        assertHeader: false,
      });
      localWorkspaceIds.add(createdWorkspace.workspaceId);
      expect(createdWorkspace.workspaceDirectory).toBe(tempRepo.path);

      expect(createdWorkspace.workspaceId).not.toBe(openedProject.workspaceId);
      await expect(page).toHaveURL(
        buildHostWorkspaceRoute(serverId, createdWorkspace.workspaceId),
        {
          timeout: 30_000,
        },
      );

      const activeWorkspaceDeckEntry = page
        .getByTestId(`workspace-deck-entry-${serverId}:${createdWorkspace.workspaceId}`)
        .filter({ visible: true });
      await expect(activeWorkspaceDeckEntry).toBeVisible({ timeout: 30_000 });

      const agentTabs = activeWorkspaceDeckEntry.locator('[data-testid^="workspace-tab-agent_"]');
      await expect(agentTabs).toHaveCount(1, { timeout: 30_000 });

      // Workspace setup may auto-open a setup tab that steals focus,
      // hiding the agent panel (display:none removes it from the
      // accessibility tree). Click the agent tab to ensure it's active.
      await agentTabs.first().click();
      const { entries } = await client.fetchAgents({});
      const workspaceAgents = entries.filter(
        ({ agent }) => agent.workspaceId === createdWorkspace.workspaceId,
      );
      expect(workspaceAgents).toHaveLength(1);
      await expect(
        page.getByTestId(`session-row-${serverId}-${workspaceAgents[0]!.agent.id}`),
      ).toBeVisible({ timeout: 30_000 });
      await expectWorkspaceHeader(page, {
        title: prompt,
        subtitle: openedProject.projectDisplayName,
      });

      const composer = page.getByRole("textbox", { name: "Message agent..." });
      await expect(composer).toBeVisible({ timeout: 30_000 });
    } finally {
      await tempRepo.cleanup();
    }
  });

  test("redirects to the optimistic draft tab before agent creation resolves", async ({ page }) => {
    const serverId = getServerId();

    const tempRepo = await createTempGitRepo("new-workspace-optimistic-");
    const agentCreatedDelay = await delayBrowserAgentCreatedStatus(page);

    try {
      const openedProject = await openProjectViaDaemon(client, tempRepo.path);
      localWorkspaceIds.add(openedProject.workspaceId);

      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      await switchWorkspaceViaCommandCenter({
        page,
        serverId,
        workspaceId: openedProject.workspaceId,
      });
      await expectWorkspaceHeader(page, {
        title: openedProject.workspaceName,
        subtitle: openedProject.projectDisplayName,
      });

      await openNewWorkspaceComposer(page, {
        projectKey: openedProject.projectKey,
        projectDisplayName: openedProject.projectDisplayName,
      });

      const composer = page.getByRole("textbox", { name: "Message agent..." });
      await expect(composer).toBeVisible({ timeout: 30_000 });
      await composer.fill("Hello from e2e");

      const createButton = page
        .getByTestId("message-input-root")
        .getByRole("button", { name: "Create" });
      await expect(createButton).toBeVisible({ timeout: 30_000 });
      await createButton.click();

      await agentCreatedDelay.waitForCreateRequest();
      await agentCreatedDelay.waitForDelayedCreatedStatus();

      const createdWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
        serverId,
        client,
        previousWorkspaceId: openedProject.workspaceId,
        projectDisplayName: openedProject.projectDisplayName,
        assertDiscoverable: false,
        assertHeader: false,
      });
      localWorkspaceIds.add(createdWorkspace.workspaceId);
      expect(createdWorkspace.workspaceDirectory).toBe(tempRepo.path);

      await expect(page).toHaveURL(
        buildHostWorkspaceRoute(serverId, createdWorkspace.workspaceId),
        {
          timeout: 30_000,
        },
      );

      const activeWorkspaceDeckEntry = page
        .getByTestId(`workspace-deck-entry-${serverId}:${createdWorkspace.workspaceId}`)
        .filter({ visible: true });
      await expect(activeWorkspaceDeckEntry).toBeVisible({ timeout: 30_000 });

      const draftTabs = activeWorkspaceDeckEntry.locator('[data-testid^="workspace-tab-draft_"]');
      await expect(draftTabs).toHaveCount(1, { timeout: 30_000 });
      await expect(
        activeWorkspaceDeckEntry.locator('[data-testid^="workspace-tab-agent_"]'),
      ).toHaveCount(0);

      agentCreatedDelay.release();
      await expect(
        activeWorkspaceDeckEntry.locator('[data-testid^="workspace-tab-agent_"]'),
      ).toHaveCount(1, { timeout: 30_000 });
    } finally {
      agentCreatedDelay.release();
      await tempRepo.cleanup();
    }
  });

  async function createAndVisitWorktree(
    page: Page,
    input: Parameters<typeof createWorktreeViaDaemon>[1],
  ) {
    const worktree = await createWorktreeViaDaemon(client, input);
    createdWorktreeDirectories.add(worktree.workspaceDirectory);
    await switchWorkspaceViaCommandCenter({
      page,
      serverId: getServerId(),
      workspaceId: worktree.workspaceId,
    });
    await expectWorkspaceHeader(page, {
      title: worktree.workspaceName,
      subtitle: worktree.projectDisplayName,
    });
    await expectWorkspaceSelected({
      page,
      serverId: getServerId(),
      workspaceId: worktree.workspaceId,
    });
    return worktree;
  }

  test("an explicit branch becomes the base of a daemon-created workspace worktree", async ({
    page,
  }) => {
    const tempRepo = await createTempGitRepo("new-workspace-ref-", {
      branches: ["main", "dev"],
    });

    try {
      const openedProject = await openProjectViaDaemon(client, tempRepo.path);
      localWorkspaceIds.add(openedProject.workspaceId);
      await gotoAppShell(page);
      await waitForSidebarHydration(page);

      const createdWorkspace = await createAndVisitWorktree(page, {
        cwd: tempRepo.path,
        slug: `dev-${Date.now()}`,
        action: "branch-off",
        refName: "refs/heads/dev",
      });
      expect(existsSync(createdWorkspace.workspaceDirectory)).toBe(true);
      const branchInfo = await readWorktreeBranchInfo({
        worktreePath: createdWorkspace.workspaceDirectory,
      });
      expect(branchInfo.currentBranch).toBe(path.basename(createdWorkspace.workspaceDirectory));
      expect(branchInfo.hasAncestor(tempRepo.branchHeads.main)).toBe(true);
      expect(branchInfo.hasAncestor(tempRepo.branchHeads.dev)).toBe(true);
    } finally {
      await tempRepo.cleanup();
    }
  });

  // Modern forms create local workspaces. Explicit worktree refs remain supported
  // through the daemon API; assert the resulting checkout, not removed picker UI.
  test.describe("worktree starting refs", () => {
    async function openRepo(page: Page, repoPath: string) {
      const openedProject = await openProjectViaDaemon(client, repoPath);
      localWorkspaceIds.add(openedProject.workspaceId);
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
    }

    async function createWorktreeAndRead(page: Page, cwd: string, refName: string) {
      const createdWorkspace = await createAndVisitWorktree(page, {
        cwd,
        slug: `base-${Date.now()}`,
        action: "branch-off",
        refName,
      });
      return {
        ...createdWorkspace,
        branchInfo: await readWorktreeBranchInfo({
          worktreePath: createdWorkspace.workspaceDirectory,
        }),
      };
    }

    test("branches off the upstream without including local-only commits", async ({ page }) => {
      const tempRepo = await createTempGitRepo("ref-default-ahead-", { withRemote: true });
      try {
        const originHead = readRepoRef(tempRepo.path, "refs/remotes/origin/main");
        commitLocalOnly(tempRepo.path, "one");
        const localHead = commitLocalOnly(tempRepo.path, "two");
        await openRepo(page, tempRepo.path);
        const created = await createWorktreeAndRead(
          page,
          tempRepo.path,
          "refs/remotes/origin/main",
        );
        expect(created.branchInfo.hasAncestor(originHead)).toBe(true);
        expect(created.branchInfo.hasAncestor(localHead)).toBe(false);
      } finally {
        await tempRepo.cleanup();
      }
    });

    test("branches off the local ref when requested explicitly", async ({ page }) => {
      const tempRepo = await createTempGitRepo("ref-default-local-pick-", { withRemote: true });
      try {
        commitLocalOnly(tempRepo.path, "one");
        const localHead = commitLocalOnly(tempRepo.path, "two");
        await openRepo(page, tempRepo.path);
        const created = await createWorktreeAndRead(page, tempRepo.path, "refs/heads/main");
        expect(created.branchInfo.hasAncestor(localHead)).toBe(true);
      } finally {
        await tempRepo.cleanup();
      }
    });

    test("branches off a fork's upstream remote and records the branch name", async ({ page }) => {
      const tempRepo = await createTempGitRepo("ref-default-fork-", { withRemote: true });
      try {
        const upstreamHead = await trackForkUpstream(tempRepo.path);
        const originHead = readRepoRef(tempRepo.path, "refs/remotes/origin/main");
        await openRepo(page, tempRepo.path);
        const created = await createWorktreeAndRead(
          page,
          tempRepo.path,
          "refs/remotes/upstream/main",
        );
        expect(created.branchInfo.hasAncestor(upstreamHead)).toBe(true);
        expect(upstreamHead).not.toBe(originHead);
        expect(await readWorktreeBaseMetadata(created.workspaceDirectory)).toEqual({
          baseRefName: "main",
          baseRef: "refs/remotes/upstream/main",
        });
      } finally {
        await tempRepo.cleanup();
      }
    });
  });

  test("pasted GitHub PR shows PR context in the composer without a starting ref control", async ({
    page,
    context,
  }) => {
    test.skip(!hasGithubAuth(), "Requires GitHub authentication (gh auth login)");
    const ghRepo = await createTempGithubRepo({
      category: "new-workspace-pr-ref",
      prs: [{ title: "Review selected start ref", state: "open" }],
    });
    const pr = ghRepo.prs[0]!;

    try {
      const openedProject = await openProjectViaDaemon(client, pr.localPath);
      localWorkspaceIds.add(openedProject.workspaceId);
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      await openNewWorkspaceComposer(page, {
        projectKey: openedProject.projectKey,
        projectDisplayName: openedProject.projectDisplayName,
      });
      await pasteGithubPrUrl(page, context, pr.url);
      await expectComposerGithubAttachmentPill(page, {
        number: pr.number,
        title: pr.title,
      });
      await expect(page.getByTestId("workspace-create-isolation-trigger")).toHaveCount(0);
      await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
    } finally {
      await ghRepo.cleanup();
    }
  });

  test("pasted GitHub PR and file attachments create a local workspace; the API can check out its PR", async ({
    page,
    context,
  }) => {
    const fixture = await createLocalGithubPrFixture();
    localGithubFixtures.add(fixture);
    const { pr, mainCheckout } = fixture;
    const openedProject = await openProjectViaDaemon(client, mainCheckout.path);
    localWorkspaceIds.add(openedProject.workspaceId);
    localProjectIds.add(openedProject.projectId);

    await gotoAppShell(page);
    await waitForSidebarHydration(page);
    await openNewWorkspaceComposer(page, {
      projectKey: openedProject.projectKey,
      projectDisplayName: openedProject.projectDisplayName,
    });
    await pasteGithubPrUrl(page, context, pr.url);
    const createButton = page.getByTestId("workspace-create-submit");
    await expect(createButton).toBeDisabled();
    await expect(createButton.getByRole("progressbar")).toHaveCount(0);
    await dropFileOnComposer(page, BACKGROUND_RESOLUTION_FILE);
    await expectAttachmentPill(page, "composer-file-attachment-pill");
    await expectComposerGithubAttachmentPill(page, {
      number: pr.number,
      title: pr.title,
    });
    await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
    await submitNewWorkspaceWithoutPrompt(page);

    const localWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
      serverId: getServerId(),
      client,
      previousWorkspaceId: openedProject.workspaceId,
      projectDisplayName: openedProject.projectDisplayName,
    });
    localWorkspaceIds.add(localWorkspace.workspaceId);
    expect(localWorkspace.workspaceDirectory).toBe(mainCheckout.path);
    expect(existsSync(path.join(localWorkspace.workspaceDirectory, "pr-1.txt"))).toBe(false);

    const worktree = await createAndVisitWorktree(page, {
      cwd: mainCheckout.path,
      slug: `pr-${Date.now()}`,
      action: "checkout",
      refName: pr.branch,
      checkoutSource: { kind: "change_request", forge: "github", number: pr.number },
    });
    const branchInfo = await readWorktreeBranchInfo({
      worktreePath: worktree.workspaceDirectory,
    });
    expect(branchInfo.currentBranch).toBe(pr.branch);
    expect(existsSync(path.join(worktree.workspaceDirectory, "pr-1.txt"))).toBe(true);
  });

  test("a pasted PR keeps local creation on main and an explicit worktree base remains independent", async ({
    page,
    context,
  }) => {
    const fixture = await createLocalGithubPrFixture();
    localGithubFixtures.add(fixture);
    const { pr, mainCheckout } = fixture;
    const openedProject = await openProjectViaDaemon(client, mainCheckout.path);
    localWorkspaceIds.add(openedProject.workspaceId);
    localProjectIds.add(openedProject.projectId);

    await gotoAppShell(page);
    await waitForSidebarHydration(page);
    await openNewWorkspaceComposer(page, {
      projectKey: openedProject.projectKey,
      projectDisplayName: openedProject.projectDisplayName,
    });
    await pasteGithubPrUrl(page, context, pr.url);
    await expectComposerGithubAttachmentPill(page, {
      number: pr.number,
      title: pr.title,
    });
    await fillNewWorkspaceDraft(page, `${pr.url}\nKeep this checkout on main`);
    await expect(page.getByTestId("new-workspace-ref-picker-trigger")).toHaveCount(0);
    await submitNewWorkspaceWithoutPrompt(page);
    const localWorkspace = await assertNewWorkspaceNavigationAndHeader(page, {
      serverId: getServerId(),
      client,
      previousWorkspaceId: openedProject.workspaceId,
      projectDisplayName: openedProject.projectDisplayName,
    });
    localWorkspaceIds.add(localWorkspace.workspaceId);
    expect(localWorkspace.workspaceDirectory).toBe(mainCheckout.path);
    const localBranch = await readWorktreeBranchInfo({ worktreePath: mainCheckout.path });
    expect(localBranch.currentBranch).toBe("main");
    expect(existsSync(path.join(localWorkspace.workspaceDirectory, "pr-1.txt"))).toBe(false);

    const worktree = await createAndVisitWorktree(page, {
      cwd: mainCheckout.path,
      slug: `main-${Date.now()}`,
      action: "branch-off",
      refName: "refs/heads/main",
    });
    expect(existsSync(path.join(worktree.workspaceDirectory, "pr-1.txt"))).toBe(false);
  });

  test("the API creates a PR worktree visible in the browser when its head branch is not fetched", async ({
    page,
  }) => {
    test.skip(!hasGithubAuth(), "Requires GitHub authentication (gh auth login)");
    const ghRepo = await createTempGithubRepo({
      category: "new-workspace-pr-worktree",
      prs: [{ title: "Checkout PR worktree", state: "open" }],
    });
    const pr = ghRepo.prs[0]!;
    const mainCheckout = await cloneGithubRepoDefaultBranchOnly(ghRepo);

    try {
      const openedProject = await openProjectViaDaemon(client, mainCheckout.path);
      localWorkspaceIds.add(openedProject.workspaceId);
      await gotoAppShell(page);
      await waitForSidebarHydration(page);
      const worktree = await createAndVisitWorktree(page, {
        cwd: mainCheckout.path,
        slug: `pr-unfetched-${Date.now()}`,
        action: "checkout",
        refName: pr.branch,
        checkoutSource: { kind: "change_request", forge: "github", number: pr.number },
      });
      const branchInfo = await readWorktreeBranchInfo({
        worktreePath: worktree.workspaceDirectory,
      });
      expect(branchInfo.currentBranch).toBe(pr.branch);
      expect(existsSync(path.join(worktree.workspaceDirectory, "pr-1.txt"))).toBe(true);
    } finally {
      await mainCheckout.cleanup();
      await ghRepo.cleanup();
    }
  });
});
