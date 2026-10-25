import { AdaptiveRenameModal } from "@/components/rename-modal";
import { getHostRuntimeStore, useHosts, useHostRegistryLoaded } from "@/runtime/host-runtime";
import { HostStatusDot } from "@/components/host-status-dot";
import { useSidebarViewStore } from "@/stores/sidebar-view-store";
import { MAX_EXPLICIT_AGENT_TITLE_CHARS } from "@getpaseo/protocol/agent-title-limits";
import { EditingTextInput } from "@/components/ui/text-input";
import { buildWorkspaceTabPersistenceKey } from "@/workspace-tabs/model";
import { memo, useCallback, useMemo, useState, type ComponentProps } from "react";
import { FlatList, Text, View } from "react-native";
import { router } from "expo-router";
import { ChevronDown, ChevronRight, MessageSquare, Pencil, Plus } from "lucide-react-native";
import { useTranslation } from "react-i18next";
import { StyleSheet } from "react-native-unistyles";
import { useStoreWithEqualityFn } from "zustand/traditional";
import equal from "fast-deep-equal";
import type { SidebarWorkspaceList } from "@/components/sidebar-workspace-list";
import { DensityIcon } from "@/components/ui/density-icon";
import { PressHighlight } from "@/components/ui/press-highlight";
import { ToolbarButton } from "@/components/ui/pane-content-toolbar";
import { useSessionStore, type SessionState } from "@/stores/session-store";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import {
  useWorkspaceLayoutStore,
  findPaneById,
  collectAllTabs,
} from "@/stores/workspace-layout-store";
import { navigateToAgent } from "@/utils/navigate-to-agent";
import { buildNewWorkspaceRoute } from "@/utils/host-routes";
import {
  buildSessionTreeRows,
  createSessionHostsSelector,
  type SidebarSession,
  type HostSession,
  type SessionHostProfile,
  type SessionTreeRow,
} from "./session-projection";

type ListProps = Omit<
  ComponentProps<typeof SidebarWorkspaceList>,
  "collapsedProjectKeys" | "onToggleProjectCollapsed"
>;

