import 'dart:async';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:uuid/uuid.dart';

import '../core/app_store.dart';
import '../core/commands.dart';
import '../core/json.dart';
import '../core/scope.dart';
import '../core/target.dart';
import '../features/chat/agent_panel.dart';
import '../features/chat/provider_icon.dart';
import '../features/chat/chrome/status_mark.dart';
import '../features/chat/chat_drafts.dart';
import '../features/files/file_editor_session.dart' show modifiedFileTabs;
import '../features/files/file_panel.dart';
import '../features/files/files_panel.dart';
import '../features/git/changes_panel.dart';
import '../features/git/git_actions.dart';
import '../features/git/open_in_button.dart';
import '../features/git/pull_request_panel.dart';
import '../features/logs/execution_log_panel.dart';
import '../features/navigation/host_dialog.dart';
import '../features/navigation/navigation_actions.dart';
import '../features/navigation/session_sidebar.dart';
import '../features/navigation/session_history_dialog.dart';
import '../features/navigation/host_connections_dialog.dart';
import '../features/navigation/workspace_labels_dialog.dart';
import '../features/navigation/welcome_screen.dart';
import '../features/settings/usage_settings.dart';
import '../features/schedules/schedules_screen.dart';
import '../features/settings/settings_screen.dart';
import '../features/workspace/new_tab_panel.dart';
import '../features/workspace/project_settings_panel.dart';
import '../features/workspace/setup_panel.dart';
import '../features/workspace/workspace_rpc.dart' show requireWorkspaceSuccess;
import '../platform/desktop/desktop_host.dart';
import 'client_activity.dart';
import 'command_center.dart';
import 'common.dart';
import 'controls.dart';
import 'localization.dart';
import 'panel_activity.dart';
import 'shortcuts.dart';
import 'shell_pages.dart';
import 'theme.dart';

class PaseoShell extends StatefulWidget {
  const PaseoShell({super.key, this.startupError, this.activity});
  final Object? startupError;
  final ClientActivity? activity;
  @override
  State<PaseoShell> createState() => _PaseoShellState();
}

