import { EditingTextInput } from "@/components/ui/text-input";
import { buildWorkspaceTabPersistenceKey } from "@/workspace-tabs/model";
import { memo, useCallback, useMemo, useState, type ComponentProps } from "react";
import { FlatList, Text, View } from "react-native";
import { router } from "expo-router";
import { ChevronDown, ChevronRight, MessageSquare, Plus } from "lucide-react-native";
import { useTranslation } from "react-i18next";
import { StyleSheet } from "react-native-unistyles";
import { useStoreWithEqualityFn } from "zustand/traditional";
import equal from "fast-deep-equal";
import type { SidebarWorkspaceList } from "@/components/sidebar-workspace-list";
import { DensityIcon } from "@/components/ui/density-icon";
import { PressHighlight } from "@/components/ui/press-highlight";
import { ToolbarButton } from "@/components/ui/pane-content-toolbar";
import { useSessionStore } from "@/stores/session-store";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import {
  useWorkspaceLayoutStore,
  findPaneById,
  collectAllTabs,
} from "@/stores/workspace-layout-store";
import { navigateToAgent } from "@/utils/navigate-to-agent";
import { buildNewWorkspaceRoute } from "@/utils/host-routes";
import { createSessionProjectsSelector, type SidebarSession } from "./session-projection";
import type { SidebarProjectEntry } from "@/hooks/use-sidebar-workspaces-list";

type ListProps = ComponentProps<typeof SidebarWorkspaceList>;
type Row =
  | { kind: "project"; key: string; project: SidebarProjectEntry }
  | { kind: "session"; key: string; session: SidebarSession };

export function SessionList(props: ListProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const projectSelector = useMemo(
    () => createSessionProjectsSelector(props.projects),
    [props.projects],
  );
  const selectProjects = useCallback(
    (state: ReturnType<typeof useSessionStore.getState>) => projectSelector(state.sessions),
    [projectSelector],
  );
  const projects = useStoreWithEqualityFn(useSessionStore, selectProjects, equal);
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
  const rows = useMemo(() => {
    const result: Row[] = [];
    const search = query.trim().toLocaleLowerCase();
    for (const group of projects) {
      const projectMatches = group.project.projectName.toLocaleLowerCase().includes(search);
      const sessions = group.sessions.filter(
        (session) =>
          projectMatches ||
          `${session.title ?? ""} ${session.provider}`.toLocaleLowerCase().includes(search),
      );
      if (search && sessions.length === 0) continue;
      result.push({
        kind: "project",
        key: `project:${group.project.viewKey}`,
        project: group.project,
      });
      if (!search && props.collapsedProjectKeys.has(group.project.viewKey)) continue;
      result.push(
        ...sessions.map(
          (session): Row => ({
            kind: "session",
            key: `${session.serverId}:${session.id}`,
            session,
          }),
        ),
      );
    }
    return result;
  }, [projects, query, props.collapsedProjectKeys]);

  const renderItem = useCallback(
    ({ item }: { item: Row }) => {
      if (item.kind === "project")
        return (
          <SessionProjectRow
            project={item.project}
            collapsed={props.collapsedProjectKeys.has(item.project.viewKey)}
            onToggle={props.onToggleProjectCollapsed}
            onBeforeNavigate={props.onWorkspacePress}
          />
        );
      return (
        <SessionRow
          session={item.session}
          onBeforeNavigate={props.onWorkspacePress}
          selected={
            selection?.serverId === item.session.serverId && activeAgentId === item.session.id
          }
        />
      );
    },
    [
      activeAgentId,
      props.collapsedProjectKeys,
      props.onToggleProjectCollapsed,
      props.onWorkspacePress,
      selection?.serverId,
    ],
  );
  const empty = useMemo(
    () => <Text style={styles.empty}>{t("sessionNavigation.empty")}</Text>,
    [t],
  );

  return (
    <View style={styles.container}>
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

function rowKey(row: Row) {
  return row.key;
}

interface BeforeNavigate {
  onBeforeNavigate?: () => void;
}
const SessionProjectRow = memo(function SessionProjectRow({
  project,
  collapsed,
  onToggle,
  onBeforeNavigate,
}: BeforeNavigate & {
  project: SidebarProjectEntry;
  collapsed: boolean;
  onToggle: (key: string) => void;
}) {
  const { t } = useTranslation();
  const host = project.hosts[0];
  const state = useMemo(() => ({ expanded: !collapsed }), [collapsed]);
  const toggle = useCallback(() => onToggle(project.viewKey), [onToggle, project.viewKey]);
  const create = useCallback(() => {
    if (!host) return;
    onBeforeNavigate?.();
    router.push(
      buildNewWorkspaceRoute({
        serverId: host.serverId,
        projectId: host.projectId,
        sourceDirectory: host.iconWorkingDir,
      }),
    );
  }, [host, onBeforeNavigate]);
  return (
    <View style={styles.project}>
      <PressHighlight
        style={styles.projectLabel}
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
        <Text numberOfLines={1} style={styles.projectText}>
          {project.projectName}
        </Text>
      </PressHighlight>
      {host ? (
        <ToolbarButton
          label={t("sessionNavigation.newSession")}
          testID={`session-new-${project.viewKey}`}
          onPress={create}
        >
          <DensityIcon icon={Plus} size="sm" color="foregroundMuted" />
        </ToolbarButton>
      ) : null}
    </View>
  );
});

const SessionRow = memo(function SessionRow({
  session,
  selected,
  onBeforeNavigate,
}: BeforeNavigate & { session: SidebarSession; selected: boolean }) {
  const { t } = useTranslation();
  const state = useMemo(() => ({ selected }), [selected]);
  const rowStyle = useMemo(() => [styles.row, selected && styles.selected], [selected]);
  const titleStyle = useMemo(
    () => [styles.title, session.requiresAttention && styles.attention],
    [session.requiresAttention],
  );
  const open = useCallback(() => {
    onBeforeNavigate?.();
    navigateToAgent({ serverId: session.serverId, agentId: session.id });
  }, [onBeforeNavigate, session.serverId, session.id]);
  return (
    <PressHighlight
      style={rowStyle}
      accessibilityRole="button"
      accessibilityState={state}
      aria-selected={selected}
      testID={`session-row-${session.serverId}-${session.id}`}
      onPress={open}
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
          {session.provider} · {t(`agentList.status.${session.status}`)}
        </Text>
      </View>
    </PressHighlight>
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
  project: {
    flexDirection: "row",
    alignItems: "center",
    paddingHorizontal: theme.spacing[2],
    paddingTop: theme.spacing[2],
  },
  projectLabel: {
    flex: 1,
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingVertical: theme.spacing[2],
  },
  projectText: {
    flex: 1,
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.medium,
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingVertical: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    marginHorizontal: theme.spacing[2],
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
