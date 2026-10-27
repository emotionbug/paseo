import { useCallback, useMemo, useState, type ReactElement } from "react";
import { useTranslation } from "react-i18next";
import { View, type PressableStateCallbackType } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { Circle, Settings2 } from "lucide-react-native";
import {
  MenuItem,
  MenuRoot,
  MenuSeparator,
  MenuSubTrigger,
  MenuSurface,
  MenuTrigger,
  type MenuPageDefinition,
} from "@/components/ui/menu";
import { HostStatusDot } from "@/components/host-status-dot";
import { isWeb } from "@/constants/platform";
import { useHosts } from "@/runtime/host-runtime";
import { useSidebarModel } from "@/components/sidebar/sidebar-model";
import { ProjectIconView } from "@/components/project-icon-view";
import { useProjectIcons } from "@/projects/icons";
import { resolveSidebarProjectIconTargets } from "@/utils/sidebar-project-row-model";
import { projectIconPlaceholderLabelFromDisplayName } from "@/utils/project-display-name";
import type { SidebarProjectEntry } from "@/hooks/use-sidebar-workspaces-list";
import type { Theme } from "@/styles/theme";
import {
  hasActiveSidebarLabelFilter,
  SIDEBAR_UNLABELLED_LABEL_KEY,
} from "@/stores/sidebar-view-store";
import { workspaceLabelKey, type WorkspaceLabelColor } from "@getpaseo/protocol/workspace-labels";
import { useSidebarDisplayPreferences } from "./model";
import { useWorkspaceLabelProjection } from "@/workspace-labels";
import { WorkspaceLabelDot } from "@/workspace-labels/swatch";
import { WorkspaceLabelManagerModal } from "@/workspace-labels/manager-modal";

const mutedIconMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

const ThemedSettings2 = withUnistyles(Settings2, mutedIconMapping);
const ThemedCircle = withUnistyles(Circle, mutedIconMapping);

/** Fits the item's 16pt leading slot with a hair of room, matching the trailing check. */
const OPTION_ICON_SIZE = 14;
const MENU_WIDTH = 232;

/**
 * Unlabelled's stand-in for a color dot: the same circle at the same size, hollow.
 *
 * 11 rather than the dot's 10 because lucide draws a `size` box and puts a stroked r=10-of-24
 * circle inside it, so the ring's outer edge lands on the dots' edge at 11 and 1pt short at 10.
 * The glyphs are what have to agree here, not the boxes they are centred in.
 */
const UNLABELLED_MARK = <ThemedCircle size={11} />;

/**
 * What the sidebar shows and how it is arranged.
 *
 * The root is one row per decision with its current value; the options live a level down. The
 * shape is deliberate — every option of every decision on one surface is what this menu used to
 * be, and it grew a row for each host on top of that.
 */
