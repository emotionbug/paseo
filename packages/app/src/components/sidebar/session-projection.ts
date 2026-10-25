import type { Agent } from "@/stores/session-store";
import type { SidebarProjectEntry } from "@/hooks/use-sidebar-workspaces-list";
import type { HostProfile } from "@/types/host-connection";

export type SidebarSession = Pick<
  Agent,
  | "id"
  | "serverId"
  | "title"
  | "provider"
  | "workspaceId"
  | "status"
  | "requiresAttention"
  | "lastActivityAt"
  | "createdAt"
  | "archivedAt"
  | "parentAgentId"
>;

export type SessionHostProfile = Pick<HostProfile, "serverId" | "label">;

export interface SessionProject {
  key: string;
  project: SidebarProjectEntry;
  host: SidebarProjectEntry["hosts"][number];
  sessions: SidebarSession[];
}

export interface SessionHost {
  host: SessionHostProfile;
  projects: SessionProject[];
}

export type SessionTreeRow =
  | { kind: "host"; key: string; host: SessionHostProfile; collapsed: boolean }
  | { kind: "project"; key: string; group: SessionProject; collapsed: boolean }
  | { kind: "session"; key: string; session: SidebarSession };

type SessionHosts = Record<string, { agents: ReadonlyMap<string, SidebarSession> } | undefined>;

export function createSessionHostsSelector(
  projects: readonly SidebarProjectEntry[],
  profiles: readonly SessionHostProfile[],
) {
  let previousMaps: Array<ReadonlyMap<string, SidebarSession> | undefined> | null = null;
  let result: SessionHost[] = [];
  return function select(hosts: SessionHosts): SessionHost[] {
    const maps = profiles.map((host) => hosts[host.serverId]?.agents);
    if (previousMaps && maps.every((map, index) => map === previousMaps?.[index])) return result;
    previousMaps = maps;
    result = buildSessionHosts(projects, profiles, hosts);
    return result;
  };
}

export function buildSessionHosts(
  projects: readonly SidebarProjectEntry[],
  profiles: readonly SessionHostProfile[],
  hosts: SessionHosts,
): SessionHost[] {
  const result: SessionHost[] = profiles.map((host) => ({ host, projects: [] }));
  const groupsByHost = new Map(result.map((group) => [group.host.serverId, group]));
  const projectsByWorkspace = new Map<string, SessionProject>();
  for (const project of projects) {
    for (const placement of project.hosts) {
      const hostGroup = groupsByHost.get(placement.serverId);
      if (!hostGroup) continue;
      const group: SessionProject = {
        key: `project:${placement.serverId}:${placement.projectId}`,
        project,
        host: placement,
        sessions: [],
      };
      hostGroup.projects.push(group);
      for (const workspace of project.workspaces) {
        if (workspace.serverId === placement.serverId) {
          projectsByWorkspace.set(workspace.workspaceKey, group);
        }
      }
    }
  }

  for (const { host, projects: hostProjects } of result) {
    for (const agent of hosts[host.serverId]?.agents.values() ?? []) {
      if (agent.archivedAt || agent.parentAgentId || !agent.workspaceId) continue;
      const group = projectsByWorkspace.get(`${host.serverId}:${agent.workspaceId}`);
      if (!group) continue;
      // Streamed messages are deliberately absent from this projection.
      group.sessions.push({
        id: agent.id,
        serverId: host.serverId,
        title: agent.title,
        provider: agent.provider,
        workspaceId: agent.workspaceId,
        status: agent.status,
        requiresAttention: agent.requiresAttention,
        lastActivityAt: agent.lastActivityAt,
        createdAt: agent.createdAt,
        archivedAt: agent.archivedAt,
        parentAgentId: agent.parentAgentId,
      });
    }
    for (const group of hostProjects) {
      group.sessions.sort(
        (a, b) =>
          b.lastActivityAt.getTime() - a.lastActivityAt.getTime() ||
          b.createdAt.getTime() - a.createdAt.getTime() ||
          a.id.localeCompare(b.id),
      );
    }
  }
  return result;
}

export function buildSessionTreeRows(
  hosts: readonly SessionHost[],
  query: string,
  collapsedHostIds: ReadonlySet<string>,
  collapsedProjectKeys: ReadonlySet<string>,
): SessionTreeRow[] {
  const rows: SessionTreeRow[] = [];
  const search = query.trim().toLocaleLowerCase();
  for (const { host, projects } of hosts) {
    const hostMatches = host.label.toLocaleLowerCase().includes(search);
    const hostIndex = rows.length;
    // Search reveals matches without overwriting either ancestor's collapsed state.
    const collapsed = !search && collapsedHostIds.has(host.serverId);
    rows.push({ kind: "host", key: `host:${host.serverId}`, host, collapsed });
    if (collapsed) continue;
    for (const group of projects) {
      const projectMatches = `${group.project.projectName} ${group.host.iconWorkingDir}`
        .toLocaleLowerCase()
        .includes(search);
      const matches =
        !search || hostMatches || projectMatches
          ? group.sessions
          : group.sessions.filter((session) =>
              `${session.title ?? ""} ${session.provider}`.toLocaleLowerCase().includes(search),
            );
      if (search && !hostMatches && !projectMatches && matches.length === 0) continue;
      const projectCollapsed = !search && collapsedProjectKeys.has(group.key);
      rows.push({ kind: "project", key: group.key, group, collapsed: projectCollapsed });
      if (projectCollapsed) continue;
      for (const session of matches) {
        rows.push({ kind: "session", key: `session:${host.serverId}:${session.id}`, session });
      }
    }
    if (search && !hostMatches && rows.length === hostIndex + 1) rows.pop();
  }
  return rows;
}
