import 'dart:convert';

import 'package:file_selector/file_selector.dart';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'host_connections_dialog.dart';
import 'workspace_labels_dialog.dart';
import 'host_dialog.dart';
import 'navigation_actions.dart';
import 'session_history_dialog.dart';
import 'sidebar_display_menu.dart';

class SessionSidebar extends StatefulWidget {
  const SessionSidebar({super.key});
  @override
  State<SessionSidebar> createState() => _SessionSidebarState();
}

class _SessionSidebarState extends State<SessionSidebar> {
  final _search = TextEditingController();
  bool _busy = false;
  String _query = '';
  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  JsonMap _prefs(AppStore store) => jsonMap(store.preferences['navigation']);
  Set<String> _set(JsonMap prefs, String key) =>
      prefs.array(key).map((item) => item.toString()).toSet();
  Future<void> _save(AppStore store, JsonMap prefs) async {
    store.preferences['navigation'] = prefs;
    if (mounted) setState(() {});
    await store.savePreferences();
  }

  Future<void> _run(Future<void> Function() action) async {
    if (_busy) return;
    setState(() => _busy = true);
    try {
      await action();
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _toggle(AppStore store, String key) async {
    final prefs = _prefs(store);
    final collapsed = _set(prefs, 'collapsed');
    if (!collapsed.remove(key)) collapsed.add(key);
    try {
      await _save(store, {...prefs, 'collapsed': collapsed.toList()});
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  int _time(JsonMap row, String field) =>
      DateTime.tryParse(row.text(field))?.millisecondsSinceEpoch ?? 0;
  String _projectKey(HostState host, JsonMap project) => sidebarProjectKey(host, project);
  String _title(JsonMap agent) => agent.text('title').isEmpty
      ? tr(context, 'sessionNavigation.untitled')
      : agent.text('title');

  void _newSession(
    AppStore store,
    HostState host, {
    JsonMap? project,
    bool addProject = false,
  }) {
    store.open(
      PanelTarget(
        kind: 'new_tab',
        serverId: host.serverId,
        workspaceId: '',
        cwd: project?.text('projectRootPath') ?? '',
        title: addProject ? 'Add project' : 'New session',
        data: {'project': ?project, if (addProject) 'addProject': true},
      ),
    );
  }

  Future<void> _history(
    AppStore store,
    HostState host, {
    JsonMap? project,
    bool importing = false,
  }) async {
    var projectKey = project?.text('projectKey') ?? '';
    if (project != null && projectKey.isEmpty) {
      for (final workspace in host.workspaces) {
        if (workspace.text('projectId') == project.text('projectId')) {
          projectKey = workspace.object('project').text('projectKey');
          if (projectKey.isNotEmpty) break;
        }
      }
    }
    final cwd = project?.text('projectRootPath') ?? '';
    if (importing) {
      await showImportSessions(
        context,
        store: store,
        serverId: host.serverId,
        cwd: cwd,
      );
    } else {
      await showSessionHistory(
        context,
        store: store,
        serverId: host.serverId,
        cwd: cwd,
        projectKey: projectKey,
      );
    }
  }

  Future<void> _move(
    AppStore store,
    String field,
    List<String> keys,
    String key,
    int delta,
  ) async {
    final index = keys.indexOf(key);
    final destination = index + delta;
    if (index < 0 || destination < 0 || destination >= keys.length) return;
    keys.removeAt(index);
    keys.insert(destination, key);
    final prefs = _prefs(store);
    final unrelated = prefs
        .array(field)
        .map((item) => item.toString())
        .where((item) => !keys.contains(item));
    await _save(store, {
      ...prefs,
      field: [...keys, ...unrelated],
    });
  }

  int _order(List<String> order, String left, String right) {
    final a = order.indexOf(left);
    final b = order.indexOf(right);
    return (a < 0 ? order.length : a).compareTo(b < 0 ? order.length : b);
  }

  /// Original `localeCompare(…, {numeric: true, sensitivity: "base"})`: case-insensitive,
  /// with digit runs compared by value.
  static int _compareNames(String left, String right) {
    final a = left.toLowerCase();
    final b = right.toLowerCase();
    bool digit(int unit) => unit >= 48 && unit <= 57;
    var i = 0;
    var j = 0;
    while (i < a.length && j < b.length) {
      final x = a.codeUnitAt(i);
      final y = b.codeUnitAt(j);
      if (digit(x) && digit(y)) {
        final si = i;
        final sj = j;
        while (i < a.length && digit(a.codeUnitAt(i))) {
          i++;
        }
        while (j < b.length && digit(b.codeUnitAt(j))) {
          j++;
        }
        final na = a.substring(si, i).replaceFirst(RegExp('^0+(?=.)'), '');
        final nb = b.substring(sj, j).replaceFirst(RegExp('^0+(?=.)'), '');
        final value = na.length != nb.length
            ? na.length.compareTo(nb.length)
            : na.compareTo(nb);
        if (value != 0) return value;
        continue;
      }
      if (x != y) return x.compareTo(y);
      i++;
      j++;
    }
    return (a.length - i).compareTo(b.length - j);
  }

  /// Original sidebar project order: `buildWorkspaceStructureProjects` sorts projects by
  /// name (then view key), and `applyStoredOrdering` permutes only the slots of projects
  /// present in the stored order, leaving every other project at its sorted position.
  List<JsonMap> _orderedProjects(HostState host, List<String> stored) {
    final projects = sidebarProjects(host)
      ..sort((a, b) {
        final name = _compareNames(
          a.text('projectDisplayName'),
          b.text('projectDisplayName'),
        );
        return name != 0
            ? name
            : _projectKey(host, a).compareTo(_projectKey(host, b));
      });
    final byKey = {for (final project in projects) _projectKey(host, project): project};
    final pruned = <String>[];
    for (final key in stored) {
      if (byKey.containsKey(key) && !pruned.contains(key)) pruned.add(key);
    }
    if (projects.length <= 1 || pruned.isEmpty) return projects;
    var next = 0;
    return [
      for (final project in projects)
        if (pruned.contains(_projectKey(host, project)))
          byKey[pruned[next++]]!
        else
          project,
    ];
  }

  bool _topLevel(JsonMap agent) =>
      agent['archivedAt'] == null &&
      agent.text('workspaceId').isNotEmpty &&
      agent.text('parentAgentId').isEmpty &&
      agent.object('labels').text('paseo.parent-agent-id').trim().isEmpty;

  /// Original `buildSessionHosts`: a project's sessions are its non-archived top-level
  /// agents in workspaces the display filters keep, newest activity first, then newest
  /// created, then id.
  List<JsonMap> _sessions(HostState host, JsonMap project, JsonMap prefs) {
    final workspaces = {
      for (final workspace in host.workspaces.where(
        (item) => item.text('projectId') == project.text('projectId'),
      ))
        workspace.text('id'): workspace,
    };
    final labels = sidebarLabelFilters(prefs);
    final sessions = host.agents.where((agent) {
      if (!_topLevel(agent)) return false;
      final workspace = workspaces[agent.text('workspaceId')];
      if (workspace == null) return false;
      if (!prefs.flag('showBackground') && workspace.flag('background')) {
        return false;
      }
      return sidebarWorkspaceMatchesLabels(workspace, labels);
    }).toList();
    // Original `lastActivityAt` is the agent snapshot's `updatedAt`.
    sessions.sort((a, b) {
      final activity = _time(b, 'updatedAt').compareTo(_time(a, 'updatedAt'));
      if (activity != 0) return activity;
      final created = _time(b, 'createdAt').compareTo(_time(a, 'createdAt'));
      return created != 0 ? created : a.text('id').compareTo(b.text('id'));
    });
    return sessions;
  }

  Future<void> _hostAction(
    AppStore store,
    HostState host,
    String action,
    List<String> order,
  ) async {
    switch (action) {
      case 'new':
        _newSession(store, host);
        return;
      case 'project':
        _newSession(store, host, addProject: true);
        return;
      case 'history':
        await _history(store, host);
        return;
      case 'import':
        await _history(store, host, importing: true);
        return;
      case 'refresh':
        await store.refreshHost(host.serverId);
        return;
      case 'edit':
        await showHostDialog(context, config: host.config);
        return;
      case 'connections':
        await showHostConnectionsDialog(
          context, store: store, serverId: host.serverId,
        );
        return;
      case 'pair':
        await showHostPairing(context, store, host.serverId);
        return;
      case 'up':
        await _move(store, 'hostOrder', order, host.serverId, -1);
        return;
      case 'down':
        await _move(store, 'hostOrder', order, host.serverId, 1);
        return;
      case 'remove':
        if (await confirmAction(
          context,
          title: 'Remove ${host.label}?',
          message: 'Remove this saved host, all its connections and credentials from this device? Sessions on the daemon are not deleted.',
          confirmLabel: 'Remove',
          destructive: true,
        )) {
          await store.removeHost(host.serverId);
        }
        return;
    }
  }

  Future<void> _projectAction(
    AppStore store,
    HostState host,
    JsonMap project,
    String action,
    List<String> order,
  ) async {
    switch (action) {
      case 'new':
        _newSession(store, host, project: project);
        return;
      case 'history':
        await _history(store, host, project: project);
        return;
      case 'import':
        await _history(store, host, project: project, importing: true);
        return;
      case 'settings':
        store.open(
          PanelTarget(
            kind: 'project_settings',
            serverId: host.serverId,
            workspaceId: '',
            cwd: project.text('projectRootPath'),
            title: '${project.text('projectDisplayName')} settings',
            data: {'project': project},
          ),
        );
        return;
      case 'up':
        await _move(
          store,
          'projectOrder',
          order,
          _projectKey(host, project),
          -1,
        );
        return;
      case 'down':
        await _move(
          store,
          'projectOrder',
          order,
          _projectKey(host, project),
          1,
        );
        return;
      case 'icon':
      case 'resetIcon':
        JsonMap source = {'type': 'automatic'};
        if (action == 'icon') {
          final file = await openFile(
            acceptedTypeGroups: const [
              XTypeGroup(
                label: 'Images',
                extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico'],
                mimeTypes: ['image/*'],
                uniformTypeIdentifiers: ['public.image'],
              ),
            ],
          );
          if (file == null) return;
          final bytes = await file.readAsBytes();
          source = {'type': 'upload', 'data': base64Encode(bytes)};
        }
        await navigationRequest(
          store,
          host.serverId,
          'project.icon.set.request',
          {'projectId': project.text('projectId'), 'source': source},
          'project.icon.set.response',
        );
        await store.refreshHost(host.serverId);
        return;
      case 'rename':
      case 'reset':
        final name = action == 'reset'
            ? ''
            : await navigationName(
                context,
                'Rename project',
                project.text('projectDisplayName'),
              );
        if (name == null) return;
        await navigationRequest(
          store,
          host.serverId,
          'project.rename.request',
          {
            'projectId': project.text('projectId'),
            'customName': name.isEmpty ? null : name,
          },
          'project.rename.response',
        );
        await store.refreshHost(host.serverId);
        return;
      case 'archive':
        await archiveNavigationWorkspaces(
          context,
          store,
          host.serverId,
          host.workspaces
              .where(
                (item) => item.text('projectId') == project.text('projectId'),
              )
              .toList(),
        );
        return;
      case 'remove':
        final workspaces = host.workspaces
            .where(
              (item) => item.text('projectId') == project.text('projectId'),
            )
            .toList();
        if (!await confirmAction(
          context,
          title: 'Remove project from ${host.label}?',
          message:
              '${project.text('projectRootPath')}\n${workspaces.length} active workspaces are affected on this host. Other hosts are unchanged.',
          confirmLabel: 'Remove',
          destructive: true,
        )) {
          return;
        }
        final result = await navigationRequest(
          store,
          host.serverId,
          'project.remove.request',
          {'projectId': project.text('projectId')},
          'project.remove.response',
        );
        final removed = result
            .array('removedWorkspaceIds')
            .map((item) => item.toString())
            .toSet();
        for (final tab in List<PanelTarget>.of(store.tabs)) {
          if (tab.serverId == host.serverId &&
              removed.contains(tab.workspaceId)) {
            await store.closeTab(tab.key);
          }
        }
        await store.refreshHost(host.serverId);
        return;
    }
  }

  Future<void> _agentAction(
    AppStore store,
    HostState host,
    JsonMap agent,
    String action,
  ) async {
    final workspace = host.workspaces.where(
      (item) => item.text('id') == agent.text('workspaceId'),
    );
    if (action == 'labels') {
      if (workspace.isEmpty) {
        throw StateError('The workspace is unavailable. Refresh this host.');
      }
      await showWorkspaceLabels(
        context,
        store: store,
        serverId: host.serverId,
        workspace: workspace.first,
      );
      return;
    }
    if (action == 'recover') {
      await recoverNavigationWorkspace(
        context,
        store,
        host.serverId,
        agent.text('workspaceId'),
      );
      return;
    }
    if (action == 'archiveWorkspace') {
      await archiveNavigationWorkspaces(
        context,
        store,
        host.serverId,
        workspace.toList(),
      );
      return;
    }
    if (action == 'pin') {
      await navigationRequest(
        store,
        host.serverId,
        'workspace.pin.set.request',
        {
          'workspaceId': agent.text('workspaceId'),
          'pinned': workspace.isEmpty || workspace.first['pinnedAt'] == null,
        },
        'workspace.pin.set.response',
      );
    } else if (action == 'unread') {
      await navigationRequest(
        store,
        host.serverId,
        'workspace.mark_unread.request',
        {'workspaceId': agent.text('workspaceId')},
        'workspace.mark_unread.response',
      );
    } else if (action == 'rename') {
      final name = await navigationName(
        context,
        'Rename session',
        _title(agent),
        maxLength: 200,
      );
      if (name == null) return;
      await navigationRequest(store, host.serverId, 'update_agent_request', {
        'agentId': agent.text('id'),
        'name': name,
      }, 'update_agent_response');
    } else if (action == 'archive') {
      if (!await confirmAction(
        context,
        title: 'Archive session?',
        message: _title(agent),
        confirmLabel: 'Archive',
      )) {
        return;
      }
      await navigationRequest(store, host.serverId, 'archive_agent_request', {
        'agentId': agent.text('id'),
      }, 'agent_archived');
    }
    await store.refreshHost(host.serverId);
  }

  Future<void> _showActions(
    BuildContext anchor,
    Map<String, String> items,
    void Function(String) onSelected, [
    Offset? globalPosition,
  ]) async {
    if (_busy || items.isEmpty) return;
    final box = anchor.findRenderObject() as RenderBox;
    final origin = globalPosition ?? box.localToGlobal(Offset(0, box.size.height));
    final action = await showMenu<String>(
      context: anchor,
      position: RelativeRect.fromLTRB(origin.dx, origin.dy, origin.dx, origin.dy),
      items: [
        for (final item in items.entries)
          PopupMenuItem(value: item.key, height: 36, child: Text(item.value)),
      ],
    );
    if (action != null && mounted) onSelected(action);
  }

  /// The original row `ToolbarButton` (small, non-compact): a 20px hitbox with a 6px
  /// radius and the interaction highlight while hovered or pressed around an sm muted
  /// glyph. Holding it (or right-clicking) opens the row's complete action menu.
  Widget _toolbarButton({
    required IconData icon,
    required String tooltip,
    required String primary,
    required Map<String, String> items,
    required void Function(String) onSelected,
  }) => PaseoChromeButton(
    tooltip: tooltip,
    icon: icon,
    extent: 20,
    iconSize: densityIcon(context, 'sm'),
    radius: 6,
    onPressed: _busy ? null : (_) => onSelected(primary),
    onLongPress: _busy ? null : (anchor) => _showActions(anchor, items, onSelected),
  );

  /// A pressable tree row body that also opens [items] on long-press / right-click. The
  /// original session-list `PressHighlight` has no highlight style, so presses paint nothing.
  Widget _pressable({
    required Widget child,
    required VoidCallback onTap,
    required Map<String, String> items,
    required void Function(String) onSelected,
  }) => Builder(
    builder: (context) => InkWell(
      onTap: onTap,
      splashFactory: NoSplash.splashFactory,
      highlightColor: Colors.transparent,
      hoverColor: Colors.transparent,
      onLongPress: _busy ? null : () => _showActions(context, items, onSelected),
      onSecondaryTapUp: _busy
          ? null
          : (details) => _showActions(context, items, onSelected, details.globalPosition),
      child: child,
    ),
  );

  @override
  Widget build(BuildContext context) {
    final store = PaseoScope.of(context);
    final prefs = _prefs(store);
    final collapsed = _set(prefs, 'collapsed');
    final hosts = List<HostState>.of(store.hosts);
    final hostOrder = prefs
        .array('hostOrder')
        .map((item) => item.toString())
        .toList();
    hosts.sort((a, b) => _order(hostOrder, a.serverId, b.serverId));
    final hostFilters = resolveSidebarHostFilters(store, prefs);
    final projectFilters = resolveSidebarProjectFilters(store, prefs);
    final projectOrder = prefs
        .array('projectOrder')
        .map((item) => item.toString())
        .toList();
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final iconSm = densityIcon(context, 'sm');
    // Original session-list `hostText`: muted, base size, medium weight.
    final groupStyle = theme.textTheme.bodyMedium!.copyWith(
      color: colors.muted,
      fontWeight: FontWeight.w500,
    );
    final rows = <Widget>[];
    for (final host in hosts) {
      if (hostFilters.isNotEmpty && !hostFilters.contains(host.serverId)) {
        continue;
      }
      final projects = _orderedProjects(host, projectOrder);
      final hostLabel = hostDisplayLabel(host);
      final hostMatches = hostLabel.toLowerCase().contains(_query);
      final projectRows = <Widget>[];
      final hostCollapsed =
          _query.isEmpty && collapsed.contains('host:${host.serverId}');
      for (final project in projects) {
        final key = _projectKey(host, project);
        if (projectFilters.isNotEmpty && !projectFilters.contains(key)) {
          continue;
        }
        final projectMatches =
            '${project.text('projectDisplayName')} ${project.text('projectRootPath')}'
                .toLowerCase()
                .contains(_query);
        final sessions = _sessions(host, project, prefs)
            .where(
              (agent) =>
                  _query.isEmpty ||
                  hostMatches ||
                  projectMatches ||
                  // Original `buildSessionTreeRows`: `${session.title ?? ""}`, never
                  // the "Untitled session" placeholder.
                  '${agent.text('title')} ${agent.text('provider')}'
                      .toLowerCase()
                      .contains(_query),
            )
            .toList();
        if (_query.isNotEmpty &&
            !hostMatches &&
            !projectMatches &&
            sessions.isEmpty) {
          continue;
        }
        final projectCollapsed = _query.isEmpty && collapsed.contains(key);
        final projectActions = {
          'new': tr(context, 'sessionNavigation.newSession'),
          'history': tr(context, 'sessions.title'),
          'import': tr(context, 'importSession.title'),
          'rename': 'Rename project',
          'reset': 'Reset project name',
          'icon': 'Choose project icon',
          'resetIcon': 'Reset project icon',
          'settings': 'Project settings',
          'archive': 'Archive all workspaces',
          'up': 'Move up',
          'down': 'Move down',
          'remove': 'Remove project',
        };
        void onProjectAction(String action) => _run(
          () => _projectAction(
            store,
            host,
            project,
            action,
            projects.map((item) => _projectKey(host, item)).toList(),
          ),
        );
        final projectName = project.text(
          'projectDisplayName',
          project.text('projectRootPath'),
        );
        projectRows.add(
          Padding(
            key: ValueKey(key),
            padding: const EdgeInsets.only(left: 24, right: 8),
            child: Row(
              children: [
                Expanded(
                  child: _pressable(
                    onTap: () => _toggle(store, key),
                    items: projectActions,
                    onSelected: onProjectAction,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 8),
                      child: Row(
                        spacing: 4,
                        children: [
                          Icon(
                            projectCollapsed
                                ? LucideIcons.chevronRight300
                                : LucideIcons.chevronDown300,
                            size: iconSm,
                            color: colors.muted,
                          ),
                          Icon(
                            LucideIcons.folder300,
                            size: iconSm,
                            color: colors.muted,
                          ),
                          Expanded(
                            child: Text(
                              projectName,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: groupStyle,
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
                _toolbarButton(
                  icon: LucideIcons.plus300,
                  tooltip: tr(context, 'sessionNavigation.newSession'),
                  primary: 'new',
                  items: projectActions,
                  onSelected: onProjectAction,
                ),
              ],
            ),
          ),
        );
        if (!projectCollapsed) {
          for (final agent in sessions) {
            final workspaceRows = host.workspaces.where(
              (item) => item.text('id') == agent.text('workspaceId'),
            );
            final workspace = workspaceRows.isEmpty
                ? <String, dynamic>{}
                : workspaceRows.first;
            final pinned = workspace['pinnedAt'] != null;
            final selected =
                store.selected?.kind == 'agent' &&
                store.selected?.serverId == host.serverId &&
                store.selected?.id == agent.text('id');
            final attention = agent.flag('requiresAttention');
            final status = agent.text('status');
            final statusKey = 'agentList.status.$status';
            final statusLabel = tr(context, statusKey);
            final sessionActions = {
              'rename': tr(context, 'workspace.tabs.menu.renameAgent'),
              'archive': tr(context, 'agentList.archiveSheet.archive'),
              'labels': 'Workspace labels',
              'pin': pinned ? 'Unpin workspace' : 'Pin workspace',
              'unread': 'Mark workspace unread',
              'recover': 'Recover workspace',
              'archiveWorkspace': tr(
                context,
                'workspace.git.actions.archive.label',
              ),
            };
            void onSessionAction(String action) =>
                _run(() => _agentAction(store, host, agent, action));
            projectRows.add(
              Padding(
                key: ValueKey('session:${host.serverId}:${agent.text('id')}'),
                padding: const EdgeInsets.only(left: 48, right: 8),
                // Original `selected`: the border colour behind a 6px-radius row.
                child: Material(
                  color: selected ? theme.dividerColor : Colors.transparent,
                  borderRadius: BorderRadius.circular(6),
                  clipBehavior: Clip.antiAlias,
                  child: Row(
                    children: [
                      Expanded(
                        child: _pressable(
                          onTap: () {
                            openNavigationAgent(store, host.serverId, agent);
                            if (isCompactLayout(context)) {
                              store.showSidebar(false);
                            }
                          },
                          items: sessionActions,
                          onSelected: onSessionAction,
                          child: Padding(
                            padding: const EdgeInsets.symmetric(
                              vertical: 8,
                              horizontal: 12,
                            ),
                            child: Row(
                              spacing: 8,
                              children: [
                                Icon(
                                  LucideIcons.messageSquare300,
                                  size: iconSm,
                                  color: attention
                                      ? theme.colorScheme.onSurface
                                      : colors.muted,
                                ),
                                Expanded(
                                  child: Column(
                                    crossAxisAlignment:
                                        CrossAxisAlignment.start,
                                    spacing: 2,
                                    children: [
                                      Text(
                                        _title(agent),
                                        maxLines: 1,
                                        overflow: TextOverflow.ellipsis,
                                        style: theme.textTheme.bodyMedium!
                                            .copyWith(
                                              fontWeight: attention
                                                  ? FontWeight.w600
                                                  : null,
                                            ),
                                      ),
                                      Text(
                                        '${agent.text('provider')} · ${statusLabel == statusKey ? status : statusLabel}',
                                        maxLines: 1,
                                        overflow: TextOverflow.ellipsis,
                                        style: theme.textTheme.bodySmall!
                                            .copyWith(color: colors.muted),
                                      ),
                                    ],
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                      _toolbarButton(
                        icon: LucideIcons.pencil300,
                        tooltip: tr(context, 'workspace.tabs.menu.renameAgent'),
                        primary: 'rename',
                        items: sessionActions,
                        onSelected: onSessionAction,
                      ),
                    ],
                  ),
                ),
              ),
            );
          }
        }
      }
      if (_query.isNotEmpty && !hostMatches && projectRows.isEmpty) continue;
      final status = host.client.status;
      final live =
          status == ConnectionStatus.connected &&
          !host.loading &&
          host.error == null;
      final hostActions = {
        'new': tr(context, 'sessionNavigation.newSession'),
        'project': tr(context, 'sidebar.actions.addProject'),
        'history': tr(context, 'sessions.title'),
        'import': tr(context, 'importSession.title'),
        'refresh': 'Reconnect / refresh',
        'edit': 'Edit host / authentication',
        'connections': 'Manage connections',
        'pair': 'Pair another device',
        'up': 'Move up',
        'down': 'Move down',
        'remove': 'Remove host',
      };
      void onHostAction(String action) => _run(
        () => _hostAction(
          store,
          host,
          action,
          hosts.map((item) => item.serverId).toList(),
        ),
      );
      rows.add(
        Padding(
          key: ValueKey('host:${host.serverId}'),
          padding: const EdgeInsets.fromLTRB(8, 8, 8, 0),
          child: Row(
            children: [
              Expanded(
                child: _pressable(
                  onTap: () => _toggle(store, 'host:${host.serverId}'),
                  items: hostActions,
                  onSelected: onHostAction,
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 8),
                    child: Row(
                      spacing: 4,
                      children: [
                        Icon(
                          hostCollapsed
                              ? LucideIcons.chevronRight300
                              : LucideIcons.chevronDown300,
                          size: iconSm,
                          color: colors.muted,
                        ),
                        // Original HostStatusDot: online / connecting / offline.
                        Container(
                          width: 8,
                          height: 8,
                          decoration: BoxDecoration(
                            shape: BoxShape.circle,
                            color: live
                                ? colors.success
                                : status == ConnectionStatus.disconnected
                                ? colors.danger
                                : colors.warning,
                          ),
                        ),
                        Expanded(
                          child: Text(
                            hostLabel,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: groupStyle,
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
              _toolbarButton(
                icon: LucideIcons.plus300,
                tooltip: tr(context, 'sessionNavigation.newSession'),
                primary: 'new',
                items: hostActions,
                onSelected: onHostAction,
              ),
            ],
          ),
        ),
      );
      if (!hostCollapsed) {
        if (host.loading) {
          rows.add(
            const Padding(
              padding: EdgeInsets.symmetric(horizontal: 12),
              child: LinearProgressIndicator(minHeight: 2),
            ),
          );
        }
        if (host.error != null) {
          rows.add(
            Padding(
              padding: const EdgeInsets.fromLTRB(32, 0, 8, 4),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    host.error.toString(),
                    maxLines: 3,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall!.copyWith(
                      color: theme.colorScheme.error,
                    ),
                  ),
                  Wrap(
                    children: [
                      TextButton(
                        onPressed: _busy
                            ? null
                            : () =>
                                  _run(() => store.refreshHost(host.serverId)),
                        child: Text(tr(context, 'common.actions.retry')),
                      ),
                      TextButton(
                        onPressed: _busy
                            ? null
                            : () => _run(
                                () => showHostDialog(
                                  context,
                                  config: host.config,
                                ),
                              ),
                        child: const Text('Authentication / connection'),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          );
        }
        rows.addAll(projectRows);
        if (projects.isEmpty && !host.loading && _query.isEmpty) {
          rows.add(
            Padding(
              padding: const EdgeInsets.only(left: 24, right: 8),
              child: _pressable(
                onTap: () => _newSession(store, host, addProject: true),
                items: const {},
                onSelected: (_) {},
                child: Padding(
                  padding: const EdgeInsets.symmetric(vertical: 8),
                  child: Row(
                    spacing: 4,
                    children: [
                      SizedBox(width: iconSm),
                      Icon(
                        LucideIcons.folderPlus300,
                        size: iconSm,
                        color: colors.muted,
                      ),
                      Expanded(
                        child: Text(
                          tr(context, 'sidebar.actions.addProject'),
                          style: theme.textTheme.bodyMedium!.copyWith(
                            color: colors.muted,
                          ),
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          );
        }
      }
    }
    final searchStyle = theme.textTheme.bodyMedium!;
    // The original search is a bare Android TextInput whose default dark placeholder is
    // 70% white; light palettes keep the theme hint.
    final searchHint = theme.brightness == Brightness.dark
        ? searchStyle.copyWith(color: const Color(0xb3ffffff))
        : null;
    final border = OutlineInputBorder(
      borderRadius: BorderRadius.circular(6),
      borderSide: BorderSide(color: theme.dividerColor),
    );
    return Material(
      color: colors.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // Original SidebarSectionHeader: muted small title, display menu at the trailing edge.
          ConstrainedBox(
            constraints: BoxConstraints(
              minHeight: isCompactLayout(context) ? 44 : 36,
            ),
            child: Padding(
              padding: const EdgeInsets.only(left: 8, right: 4),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      tr(context, 'sessionNavigation.sessions'),
                      style: theme.textTheme.bodySmall!.copyWith(
                        color: colors.muted,
                      ),
                    ),
                  ),
                  // Original display-preferences trigger: 28px, 6px radius,
                  // surfaceSidebarHover while hovered, 14px muted Settings2.
                  PaseoChromeButton(
                    tooltip: tr(context, 'sidebar.display.trigger'),
                    icon: LucideIcons.settings2300,
                    extent: 28,
                    iconSize: 14,
                    radius: 6,
                    highlight: paseoSidebarHover(context),
                    hoverOnly: true,
                    onPressed: _busy
                        ? null
                        : (anchor) => _run(
                            () => showSidebarDisplayPreferences(
                              context,
                              store: store,
                              anchor: paseoAnchorRect(anchor),
                            ),
                          ),
                  ),
                ],
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 0, 12, 8),
            child: TextField(
              controller: _search,
              autocorrect: false,
              style: searchStyle,
              decoration: InputDecoration(
                hintText: tr(context, 'sessionNavigation.search'),
                hintStyle: searchHint,
                filled: true,
                fillColor: colors.sidebar,
                isDense: true,
                contentPadding: const EdgeInsets.all(8),
                border: border,
                enabledBorder: border,
                focusedBorder: border.copyWith(
                  borderSide: BorderSide(color: colors.muted),
                ),
              ),
              onChanged: (value) =>
                  setState(() => _query = value.trim().toLowerCase()),
            ),
          ),
          if (_busy) const LinearProgressIndicator(minHeight: 2),
          Expanded(
            child: RefreshIndicator(
              onRefresh: store.refreshHosts,
              child: ListView(
                physics: const AlwaysScrollableScrollPhysics(),
                padding: const EdgeInsets.only(bottom: 8),
                children: rows.isNotEmpty
                    ? rows
                    : [
                        Padding(
                          padding: const EdgeInsets.all(16),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            spacing: 8,
                            children: [
                              Text(
                                store.hosts.isEmpty
                                    ? 'Connect to a Paseo host to see your sessions.'
                                    : tr(context, 'sessionNavigation.empty'),
                                style: searchStyle.copyWith(color: colors.muted),
                              ),
                              if (store.hosts.isEmpty)
                                OutlinedButton(
                                  onPressed: () =>
                                      _run(() => showHostDialog(context)),
                                  child: Text(tr(context, 'settings.addHost')),
                                ),
                              if (store.hosts.isNotEmpty)
                                Builder(
                                  builder: (anchor) => TextButton(
                                    onPressed: () => _run(
                                      () => showSidebarDisplayPreferences(
                                        context,
                                        store: store,
                                        anchor: paseoAnchorRect(anchor),
                                      ),
                                    ),
                                    child: Text(
                                      tr(context, 'sidebar.display.trigger'),
                                    ),
                                  ),
                                ),
                            ],
                          ),
                        ),
                      ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}
