import {
  buildSessionProjects,
  createSessionProjectsSelector,
  type SidebarSession,
} from "./session-projection";
import { i18n } from "@/i18n/i18next";
import { describe, expect, it } from "vitest";
import type {
  SidebarProjectEntry,
  SidebarWorkspaceEntry,
  SidebarWorkspacePlacement,
} from "@/hooks/use-sidebar-workspaces-list";
import { buildSidebarProjection } from "./sidebar-projection";

function makeWorkspace(
  id: string,
  statusBucket: SidebarWorkspaceEntry["statusBucket"] = "done",
  labels: string[] = [],
  projectViewKey = "project",
) {
  const placement: SidebarWorkspacePlacement = {
    workspaceKey: `srv:${id}`,
    serverId: "srv",
    workspaceId: id,
    projectViewKey,
    projectName: "Project",
    projectKind: "git",
    workspaceKind: "worktree",
    name: id,
  };
  const entry: SidebarWorkspaceEntry = {
    ...placement,
    workspaceDirectory: "",
    workspaceDirectoryLabel: "",
    title: null,
    currentBranch: null,
    statusBucket,
    statusEnteredAt: null,
    archivingAt: null,
    diffStat: null,
    prHint: null,
    archiveHasUncommittedChanges: null,
    archiveUnpushedCommitCount: null,
    scripts: [],
    hasRunningScripts: false,
    labels,
  };
  return { placement, entry };
}

function makeProject(
  workspaces: SidebarWorkspacePlacement[],
  viewKey = "project",
): SidebarProjectEntry {
  return {
    viewKey,
    projectName: "Project",
    projectKind: "git",
    iconWorkingDir: `/repo/${viewKey}`,
    hosts: [
      {
        serverId: "srv",
        projectId: viewKey,
        iconWorkingDir: `/repo/${viewKey}`,
        worktreeSupport: "supported" as const,
      },
    ],
    workspaces,
  };
}

function projectionInput(options?: {
  groupMode?: "project" | "status";
  pinnedCollapsed?: boolean;
}) {
  const pinned = makeWorkspace("pinned", "running");
  const unpinned = makeWorkspace("unpinned", "needs_input");
  return {
    projects: [makeProject([pinned.placement, unpinned.placement])],
    pinnedKeys: {
      pinnedWorkspaceKeys: [pinned.placement.workspaceKey],
      pinnedAtByKey: { [pinned.placement.workspaceKey]: "2026-07-12T12:00:00.000Z" },
    },
    pinnedWorkspaceOrder: [],
    workspaceEntriesByKey: new Map([
      [pinned.entry.workspaceKey, pinned.entry],
      [unpinned.entry.workspaceKey, unpinned.entry],
    ]),
    projectNamesByViewKey: new Map([["project", "Project"]]),
    groupMode: options?.groupMode ?? ("project" as const),
    pinnedCollapsed: options?.pinnedCollapsed ?? false,
    collapsedProjectKeys: new Set<string>(),
    collapsedWorkspaceGroupKeys: new Set<string>(),
    t: i18n.t,
  };
}

/**
 * Two projects, one workspace each, both labelled — so every grouping mode puts rows from more
 * than one project on screen, and a mode that asked for fewer icons than it renders would show it.
 */
function twoProjectInput(groupMode: "project" | "status") {
  const first = makeWorkspace("first", "running", ["Urgent"], "project");
  const second = makeWorkspace("second", "needs_input", ["Backend"], "other-project");
  return {
    ...projectionInput({ groupMode }),
    projects: [makeProject([first.placement]), makeProject([second.placement], "other-project")],
    pinnedKeys: { pinnedWorkspaceKeys: [], pinnedAtByKey: {} },
    workspaceEntriesByKey: new Map([
      [first.entry.workspaceKey, first.entry],
      [second.entry.workspaceKey, second.entry],
    ]),
    projectNamesByViewKey: new Map([
      ["project", "Project"],
      ["other-project", "Other project"],
    ]),
  };
}