export function SidebarDisplayPreferencesMenu(): ReactElement {
  const { t } = useTranslation();
  const preferences = useSidebarDisplayPreferences();
  const hosts = useHosts();
  // `allProjects`, never `projects`: the model's `projects` is already filtered, so a picker fed
  // from it would lose the row that undoes the filter as soon as the filter narrowed to one.
  const { allProjects, resolvedProjectFilters } = useSidebarModel();
  const { labels } = useWorkspaceLabelProjection();
  const [managerOpen, setManagerOpen] = useState(false);
  const openManager = useCallback(() => setManagerOpen(true), []);
  const closeManager = useCallback(() => setManagerOpen(false), []);

  const triggerStyle = useCallback(
    ({ hovered = false }: PressableStateCallbackType & { hovered?: boolean }) => [
      styles.trigger,
      hovered && styles.triggerHovered,
    ],
    [],
  );

  const showHostFilter = hosts.length > 1;
  // One project is the whole sidebar, so filtering to it is a no-op with a menu row attached.
  const showProjectFilter = allProjects.length > 1;
  // Nothing to filter by means no row at all. The active-filter half is not redundant: the merged
  // catalog only counts hosts that are online, so a host dropping off would otherwise take away
  // the only way back to a filter that is still hiding workspaces.
  const showLabelFilter = labels.length > 0 || hasActiveSidebarLabelFilter(preferences.labelFilter);

  const pages = useMemo<MenuPageDefinition[]>(() => {
    const definitions: MenuPageDefinition[] = [];
    if (showHostFilter) {
      definitions.push({
        id: "hostFilter",
        title: t("sidebar.display.hostFilter.label"),
        content: <HostFilterPage preferences={preferences} hosts={hosts} />,
      });
    }
    if (showProjectFilter) {
      definitions.push({
        id: "projectFilter",
        title: t("sidebar.display.projectFilter.label"),
        content: (
          <ProjectFilterPage
            projects={allProjects}
            resolvedProjectFilters={resolvedProjectFilters}
            preferences={preferences}
          />
        ),
      });
    }
    if (showLabelFilter) {
      definitions.push({
        id: "labelFilter",
        title: t("workspaceLabels.title"),
        content: (
          <LabelFilterPage labels={labels} preferences={preferences} onManage={openManager} />
        ),
      });
    }
    return definitions;
  }, [
    t,
    preferences,
    hosts,
    showHostFilter,
    showProjectFilter,
    allProjects,
    resolvedProjectFilters,
    showLabelFilter,
    labels,
    openManager,
  ]);

  return (
    <>
      <MenuRoot compactMode="sheet">
        <MenuTrigger
          style={triggerStyle}
          accessibilityRole={isWeb ? undefined : "button"}
          accessibilityLabel={t("sidebar.display.trigger")}
          testID="sidebar-display-preferences-menu"
        >
          <ThemedSettings2 size={14} />
        </MenuTrigger>
        <MenuSurface
          align="end"
          width={MENU_WIDTH}
          pages={pages}
          sheetTitle={t("sidebar.display.heading")}
          testID="sidebar-display-preferences-content"
        >
          {showHostFilter ? (
            <>
              <MenuSeparator />
              {/* A filtered sidebar looks like workspaces went missing, so the branch says so
                from the root rather than making you open it to find out. */}
              <MenuSubTrigger
                id="hostFilter"
                indicator={preferences.hostFilters.length > 0}
                testID="sidebar-display-host-filter"
              >
                {t("sidebar.display.hostFilter.label")}
              </MenuSubTrigger>
            </>
          ) : null}
          {showProjectFilter ? (
            <>
              {/* Host and Project narrow the same list, so they read as one block. The separator
                belongs above whichever of the two is showing first — with a single host there is
                no Host row and Project is what has to carry it. */}
              {showHostFilter ? null : <MenuSeparator />}
              <MenuSubTrigger
                id="projectFilter"
                indicator={resolvedProjectFilters.length > 0}
                testID="sidebar-display-project-filter"
              >
                {t("sidebar.display.projectFilter.label")}
              </MenuSubTrigger>
            </>
          ) : null}
          {showLabelFilter ? (
            <>
              <MenuSeparator />
              <MenuSubTrigger
                id="labelFilter"
                indicator={hasActiveSidebarLabelFilter(preferences.labelFilter)}
                testID="sidebar-display-label-filter"
              >
                {t("workspaceLabels.title")}
              </MenuSubTrigger>
            </>
          ) : null}
          <MenuSeparator />
          <MenuItem
            selected={preferences.showBackground}
            closeOnSelect={false}
            onSelect={preferences.toggleBackground}
          >
            {t("sidebar.display.showBackground")}
          </MenuItem>
        </MenuSurface>
      </MenuRoot>
      <WorkspaceLabelManagerModal visible={managerOpen} onClose={closeManager} />
    </>
  );
}

/**
 * Every label you could filter by, wherever it lives, one row each.
 *
 * The catalog is the merged cross-host one on purpose: a workspace row draws its label in its own
 * host's color because it would otherwise lie, but this page is the whole set of things to filter
 * on and a per-host split would only make you visit it twice.
 *
 * Clear is absent rather than disabled when it has nothing to act on.
 */
