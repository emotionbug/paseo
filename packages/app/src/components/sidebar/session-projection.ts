import type { Agent } from "@/stores/session-store";
import type {
  SidebarProjectEntry,
  SidebarWorkspacePlacement,
} from "@/hooks/use-sidebar-workspaces-list";

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

export interface SessionProject {
  project: SidebarProjectEntry;
  sessions: SidebarSession[];
  emptyWorkspaces: SidebarWorkspacePlacement[];
}

type SessionHosts = Record<string, { agents: ReadonlyMap<string, SidebarSession> } | undefined>;

export function createSessionProjectsSelector(projects: readonly SidebarProjectEntry[]) {
  const serverIds = [
    ...new Set(projects.flatMap((project) => project.hosts.map((host) => host.serverId))),
  ];
  let previousMaps: Array<ReadonlyMap<string, SidebarSession> | undefined> | null = null;
  let result: SessionProject[] = [];
  return function select(hosts: SessionHosts): SessionProject[] {
    const maps = serverIds.map((serverId) => hosts[serverId]?.agents);
    if (previousMaps && maps.every((map, index) => map === previousMaps?.[index])) return result;
    previousMaps = maps;
    result = buildSessionProjects(
      projects,
      Object.fromEntries(serverIds.map((id) => [id, hosts[id]])),
    );
    return result;
  };
}

export function buildSessionProjects(
  projects: readonly SidebarProjectEntry[],
  hosts: SessionHosts,
): SessionProject[] {
  const agentsByWorkspace = new Map<string, SidebarSession[]>();
  for (const [serverId, host] of Object.entries(hosts)) {
    if (!host) continue;
    for (const agent of host.agents.values()) {
      if (agent.archivedAt || agent.parentAgentId || !agent.workspaceId) continue;
      const key = `${serverId}:${agent.workspaceId}`;
      const agents = agentsByWorkspace.get(key) ?? [];
      // Project only sidebar fields; streamed messages must not invalidate this list.
      agents.push({
        id: agent.id,
        serverId,
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
      agentsByWorkspace.set(key, agents);
    }
  }
  return projects.map((project) => {
    const sessions = project.workspaces.flatMap(
      (workspace) => agentsByWorkspace.get(workspace.workspaceKey) ?? [],
    );
    sessions.sort(
      (a, b) =>
        b.lastActivityAt.getTime() - a.lastActivityAt.getTime() ||
        b.createdAt.getTime() - a.createdAt.getTime() ||
        a.id.localeCompare(b.id),
    );
    return {
      project,
      sessions,
      // Keep empty drafts and terminal-only workspaces reachable.
      emptyWorkspaces: project.workspaces.filter(
        (workspace) => !agentsByWorkspace.has(workspace.workspaceKey),
      ),
    };
  });
}
