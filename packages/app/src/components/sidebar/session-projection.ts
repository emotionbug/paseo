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
export type HostSession = SidebarSession & { projectName: string };

export interface SessionHost {
  host: SessionHostProfile;
  sessions: HostSession[];
}

export type SessionTreeRow =
  | { kind: "host"; key: string; host: SessionHostProfile; collapsed: boolean }
  | { kind: "session"; key: string; session: HostSession };

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
  const projectNamesByWorkspace = new Map<string, string>();
  for (const project of projects) {
    for (const workspace of project.workspaces) {
      projectNamesByWorkspace.set(workspace.workspaceKey, project.projectName);
    }
  }

  return profiles.map((host) => {
    const sessions: HostSession[] = [];
    for (const agent of hosts[host.serverId]?.agents.values() ?? []) {
      if (agent.archivedAt || agent.parentAgentId || !agent.workspaceId) continue;
      const projectName = projectNamesByWorkspace.get(`${host.serverId}:${agent.workspaceId}`);
      if (projectName === undefined) continue;
      // Streamed messages are deliberately absent from this projection.
      sessions.push({
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
        projectName,
      });
    }
    sessions.sort(
      (a, b) =>
        b.lastActivityAt.getTime() - a.lastActivityAt.getTime() ||
        b.createdAt.getTime() - a.createdAt.getTime() ||
        a.id.localeCompare(b.id),
    );
    return { host, sessions };
  });
}

export function buildSessionTreeRows(
  hosts: readonly SessionHost[],
  query: string,
  collapsedHostIds: ReadonlySet<string>,
): SessionTreeRow[] {
  const rows: SessionTreeRow[] = [];
  const search = query.trim().toLocaleLowerCase();
  for (const { host, sessions } of hosts) {
    const hostMatches = host.label.toLocaleLowerCase().includes(search);
    const matches =
      !search || hostMatches
        ? sessions
        : sessions.filter((session) =>
            `${session.title ?? ""} ${session.provider} ${session.projectName}`
              .toLocaleLowerCase()
              .includes(search),
          );
    if (search && !hostMatches && matches.length === 0) continue;
    // Search reveals matches without overwriting the user's collapsed state.
    const collapsed = !search && collapsedHostIds.has(host.serverId);
    rows.push({ kind: "host", key: `host:${host.serverId}`, host, collapsed });
    if (collapsed) continue;
    for (const session of matches) {
      rows.push({ kind: "session", key: `session:${host.serverId}:${session.id}`, session });
    }
  }
  return rows;
}