function LabelFilterPage({
  labels,
  preferences,
  onManage,
}: {
  labels: ReturnType<typeof useWorkspaceLabelProjection>["labels"];
  preferences: Preferences;
  onManage: () => void;
}): ReactElement {
  const { t } = useTranslation();
  const selected = preferences.labelFilter.labels;
  return (
    <>
      {labels.map((label) => (
        <LabelFilterItem
          key={workspaceLabelKey(label.name)}
          name={label.name}
          label={label.name}
          color={label.color}
          selected={selected.includes(workspaceLabelKey(label.name))}
          onToggle={preferences.toggleLabelFilter}
          testID={`sidebar-label-filter-option-${label.name}`}
        />
      ))}
      <LabelFilterItem
        name={SIDEBAR_UNLABELLED_LABEL_KEY}
        label={t("workspaceLabels.unlabelled")}
        color={null}
        selected={selected.includes(SIDEBAR_UNLABELLED_LABEL_KEY)}
        onToggle={preferences.toggleLabelFilter}
        testID="sidebar-label-filter-option-unlabelled"
      />
      {hasActiveSidebarLabelFilter(preferences.labelFilter) ? (
        <>
          <MenuSeparator />
          <MenuItem
            closeOnSelect={false}
            onSelect={preferences.clearLabelFilter}
            testID="sidebar-label-filter-clear"
          >
            {t("workspaceLabels.filter.clear")}
          </MenuItem>
        </>
      ) : null}
      <MenuSeparator />
      <MenuItem onSelect={onManage} testID="sidebar-label-manage">
        {t("workspaceLabels.manage.open")}
      </MenuItem>
    </>
  );
}

/**
 * One label: its colour leading, its name, and the engine's own check when it is filtered on.
 *
 * The same row the host filter uses, because it is the same question — one press in, one press
 * out, several at once. Four attempts at giving exclusion a shape in this row are in the branch
 * history; each bought a new awkwardness, because two controls in one 232pt menu row is a lot of
 * machinery for a filter nobody asked to invert.
 */
function LabelFilterItem({
  name,
  label,
  color,
  selected,
  onToggle,
  testID,
}: {
  /** The filter key this row acts on. Empty for Unlabelled — see `SIDEBAR_UNLABELLED_LABEL_KEY`. */
  name: string;
  label: string;
  /** `null` is Unlabelled, the one row with no color to stand for. */
  color: WorkspaceLabelColor | null;
  selected: boolean;
  onToggle: (name: string) => void;
  testID: string;
}): ReactElement {
  const handleSelect = useCallback(() => onToggle(name), [name, onToggle]);
  const leading = useMemo(
    () => (color ? <WorkspaceLabelDot color={color} /> : UNLABELLED_MARK),
    [color],
  );

  return (
    <MenuItem
      selected={selected}
      leading={leading}
      closeOnSelect={false}
      onSelect={handleSelect}
      testID={testID}
    >
      {label}
    </MenuItem>
  );
}

type Preferences = ReturnType<typeof useSidebarDisplayPreferences>;

/**
 * Every project the sidebar could show, one row each.
 *
 * A workspace belongs to exactly one project, so this is a plain allowlist — the same shape as the
 * host page, and deliberately not the label page's tri-state.
 *
 * Selection reads `resolvedProjectFilters`, not the stored list. A stored key whose project is not
 * currently visible filters nothing, so showing it as checked here would contradict the sidebar.
 */
