import { useState } from "react";
import { useKeyboardActionHandler } from "@/hooks/use-keyboard-action-handler";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import { useWorkspaceFields } from "@/stores/session-store-hooks";
import { redirectIfArchivingActiveWorkspace } from "@/utils/sidebar-workspace-archive-redirect";
import { useWorkspaceArchive } from "@/workspace/use-workspace-archive";

// Archive follows the routed workspace, including empty workspaces with no session sidebar row.
// Keep this subscription out of the root layout so archive-risk updates do not render the shell.
export function WorkspaceArchiveShortcutHandler() {
  const selection = useActiveWorkspaceSelection();
  const serverId = selection?.serverId ?? null;
  const workspace = useWorkspaceFields(serverId, selection?.workspaceId ?? null, (entry) => ({
    id: entry.id,
    name: entry.name,
    workspaceKind: entry.workspaceKind,
    archivingAt: entry.archivingAt,
    isDirty: entry.gitRuntime?.isDirty ?? null,
    aheadOfOrigin: entry.gitRuntime?.aheadOfOrigin ?? null,
    diffStat: entry.diffStat,
  }));
  const [isHidingWorkspace, setIsHidingWorkspace] = useState(false);
  const archiveController = useWorkspaceArchive({
    serverId: serverId ?? "",
    workspaceId: workspace?.id ?? "",
    workspaceKind: workspace?.workspaceKind ?? "directory",
    name: workspace?.name ?? "",
    isDirty: workspace?.isDirty,
    aheadOfOrigin: workspace?.aheadOfOrigin,
    diffStat: workspace?.diffStat,
    onSetHiding: setIsHidingWorkspace,
    onArchiveStarted: () => {
      if (!serverId || !workspace) return;
      redirectIfArchivingActiveWorkspace({
        serverId,
        workspaceId: workspace.id,
        activeWorkspaceSelection: selection,
      });
    },
  });

  useKeyboardActionHandler({
    handlerId: "workspace-archive-global",
    actions: ["workspace.archive"],
    enabled:
      serverId !== null &&
      workspace !== null &&
      workspace.archivingAt === null &&
      !isHidingWorkspace,
    priority: 0,
    handle: () => {
      archiveController.archive();
      return true;
    },
  });

  return null;
}