export function SessionList(props: ListProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [collapsedHostIds, setCollapsedHostIds] = useState<ReadonlySet<string>>(() => new Set());
  const allHosts = useHosts();
  const hostRegistryLoaded = useHostRegistryLoaded();
  const hostFilters = useSidebarViewStore((state) => state.hostFilters);
  const visibleHosts = useMemo(() => {
    if (hostFilters.length === 0) return allHosts;
    const matched = allHosts.filter((host) => hostFilters.includes(host.serverId));
    return hostRegistryLoaded && matched.length === 0 ? allHosts : matched;
  }, [allHosts, hostFilters, hostRegistryLoaded]);
  const toggleHost = useCallback((serverId: string) => {
    setCollapsedHostIds((previous) => {
      const next = new Set(previous);
      if (next.has(serverId)) next.delete(serverId);
      else next.add(serverId);
      return next;
    });
  }, []);
  const [renamingSession, setRenamingSession] = useState<SidebarSession | null>(null);
  const closeRename = useCallback(() => setRenamingSession(null), []);
  const submitRename = useCallback(
    async (name: string) => {
      if (!renamingSession) return;
      const client = getHostRuntimeStore().getClient(renamingSession.serverId);
      if (!client) throw new Error(t("workspace.terminal.hostDisconnected"));
      await client.updateAgent(renamingSession.id, { name: name.trim() });
    },
    [renamingSession, t],
  );
  const hostSelector = useMemo(
    () => createSessionHostsSelector(props.projects, visibleHosts),
    [props.projects, visibleHosts],
  );
  const selectHosts = useCallback(
    (state: { sessions: Record<string, SessionState> }) => hostSelector(state.sessions),
    [hostSelector],
  );
  const hosts = useStoreWithEqualityFn(useSessionStore, selectHosts, equal);
  const selection = useActiveWorkspaceSelection();
  const activeAgentId = useWorkspaceLayoutStore((state) => {
    if (!selection) return null;
    const key = buildWorkspaceTabPersistenceKey(selection);
    const layout = key ? state.layoutByWorkspace[key] : null;
    if (!layout) return null;
    const pane = findPaneById(layout.root, layout.focusedPaneId);
    const tab = collectAllTabs(layout.root).find(
      (candidate) => candidate.tabId === pane?.focusedTabId,
    );
    return tab?.target.kind === "agent" ? tab.target.agentId : null;
  });
  const rows = useMemo(
    () => buildSessionTreeRows(hosts, query, collapsedHostIds),
    [hosts, query, collapsedHostIds],
  );

  const renderItem = useCallback(
    ({ item }: { item: SessionTreeRow }) => {
      if (item.kind === "host")
        return (
          <SessionHostRow
            host={item.host}
            collapsed={item.collapsed}
            onToggle={toggleHost}
            onBeforeNavigate={props.onWorkspacePress}
          />
        );
      return (
        <SessionRow
          session={item.session}
          onRename={setRenamingSession}
          onBeforeNavigate={props.onWorkspacePress}
          selected={
            selection?.serverId === item.session.serverId && activeAgentId === item.session.id
          }
        />
      );
    },
    [activeAgentId, toggleHost, props.onWorkspacePress, selection?.serverId],
  );
  const empty = useMemo(
    () => <Text style={styles.empty}>{t("sessionNavigation.empty")}</Text>,
    [t],
  );

  return (
    <View style={styles.container}>
      <AdaptiveRenameModal
        visible={renamingSession !== null}
        title={t("workspace.tabs.menu.renameAgent")}
        initialValue={renamingSession?.title ?? ""}
        submitLabel={t("workspace.tabs.menu.rename")}
        maxLength={MAX_EXPLICIT_AGENT_TITLE_CHARS}
        onClose={closeRename}
        onSubmit={submitRename}
        testID="session-rename-modal"
      />
      {props.listHeaderComponent}
      <EditingTextInput
        initialValue=""
        onChangeText={setQuery}
        style={styles.search}
        placeholder={t("sessionNavigation.search")}
        accessibilityLabel={t("sessionNavigation.search")}
        testID="session-search"
        autoCorrect={false}
      />
      <FlatList
        data={rows}
        renderItem={renderItem}
        keyExtractor={rowKey}
        style={styles.container}
        keyboardShouldPersistTaps="handled"
        refreshing={props.isRefreshing}
        onRefresh={props.onRefresh}
        ListEmptyComponent={empty}
      />
    </View>
  );
}

function rowKey(row: SessionTreeRow) {
  return row.key;
}

interface BeforeNavigate {
  onBeforeNavigate?: () => void;
}
const SessionHostRow = memo(function SessionHostRow({
  host,
  collapsed,
  onToggle,
  onBeforeNavigate,
}: BeforeNavigate & {
  host: SessionHostProfile;
  collapsed: boolean;
  onToggle: (key: string) => void;
}) {
  const { t } = useTranslation();
  const state = useMemo(() => ({ expanded: !collapsed }), [collapsed]);
  const toggle = useCallback(() => onToggle(host.serverId), [onToggle, host.serverId]);
  const create = useCallback(() => {
    onBeforeNavigate?.();
    router.push(buildNewWorkspaceRoute({ serverId: host.serverId }));
  }, [host, onBeforeNavigate]);
  return (
    <View style={styles.host}>
      <PressHighlight
        style={styles.hostLabel}
        testID={`session-host-${host.serverId}`}
        accessibilityRole="button"
        accessibilityState={state}
        aria-expanded={!collapsed}
        onPress={toggle}
      >
        <DensityIcon
          icon={collapsed ? ChevronRight : ChevronDown}
          size="sm"
          color="foregroundMuted"
        />
        <HostStatusDot serverId={host.serverId} />
        <Text numberOfLines={1} style={styles.hostText}>
          {host.label}
        </Text>
      </PressHighlight>
      <ToolbarButton
        label={t("sessionNavigation.newSession")}
        testID={`session-new-${host.serverId}`}
        onPress={create}
      >
        <DensityIcon icon={Plus} size="sm" color="foregroundMuted" />
      </ToolbarButton>
    </View>
  );
});