function ProjectFilterPage({
  projects,
  resolvedProjectFilters,
  preferences,
}: {
  projects: readonly SidebarProjectEntry[];
  resolvedProjectFilters: readonly string[];
  preferences: Preferences;
}): ReactElement {
  const { t } = useTranslation();
  const iconTargets = useMemo(() => resolveSidebarProjectIconTargets(projects), [projects]);
  // Shares TanStack's cache with the sidebar's own call, so this subscribes rather than refetches.
  const iconByProjectViewKey = useProjectIcons({ projects: iconTargets });

  return (
    <>
      <MenuItem
        selected={resolvedProjectFilters.length === 0}
        closeOnSelect={false}
        onSelect={preferences.clearProjectFilters}
        testID="sidebar-project-filter-all"
      >
        {t("sidebar.display.projectFilter.all")}
      </MenuItem>
      {projects.map((project) => (
        <ProjectFilterItem
          key={project.viewKey}
          viewKey={project.viewKey}
          label={project.projectName}
          iconDataUri={iconByProjectViewKey.get(project.viewKey) ?? null}
          selected={resolvedProjectFilters.includes(project.viewKey)}
          onToggle={preferences.toggleProjectFilter}
        />
      ))}
    </>
  );
}

function ProjectFilterItem({
  viewKey,
  label,
  iconDataUri,
  selected,
  onToggle,
}: {
  viewKey: string;
  label: string;
  iconDataUri: string | null;
  selected: boolean;
  onToggle: (viewKey: string) => void;
}): ReactElement {
  const handleSelect = useCallback(() => onToggle(viewKey), [viewKey, onToggle]);
  const leading = useMemo(
    () => (
      <ProjectIconView
        iconDataUri={iconDataUri}
        initial={projectIconPlaceholderLabelFromDisplayName(label).charAt(0).toUpperCase()}
        projectViewKey={viewKey}
        size={OPTION_ICON_SIZE}
        textStyle={styles.projectIconText}
      />
    ),
    [iconDataUri, label, viewKey],
  );

  return (
    <MenuItem
      selected={selected}
      leading={leading}
      closeOnSelect={false}
      onSelect={handleSelect}
      testID={`sidebar-project-filter-${viewKey}`}
    >
      {label}
    </MenuItem>
  );
}

function HostFilterPage({
  preferences,
  hosts,
}: {
  preferences: Preferences;
  hosts: ReturnType<typeof useHosts>;
}): ReactElement {
  const { t } = useTranslation();
  return (
    <>
      <MenuItem
        selected={preferences.hostFilters.length === 0}
        closeOnSelect={false}
        onSelect={preferences.clearHostFilters}
        testID="sidebar-host-filter-all"
      >
        {t("sidebar.display.hostFilter.all")}
      </MenuItem>
      {hosts.map((host) => (
        <HostFilterItem
          key={host.serverId}
          serverId={host.serverId}
          label={host.label?.trim() || host.serverId}
          selected={preferences.hostFilters.includes(host.serverId)}
          onToggle={preferences.toggleHostFilter}
        />
      ))}
    </>
  );
}

/** The one option row whose mark is live state rather than an icon. */
function HostFilterItem({
  serverId,
  label,
  selected,
  onToggle,
}: {
  serverId: string;
  label: string;
  selected: boolean;
  onToggle: (serverId: string) => void;
}): ReactElement {
  const handleSelect = useCallback(() => onToggle(serverId), [onToggle, serverId]);
  const leading = useMemo(
    () => (
      <View testID={`sidebar-host-filter-status-${serverId}`}>
        <HostStatusDot serverId={serverId} />
      </View>
    ),
    [serverId],
  );

  return (
    <MenuItem
      selected={selected}
      closeOnSelect={false}
      leading={leading}
      onSelect={handleSelect}
      testID={`sidebar-host-filter-${serverId}`}
    >
      {label}
    </MenuItem>
  );
}

const styles = StyleSheet.create((theme) => ({
  trigger: {
    width: 28,
    height: 28,
    alignItems: "center",
    justifyContent: "center",
    borderRadius: theme.borderRadius.md,
  },
  triggerHovered: {
    backgroundColor: theme.colors.surfaceSidebarHover,
  },
  // The icon sits in a 14pt menu slot, so the fallback initial is sized down to match rather
  // than reusing the sidebar row's 16pt figure.
  projectIconText: {
    fontSize: 8,
  },
}));