describe("buildSidebarProjection", () => {
  // The rule that outlived the bug it was written for: a project icon is fetched per project, so
  // whatever a mode groups by, the rows it produces can only reference projects already covered.
  for (const groupMode of ["project", "status"] as const) {
    it(`covers every row ${groupMode} grouping renders with a project icon target`, () => {
      const projection = buildSidebarProjection(twoProjectInput(groupMode));
      const covered = new Set(projection.projectIconTargets.map((target) => target.projectViewKey));

      // Every leading visual the sidebar can paint from this projection: pinned rows, grouped
      // rows, project headers and the rows under them.
      const renderedProjectViewKeys = new Set<string>();
      for (const entry of projection.pinnedGroups.pinnedChats) {
        renderedProjectViewKeys.add(entry.projectViewKey);
      }
      for (const group of projection.workspaceGroups) {
        for (const entry of group.rows) renderedProjectViewKeys.add(entry.projectViewKey);
      }
      for (const project of projection.pinnedGroups.unpinnedProjects) {
        renderedProjectViewKeys.add(project.viewKey);
        for (const entry of project.workspaces) renderedProjectViewKeys.add(entry.projectViewKey);
      }

      expect([...renderedProjectViewKeys].sort()).toEqual(["other-project", "project"]);
      expect([...renderedProjectViewKeys].filter((viewKey) => !covered.has(viewKey))).toEqual([]);
    });
  }

  it("uses one pin-aware projection for project rows and shortcut order", () => {
    const projection = buildSidebarProjection(projectionInput());

    expect(projection.pinnedGroups.pinnedChats.map((entry) => entry.workspaceId)).toEqual([
      "pinned",
    ]);
    const remainingProject = projection.pinnedGroups.unpinnedProjects[0];
    expect(remainingProject?.workspaces.map((entry) => entry.workspaceId)).toEqual(["unpinned"]);
    expect(projection.shortcutModel.shortcutTargets).toEqual([
      { serverId: "srv", workspaceId: "pinned" },
      { serverId: "srv", workspaceId: "unpinned" },
    ]);
  });

  it("keeps pinned chats above status groups and removes them from those groups", () => {
    const projection = buildSidebarProjection(projectionInput({ groupMode: "status" }));

    expect(projection.workspaceGroups.map((group) => group.key)).toEqual(["needs_input"]);
    expect(projection.workspaceGroups[0]?.rows.map((entry) => entry.workspaceId)).toEqual([
      "unpinned",
    ]);
    expect(projection.shortcutModel.shortcutTargets).toEqual([
      { serverId: "srv", workspaceId: "pinned" },
      { serverId: "srv", workspaceId: "unpinned" },
    ]);
  });

  it("does not number pinned chats while the pinned section is collapsed", () => {
    const projection = buildSidebarProjection(
      projectionInput({ groupMode: "status", pinnedCollapsed: true }),
    );

    expect(projection.shortcutModel.shortcutTargets).toEqual([
      { serverId: "srv", workspaceId: "unpinned" },
    ]);
  });
});

describe("session-first sidebar", () => {
  function session(id: string, overrides: Partial<SidebarSession> = {}): SidebarSession {
    return {
      id,
      serverId: "srv",
      workspaceId: "main",
      title: id,
      provider: "codex",
      status: "idle",
      createdAt: new Date("2026-10-01"),
      lastActivityAt: new Date("2026-10-01"),
      archivedAt: null,
      parentAgentId: null,
      ...overrides,
    };
  }

  it("retains the session projection through unrelated stream updates", () => {
    const selector = createSessionProjectsSelector([
      makeProject([makeWorkspace("main").placement]),
    ]);
    const agents = new Map([["chat", session("chat")]]);
    const first = selector({ srv: { agents } });
    const next = selector({ srv: { agents }, unrelated: { agents: new Map() } });
    expect(next).toBe(first);
    expect(next[0].sessions.map((agent) => agent.id)).toEqual(["chat"]);
  });

  it("lists conversations on the same branch separately, newest activity first", () => {
    const old = session("old");
    const recent = session("recent", { lastActivityAt: new Date("2026-10-24") });
    const project = makeProject([makeWorkspace("main").placement]);
    const groups = buildSessionProjects([project], {
      srv: {
        agents: new Map([
          [old.id, old],
          [recent.id, recent],
        ]),
      },
    });
    expect(groups[0].sessions.map((agent) => agent.id)).toEqual(["recent", "old"]);
    expect(groups[0].emptyWorkspaces).toEqual([]);
  });

  it("does not leak archived, child, or filtered-project sessions into the list", () => {
    const agents = [
      session("visible"),
      session("archived", { archivedAt: new Date("2026-10-24") }),
      session("child", { parentAgentId: "visible" }),
      session("elsewhere", { workspaceId: "other" }),
    ];
    const groups = buildSessionProjects([makeProject([makeWorkspace("main").placement])], {
      srv: { agents: new Map(agents.map((agent) => [agent.id, agent])) },
    });
    expect(groups[0].sessions.map((agent) => agent.id)).toEqual(["visible"]);
  });

  it("keeps terminal-only workspaces and empty projects accessible", () => {
    const terminal = makeWorkspace("terminal").placement;
    const project = makeProject([terminal]);
    const emptyProject = makeProject([], "empty");
    expect(buildSessionProjects([project, emptyProject], {})).toEqual([
      { project, sessions: [], emptyWorkspaces: [terminal] },
      { project: emptyProject, sessions: [], emptyWorkspaces: [] },
    ]);
  });

  it("scopes identical workspace and session IDs to their host", () => {
    const first = makeWorkspace("main").placement;
    const second = { ...first, workspaceKey: "other:main", serverId: "other" };
    const groups = buildSessionProjects([makeProject([first, second])], {
      srv: { agents: new Map([["same", session("same")]]) },
      other: { agents: new Map([["same", session("same", { serverId: "other" })]]) },
    });
    expect(groups[0].sessions.map((agent) => `${agent.serverId}:${agent.id}`)).toEqual([
      "srv:same",
      "other:same",
    ]);
  });
});