class _PaseoShellState extends State<PaseoShell>
    with WidgetsBindingObserver, SingleTickerProviderStateMixin
    implements ShellPageHost {
  AppStore? _store;
  late final _shortcuts = PaseoShortcutRouter(
    (action, args) => unawaited(_run(() => _execute(action, args))),
  );
  final _panelKeys = <String, GlobalKey>{};
  final _paneKeys = <String, GlobalKey>{};
  final _activated = <String>{};
  final _sidebarKey = GlobalKey();
  final _explorerKey = GlobalKey();
  bool _compact = false;
  bool _visible = true;
  bool _active =
      WidgetsBinding.instance.lifecycleState == null ||
      WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed;
  bool _commandCenter = false;
  bool _leaving = false;
  Offset? _edgeStart;
  // Original mobile-panels: the compact Explorer slides in from the right over the
  // conversation (0 closed, 1 open) with a 220ms bezier(.25,.1,.25,1) timing, and a
  // horizontal drag past a third of the width or 500px/s commits the gesture.
  late final _explorerMotion = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 220),
  );
  bool? _explorerOpen;
  bool _explorerDragging = false;
  double _drag = 0;
  VoidCallback? _draftGuard;
  // A desktop page (history, schedules) shown beside the sidebar until a session
  // or tab is opened; its own routes pop back to the conversation.
  Widget? _page;
  int _pageSerial = 0;
  GlobalKey<NavigatorState>? _pageNavigator;
  late final _pageObserver = ShellPageObserver(
    () => WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) setState(() => _page = null);
    }),
  );
  AppStore get store => _store!;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    ShellPages.attach(this);
    _shortcuts.initialize().catchError((Object error) {
      if (mounted) showError(context, error);
    });
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final next = PaseoScope.of(context);
    if (identical(next, _store)) return;
    _draftGuard?.call();
    _store = next;
    _draftGuard = store.registerCloseGuard('application-drafts', () async {
      await ChatDrafts.instance.flush();
      return true;
    });
    store.hostChooser = () async => mounted ? showHostChooser(context, store) : null;
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final visible =
        state != AppLifecycleState.hidden &&
        state != AppLifecycleState.paused &&
        state != AppLifecycleState.detached;
    final active = state == AppLifecycleState.resumed;
    // Original host-runtime-bootstrap: AppState "active" → `setAppVisible(true)`.
    if (active && !_active && _store != null) store.setAppVisible(true);
    if ((visible != _visible || active != _active) && mounted) {
      setState(() {
        _visible = visible;
        _active = active;
      });
    }
  }

  @override
  void dispose() {
    _draftGuard?.call();
    ShellPages.detach(this);
    WidgetsBinding.instance.removeObserver(this);
    _shortcuts.dispose();
    _explorerMotion.dispose();
    super.dispose();
  }

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  Future<void> _back() async {
    if (_leaving) return;
    final target = store.selected;
    if (_compact &&
        store.compactDestination == CompactDestination.agent &&
        target != null &&
        const {
          'file',
          'working_diff',
          'commit_diff',
          'provider_subagent',
          'pull_request',
          'execution_log',
          'project_settings',
        }.contains(target.kind)) {
      await store.closeTab(target.key);
      return;
    }
    if (store.handleBack(compact: _compact)) return;
    _leaving = true;
    try {
      if (!await store.canCloseWindow()) return;
      if (DesktopHost.supported) {
        await DesktopHost.instance?.close();
      } else {
        await SystemNavigator.pop();
      }
    } finally {
      _leaving = false;
    }
  }

  Future<HostState?> _chooseHost() async {
    final current = store.selected;
    final host = current == null ? null : store.hostFor(current.serverId);
    if (host != null) return host;
    if (store.hosts.isEmpty) {
      await showHostDialog(context);
      return store.hosts.firstOrNull;
    }
    if (store.hosts.length == 1) return store.hosts.first;
    return showHostChooser(context, store);
  }

  Future<void> _newSession({
    bool newWorkspace = false,
    bool worktree = false,
  }) async {
    final host = await _chooseHost();
    if (host == null || !mounted) return;
    final selected = store.selected;
    store.open(
      PanelTarget(
        kind: 'new_tab',
        serverId: host.serverId,
        workspaceId: newWorkspace ? '' : selected?.workspaceId ?? '',
        cwd: selected?.cwd ?? '',
        id: const Uuid().v4(),
        title: tr(context, 'sessionNavigation.newSession'),
        data: {if (worktree) 'source': 'branch-off'},
      ),
    );
  }

  void _openWorkspacePanel(String kind, {bool explorer = false}) {
    final target = store.selected;
    if (target == null || target.workspaceId.isEmpty) {
      throw StateError('Open a session or workspace first.');
    }
    final next = PanelTarget(
      kind: kind,
      serverId: target.serverId,
      workspaceId: target.workspaceId,
      cwd: target.cwd,
    );
    if (explorer) {
      store.openExplorer(next);
    } else {
      store.open(next);
    }
  }

  bool _inWorkspace(PanelTarget? tab, PanelTarget workspace) =>
      tab != null &&
      tab.serverId == workspace.serverId &&
      tab.workspaceId == workspace.workspaceId;

  /// Original compact `toggleExplorerSidebar`: the Explorer is an overlay over the
  /// conversation, never a tab. Opening it reveals this workspace's explorer view (the
  /// default Files + Changes views when it has none yet); closing returns to the agent.
  void _toggleCompactExplorer() {
    if (store.compactDestination == CompactDestination.explorer) {
      store.showCompactExplorer(false);
      return;
    }
    final target = store.selected;
    if (target == null || target.workspaceId.isEmpty) return;
    if (_inWorkspace(store.selectedExplorer, target)) {
      store.showCompactExplorer(true);
      return;
    }
    // Original per-workspace Explorer pane: it reopens on the view it last focused,
    // and a pane without a remembered focus shows its default-focused Changes view.
    final views = [
      for (final tab in store.explorerTabs)
        if (_inWorkspace(tab, target)) tab,
    ];
    final existing = store.explorerFocusFor(target) ??
        (views.where((tab) => tab.kind == 'changes_tree').firstOrNull ??
                views.firstOrNull)
            ?.key;
    if (existing != null) {
      store.selectExplorerTab(existing);
    } else {
      _openDefaultExplorer();
    }
  }

  /// Original `createDefaultExplorerSidebarTabs`: a workspace's Explorer starts with the
  /// Files and Changes views, Changes in front (a new pane focuses its last tab).
  void _openDefaultExplorer() {
    _openWorkspacePanel('files', explorer: true);
    _openWorkspacePanel('changes_tree', explorer: true);
  }

  /// Original explorer "+" (`useWorkspaceTabLaunchCatalog`, purpose "supporting", host
  /// "explorer", `SUPPORTING_LAUNCH_ORDER` minus the excluded terminal and browser):
  /// Changes, the Diff view (wide layouts only — compact Changes already shows each
  /// file's diff inline), Files, a new agent and the pull request, the Changes and Files
  /// singletons dropped once the Explorer holds them.
  Future<void> _explorerNewTab(Rect anchor, List<String> keys) async {
    final kinds = {for (final key in keys) store.targetFor(key)?.kind};
    final kind = await showPaseoMenu<String>(
      context,
      title: tr(context, 'workspace.tabs.actions.newTab'),
      anchor: anchor,
      entries: [
        if (!kinds.contains('changes_tree'))
          PaseoMenuEntry.item(
            value: 'changes_tree',
            label: tr(context, 'panels.diff.changesLabel'),
            icon: _icon('changes_tree'),
          ),
        if (!_compact)
          PaseoMenuEntry.item(
            value: 'working_diff',
            label: tr(context, 'panels.diff.diffLabel'),
            icon: _icon('working_diff'),
          ),
        if (!kinds.contains('files'))
          PaseoMenuEntry.item(
            value: 'files',
            label: tr(context, 'panels.files.label'),
            icon: _icon('files'),
          ),
        PaseoMenuEntry.item(
          value: 'new_tab',
          label: tr(context, 'workspace.tabs.fallback.agent'),
          icon: LucideIcons.squarePen300,
        ),
        PaseoMenuEntry.item(
          value: 'pull_request',
          label: tr(context, 'panels.pullRequest.label'),
          icon: _icon('pull_request'),
        ),
      ],
    );
    if (kind == null || !mounted) return;
    if (kind != 'new_tab') {
      _openWorkspacePanel(kind, explorer: true);
      return;
    }
    // The original agent launch opens a new-agent draft in the Explorer pane itself.
    final target = store.selected;
    if (target == null || target.workspaceId.isEmpty) {
      throw StateError('Open a session or workspace first.');
    }
    store.openExplorer(
      PanelTarget(
        kind: 'new_tab',
        serverId: target.serverId,
        workspaceId: target.workspaceId,
        cwd: target.cwd,
        id: const Uuid().v4(),
        title: tr(context, 'sessionNavigation.newSession'),
      ),
    );
  }

  Future<void> _openSettings() async {
    await Navigator.of(context)
        .push(MaterialPageRoute<void>(builder: (_) => const SettingsScreen()));
  }

  Future<void> _showCommands({bool files = false}) async {
    if (_commandCenter) return;
    setState(() => _commandCenter = true);
    _shortcuts.reset();
    try {
      await showPaseoCommandCenter(
        context,
        store: store,
        files: files,
        onCommand: _execute,
      );
    } finally {
      if (mounted) setState(() => _commandCenter = false);
    }
  }

  Future<void> _execute(String action, JsonMap args) async {
    final target = store.selected;
    if (target != null &&
        await PaseoCommands.instance.invoke(target.key, action, args)) {
      return;
    }
    if (!mounted) return;
    switch (action) {
      case 'agent.new':
      case 'workspace.project.pick':
      case 'workspace.new':
        await _newSession(newWorkspace: true);
      case 'worktree.new':
        await _newSession(newWorkspace: true, worktree: true);
      case 'workspace.tab.menu.open':
      case 'workspace.tab.target.agent':
        await _newSession();
      case 'workspace.tab.target.changes':
        _openWorkspacePanel('changes_tree', explorer: true);
      case 'workspace.tab.target.files':
        _openWorkspacePanel('files', explorer: true);
      case 'workspace.tab.close.current':
        // Original `handleCloseTabById`: an agent tab closes through the agent policy.
        if (target != null) await _closeWorkspaceTab(target);
      // Original `handleWorkspaceCurrentTabCloseAction`: bulk closes around the active tab.
      case 'workspace.tab.close-left':
      case 'workspace.tab.close-right':
      case 'workspace.tab.close-others':
        final pane = store.focusedPane;
        final active = pane.tabKeys.indexOf(pane.selectedKey ?? '');
        if (active < 0) return;
        await _closeWorkspaceTabs([
          for (final (index, key) in pane.tabKeys.indexed)
            if (switch (action) {
              'workspace.tab.close-left' => index < active,
              'workspace.tab.close-right' => index > active,
              _ => index != active,
            })
              ?store.targetFor(key),
        ], switch (action) {
          'workspace.tab.close-left' => 'workspace.tabs.confirmations.closeTabsLeftTitle',
          'workspace.tab.close-right' => 'workspace.tabs.confirmations.closeTabsRightTitle',
          _ => 'workspace.tabs.confirmations.closeOtherTabsTitle',
        });
      case 'workspace.tab.navigate.index':
        final keys = store.focusedPane.tabKeys;
        final index = args.integer('index', 1) - 1;
        if (index >= 0 && index < keys.length) store.selectTab(keys[index]);
      case 'workspace.tab.navigate.relative':
        final pane = store.focusedPane;
        if (pane.tabKeys.isNotEmpty) {
          final index = pane.tabKeys.indexOf(pane.selectedKey ?? '');
          store.selectTab(
            pane.tabKeys[(index + args.integer('delta', 1)) %
                pane.tabKeys.length],
          );
        }
      case 'workspace.pane.split.right':
        _splitWithLauncher(Axis.horizontal);
      case 'workspace.pane.split.down':
        _splitWithLauncher(Axis.vertical);
      case 'workspace.pane.close':
        await _closePane(store.focusedPaneId);
      case 'workspace.pane.focus.left':
      case 'workspace.pane.focus.right':
      case 'workspace.pane.focus.up':
      case 'workspace.pane.focus.down':
        store.focusAdjacentPane(_direction(action));
      case 'workspace.pane.move-tab.left':
      case 'workspace.pane.move-tab.right':
      case 'workspace.pane.move-tab.up':
      case 'workspace.pane.move-tab.down':
        store.moveFocusedTab(_direction(action));
      case 'sidebar.toggle.left':
        store.showSidebar(
          _compact
              ? store.compactDestination != CompactDestination.sidebar
              : !store.sidebarVisible,
        );
      case 'sidebar.toggle.right':
        if (_compact) {
          _toggleCompactExplorer();
        } else if (store.explorerTabs.isEmpty) {
          _openDefaultExplorer();
        } else {
          store.showExplorer(!store.explorerVisible);
        }
      case 'sidebar.toggle.both':
        final show = !store.sidebarVisible || !store.explorerVisible;
        store.showSidebar(show);
        if (!_compact) store.showExplorer(show);
      case 'view.toggle.focus':
        store.maximizePane(
          store.maximizedPaneId == null ? store.focusedPaneId : null,
        );
      case 'settings.toggle':
        await _openSettings();
      case 'command-center.toggle':
        await _showCommands();
      case 'command-center.files':
        await _showCommands(files: true);
      case 'shortcuts.dialog.toggle':
        await showShortcutHelp(context, store);
      case 'theme.cycle':
        final current = store.preferences.values.text('theme', 'auto');
        store.preferences['theme'] = switch (current) {
          'auto' => 'light',
          'light' => 'dark',
          _ => 'auto',
        };
        await store.savePreferences();
      case 'workspace.archive':
        if (target == null || !await store.canCloseWindow() || !mounted) return;
        final host = store.hostFor(target.serverId);
        final workspace = host?.workspaces
            .where((item) => item.text('id') == target.workspaceId)
            .firstOrNull;
        if (workspace != null) {
          await archiveNavigationWorkspaces(context, store, target.serverId, [
            workspace,
          ]);
        }
      case 'workspace.pin':
        if (target == null || target.workspaceId.isEmpty) return;
        final pinHost = store.hostFor(target.serverId);
        final pinWorkspace = pinHost?.workspaces
            .where((item) => item.text('id') == target.workspaceId)
            .firstOrNull;
        await navigationRequest(
          store,
          target.serverId,
          'workspace.pin.set.request',
          {
            'workspaceId': target.workspaceId,
            'pinned': pinWorkspace?['pinnedAt'] == null,
          },
          'workspace.pin.set.response',
        );
        await store.refreshHost(target.serverId);
      case 'workspace.navigate.index':
      case 'workspace.navigate.relative':
        _navigateSession(action, args);
      case 'agent.interrupt':
        if (target != null && target.kind == 'agent' && target.id != null) {
          await store.clientFor(target.serverId).request(
            'cancel_agent_request',
            {'agentId': target.id},
            responseType: 'cancel_agent_response',
          );
        }
      default:
        if (action.startsWith('message-input.') &&
            target?.kind != 'agent' &&
            target?.kind != 'draft') {
          return;
        }
        throw StateError('Command is unavailable in this view: $action');
    }
  }

  AxisDirection _direction(String action) => switch (action.split('.').last) {
    'left' => AxisDirection.left,
    'right' => AxisDirection.right,
    'up' => AxisDirection.up,
    _ => AxisDirection.down,
  };

  void _navigateSession(String action, JsonMap args) {
    final sessions = <(HostState, JsonMap)>[];
    for (final host in store.hosts) {
      for (final entry in host.agents) {
        final agent = entry.object('agent').isEmpty
            ? entry
            : entry.object('agent');
        if (agent['archivedAt'] == null &&
            agent['parentAgentId'] == null &&
            agent.text('workspaceId').isNotEmpty) {
          sessions.add((host, agent));
        }
      }
    }
    if (sessions.isEmpty) return;
    final current = store.selected;
    final index = action.endsWith('.index')
        ? args.integer('index', 1) - 1
        : (sessions.indexWhere(
                    (row) =>
                        row.$1.serverId == current?.serverId &&
                        row.$2.text('id') == current?.id,
                  ) +
                  args.integer('delta', 1)) %
              sessions.length;
    if (index >= 0 && index < sessions.length) {
      openNavigationAgent(
        store,
        sessions[index].$1.serverId,
        sessions[index].$2,
      );
    }
  }

  String _title(PanelTarget target) {
    if (target.kind == 'agent') {
      final rows = store.hostFor(target.serverId)?.agents ?? const <JsonMap>[];
      for (final row in rows) {
        final agent = row.object('agent').isEmpty ? row : row.object('agent');
        if (agent.text('id') == target.id && agent.text('title').isNotEmpty) {
          return agent.text('title');
        }
      }
    }
    if (target.title?.isNotEmpty == true) return target.title!;
    return switch (target.kind) {
      'agent' => tr(context, 'sessionNavigation.untitled'),
      'draft' || 'new_tab' => tr(context, 'sessionNavigation.newSession'),
      'files' => tr(context, 'workspace.tabs.actions.files'),
      'changes_tree' => tr(context, 'workspace.tabs.actions.changes'),
      'file' =>
        target.data
            .text('path', target.id ?? 'File')
            .split(RegExp(r'[/\\]'))
            .last,
      'working_diff' || 'commit_diff' => 'Diff',
      'pull_request' => 'Pull request',
      'execution_log' => 'Execution log',
      'setup' => 'Workspace setup',
      'project_settings' => 'Project settings',
      'provider_subagent' => 'Subagent',
      _ => target.kind,
    };
  }

  /// workspace-screen.tsx useSessionHeader: an agent header shows the agent
  /// directory's title, else "Untitled session" — never the tab's cached
  /// title, so an archived agent (absent from the directory) reads untitled.
  String _headerTitle(PanelTarget target) {
    if (target.kind != 'agent') return _title(target);
    final rows = store.hostFor(target.serverId)?.agents ?? const <JsonMap>[];
    for (final row in rows) {
      final agent = row.object('agent').isEmpty ? row : row.object('agent');
      if (agent.text('id') == target.id) {
        final title = agent.text('title').trim();
        if (title.isNotEmpty) return title;
      }
    }
    return tr(context, 'sessionNavigation.untitled');
  }

  IconData _icon(String kind) => switch (kind) {
    'draft' => LucideIcons.squarePen300,
    'agent' || 'provider_subagent' => LucideIcons.messageSquare300,
    'files' => LucideIcons.files300,
    'file' => LucideIcons.file300,
    'changes_tree' || 'working_diff' => LucideIcons.fileDiff300,
    'commit_diff' => LucideIcons.gitCommitHorizontal300,
    'pull_request' => LucideIcons.gitPullRequest300,
    'execution_log' => LucideIcons.scrollText300,
    'setup' => LucideIcons.squareTerminal300,
    'project_settings' => LucideIcons.settings300,
    _ => LucideIcons.plus300,
  };

  Widget _panel(PanelTarget target) => switch (target.kind) {
    'agent' || 'draft' || 'provider_subagent' => AgentPanel(target: target),
    'files' => FilesPanel(target: target),
    'file' => FilePanel(target: target),
    'changes_tree' ||
    'working_diff' ||
    'commit_diff' => ChangesPanel(target: target),
    'pull_request' => PullRequestPanel(target: target),
    'execution_log' => ExecutionLogPanel(target: target),
    'setup' => SetupPanel(target: target),
    'project_settings' => ProjectSettingsPanel(target: target),
    'new_tab' => NewTabPanel(target: target),
    _ => ErrorState(error: StateError('Unrecognized panel: ${target.kind}')),
  };

  Widget _pane({
    required String id,
    required List<String> keys,
    required String? selected,
    required bool visible,
    bool explorer = false,
  }) {
    if (visible && selected != null) _activated.add(selected);
    return DragTarget<String>(
      key: _paneKeys.putIfAbsent(id, () => GlobalKey(debugLabel: 'pane:$id')),
      onWillAcceptWithDetails: (details) =>
          !explorer && store.targetFor(details.data) != null,
      onAcceptWithDetails: (details) => store.moveTab(details.data, id),
      builder: (context, candidates, rejected) => Listener(
        onPointerDown: (_) {
          if (!explorer) store.focusPane(id);
        },
        child: DecoratedBox(
          decoration: BoxDecoration(
            border: Border.all(
              color: candidates.isNotEmpty
                  ? Theme.of(context).colorScheme.primary
                  : Colors.transparent,
            ),
          ),
          child: Column(
            children: [
              // Compact panes have no tab strip, but the compact Explorer keeps its own
              // Files/Changes rail (original ExplorerSidebarDock in an overlay frame).
              if (!_compact || explorer) SizedBox(
                height: 38,
                child: Row(
                  children: [
                    Expanded(
                      child: ReorderableListView.builder(
                        scrollDirection: Axis.horizontal,
                        buildDefaultDragHandles: false,
                        itemCount: keys.length,
                        onReorderItem: (oldIndex, newIndex) {
                          if (!explorer) {
                            store.reorderTab(id, oldIndex, newIndex);
                          }
                        },
                        // Original explorer `WorkspaceDesktopTabsRow` (launchPurpose
                        // "supporting"): the new-tab "+" follows the last tab.
                        footer: explorer
                            ? Center(
                                child: PaseoChromeButton(
                                  tooltip: tr(context, 'workspace.tabs.actions.newTab'),
                                  icon: LucideIcons.plus300,
                                  extent: 28,
                                  iconSize: 14,
                                  radius: 6,
                                  onPressed: (anchor) => _run(
                                    () => _explorerNewTab(paseoAnchorRect(anchor), keys),
                                  ),
                                ),
                              )
                            : null,
                        itemBuilder: (context, index) {
                          final key = keys[index];
                          final target = store.targetFor(key);
                          if (target == null) {
                            return SizedBox.shrink(key: ValueKey(key));
                          }
                          final active = key == selected;
                          // Original compact rail (`closeButtonPolicy` "none" on compact
                          // layouts): sidebar-surface pills of icon + label, the active one
                          // filled, with no per-tab menu or close buttons.
                          final pill = explorer && _compact;
                          final colors = PaseoColors.of(context);
                          final pillForeground = active
                              ? Theme.of(context).colorScheme.onSurface
                              : colors.muted;
                          final chip = Material(
                            color: pill
                                ? (active ? colors.surface2 : Colors.transparent)
                                : active
                                ? Theme.of(context)
                                      .colorScheme
                                      .surfaceContainerHighest
                                : Colors.transparent,
                            shape: pill
                                ? RoundedRectangleBorder(borderRadius: BorderRadius.circular(6))
                                : null,
                            clipBehavior: pill ? Clip.antiAlias : Clip.none,
                            child: InkWell(
                              onTap: () => explorer
                                  ? store.selectExplorerTab(key)
                                  : store.selectTab(key),
                              onSecondaryTapDown: (details) => _tabMenu(
                                target,
                                id,
                                Rect.fromLTWH(details.globalPosition.dx, details.globalPosition.dy, 0, 0),
                                explorer: explorer,
                                fromPointer: true,
                              ),
                              child: Container(
                                constraints: const BoxConstraints(
                                  maxWidth: 240,
                                ),
                                padding: pill
                                    ? const EdgeInsets.symmetric(horizontal: 10)
                                    : const EdgeInsets.only(
                                        left: 10,
                                        right: 2,
                                      ),
                                decoration: pill
                                    ? null
                                    : BoxDecoration(
                                        border: Border(
                                          bottom: BorderSide(
                                            width: 2,
                                            color: active
                                                ? Theme.of(context)
                                                      .colorScheme
                                                      .primary
                                                : Colors.transparent,
                                          ),
                                        ),
                                      ),
                                child: Row(
                                  mainAxisSize: MainAxisSize.min,
                                  children: [
                                    pill
                                        ? Icon(_icon(target.kind), size: 14, color: pillForeground)
                                        : Icon(_icon(target.kind), size: 15),
                                    SizedBox(width: pill ? 6 : 7),
                                    Flexible(
                                      child: Text(
                                        _title(target),
                                        overflow: TextOverflow.ellipsis,
                                        style: pill
                                            ? Theme.of(context).textTheme.bodyMedium!
                                                .copyWith(color: pillForeground)
                                            : Theme.of(context)
                                                .textTheme
                                                .labelMedium,
                                      ),
                                    ),
                                    // Original TabHandleContent: the 8px modified dot is a
                                    // laid-out sibling after the label (4px content gap).
                                    _ModifiedTabDot(tabKey: key, gap: 4),
                                    if (!pill) ...[
                                      Builder(
                                        builder: (anchor) => IconButton(
                                          tooltip: tr(context, 'workspace.tabs.menu.openFor', args: {'label': _title(target)}),
                                          visualDensity: VisualDensity.compact,
                                          iconSize: 14,
                                          onPressed: () => _tabMenu(
                                            target,
                                            id,
                                            paseoAnchorRect(anchor),
                                            explorer: explorer,
                                          ),
                                          icon: const Icon(LucideIcons.ellipsis300),
                                        ),
                                      ),
                                      IconButton(
                                        tooltip: tr(
                                          context,
                                          'common.actions.close',
                                        ),
                                        visualDensity: VisualDensity.compact,
                                        iconSize: 14,
                                        onPressed: () => _run(
                                          () => _closeWorkspaceTab(target),
                                        ),
                                        icon: const Icon(LucideIcons.x300),
                                      ),
                                    ],
                                  ],
                                ),
                              ),
                            ),
                          );
                          return ReorderableDragStartListener(
                            key: ValueKey(key),
                            index: index,
                            enabled: !explorer,
                            child: LongPressDraggable<String>(
                              data: key,
                              feedback: Material(
                                elevation: 6,
                                child: Padding(
                                  padding: const EdgeInsets.all(12),
                                  child: Text(_title(target)),
                                ),
                              ),
                              child: pill
                                  ? Padding(
                                      padding: const EdgeInsets.symmetric(
                                        horizontal: 2,
                                        vertical: 5,
                                      ),
                                      child: chip,
                                    )
                                  : chip,
                            ),
                          );
                        },
                      ),
                    ),
                    if (!explorer)
                      IconButton(
                        tooltip: tr(context, 'sessionNavigation.newSession'),
                        icon: const Icon(LucideIcons.plus300, size: 18),
                        onPressed: () => _run(_newSession),
                      ),
                    // Original WorkspaceExplorerSidebarToggle: compact layouts close the
                    // overlay with the agent list's SidebarCloseButton X.
                    if (explorer && _compact)
                      Padding(
                        padding: const EdgeInsets.only(right: 4),
                        child: PaseoChromeButton(
                          tooltip: tr(context, 'workspace.tabs.explorerSidebar.close'),
                          icon: LucideIcons.x300,
                          extent: 32,
                          iconSize: 16,
                          onPressed: (_) => store.showCompactExplorer(false),
                        ),
                      )
                    else if (explorer)
                      IconButton(
                        tooltip: tr(context, 'common.actions.close'),
                        icon: const Icon(LucideIcons.x300, size: 18),
                        onPressed: () => store.showExplorer(false),
                      ),
                  ],
                ),
              ),
              if (!_compact || explorer) const Divider(),
              Expanded(
                child: keys.isEmpty
                    ? _welcome()
                    : Stack(
                        children: [
                          for (final key in keys)
                            if (_activated.contains(key) &&
                                store.targetFor(key) != null)
                              _RetainedPanel(
                                key: _panelKeys.putIfAbsent(
                                  key,
                                  () => GlobalKey(debugLabel: key),
                                ),
                                active: visible && key == selected && _visible,
                                // The compact shell shows one pane; wide panes take focus
                                // from the focused pane id (pane-context isPaneFocused).
                                focused: !explorer &&
                                    (_compact || store.focusedPaneId == id),
                                appActive: _active,
                                child: _panel(store.targetFor(key)!),
                              ),
                        ],
                      ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// Original desktop tab menu (`buildWorkspaceTabMenuEntries`, surface "desktop"):
  /// copy resume command, agent id or file path, rename, close left/right/others,
  /// reload and close. The conversation's own actions (find, outline, fork, archive…)
  /// join it so the pane needs no second title row, and main-pane tabs add the
  /// native split, pane focus and close-pane actions.
  Future<void> _tabMenu(
    PanelTarget target,
    String paneId,
    Rect anchor, {
    required bool explorer,
    bool fromPointer = false,
  }) async {
    final agent = target.kind == 'agent';
    final id = target.id ?? '';
    final keys = explorer
        ? store.explorerTabKeys
        : store.layout.panes.where((pane) => pane.id == paneId).firstOrNull?.tabKeys ?? const <String>[];
    final index = keys.indexOf(target.key);
    final conversation = ConversationMenus.of(target.key);
    final conversationActions = [
      for (final action in conversation?.actions() ?? const <(String, String)>[])
        if (action.$1 != 'rename') action,
    ];
    final conversationReloads = conversationActions.any((action) => action.$1 == 'refresh');
    final maximized = store.maximizedPaneId == paneId;
    String t(String key) => tr(context, key);
    final action = await showPaseoMenu<String>(
      context,
      title: _tabKindLabel(target),
      anchor: anchor,
      // A context menu grows from the pointer; the ··· button's menu ends at it.
      alignEnd: !fromPointer,
      entries: [
        if (agent) ...[
          PaseoMenuEntry.item(
            value: 'resume',
            label: t('workspace.tabs.menu.copyResumeCommand'),
            icon: LucideIcons.copy300,
          ),
          PaseoMenuEntry.item(
            value: 'agentId',
            label: t('workspace.tabs.menu.copyAgentId'),
            icon: LucideIcons.copy300,
            hint: id.length > 7 ? id.substring(0, 7) : id,
          ),
        ],
        if (target.kind == 'file')
          PaseoMenuEntry.item(
            value: 'path',
            label: t('workspace.tabs.menu.copyFilePath'),
            icon: LucideIcons.copy300,
          ),
        PaseoMenuEntry.item(
          value: 'rename',
          label: t(agent ? 'workspace.tabs.menu.renameAgent' : 'workspace.tabs.menu.rename'),
          icon: LucideIcons.pencil300,
        ),
        if (conversationActions.isNotEmpty) ...[
          const PaseoMenuEntry.separator(),
          for (final (value, label) in conversationActions)
            PaseoMenuEntry.item(value: 'conversation:$value', label: label),
        ],
        const PaseoMenuEntry.separator(),
        if (!explorer) ...[
          PaseoMenuEntry.item(
            value: 'left',
            label: t('workspace.tabs.menu.closeLeft'),
            icon: LucideIcons.arrowLeftToLine300,
            enabled: index > 0,
          ),
          PaseoMenuEntry.item(
            value: 'right',
            label: t('workspace.tabs.menu.closeRight'),
            icon: LucideIcons.arrowRightToLine300,
            enabled: index >= 0 && index < keys.length - 1,
          ),
          PaseoMenuEntry.item(
            value: 'others',
            label: t('workspace.tabs.menu.closeOthers'),
            icon: LucideIcons.copyX300,
            enabled: keys.length > 1,
          ),
          if (!_compact) ...[
            const PaseoMenuEntry.separator(),
            PaseoMenuEntry.item(
              value: 'splitRight',
              label: t('workspace.tabs.actions.splitRight'),
              icon: LucideIcons.columns2300,
            ),
            PaseoMenuEntry.item(
              value: 'splitDown',
              label: t('workspace.tabs.actions.splitDown'),
              icon: LucideIcons.rows2300,
            ),
            PaseoMenuEntry.item(
              value: 'maximize',
              label: t(maximized ? 'workspace.tabs.actions.restorePane' : 'workspace.tabs.actions.maximizePane'),
              icon: maximized ? LucideIcons.minimize2300 : LucideIcons.maximize2300,
            ),
            PaseoMenuEntry.item(
              value: 'closePane',
              label: t('workspace.tabs.actions.closePane'),
              icon: LucideIcons.squareX300,
            ),
          ],
          const PaseoMenuEntry.separator(),
        ],
        if (agent && !conversationReloads)
          PaseoMenuEntry.item(
            value: 'reload',
            label: t('workspace.tabs.menu.reloadAgent'),
            icon: LucideIcons.rotateCw300,
          ),
        PaseoMenuEntry.item(
          value: 'close',
          label: t('workspace.tabs.menu.close'),
          icon: LucideIcons.x300,
        ),
      ],
    );
    if (action == null || !mounted) return;
    if (action.startsWith('conversation:')) {
      conversation?.run(action.substring('conversation:'.length));
      return;
    }
    await _run(() async {
      switch (action) {
        case 'resume':
          await _copyResumeCommand(target);
        case 'agentId':
          await _copy(id, 'workspace.tabs.toasts.agentIdCopiedLabel');
        case 'path':
          await _copy(
            target.data.text('path', target.id ?? ''),
            'workspace.tabs.toasts.filePathCopiedLabel',
          );
        case 'rename':
          await _renameTab(target);
        case 'left':
          await _closeWorkspaceTabs([
            for (final key in keys.take(index)) ?store.targetFor(key),
          ], 'workspace.tabs.confirmations.closeTabsLeftTitle');
        case 'right':
          await _closeWorkspaceTabs([
            for (final key in keys.skip(index + 1)) ?store.targetFor(key),
          ], 'workspace.tabs.confirmations.closeTabsRightTitle');
        case 'others':
          await _closeWorkspaceTabs([
            for (final key in keys)
              if (key != target.key) ?store.targetFor(key),
          ], 'workspace.tabs.confirmations.closeOtherTabsTitle');
        case 'splitRight':
        case 'splitDown':
          final next = store.splitPane(
            paneId,
            action == 'splitRight' ? Axis.horizontal : Axis.vertical,
          );
          store.moveTab(target.key, next);
        case 'maximize':
          store.maximizePane(maximized ? null : paneId);
        case 'closePane':
          await _closePane(paneId);
        case 'reload':
          await _reloadAgent(target);
        case 'close':
          await _closeWorkspaceTab(target);
      }
    });
  }

  Widget _layout(LayoutNode node) {
    if (node is PanelPane) {
      return _pane(
        id: node.id,
        keys: node.tabKeys,
        selected: node.selectedKey,
        visible:
            store.maximizedPaneId == null || store.maximizedPaneId == node.id,
      );
    }
    final split = node as PanelSplit;
    final maximized = store.maximizedPaneId;
    if (maximized != null) {
      final showFirst = split.first.panes.any((pane) => pane.id == maximized);
      final showSecond = split.second.panes.any((pane) => pane.id == maximized);
      return Stack(
        children: [
          Positioned.fill(
            child: Offstage(offstage: !showFirst, child: _layout(split.first)),
          ),
          Positioned.fill(
            child: Offstage(
              offstage: !showSecond,
              child: _layout(split.second),
            ),
          ),
        ],
      );
    }
    return LayoutBuilder(
      builder: (context, constraints) {
        final horizontal = split.axis == Axis.horizontal;
        final extent = horizontal
            ? constraints.maxWidth
            : constraints.maxHeight;
        final available = (extent - 5).clamp(0.0, double.infinity);
        final first = available * split.ratio.clamp(0.15, 0.85);
        return Flex(
          direction: split.axis,
          children: [
            SizedBox(
              width: horizontal ? first : null,
              height: horizontal ? null : first,
              child: _layout(split.first),
            ),
            MouseRegion(
              cursor: horizontal
                  ? SystemMouseCursors.resizeColumn
                  : SystemMouseCursors.resizeRow,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onPanUpdate: (details) {
                  if (available > 0) {
                    store.resizeSplit(
                      split.id,
                      (split.ratio +
                              (horizontal
                                      ? details.delta.dx
                                      : details.delta.dy) /
                                  available)
                          .clamp(0.15, 0.85),
                    );
                  }
                },
                child: SizedBox(
                  width: horizontal ? 5 : null,
                  height: horizontal ? null : 5,
                  child: ColoredBox(color: Theme.of(context).dividerColor),
                ),
              ),
            ),
            Expanded(child: _layout(split.second)),
          ],
        );
      },
    );
  }

  Widget _sidebarNavigation() {
    const defaults = ['new-workspace', 'history', 'search', 'schedules'];
    final preferences = jsonList(store.preferences['sidebarNavItems']).map(jsonMap);
    final order = <String>[];
    final placed = <String>{};
    for (final row in preferences) {
      final id = row.text('key');
      if (defaults.contains(id) && placed.add(id) && row['visible'] != false) {
        order.add(id);
      }
    }
    order.addAll(defaults.where((id) => !placed.contains(id)));
    Widget row(String id) => switch (id) {
      'new-workspace' => SidebarHeaderRow(
        icon: LucideIcons.plus300,
        label: tr(context, 'sessionNavigation.newSession'),
        onTap: () => _run(() => _newSession(newWorkspace: true)),
      ),
      'history' => SidebarHeaderRow(
        icon: LucideIcons.history300,
        label: tr(context, 'sidebar.sections.sessions'),
        // The original sidebar row routes to /sessions, which opens on "All hosts".
        onTap: () => _run(() async {
          final serverId =
              store.selected?.serverId ?? store.hosts.firstOrNull?.serverId;
          if (serverId == null) {
            await showHostDialog(context);
            return;
          }
          await showSessionHistory(
            context,
            store: store,
            serverId: serverId,
            allHosts: true,
          );
        }),
      ),
      'search' => SidebarHeaderRow(
        icon: LucideIcons.search300,
        label: tr(context, 'sidebar.sections.search'),
        onTap: () => _run(_showCommands),
      ),
      _ => SidebarHeaderRow(
        icon: LucideIcons.calendarClock300,
        label: tr(context, 'sidebar.sections.schedules'),
        onTap: () => _run(() => ShellPages.open(context, const SchedulesScreen())),
      ),
    };
    if (order.isEmpty && !_compact) return const SizedBox.shrink();
    return DecoratedBox(
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: Theme.of(context).dividerColor)),
      ),
      child: Padding(
        padding: const EdgeInsets.only(top: 8, bottom: 6),
        child: Column(
          spacing: 2,
          children: [
            if (_compact)
              Row(
                children: [
                  Expanded(
                    child: order.isEmpty ? const SizedBox.shrink() : row(order.first),
                  ),
                  // Original SidebarCloseButton: the header-island X (32px round, 16px
                  // muted glyph), 8px from the edge.
                  Padding(
                    padding: const EdgeInsets.only(right: 8),
                    child: PaseoChromeButton(
                      tooltip: tr(context, 'sidebar.actions.closeSidebar'),
                      icon: LucideIcons.x300,
                      extent: 32,
                      iconSize: 16,
                      onPressed: (_) => store.showSidebar(false),
                    ),
                  ),
                ],
              )
            else
              row(order.first),
            for (final id in order.skip(1)) row(id),
          ],
        ),
      ),
    );
  }

  /// Original `FooterIconButton`: a 44/28px square hitbox around an md glyph, muted
  /// (foreground while hovered), with a round interaction highlight only while pressed.
  Widget _footerButton({
    required String tooltip,
    required IconData icon,
    required void Function(BuildContext anchor) onPressed,
    double sizeAdjustment = 0,
  }) => PaseoChromeButton(
    tooltip: tooltip,
    icon: icon,
    extent: _compact ? 44 : 28,
    iconSize: densityIcon(context, 'md') + sizeAdjustment,
    hoverColor: Theme.of(context).colorScheme.onSurface,
    pressOnly: true,
    onPressed: onPressed,
  );

  Future<void> _openHost(String id) async {
    if (id == 'add') {
      await showHostDialog(context);
      return;
    }
    // Original HostPicker: choosing a host or its gear opens that host's settings.
    final serverId = id.startsWith('settings:') ? id.substring(9) : id;
    await Navigator.of(context).push(MaterialPageRoute<void>(
      builder: (_) => SettingsScreen(
        initialSection: 'host/host',
        initialServerId: serverId,
      ),
    ));
  }

  /// Original `SidebarHelpMenu`: help entries, report links, and the app version.
  Future<void> _helpMenu(BuildContext anchor) async {
    final box = anchor.findRenderObject()! as RenderBox;
    final overlay = Overlay.of(anchor).context.findRenderObject()! as RenderBox;
    final origin = box.localToGlobal(Offset.zero, ancestor: overlay);
    final colors = PaseoColors.of(context);
    final version = (await PackageInfo.fromPlatform()).version;
    if (!mounted) return;
    final muted = Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.muted);
    PopupMenuItem<String> item(String value, IconData icon, String label) => PopupMenuItem(
      value: value,
      height: 36,
      child: Row(
        spacing: 10,
        children: [Icon(icon, size: 16, color: colors.muted), Text(label)],
      ),
    );
    PopupMenuItem<String> label(String text) =>
        PopupMenuItem(enabled: false, height: 28, child: Text(text, style: muted));
    final action = await showMenu<String>(
      context: context,
      constraints: const BoxConstraints(minWidth: 280, maxWidth: 280),
      position: RelativeRect.fromLTRB(
        origin.dx + box.size.width - 280,
        origin.dy - 8,
        overlay.size.width - origin.dx - box.size.width,
        overlay.size.height - origin.dy + 8,
      ),
      items: [
        label(tr(context, 'sidebar.help.sectionHelp')),
        if (DesktopHost.supported)
          item('shortcuts', LucideIcons.keyboard300, tr(context, 'sidebar.help.shortcuts')),
        item('diagnostics', LucideIcons.activity300, tr(context, 'sidebar.help.diagnostics')),
        const PopupMenuDivider(),
        label(tr(context, 'sidebar.help.reportIssue')),
        item('discord', LucideIcons.messageCircle300, tr(context, 'sidebar.help.discord')),
        item('github', LucideIcons.bug300, tr(context, 'sidebar.help.github')),
        const PopupMenuDivider(),
        PopupMenuItem(
          enabled: false,
          height: 28,
          child: Row(
            children: [
              Expanded(child: Text(tr(context, 'sidebar.help.appName'), style: muted)),
              Text(version, style: muted),
            ],
          ),
        ),
      ],
    );
    if (!mounted) return;
    switch (action) {
      case 'shortcuts':
        await showShortcutHelp(context, store);
      case 'diagnostics':
        await Navigator.of(context).push(MaterialPageRoute<void>(
          builder: (_) => const SettingsScreen(initialSection: 'diagnostics'),
        ));
      case 'discord':
        await launchUrl(Uri.parse('https://discord.gg/jz8T2uahpH'),
            mode: LaunchMode.externalApplication);
      case 'github':
        await launchUrl(Uri.parse('https://github.com/getpaseo/paseo/issues/new'),
            mode: LaunchMode.externalApplication);
    }
  }

  Widget _sidebar() => ColoredBox(
    key: _sidebarKey,
    color: PaseoColors.of(context).sidebar,
    child: Column(
      children: [
        _sidebarNavigation(),
        const Expanded(child: SessionSidebar()),
        DecoratedBox(
          decoration: BoxDecoration(
            border: Border(top: BorderSide(color: Theme.of(context).dividerColor)),
          ),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
            child: Row(
              children: [
                _footerButton(
                  tooltip: tr(context, 'sidebar.actions.addProject'),
                  icon: LucideIcons.folderPlus300,
                  onPressed: (_) => _run(() async {
                    final host = await _chooseHost();
                    if (host == null || !mounted) return;
                    store.open(PanelTarget(
                      kind: 'new_tab',
                      serverId: host.serverId,
                      workspaceId: '',
                      cwd: '',
                      title: 'Add project',
                      data: const {'addProject': true},
                    ));
                  }),
                ),
                _footerButton(
                  tooltip: tr(context, 'sidebar.footer.usage'),
                  icon: LucideIcons.circleGauge300,
                  onPressed: (_) => _run(() async {
                    final host = await _chooseHost();
                    if (host == null || !mounted) return;
                    await Navigator.of(context).push(MaterialPageRoute<void>(
                      builder: (_) => Scaffold(
                        appBar: AppBar(title: Text(tr(context, 'sidebar.footer.usage'))),
                        body: ListView(
                          padding: const EdgeInsets.all(16),
                          children: [
                            UsageSettings(client: host.client, serverId: host.serverId),
                          ],
                        ),
                      ),
                    ));
                  }),
                ),
                _footerButton(
                  tooltip: tr(context, 'sidebar.actions.hosts'),
                  icon: LucideIcons.server300,
                  sizeAdjustment: -1,
                  onPressed: (anchor) => _run(() async {
                    final id = await showHostPicker(anchor, store);
                    if (id != null && mounted) await _openHost(id);
                  }),
                ),
                const Spacer(),
                _footerButton(
                  tooltip: tr(context, 'sidebar.help.trigger'),
                  icon: LucideIcons.circleHelp300,
                  onPressed: (anchor) => _run(() => _helpMenu(anchor)),
                ),
                _footerButton(
                  tooltip: tr(context, 'sidebar.actions.settings'),
                  icon: LucideIcons.settings300,
                  onPressed: (_) => _run(_openSettings),
                ),
              ],
            ),
          ),
        ),
      ],
    ),
  );

  JsonMap? _agentFor(PanelTarget target) {
    for (final row in store.hostFor(target.serverId)?.agents ?? const <JsonMap>[]) {
      final agent = row.object('agent').isEmpty ? row : row.object('agent');
      if (agent.text('id') == target.id) return agent;
    }
    return null;
  }

  JsonMap? _workspaceFor(PanelTarget? target) => target == null
      ? null
      : store.hostFor(target.serverId)?.workspaces
            .where((row) => row.text('id') == target.workspaceId)
            .firstOrNull;

  /// Original workspace tabs: the tabs of every pane that belong to [target]'s workspace
  /// (the original keeps one tab layout per workspace), in pane order. Files and Changes
  /// are Explorer views, never workspace tabs (original `openExplorerSidebarView`). The
  /// original `reconcileWorkspaceTabs` (`addMissingEntityTabs`) puts every non-archived
  /// root agent of the workspace into the layout in agent-id order as soon as the
  /// workspace's agents are known, so opening one later only focuses its existing tab and
  /// never moves it, while any other tab opened later — an explicitly opened (pinned)
  /// archived agent included — is appended after them (`insertTabIntoPane`). Here those
  /// agents are opened lazily, so their tabs (opened or not yet) take the agent-id order
  /// across the slots the opened ones hold, the unopened ones continuing that block right
  /// after its last slot (at the front when none is open yet); other tabs keep their
  /// pane position.
  List<PanelTarget> _workspaceTabs(PanelTarget? target) {
    if (target == null) return const [];
    final tabs = [
      for (final pane in store.layout.panes)
        for (final key in pane.tabKeys)
          if (store.targetFor(key) case final tab?
              when _inWorkspace(tab, target) &&
                  tab.kind != 'files' &&
                  tab.kind != 'changes_tree')
            tab,
    ];
    if (target.workspaceId.isEmpty) return tabs;
    final roots = _workspaceRootAgents(target);
    final rootIds = {for (final agent in roots) agent.text('id')};
    bool reconciled(PanelTarget tab) => tab.kind == 'agent' && rootIds.contains(tab.id);
    final open = {
      for (final tab in tabs)
        if (tab.kind == 'agent') tab.id,
    };
    final agents = [
      for (final tab in tabs)
        if (reconciled(tab)) tab,
      for (final agent in roots)
        if (!open.contains(agent.text('id')))
          navigationAgentTarget(target.serverId, agent),
    ]..sort((a, b) => (a.id ?? '').compareTo(b.id ?? ''));
    final slots = tabs.where(reconciled).length;
    final lastSlot = tabs.lastIndexWhere(reconciled);
    var slot = 0;
    return [
      if (lastSlot < 0) ...agents,
      for (final (index, tab) in tabs.indexed) ...[
        reconciled(tab) ? agents[slot++] : tab,
        if (index == lastSlot) ...agents.skip(slots),
      ],
    ];
  }

  /// Original `deriveWorkspaceAgentVisibility.autoOpenAgentIds`: the workspace's
  /// non-archived agents that are workspace roots (`isWorkspaceRootAgent`: no parent, or a
  /// parent in another workspace).
  List<JsonMap> _workspaceRootAgents(PanelTarget workspace) {
    final agents = [
      for (final row in store.hostFor(workspace.serverId)?.agents ?? const <JsonMap>[])
        row.object('agent').isEmpty ? row : row.object('agent'),
    ];
    final byId = {for (final agent in agents) agent.text('id'): agent};
    bool root(JsonMap agent) {
      final parent = _parentAgentId(agent);
      if (parent.isEmpty) return true;
      final parentWorkspace = byId[parent]?.text('workspaceId') ?? '';
      return parentWorkspace.isNotEmpty && parentWorkspace != workspace.workspaceId;
    }

    return [
      for (final agent in agents)
        if (agent.text('workspaceId') == workspace.workspaceId &&
            agent['archivedAt'] == null &&
            agent.text('id').isNotEmpty &&
            root(agent))
          agent,
    ];
  }

  String _parentAgentId(JsonMap agent) {
    final parent = agent.text('parentAgentId');
    return parent.isNotEmpty
        ? parent
        : agent.object('labels').text('paseo.parent-agent-id').trim();
  }

  /// Original `getFallbackTabOptionLabel`: the tab kind's own name, used as the title of a
  /// switcher row's menu ("Agent", "New agent", "Changes", a file's name, …).
  String _tabKindLabel(PanelTarget tab) => switch (tab.kind) {
    'new_tab' => tr(context, 'workspace.tabs.actions.newTab'),
    'draft' => tr(context, 'workspace.tabs.fallback.newAgent'),
    'setup' => tr(context, 'workspace.tabs.fallback.setup'),
    'file' => tab.data
        .text('path', tab.id ?? '')
        .split('/')
        .lastWhere((part) => part.isNotEmpty, orElse: () => tab.data.text('path', tab.id ?? '')),
    'working_diff' || 'changes_tree' => tr(context, 'panels.diff.changesLabel'),
    'files' => tr(context, 'panels.files.label'),
    'pull_request' => tr(context, 'panels.pullRequest.label'),
    'commit_diff' => (tab.id ?? '').length > 7 ? tab.id!.substring(0, 7) : tab.id ?? '',
    _ => tr(context, 'workspace.tabs.fallback.agent'),
  };

  Future<void> _copy(String text, String labelKey) async {
    await Clipboard.setData(ClipboardData(text: text));
    if (mounted) showCopiedToast(context, tr(context, labelKey));
  }

  // Original `provider-command-templates.ts`: provider-native resume commands.
  static const _resumeTemplates = {
    'codex': 'codex resume {sessionId}',
    'claude': 'claude --resume {sessionId}',
    'hermes': 'hermes --resume {sessionId}',
    'pi': 'pi --session {sessionId}',
    'omp': 'omp --session {sessionId}',
    'opencode': 'opencode --session {sessionId}',
  };

  Future<void> _copyResumeCommand(PanelTarget target) async {
    final agent = _agentFor(target);
    final sessionId = agent == null
        ? ''
        : agent.object('runtimeInfo').text(
            'sessionId',
            agent.object('persistence').text('sessionId'),
          );
    if (sessionId.isEmpty) {
      showError(context, tr(context, 'workspace.tabs.toasts.resumeIdUnavailable'));
      return;
    }
    final template = _resumeTemplates[agent!.text('provider')];
    if (template == null) {
      showError(context, tr(context, 'workspace.tabs.toasts.resumeCommandUnavailable'));
      return;
    }
    await _copy(
      template.replaceAll('{sessionId}', sessionId),
      'workspace.tabs.toasts.resumeCommandCopiedLabel',
    );
  }

  /// Original `handleReloadAgent`: refresh the provider session, then reload its timeline.
  Future<void> _reloadAgent(PanelTarget target) async {
    final dismiss = showToast(
      context,
      tr(context, 'workspace.tabs.toasts.reloadingAgent'),
      duration: null,
    );
    try {
      await store.clientFor(target.serverId).request(
        'refresh_agent_request',
        {'agentId': target.id},
        responseType: 'status',
      );
      await PaseoCommands.instance.invoke(target.key, 'conversation.sync', const {});
      if (mounted) {
        showToast(
          context,
          tr(context, 'workspace.tabs.toasts.reloadedAgent'),
          variant: ToastVariant.success,
        );
      }
    } catch (error) {
      dismiss();
      if (mounted) showError(context, error);
    }
  }

  Future<void> _renameTab(PanelTarget target) async {
    final title = await promptText(
      context,
      title: tr(context, target.kind == 'agent'
          ? 'workspace.tabs.menu.renameAgent'
          : 'workspace.tabs.menu.rename'),
      initialValue: _title(target),
    );
    if (title == null || title.trim().isEmpty) return;
    if (target.kind == 'agent') {
      await store.clientFor(target.serverId).request(
        'update_agent_request',
        {'agentId': target.id, 'name': title.trim()},
        responseType: 'update_agent_response',
      );
    } else {
      store.open(target.copyWith(title: title.trim()));
    }
  }

  /// Original `handleCloseAgentTab` (`resolveCloseAgentTabPolicy`): closing a root agent's
  /// tab archives the agent — confirming first while it runs unless [confirmRunning] is
  /// off (bulk closes confirm once for all) — while a subagent's tab only leaves this
  /// client's layout. Any other tab simply closes.
  Future<void> _closeWorkspaceTab(PanelTarget tab, {bool confirmRunning = true}) async {
    final open = store.targetFor(tab.key) != null;
    if (tab.kind != 'agent') {
      if (open) await store.closeTab(tab.key);
      return;
    }
    final agent = _agentFor(tab);
    if (agent != null && _parentAgentId(agent).isNotEmpty) {
      final host = store.hostFor(tab.serverId);
      if (host != null) {
        await navigationRequest(store, tab.serverId, 'update_agent_request', {
          'agentId': tab.id,
          'labels': {'paseo.open-agent-tab.${await host.client.getClientId()}': 'false'},
        }, 'update_agent_response');
      }
      if (open) await store.closeTab(tab.key);
      return;
    }
    if (confirmRunning &&
        agent?.text('status') == 'running' &&
        !await confirmAction(
          context,
          title: tr(context, 'workspace.tabs.confirmations.archiveRunningAgentTitle'),
          message: tr(context, 'workspace.tabs.confirmations.archiveRunningAgentMessage'),
          confirmLabel: tr(context, 'workspace.tabs.confirmations.archive'),
          destructive: true,
        )) {
      return;
    }
    if (open && !await store.closeTab(tab.key)) return;
    // Original: `void archiveAgent(...).catch(() => {})` — the tab is already gone and
    // an archive failure (e.g. timeout) settles in the background without a toast.
    unawaited(
      navigationRequest(store, tab.serverId, 'archive_agent_request', {
        'agentId': tab.id,
      }, 'agent_archived')
          .then((_) => store.refreshHost(tab.serverId))
          .catchError((Object _) {}),
    );
  }

  /// Original `handleClosePane`: a pane that cannot be dismissed (the last one) is left
  /// alone before anything closes; otherwise its tabs close through the bulk policy and,
  /// once confirmed, the pane goes.
  Future<void> _closePane(String paneId) async {
    final pane = store.layout.panes.where((p) => p.id == paneId).firstOrNull;
    if (pane == null || store.layout.panes.length <= 1) return;
    final closed = await _closeWorkspaceTabs([
      for (final key in pane.tabKeys) ?store.targetFor(key),
    ], 'workspace.tabs.confirmations.closePaneTitle');
    if (closed) await store.closePane(paneId);
  }

  /// Original `handleCreateEmptySplit` (`splitPaneEmptyInLayout`): the new pane beside
  /// the focused one opens on a new-tab launcher for the focused tab's workspace.
  void _splitWithLauncher(Axis axis) {
    final source = store.selected;
    store.splitPane(store.focusedPaneId, axis);
    if (source == null) return;
    store.open(
      PanelTarget(
        kind: 'new_tab',
        serverId: source.serverId,
        workspaceId: source.workspaceId,
        cwd: source.cwd,
        id: const Uuid().v4(),
        title: tr(context, 'sessionNavigation.newSession'),
      ),
    );
  }

  /// Original `handleBulkCloseTabs`: one destructive confirmation titled [titleKey] that
  /// says what closing [tabs] does (`buildBulkCloseConfirmationMessage`: archive N agents,
  /// close N tabs), then each tab closes as [_closeWorkspaceTab] does. Nothing to close
  /// counts as closed; a declined confirmation does not.
  Future<bool> _closeWorkspaceTabs(List<PanelTarget> tabs, String titleKey) async {
    if (tabs.isEmpty) return true;
    var agents = 0;
    for (final tab in tabs) {
      final agent = tab.kind == 'agent' ? _agentFor(tab) : null;
      if (tab.kind == 'agent' && (agent == null || _parentAgentId(agent).isEmpty)) agents++;
    }
    final others = tabs.length - agents;
    final message = agents > 0 && others > 0
        ? tr(context, 'workspace.tabs.confirmations.bulk.agentsAndTabs',
            args: {'agents': agents, 'tabs': others})
        : others > 0
        ? tr(context, 'workspace.tabs.confirmations.bulk.tabs', args: {'tabs': others})
        : tr(context, 'workspace.tabs.confirmations.bulk.agents', args: {'agents': agents});
    if (!await confirmAction(
      context,
      title: tr(context, titleKey),
      message: message,
      confirmLabel: tr(context, 'workspace.tabs.confirmations.close'),
      destructive: true,
    )) {
      return false;
    }
    for (final tab in tabs) {
      await _closeWorkspaceTab(tab, confirmRunning: false);
    }
    return true;
  }

  /// Original mobile tab menu (`MobileTabTrailingAccessory` over
  /// `buildWorkspaceTabMenuEntries`, surface "mobile"): a sheet titled with the tab kind
  /// that stacks over the switcher. Fork, usage, subagent detach and archive live on the
  /// conversation itself (turn footer, context meter, subagent pills, closing the tab).
  Future<void> _workspaceTabMenu(
    PanelTarget tab,
    PanelTarget workspace,
    Rect anchor,
  ) async {
    final tabs = _workspaceTabs(workspace);
    final index = tabs.indexWhere((item) => item.key == tab.key);
    final agent = tab.kind == 'agent';
    final id = tab.id ?? '';
    String t(String key) => tr(context, key);
    final action = await showPaseoMenu<String>(
      context,
      title: _tabKindLabel(tab),
      anchor: anchor,
      alignEnd: true,
      entries: [
        if (agent) ...[
          PaseoMenuEntry.item(
            value: 'resume',
            label: t('workspace.tabs.menu.copyResumeCommand'),
            icon: LucideIcons.copy300,
          ),
          PaseoMenuEntry.item(
            value: 'agentId',
            label: t('workspace.tabs.menu.copyAgentId'),
            icon: LucideIcons.copy300,
            hint: id.length > 7 ? id.substring(0, 7) : id,
          ),
        ],
        if (tab.kind == 'file')
          PaseoMenuEntry.item(
            value: 'path',
            label: t('workspace.tabs.menu.copyFilePath'),
            icon: LucideIcons.copy300,
          ),
        if (agent) ...[
          PaseoMenuEntry.item(
            value: 'rename',
            label: t('workspace.tabs.menu.rename'),
            icon: LucideIcons.pencil300,
          ),
          const PaseoMenuEntry.separator(),
        ],
        PaseoMenuEntry.item(
          value: 'above',
          label: t('workspace.tabs.menu.closeAbove'),
          icon: LucideIcons.arrowLeftToLine300,
          enabled: index > 0,
        ),
        PaseoMenuEntry.item(
          value: 'below',
          label: t('workspace.tabs.menu.closeBelow'),
          icon: LucideIcons.arrowRightToLine300,
          enabled: index >= 0 && index < tabs.length - 1,
        ),
        PaseoMenuEntry.item(
          value: 'others',
          label: t('workspace.tabs.menu.closeOthers'),
          icon: LucideIcons.copyX300,
          enabled: tabs.length > 1,
        ),
        if (agent)
          PaseoMenuEntry.item(
            value: 'reload',
            label: t('workspace.tabs.menu.reloadAgent'),
            icon: LucideIcons.rotateCw300,
          ),
        PaseoMenuEntry.item(
          value: 'close',
          label: t('workspace.tabs.menu.close'),
          icon: LucideIcons.x300,
        ),
      ],
    );
    if (action == null || !mounted) return;
    await _run(() async {
      switch (action) {
        case 'resume':
          await _copyResumeCommand(tab);
        case 'agentId':
          await _copy(id, 'workspace.tabs.toasts.agentIdCopiedLabel');
        case 'path':
          await _copy(
            tab.data.text('path', tab.id ?? ''),
            'workspace.tabs.toasts.filePathCopiedLabel',
          );
        case 'rename':
          await _renameTab(tab);
        case 'above':
          await _closeWorkspaceTabs(
            tabs.take(index).toList(),
            'workspace.tabs.confirmations.closeTabsLeftTitle',
          );
        case 'below':
          await _closeWorkspaceTabs(
            tabs.skip(index + 1).toList(),
            'workspace.tabs.confirmations.closeTabsRightTitle',
          );
        case 'others':
          await _closeWorkspaceTabs(
            tabs.where((item) => item.key != tab.key).toList(),
            'workspace.tabs.confirmations.closeOtherTabsTitle',
          );
        case 'reload':
          await _reloadAgent(tab);
        case 'close':
          await _closeWorkspaceTab(tab);
      }
    });
  }

  /// agent-presentation buildAgentDescriptor + workspace-tab-presentation
  /// WorkspaceTabIcon: the provider glyph carrying the agent's status (running
  /// ring, failed/attention dot, needs-input alert).
  Widget _tabIcon(PanelTarget target, Color color) {
    if (target.kind == 'agent') {
      final agent = _agentFor(target);
      final provider = agent?.text('provider') ?? '';
      if (provider.isNotEmpty) {
        final status = agent?['status'] as Object?;
        final turnActive = agent?['activeTurn'] != null ||
            (agent?['turn'] is Map && (agent!['turn'] as Map)['phase'] == 'open');
        final pending = agent?['pendingPermissions'];
        return StatusIcon(
          bucket: status is String
              ? deriveStatusBucket(
                  status: turnActive ? 'running' : status,
                  pendingPermissionCount: pending is List ? pending.length : 0,
                  requiresAttention: agent?['requiresAttention'] == true,
                  attentionReason: agent?['attentionReason'] is String
                      ? agent!['attentionReason'] as String
                      : null,
                )
              : null,
          backdrop: PaseoColors.of(context).surface0,
          child: ProviderIcon(provider: provider, size: 14, color: color),
        );
      }
    }
    return Icon(_icon(target.kind), size: 14, color: color);
  }

  /// Original `MobileWorkspaceTabSwitcher`: the workspace's tabs in a combobox sheet
  /// (60%/90% snap, surface0 with 16px top radius, base medium title at 24px) whose
  /// footer holds the New agent button.
  Future<void> _showTabs(PanelTarget workspace) async {
    FocusManager.instance.primaryFocus?.unfocus();
    final colors = PaseoColors.of(context);
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (sheet) => DraggableScrollableSheet(
        expand: false,
        initialChildSize: .6,
        minChildSize: .3,
        maxChildSize: .9,
        snap: true,
        snapSizes: const [.6],
        builder: (sheet, controller) => ListenableBuilder(
          listenable: store,
          builder: (sheet, _) {
            final theme = Theme.of(sheet);
            final tabs = _workspaceTabs(workspace);
            final active = store.selected?.key;
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Center(
                  child: Container(
                    margin: const EdgeInsets.symmetric(vertical: 10),
                    width: 36,
                    height: 4,
                    decoration: BoxDecoration(
                      color: colors.surface2,
                      borderRadius: BorderRadius.circular(2),
                    ),
                  ),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(24, 0, 24, 8),
                  child: Text(
                    tr(sheet, 'workspace.tabs.switcher.title'),
                    style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500),
                  ),
                ),
                Expanded(
                  child: ListView(
                    controller: controller,
                    padding: const EdgeInsets.fromLTRB(8, 4, 8, 32),
                    children: [
                      for (final tab in tabs)
                        _TabOption(
                          key: ValueKey(tab.key),
                          tabKey: tab.key,
                          label: _title(tab),
                          selected: tab.key == active,
                          icon: (color) => _tabIcon(tab, color),
                          onTap: () {
                            if (store.targetFor(tab.key) == null) {
                              store.open(tab);
                            } else {
                              store.selectTab(tab.key);
                            }
                            Navigator.pop(sheet);
                          },
                          menuLabel: _tabKindLabel(tab),
                          onMenu: (anchor) => _run(
                            () => _workspaceTabMenu(tab, workspace, anchor),
                          ),
                        ),
                    ],
                  ),
                ),
                DecoratedBox(
                  decoration: BoxDecoration(
                    border: Border(top: BorderSide(color: theme.dividerColor)),
                  ),
                  child: Padding(
                    padding: EdgeInsets.fromLTRB(
                      24, 12, 24, 12 + MediaQuery.paddingOf(sheet).bottom,
                    ),
                    child: Row(
                      spacing: 12,
                      children: [
                        Expanded(
                          child: PaseoButton(
                            icon: LucideIcons.squarePen300,
                            label: tr(sheet, 'workspace.header.actions.newAgent'),
                            expand: true,
                            onPressed: () {
                              Navigator.pop(sheet);
                              unawaited(_run(_newSession));
                            },
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ],
            );
          },
        ),
      ),
    );
  }

  /// Original compact `WorkspaceHeaderMenuMobile` (wide: `WorkspaceHeaderMenuDesktop`):
  /// the shared `WorkspaceHeaderWorkspaceActionItems` — copy path, copy branch name,
  /// import session, then Show setup after a separator. The original's browser and
  /// terminal-profile rows launch surfaces this app does not have.
  Future<void> _headerMenu(PanelTarget? target, Rect anchor) async {
    final workspace = _workspaceFor(target);
    final directory = workspace?.text('workspaceDirectory', target?.cwd ?? '') ??
        target?.cwd ?? '';
    final branch = workspace?.object('gitRuntime').text('currentBranch').trim() ?? '';
    final hasBranch = branch.isNotEmpty && branch != 'HEAD';
    // Original `shouldShowWorkspaceSetup`: hosts without setup observation always offer it.
    final showSetup = workspace != null &&
        (workspace['setupStatus'] != null ||
            store.hostFor(target!.serverId)?.client.supports('workspaceSetupObservation') !=
                true);
    String t(String key) => tr(context, key);
    PaseoMenuEntry<String> item(String value, String label, IconData icon) =>
        PaseoMenuEntry.item(value: value, label: label, icon: icon);
    final action = await showPaseoMenu<String>(
      context,
      title: t('workspace.header.actions.workspaceActions'),
      anchor: anchor,
      entries: [
        // Original compact menu leads with the Scripts row (`useWorkspaceScriptsMenuPages`)
        // while the workspace has scripts; here it opens the setup panel's script list.
        if (_compact && workspace != null && workspace.array('scripts').isNotEmpty)
          item('scripts', t('workspace.scripts.title'), LucideIcons.play300),
        PaseoMenuEntry.item(
          value: 'copyPath',
          label: t('workspace.header.actions.copyPath'),
          icon: LucideIcons.copy300,
          enabled: directory.isNotEmpty,
        ),
        if (hasBranch)
          item('copyBranch', t('workspace.header.actions.copyBranchName'), LucideIcons.copy300),
        PaseoMenuEntry.item(
          value: 'import',
          label: t('workspace.header.actions.importSession'),
          icon: LucideIcons.import300,
          enabled: target != null,
        ),
        if (showSetup) ...[
          const PaseoMenuEntry.separator(),
          item('setup', t('workspace.header.actions.showSetup'), LucideIcons.settings300),
        ],
      ],
    );
    if (action == null || !mounted) return;
    switch (action) {
      case 'copyPath':
        await _copy(directory, 'workspace.header.toasts.workspacePathCopiedLabel');
      case 'copyBranch':
        await _copy(branch, 'workspace.header.toasts.branchNameCopiedLabel');
      case 'import':
        await showImportSessions(
          context,
          store: store,
          serverId: target!.serverId,
          cwd: directory,
        );
      case 'setup' || 'scripts':
        store.open(PanelTarget(
          kind: 'setup',
          serverId: target!.serverId,
          workspaceId: target.workspaceId,
          cwd: directory,
          data: {'repoRoot': workspace!.text('projectRootPath', directory)},
        ));
    }
  }

  /// Original desktop `WorkspaceScriptsButton` (header, `hideLabels`): every
  /// workspace script with its state, started or stopped from the menu, its
  /// read-only log and its service URL; the full list stays in setup.
  Future<void> _scriptsMenu(PanelTarget target, JsonMap workspace, Rect anchor) async {
    String t(String key, [Map<String, Object?> args = const {}]) => tr(context, key, args: args);
    String url(JsonMap script) =>
        script.text('proxyUrl', script.text('publicProxyUrl', script.text('localProxyUrl')));
    final scripts = workspace.array('scripts').map(jsonMap).toList();
    final action = await showPaseoMenu<String>(
      context,
      title: t('workspace.scripts.title'),
      anchor: anchor,
      alignEnd: true,
      entries: [
        for (final script in scripts) ...[
          PaseoMenuEntry.item(
            value: 'toggle:${script.text('scriptName')}',
            label: script.text('scriptName'),
            icon: script.text('lifecycle') == 'running' ? LucideIcons.square300 : LucideIcons.play300,
            hint: [
              if (script.text('lifecycle').isNotEmpty) script.text('lifecycle'),
              if (script['exitCode'] != null)
                t('workspace.scripts.states.exitCode', {'code': '${script['exitCode']}'}),
            ].join(' · '),
          ),
          if (script.text('terminalId').isNotEmpty)
            PaseoMenuEntry.item(
              value: 'log:${script.text('scriptName')}',
              label: t('workspace.scripts.actions.view'),
              icon: LucideIcons.squareTerminal300,
              muted: true,
            ),
          if (url(script).isNotEmpty)
            PaseoMenuEntry.item(
              value: 'open:${script.text('scriptName')}',
              label: t('workspace.scripts.actions.openService'),
              icon: LucideIcons.externalLink300,
              muted: true,
            ),
        ],
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'setup',
          label: t('workspace.header.actions.showSetup'),
          icon: LucideIcons.settings300,
        ),
      ],
    );
    if (action == null || !mounted) return;
    if (action == 'setup') {
      store.open(PanelTarget(
        kind: 'setup',
        serverId: target.serverId,
        workspaceId: target.workspaceId,
        cwd: target.cwd,
        data: {'repoRoot': workspace.text('projectRootPath', target.cwd)},
      ));
      return;
    }
    final separator = action.indexOf(':');
    final verb = action.substring(0, separator);
    final name = action.substring(separator + 1);
    final script = scripts.firstWhere((entry) => entry.text('scriptName') == name);
    switch (verb) {
      case 'log':
        store.open(PanelTarget(
          kind: 'execution_log',
          serverId: target.serverId,
          workspaceId: target.workspaceId,
          cwd: target.cwd,
          id: script.text('terminalId'),
          title: name,
        ));
      case 'open':
        final uri = Uri.parse(url(script));
        if (!const ['http', 'https'].contains(uri.scheme) ||
            !await launchUrl(uri, mode: LaunchMode.externalApplication)) {
          throw StateError('Could not open service URL in the system browser.');
        }
      default:
        final command = script.text('lifecycle') == 'running' ? 'stop' : 'start';
        requireWorkspaceSuccess(await store.clientFor(target.serverId).request(
          'workspace.script.$command.request',
          {'workspaceId': target.workspaceId, 'scriptName': name},
          responseType: 'workspace.script.$command.response',
        ));
    }
  }

  /// Original `WorkspaceHeaderProjectRow`: project name, `·`, then the HostBadge (12px
  /// server glyph, 3px gap, sm label on a 16px line, both in the host's identity colour).
  /// The badge yields its label before the project gives up a pixel; local hosts hide it.
  Widget _headerSubtitle(String project, HostState host) {
    final small = Theme.of(context).textTheme.bodySmall!;
    final colors = PaseoColors.of(context);
    final showBadge = host.config.localCredentialPath == null;
    final badgeColor = labelColor(context, host.config.color ?? 'none');
    final separatorStyle = small.copyWith(color: colors.extraMuted);
    final showProject = project.isNotEmpty;
    return LayoutBuilder(
      builder: (context, constraints) {
        final separator = TextPainter(
          text: TextSpan(text: '·', style: separatorStyle),
          textDirection: Directionality.of(context),
          textScaler: MediaQuery.textScalerOf(context),
        )..layout();
        final reserved = showBadge ? (showProject ? 12 + separator.width : 0) + 12 : 0.0;
        return Row(
          children: [
            if (showProject)
              ConstrainedBox(
                constraints: BoxConstraints(
                  maxWidth: (constraints.maxWidth - reserved).clamp(0, double.infinity),
                ),
                child: Text(
                  project,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: small.copyWith(color: colors.muted),
                ),
              ),
            if (showBadge) ...[
              if (showProject) ...[
                const SizedBox(width: 6),
                Text('·', style: separatorStyle),
                const SizedBox(width: 6),
              ],
              Icon(LucideIcons.server300, size: 12, color: badgeColor),
              const SizedBox(width: 3),
              Flexible(
                child: Text(
                  hostDisplayLabel(host),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: small.copyWith(
                    color: badgeColor,
                    height: 16 / (small.fontSize ?? 12),
                  ),
                ),
              ),
            ],
          ],
        );
      },
    );
  }

  /// Original `CompactHeader` (12px insets, 8px top padding, 56px row, 8px gaps between the
  /// menu island, the left-aligned title group and the action island) and its wide-layout
  /// 36px counterpart.
  Widget _header() {
    // The compact Explorer covers the header; it never becomes the titled tab.
    final target = store.selected;
    final host = target == null ? null : store.hostFor(target.serverId);
    final workspace = _workspaceFor(target);
    final project = workspace?.text('projectDisplayName') ?? '';
    final directory = (target?.cwd ?? '').split('/').where((p) => p.isNotEmpty).lastOrNull ?? '';
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final subtitle = _compact && host != null;
    final tabs = _workspaceTabs(target);
    // Original `new-workspace-screen.tsx` `NewWorkspaceLayout`: the new-session screen is
    // not a workspace tab, so it carries the plain `MenuHeader` (the sidebar toggle, with
    // Import session on the right on compact layouts) instead of the workspace header.
    final newWorkspace = target != null && target.kind == 'new_tab' && target.workspaceId.isEmpty;
    return Padding(
      padding: EdgeInsets.fromLTRB(12, _compact ? 8 : 0, 12, 0),
      child: SizedBox(
        height: _compact ? 56 : 36,
        child: Row(
          spacing: 8,
          children: [
            _IslandButton(
              compact: _compact,
              tooltip: tr(context, 'shell.menu.toggleSidebar'),
              onPressed: (_) => store.showSidebar(
                _compact ? true : !store.sidebarVisible,
              ),
              // Original MobileMenuIcon: 16/16/8 x 1.5 muted lines in a 16x12 box.
              child: _compact
                  ? SizedBox(
                      width: 16,
                      height: 12,
                      child: Column(
                        mainAxisAlignment: MainAxisAlignment.spaceBetween,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          for (final width in const [16.0, 16.0, 8.0])
                            Container(
                              width: width,
                              height: 1.5,
                              decoration: BoxDecoration(
                                color: colors.muted,
                                borderRadius: BorderRadius.circular(1),
                              ),
                            ),
                        ],
                      ),
                    )
                  : Icon(LucideIcons.panelLeft300, size: 16, color: colors.muted),
            ),
            if (newWorkspace) ...[
              const Spacer(),
              if (_compact)
                PaseoButton(
                  label: tr(context, 'importSession.title'),
                  icon: LucideIcons.import300,
                  variant: PaseoButtonVariant.ghost,
                  size: PaseoControlSize.sm,
                  onPressed: () => _run(
                    () => showImportSessions(context, store: store, serverId: target.serverId),
                  ),
                ),
            ] else ...[
            Expanded(
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Text(
                    target == null ? 'Paseo' : _headerTitle(target),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    // Original title: base medium over a subtitle, lg medium alone.
                    style: (subtitle
                            ? theme.textTheme.bodyMedium
                            : theme.textTheme.titleMedium)!
                        .copyWith(fontWeight: FontWeight.w500),
                  ),
                  if (subtitle) _headerSubtitle(project.isEmpty ? directory : project, host),
                ],
              ),
            ),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (_compact && target != null)
                  _IslandButton(
                    compact: true,
                    tooltip: tr(
                      context,
                      'workspace.tabs.switcher.trigger',
                      args: {'count': tabs.length},
                    ),
                    onPressed: (_) => _run(() => _showTabs(target)),
                    // Original tabCountBadge: 16px, 1px muted stroke, 2px radius, 10/12 digits.
                    child: Container(
                      constraints: const BoxConstraints(minWidth: 16),
                      height: 16,
                      padding: const EdgeInsets.symmetric(horizontal: 2),
                      decoration: BoxDecoration(
                        border: Border.all(color: colors.muted),
                        borderRadius: BorderRadius.circular(2),
                      ),
                      child: Center(
                        widthFactor: 1,
                        child: Text(
                          '${tabs.length}',
                          textAlign: TextAlign.center,
                          textHeightBehavior: const TextHeightBehavior(
                            leadingDistribution: TextLeadingDistribution.even,
                          ),
                          style: TextStyle(
                            fontSize: 10,
                            height: 1.2,
                            fontWeight: FontWeight.w500,
                            color: colors.muted,
                            fontFeatures: const [FontFeature.tabularFigures()],
                          ),
                        ),
                      ),
                    ),
                  ),
                _IslandButton(
                  compact: _compact,
                  tooltip: tr(context, 'workspace.header.actions.workspaceActions'),
                  onPressed: (anchor) =>
                      _run(() => _headerMenu(target, paseoAnchorRect(anchor))),
                  child: Icon(LucideIcons.ellipsis300, size: 16, color: colors.extraMuted),
                ),
                if (!_compact && target != null && (_workspaceFor(target)?.array('scripts').isNotEmpty ?? false))
                  _IslandButton(
                    compact: false,
                    tooltip: tr(context, 'workspace.scripts.title'),
                    onPressed: (anchor) => _run(
                      () => _scriptsMenu(target, _workspaceFor(target)!, paseoAnchorRect(anchor)),
                    ),
                    child: Icon(LucideIcons.play300, size: 16, color: colors.extraMuted),
                  ),
                // Original workspace header `WorkspaceOpenInEditorButton` then
                // `WorkspaceActions` (git split button), right before the explorer
                // toggle; compact layouts keep git actions in Changes.
                if (!_compact && target != null && target.cwd.isNotEmpty) ...[
                  WorkspaceOpenInButton(
                    serverId: target.serverId,
                    cwd: target.cwd,
                    activeFilePath: target.kind == 'file' ? target.data.text('path', target.id ?? '') : null,
                  ),
                  const SizedBox(width: 4),
                  WorkspaceGitActionsButton(
                    serverId: target.serverId,
                    workspaceId: target.workspaceId,
                    cwd: target.cwd,
                  ),
                ],
                _IslandButton(
                  compact: _compact,
                  tooltip: tr(context, 'workspace.tabs.explorerSidebar.toggle'),
                  onPressed: (_) => _run(() async {
                    if (_compact) {
                      _toggleCompactExplorer();
                    } else if (!store.explorerVisible && store.explorerTabs.isEmpty) {
                      if (target?.workspaceId.isNotEmpty != true) return;
                      _openDefaultExplorer();
                    } else {
                      store.showExplorer(!store.explorerVisible);
                    }
                  }),
                  child: Icon(
                    LucideIcons.panelRight300,
                    size: 16,
                    color: _compact ? colors.muted : colors.extraMuted,
                  ),
                ),
              ],
            ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _explorer(bool visible) {
    var keys = store.explorerTabKeys;
    var selected = store.selectedExplorerKey;
    final workspace = store.selected;
    // The compact Explorer belongs to the workspace on screen (original per-workspace
    // explorer pane), so its rail lists only that workspace's views.
    if (_compact && workspace != null) {
      keys = [
        for (final key in keys)
          if (_inWorkspace(store.targetFor(key), workspace)) key,
      ];
      if (!keys.contains(selected)) selected = keys.firstOrNull;
    }
    return SizedBox(
      key: _explorerKey,
      child: _pane(
        id: 'explorer',
        keys: keys,
        selected: selected,
        visible: visible,
        explorer: true,
      ),
    );
  }

  static const _explorerCurve = Cubic(0.25, 0.1, 0.25, 1);

  /// Follows the compact destination with the original 220ms panel motion unless a
  /// gesture currently owns the position.
  void _syncExplorerMotion() {
    final open = _compact && store.compactDestination == CompactDestination.explorer;
    if (_explorerDragging || open == _explorerOpen) return;
    _explorerOpen = open;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || _explorerOpen != open || _explorerDragging) return;
      if (_compact) {
        unawaited(_explorerMotion.animateTo(open ? 1 : 0, curve: _explorerCurve));
      } else {
        _explorerMotion.value = 0;
      }
    });
  }

  void _settleExplorerDrag({required bool open}) {
    _explorerDragging = false;
    _explorerOpen = open;
    if (!open) store.showCompactExplorer(false);
    unawaited(_explorerMotion.animateTo(open ? 1 : 0, curve: _explorerCurve));
  }

  /// Original `useOpenFileExplorerGesture`: a leftward horizontal pan on the conversation
  /// drags the Explorer in, committing past a third of the width or at 500px/s.
  Widget _explorerOpenGesture(Widget child) => GestureDetector(
    onHorizontalDragStart: (_) => _drag = 0,
    onHorizontalDragUpdate: (details) {
      final width = MediaQuery.sizeOf(context).width;
      _drag += details.primaryDelta ?? 0;
      if (!_explorerDragging) {
        final workspace = store.selected;
        if (_drag >= 0 ||
            store.compactDestination != CompactDestination.agent ||
            workspace == null ||
            workspace.workspaceId.isEmpty) {
          return;
        }
        _explorerDragging = true;
        _toggleCompactExplorer();
      }
      _explorerMotion.value = (-_drag / width).clamp(0.0, 1.0);
    },
    onHorizontalDragEnd: (details) {
      if (!_explorerDragging) return;
      final width = MediaQuery.sizeOf(context).width;
      _settleExplorerDrag(
        open: -_drag > width / 3 || (details.primaryVelocity ?? 0) < -500,
      );
    },
    onHorizontalDragCancel: () {
      if (_explorerDragging) _settleExplorerDrag(open: false);
    },
    child: child,
  );

  /// Original compact `ExplorerFrame` + `MobilePanelOverlay`: the Explorer dock (its
  /// Files/Changes rail and panels) slides in from the right over the whole workspace
  /// screen, above a 50% black backdrop, padded by the safe area plus the 8px mobile header
  /// inset; a rightward pan closes it (`useCloseFileExplorerGesture`).
  Widget _compactExplorerOverlay() {
    final open = store.compactDestination == CompactDestination.explorer;
    final insets = MediaQuery.paddingOf(context);
    final panel = GestureDetector(
      onHorizontalDragStart: (_) => _drag = 0,
      onHorizontalDragUpdate: (details) {
        final width = MediaQuery.sizeOf(context).width;
        _drag += details.primaryDelta ?? 0;
        if (!_explorerDragging) {
          if (_drag <= 0 || !open) return;
          _explorerDragging = true;
        }
        _explorerMotion.value = (1 - _drag / width).clamp(0.0, 1.0);
      },
      onHorizontalDragEnd: (details) {
        if (!_explorerDragging) return;
        final width = MediaQuery.sizeOf(context).width;
        _settleExplorerDrag(
          open: !(_drag > width / 3 || (details.primaryVelocity ?? 0) > 500),
        );
      },
      onHorizontalDragCancel: () {
        if (_explorerDragging) _settleExplorerDrag(open: true);
      },
      child: ColoredBox(
        color: PaseoColors.of(context).sidebar,
        child: Padding(
          padding: EdgeInsets.fromLTRB(
            insets.left,
            insets.top + 8,
            insets.right,
            insets.bottom,
          ),
          child: MediaQuery.removePadding(
            context: context,
            removeTop: true,
            removeBottom: true,
            removeLeft: true,
            removeRight: true,
            child: _explorer(open),
          ),
        ),
      ),
    );
    return AnimatedBuilder(
      animation: _explorerMotion,
      child: panel,
      builder: (context, child) {
        final position = _explorerMotion.value;
        return Offstage(
          offstage: position == 0 && !open && !_explorerDragging,
          child: IgnorePointer(
            ignoring: !open,
            child: Stack(
              children: [
                Positioned.fill(
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => store.showCompactExplorer(false),
                    child: ColoredBox(
                      color: Colors.black.withValues(alpha: .5 * position),
                    ),
                  ),
                ),
                Positioned.fill(
                  child: FractionalTranslation(
                    translation: Offset(1 - position, 0),
                    child: child,
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  Widget _welcome() => EmptyState(
    title: 'Paseo Sessions',
    message: tr(context, 'sessionNavigation.empty'),
    action: FilledButton.icon(
      onPressed: () => _run(_newSession),
      icon: const Icon(LucideIcons.plus300),
      label: Text(tr(context, 'sessionNavigation.newSession')),
    ),
  );

  @override
  void showPage(Widget page) => setState(() {
    _page = page;
    _pageSerial = store.navigationSerial;
    _pageNavigator = GlobalKey<NavigatorState>();
  });

  /// The desktop page beside the sidebar; opening any session or tab ends it.
  Widget? get _shownPage {
    if (_compact || _page == null) return null;
    if (store.navigationSerial != _pageSerial) _page = null;
    return _page;
  }

  Widget _pageHost(Widget page) => EmbeddedShellPage(
    child: Navigator(
      key: _pageNavigator,
      onGenerateInitialRoutes: (_, _) => [
        PageRouteBuilder<void>(pageBuilder: (_, _, _) => const SizedBox.shrink()),
        PageRouteBuilder<void>(
          pageBuilder: (_, _, _) => page,
          transitionDuration: Duration.zero,
          reverseTransitionDuration: Duration.zero,
        ),
      ],
      observers: [_pageObserver],
    ),
  );

  Widget _content() {
    // The original routes host-less clients to the standalone /welcome screen.
    if (store.hosts.isEmpty) {
      return WelcomeScreen(
        store: store,
        onOpenSettings: () => _run(_openSettings),
      );
    }
    final pane = store.focusedPane;
    if (_compact) {
      return Listener(
        onPointerDown: (event) {
          _edgeStart =
              (event.localPosition.dx < 24 ||
                  store.compactDestination == CompactDestination.sidebar)
              ? event.localPosition
              : null;
        },
        onPointerCancel: (_) => _edgeStart = null,
        onPointerUp: (event) {
          final start = _edgeStart;
          _edgeStart = null;
          if (start == null || _explorerDragging) return;
          final delta = event.localPosition - start;
          if (delta.dx.abs() < 80 || delta.dx.abs() < delta.dy.abs() * 2) {
            return;
          }
          if (delta.dx > 0 && store.compactDestination == CompactDestination.agent) {
            store.showSidebar(true);
          } else if (delta.dx < 0 &&
              store.compactDestination == CompactDestination.sidebar) {
            store.showSidebar(false);
          }
        },
        child: IndexedStack(
          // The Explorer is an overlay above this stack; the conversation stays beneath it.
          index: store.compactDestination == CompactDestination.sidebar ? 0 : 1,
          children: [
            _sidebar(),
            _explorerOpenGesture(
              Stack(
                children: [
                  for (final candidate in store.layout.panes)
                    Positioned.fill(
                      child: Offstage(
                        offstage: candidate.id != pane.id,
                        child: _pane(
                          id: candidate.id,
                          keys: candidate.tabKeys,
                          selected: candidate.selectedKey,
                          // Still painted beneath the sliding Explorer overlay.
                          visible:
                              candidate.id == pane.id &&
                              store.compactDestination !=
                                  CompactDestination.sidebar,
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ],
        ),
      );
    }
    return Row(
      children: [
        SizedBox(
          width: store.sidebarVisible ? store.sidebarWidth : 0,
          child: Offstage(
            offstage: !store.sidebarVisible,
            child: ColoredBox(
              color: PaseoColors.of(context).sidebar,
              child: _sidebar(),
            ),
          ),
        ),
        if (store.sidebarVisible)
          _resizeHandle(
            (delta) => store.setSidebarWidth(store.sidebarWidth + delta),
          ),
        Expanded(
          child: switch (_shownPage) {
            final page? => _pageHost(page),
            null => _layout(store.layout),
          },
        ),
        if (store.explorerVisible && _shownPage == null)
          _resizeHandle(
            (delta) => store.setExplorerWidth(
              (store.explorerWidth - delta).clamp(260, 720),
            ),
          ),
        SizedBox(
          width: store.explorerVisible && _shownPage == null ? store.explorerWidth : 0,
          child: Offstage(
            offstage: !store.explorerVisible || _shownPage != null,
            child: _explorer(store.explorerVisible),
          ),
        ),
      ],
    );
  }

  Widget _resizeHandle(void Function(double) resize) => MouseRegion(
    cursor: SystemMouseCursors.resizeColumn,
    child: GestureDetector(
      behavior: HitTestBehavior.opaque,
      onHorizontalDragUpdate: (details) => resize(details.delta.dx),
      child: SizedBox(
        width: 5,
        child: ColoredBox(color: Theme.of(context).dividerColor),
      ),
    ),
  );

  @override
  Widget build(BuildContext context) {
    PaseoScope.of(context);
    _shortcuts.updateOverrides(jsonMap(store.preferences['keyboardShortcuts']));
    final live = store.tabs.map((target) => target.key).toSet();
    _panelKeys.removeWhere((key, _) => !live.contains(key));
    _activated.removeWhere((key) => !live.contains(key));
    final paneIds = {
      for (final pane in store.layout.panes) pane.id,
      'explorer',
    };
    _paneKeys.removeWhere((id, _) => !paneIds.contains(id));
    return LayoutBuilder(
      builder: (context, constraints) {
        _compact = constraints.maxWidth < 600;
        _syncExplorerMotion();
        final current = ModalRoute.of(context)?.isCurrent ?? true;
        final focused =
            !current ||
                (_compact &&
                    store.compactDestination == CompactDestination.sidebar)
            ? null
            : _compact &&
                  store.compactDestination == CompactDestination.explorer
            ? store.selectedExplorer
            : store.selected;
        widget.activity?.updateView(focused, visible: _active);
        final error = widget.startupError ?? store.storageError;
        return PopScope(
          canPop: !_compact,
          onPopInvokedWithResult: (didPop, result) {
            if (!didPop) unawaited(_run(_back));
          },
          child: Focus(
            autofocus: true,
            onKeyEvent: (_, event) {
              // Original application menu "New Window" (CmdOrCtrl+Shift+N);
              // the original sidebar footer has no new-window button.
              final keyboard = HardwareKeyboard.instance;
              if (DesktopHost.supported &&
                  event is KeyDownEvent &&
                  event.logicalKey == LogicalKeyboardKey.keyN &&
                  keyboard.isShiftPressed &&
                  !keyboard.isAltPressed &&
                  (defaultTargetPlatform == TargetPlatform.macOS
                      ? keyboard.isMetaPressed && !keyboard.isControlPressed
                      : keyboard.isControlPressed && !keyboard.isMetaPressed)) {
                unawaited(_run(() async {
                  await DesktopHost.instance?.openNew();
                }));
                return KeyEventResult.handled;
              }
              if (!(ModalRoute.of(context)?.isCurrent ?? true)) {
                return KeyEventResult.ignored;
              }
              final focus = FocusManager.instance.primaryFocus;
              final editable =
                  focus?.context
                      ?.findAncestorStateOfType<EditableTextState>() !=
                  null;
              final scope = focus?.debugLabel == 'message-input'
                  ? 'message-input'
                  : editable
                  ? 'editable'
                  : 'workspace';
              return _shortcuts.handle(
                event,
                focusScope: scope,
                commandCenter: _commandCenter,
              );
            },
            child: Scaffold(
              backgroundColor: _compact &&
                      store.compactDestination == CompactDestination.sidebar
                  ? PaseoColors.of(context).sidebar
                  : null,
              body: Stack(
                children: [
                  Positioned.fill(
                    child: SafeArea(
                      top: true,
                      child: Column(
                        children: [
                          if ((!_compact ||
                                  store.compactDestination !=
                                      CompactDestination.sidebar) &&
                              !(_compact && store.hosts.isEmpty) &&
                              _shownPage == null)
                            _header(),
                          if (error != null)
                            MaterialBanner(
                              content: Text(error.toString()),
                              actions: [
                                TextButton(
                                  onPressed: () => _run(_openSettings),
                                  child: Text(tr(context, 'settings.title')),
                                ),
                              ],
                            ),
                          Expanded(child: _content()),
                        ],
                      ),
                    ),
                  ),
                  if (_compact) Positioned.fill(child: _compactExplorerOverlay()),
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}

class _RetainedPanel extends StatelessWidget {
  const _RetainedPanel({
    super.key,
    required this.active,
    required this.focused,
    required this.appActive,
    required this.child,
  });
  final bool active, focused, appActive;
  final Widget child;
  @override
  Widget build(BuildContext context) => Positioned.fill(
    child: Offstage(
      offstage: !active,
      child: TickerMode(
        enabled: active,
        child: ExcludeFocus(
          excluding: !active,
          child: IgnorePointer(
            ignoring: !active,
            child: PanelActivity(
              visible: active,
              focused: active && focused,
              appActive: appActive,
              child: child,
            ),
          ),
        ),
      ),
    ),
  );
}

/// Original header-island icon button (`iconButtonChromeStyle` large + round): a 32px
/// circle (26px on wide layouts) filled with `interactionHighlight` while hovered or
/// pressed. On compact layouts the touch target grows 8px above and below, as the
/// original `hitSlop` does; tooltips are desktop-only.
class _IslandButton extends StatefulWidget {
  const _IslandButton({
    required this.compact,
    required this.tooltip,
    required this.onPressed,
    required this.child,
  });
  final bool compact;
  final String tooltip;
  final void Function(BuildContext anchor) onPressed;
  final Widget child;
  @override
  State<_IslandButton> createState() => _IslandButtonState();
}

class _IslandButtonState extends State<_IslandButton> {
  final _frame = GlobalKey();
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final extent = widget.compact ? 32.0 : 26.0;
    Widget button = Semantics(
      button: true,
      label: widget.tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => _pressed = true),
          onTapCancel: () => setState(() => _pressed = false),
          onTap: () {
            setState(() => _pressed = false);
            widget.onPressed(_frame.currentContext ?? context);
          },
          child: Padding(
            padding: EdgeInsets.symmetric(vertical: widget.compact ? 8 : 0),
            child: Container(
              key: _frame,
              width: extent,
              height: extent,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                shape: BoxShape.circle,
                color: _hovered || _pressed ? paseoInteractionHighlight(context) : null,
              ),
              child: widget.child,
            ),
          ),
        ),
      ),
    );
    if (!widget.compact) {
      button = Tooltip(message: widget.tooltip, child: button);
    }
    return button;
  }
}

/// Original `WorkspaceTabOptionRow`: a 36px row (4px inset, 4px bottom gap) whose pressable
/// body (8px padding, 8px gap) fills surface1 while pressed or hovered, a 14px tab icon in a
/// 16px slot (foreground when selected), a base-size label, a 16px muted check on the
/// current tab and the 28px `MobileTabTrailingAccessory` ellipsis.
class _TabOption extends StatefulWidget {
  const _TabOption({
    super.key,
    required this.tabKey,
    required this.label,
    required this.menuLabel,
    required this.selected,
    required this.icon,
    required this.onTap,
    required this.onMenu,
  });
  final String tabKey;
  final String label;

  /// Original `presentationLabel`: the tab kind's name, naming the row's menu.
  final String menuLabel;
  final bool selected;
  final Widget Function(Color color) icon;
  final VoidCallback onTap;
  final Future<void> Function(Rect anchor) onMenu;
  @override
  State<_TabOption> createState() => _TabOptionState();
}

class _TabOptionState extends State<_TabOption> {
  final _trigger = GlobalKey();
  bool _active = false, _menuPressed = false;

  /// The trigger keeps its pressed fill while its menu is open over the switcher.
  bool _menuOpen = false;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: modifiedFileTabs,
    builder: (context, _) => _option(context),
  );

  Widget _option(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final foreground = theme.colorScheme.onSurface;
    return Padding(
      padding: const EdgeInsets.fromLTRB(4, 0, 4, 4),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: 36),
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: Row(
            spacing: 8,
            children: [
              Expanded(
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => _active = true),
                  onExit: (_) => setState(() => _active = false),
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTapDown: (_) => setState(() => _active = true),
                    onTapCancel: () => setState(() => _active = false),
                    onTap: () {
                      setState(() => _active = false);
                      widget.onTap();
                    },
                    child: Container(
                      color: _active ? colors.surface1 : null,
                      padding: const EdgeInsets.all(8),
                      child: Row(
                        spacing: 8,
                        children: [
                          SizedBox(
                            width: 16,
                            child: Center(
                              child: widget.icon(widget.selected ? foreground : colors.muted),
                            ),
                          ),
                          Expanded(
                            child: Text(
                              widget.label,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.bodyMedium!.copyWith(color: foreground),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ),
              // Original WorkspaceTabOptionRow optionModifiedDot, between the body and
              // the selected check.
              if (modifiedFileTabs.contains(widget.tabKey)) const _ModifiedTabDot(),
              if (widget.selected)
                SizedBox(
                  width: 16,
                  child: Icon(LucideIcons.check300, size: 16, color: colors.muted),
                ),
              Semantics(
                button: true,
                label: tr(context, 'workspace.tabs.menu.openFor', args: {'label': widget.menuLabel}),
                child: GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onTapDown: (_) => setState(() => _menuPressed = true),
                  onTapCancel: () => setState(() => _menuPressed = false),
                  onTap: () async {
                    setState(() {
                      _menuPressed = false;
                      _menuOpen = true;
                    });
                    final box = _trigger.currentContext;
                    if (box != null) await widget.onMenu(paseoAnchorRect(box));
                    if (mounted) setState(() => _menuOpen = false);
                  },
                  child: Container(
                    key: _trigger,
                    width: 28,
                    height: 28,
                    alignment: Alignment.center,
                    decoration: BoxDecoration(
                      color: _menuPressed || _menuOpen ? colors.surface2 : null,
                      borderRadius: BorderRadius.circular(6),
                    ),
                    child: Icon(LucideIcons.ellipsis300, size: 14, color: colors.muted),
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Original `tabModifiedDot` / `optionModifiedDot`: an 8px foregroundMuted circle marking a
/// tab whose file has unsaved changes. With [tabKey] it follows that tab's state, laid out
/// [gap] after the preceding sibling, and takes no space while the tab is clean.
class _ModifiedTabDot extends StatelessWidget {
  const _ModifiedTabDot({this.tabKey, this.gap = 0});
  final String? tabKey;
  final double gap;

  Widget _dot(BuildContext context) => Semantics(
    label: tr(context, 'workspace.tabs.modified'),
    child: Container(
      width: 8,
      height: 8,
      margin: EdgeInsets.only(left: gap),
      decoration: BoxDecoration(color: PaseoColors.of(context).muted, shape: BoxShape.circle),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final key = tabKey;
    if (key == null) return _dot(context);
    return ListenableBuilder(
      listenable: modifiedFileTabs,
      builder: (context, _) =>
          modifiedFileTabs.contains(key) ? _dot(context) : const SizedBox.shrink(),
    );
  }
}