const SessionRow = memo(function SessionRow({
  session,
  selected,
  onRename,
  onBeforeNavigate,
}: BeforeNavigate & {
  session: HostSession;
  selected: boolean;
  onRename: (session: SidebarSession) => void;
}) {
  const { t } = useTranslation();
  const state = useMemo(() => ({ selected }), [selected]);
  const rowStyle = useMemo(() => [styles.sessionRow, selected && styles.selected], [selected]);
  const rename = useCallback(() => onRename(session), [onRename, session]);
  const titleStyle = useMemo(
    () => [styles.title, session.requiresAttention && styles.attention],
    [session.requiresAttention],
  );
  const open = useCallback(() => {
    onBeforeNavigate?.();
    navigateToAgent({ serverId: session.serverId, agentId: session.id });
  }, [onBeforeNavigate, session.serverId, session.id]);
  return (
    <View style={rowStyle}>
      <PressHighlight
        style={styles.row}
        accessibilityRole="button"
        accessibilityState={state}
        aria-selected={selected}
        testID={`session-row-${session.serverId}-${session.id}`}
        onPress={open}
        onLongPress={rename}
      >
        <DensityIcon
          icon={MessageSquare}
          size="sm"
          color={session.requiresAttention ? "foreground" : "foregroundMuted"}
        />
        <View style={styles.sessionText}>
          <Text numberOfLines={1} style={titleStyle}>
            {session.title?.trim() || t("sessionNavigation.untitled")}
          </Text>
          <Text numberOfLines={1} style={styles.detail}>
            {session.projectName} · {session.provider} · {t(`agentList.status.${session.status}`)}
          </Text>
        </View>
      </PressHighlight>
      <ToolbarButton
        label={t("workspace.tabs.menu.renameAgent")}
        testID={`session-rename-${session.serverId}-${session.id}`}
        onPress={rename}
      >
        <DensityIcon icon={Pencil} size="sm" color="foregroundMuted" />
      </ToolbarButton>
    </View>
  );
});

const styles = StyleSheet.create((theme) => ({
  container: { flex: 1, minHeight: 0 },
  search: {
    color: theme.colors.foreground,
    backgroundColor: theme.colors.surfaceSidebar,
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius.md,
    marginHorizontal: theme.spacing[3],
    marginBottom: theme.spacing[2],
    padding: theme.spacing[2],
    fontSize: theme.fontSize.base,
  },
  host: {
    flexDirection: "row",
    alignItems: "center",
    paddingHorizontal: theme.spacing[2],
    paddingTop: theme.spacing[2],
  },
  hostLabel: {
    flex: 1,
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingVertical: theme.spacing[2],
  },
  hostText: {
    flex: 1,
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.medium,
  },
  sessionRow: {
    flexDirection: "row",
    alignItems: "center",
    marginLeft: theme.spacing[6],
    marginRight: theme.spacing[2],
    borderRadius: theme.borderRadius.md,
  },
  row: {
    flex: 1,
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingVertical: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    borderRadius: theme.borderRadius.md,
  },
  selected: { backgroundColor: theme.colors.border },
  sessionText: { flex: 1, gap: 2 },
  title: { color: theme.colors.foreground, fontSize: theme.fontSize.base },
  attention: { fontWeight: theme.fontWeight.semibold },
  detail: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.sm },
  muted: { flex: 1, color: theme.colors.foregroundMuted, fontSize: theme.fontSize.base },
  empty: {
    padding: theme.spacing[4],
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
  },
}));
