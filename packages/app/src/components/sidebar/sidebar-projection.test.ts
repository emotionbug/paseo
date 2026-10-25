import {
  buildSessionHosts,
  buildSessionTreeRows,
  createSessionHostsSelector,
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

  const profiles = [
    { serverId: "srv", label: "Laptop" },
    { serverId: "other", label: "Build server" },
  ];

  it("retains the directory and session projection through unrelated stream updates", () => {
    const selector = createSessionHostsSelector(
      [makeProject([makeWorkspace("main").placement])],
      profiles,
    );
    const agents = new Map([["chat", session("chat")]]);
    const first = selector({ srv: { agents } });
    const next = selector({ srv: { agents }, unrelated: { agents: new Map() } });
    expect(next).toBe(first);
    expect(next[0].projects[0].sessions.map((agent) => agent.id)).toEqual(["chat"]);
  });

  it("always renders host, directory, then session, including a single host and directory", () => {
    const groups = buildSessionHosts(
      [makeProject([makeWorkspace("main").placement])],
      [profiles[0]],
      { srv: { agents: new Map([["chat", session("chat")]]) } },
    );
    expect(buildSessionTreeRows(groups, "", new Set(), new Set()).map((row) => row.key)).toEqual([
      "host:srv",
      "project:srv:project",
      "session:srv:chat",
    ]);
    expect(groups[0].projects[0]).toMatchObject({
      key: "project:srv:project",
      host: { serverId: "srv", projectId: "project", iconWorkingDir: "/repo/project" },
    });
  });

  it("preserves directory order and sorts activity only within each directory", () => {
    const groups = buildSessionHosts(
      [
        makeProject([makeWorkspace("main").placement]),
        makeProject(
          [makeWorkspace("second", "done", [], "second-project").placement],
          "second-project",
        ),
      ],
      [profiles[0]],
      {
        srv: {
          agents: new Map([
            ["old", session("old")],
            [
              "newest",
              session("newest", {
                workspaceId: "second",
                lastActivityAt: new Date("2026-10-25"),
              }),
            ],
            ["recent", session("recent", { lastActivityAt: new Date("2026-10-24") })],
          ]),
        },
      },
    );
    expect(buildSessionTreeRows(groups, "", new Set(), new Set()).map((row) => row.key)).toEqual([
      "host:srv",
      "project:srv:project",
      "session:srv:recent",
      "session:srv:old",
      "project:srv:second-project",
      "session:srv:newest",
    ]);
  });

  it("retains empty directories and distinct directories with the same display name", () => {
    const first = makeProject([], "first");
    const second = makeProject([], "second");
    first.projectName = "shared";
    second.projectName = "shared";
    const groups = buildSessionHosts([first, second], [profiles[0]], {});
    expect(
      groups[0].projects.map((group) => ({
        key: group.key,
        name: group.project.projectName,
        directory: group.host.iconWorkingDir,
        sessions: group.sessions,
      })),
    ).toEqual([
      { key: "project:srv:first", name: "shared", directory: "/repo/first", sessions: [] },
      { key: "project:srv:second", name: "shared", directory: "/repo/second", sessions: [] },
    ]);
    expect(
      buildSessionTreeRows(groups, "shared", new Set(["srv"]), new Set()).map((row) => row.key),
    ).toEqual(["host:srv", "project:srv:first", "project:srv:second"]);
    expect(
      buildSessionTreeRows(groups, "/repo/second", new Set(), new Set()).map((row) => row.key),
    ).toEqual(["host:srv", "project:srv:second"]);
  });

  it("does not leak archived, child, or filtered-project sessions into a directory", () => {
    const agents = [
      session("visible"),
      session("archived", { archivedAt: new Date("2026-10-24") }),
      session("child", { parentAgentId: "visible" }),
      session("elsewhere", { workspaceId: "other" }),
    ];
    const groups = buildSessionHosts([makeProject([makeWorkspace("main").placement])], profiles, {
      srv: { agents: new Map(agents.map((agent) => [agent.id, agent])) },
    });
    expect(groups[0].projects[0].sessions.map((agent) => agent.id)).toEqual(["visible"]);
    expect(groups[1].projects).toEqual([]);
  });

  it("keeps offline and empty hosts visible even without directories", () => {
    expect(buildSessionHosts([], profiles, {})).toEqual(
      profiles.map((host) => ({ host, projects: [] })),
    );
  });

  it("scopes identical directory, workspace and session IDs to their actual host placements", () => {
    const first = makeWorkspace("main").placement;
    const second = { ...first, workspaceKey: "other:main", serverId: "other" };
    const project = makeProject([first, second]);
    project.hosts.push({
      ...project.hosts[0],
      serverId: "other",
    });
    const groups = buildSessionHosts([project], profiles, {
      srv: { agents: new Map([["same", session("same")]]) },
      other: { agents: new Map([["same", session("same", { serverId: "other" })]]) },
    });
    expect(buildSessionTreeRows(groups, "", new Set(), new Set()).map((row) => row.key)).toEqual([
      "host:srv",
      "project:srv:project",
      "session:srv:same",
      "host:other",
      "project:other:project",
      "session:other:same",
    ]);
    expect(
      buildSessionTreeRows(groups, "", new Set(), new Set(["project:srv:project"])).map(
        (row) => row.key,
      ),
    ).toEqual([
      "host:srv",
      "project:srv:project",
      "host:other",
      "project:other:project",
      "session:other:same",
    ]);
    expect(
      buildSessionTreeRows(groups, "", new Set(["srv"]), new Set()).map((row) => row.key),
    ).toEqual(["host:srv", "host:other", "project:other:project", "session:other:same"]);
  });

  it("collapses one directory without hiding its siblings or their sessions", () => {
    const groups = buildSessionHosts(
      [
        makeProject([makeWorkspace("main").placement]),
        makeProject([makeWorkspace("second", "done", [], "second").placement], "second"),
      ],
      [profiles[0]],
      {
        srv: {
          agents: new Map([
            ["first", session("first")],
            ["second", session("second", { workspaceId: "second" })],
          ]),
        },
      },
    );
    const rows = buildSessionTreeRows(groups, "", new Set(), new Set(["project:srv:project"]));
    expect(rows.map((row) => row.key)).toEqual([
      "host:srv",
      "project:srv:project",
      "project:srv:second",
      "session:srv:second",
    ]);
    expect(rows[1]).toMatchObject({ kind: "project", collapsed: true });
    expect(rows[2]).toMatchObject({ kind: "project", collapsed: false });
  });

  it("searches collapsed hosts and directories and restores both collapse states", () => {
    const groups = buildSessionHosts([makeProject([makeWorkspace("main").placement])], profiles, {
      srv: { agents: new Map([["chat", session("chat", { title: "Fix navigation" })]]) },
    });
    const collapsedHosts = new Set(["srv"]);
    const collapsedProjects = new Set(["project:srv:project"]);
    for (const query of ["navigation", "laptop", "project", "/repo/project", "codex"]) {
      const rows = buildSessionTreeRows(groups, query, collapsedHosts, collapsedProjects);
      expect(rows.map((row) => row.key)).toEqual([
        "host:srv",
        "project:srv:project",
        "session:srv:chat",
      ]);
      expect(rows[0]).toMatchObject({ kind: "host", collapsed: false });
      expect(rows[1]).toMatchObject({ kind: "project", collapsed: false });
    }
    expect(
      buildSessionTreeRows(groups, "", collapsedHosts, collapsedProjects).map((row) => row.key),
    ).toEqual(["host:srv", "host:other"]);
    expect(
      buildSessionTreeRows(groups, "", new Set(), collapsedProjects).map((row) => row.key),
    ).toEqual(["host:srv", "project:srv:project", "host:other"]);
    expect(collapsedHosts).toEqual(new Set(["srv"]));
    expect(collapsedProjects).toEqual(new Set(["project:srv:project"]));
    expect(buildSessionTreeRows(groups, "missing", collapsedHosts, collapsedProjects)).toEqual([]);
  });
});
