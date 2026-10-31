import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../workspace/project_settings_panel.dart';
import 'host_connections_dialog.dart';
import 'navigation_actions.dart';
import 'workspace_labels_dialog.dart';

/// Original `SIDEBAR_UNLABELLED_LABEL_KEY`: the empty key is the Unlabelled row.
const sidebarUnlabelledLabelKey = '';

/// Original `workspaceLabelKey`: the normalized (trimmed, single-spaced) name, lowercased.
String sidebarLabelKey(String name) =>
    name.trim().replaceAll(RegExp(r'\s+'), ' ').toLowerCase();

/// The sidebar's project key for one host placement (`project:<serverId>:<projectId>`).
String sidebarProjectKey(HostState host, JsonMap project) =>
    'project:${host.serverId}:${project.text('projectId')}';

/// Every project placed on [host]: its registered projects plus any project only known
/// through one of its workspaces.
List<JsonMap> sidebarProjects(HostState host) {
  final projects = <String, JsonMap>{
    for (final project in host.projects) project.text('projectId'): project,
  };
  for (final workspace in host.workspaces) {
    projects.putIfAbsent(
      workspace.text('projectId'),
      () => {
        'projectId': workspace.text('projectId'),
        'projectKey': workspace.object('project').text('projectKey'),
        'projectDisplayName': workspace.text('projectDisplayName'),
        'projectRootPath': workspace.text('projectRootPath'),
      },
    );
  }
  return projects.values.toList();
}

/// Original `SidebarModel.resolvedProjectFilters`: a stored key whose project is not
/// currently listed filters nothing, so only the keys of existing projects count.
Set<String> resolveSidebarProjectFilters(AppStore store, JsonMap prefs) {
  final stored = prefs.array('projectFilters').map((item) => item.toString()).toSet();
  if (stored.isEmpty) return const {};
  return {
    for (final host in store.hosts)
      for (final project in sidebarProjects(host))
        if (stored.contains(sidebarProjectKey(host, project))) sidebarProjectKey(host, project),
  };
}

/// Original `SessionList.visibleHosts`: a host filter that matches no saved host shows
/// every host rather than an empty sidebar.
Set<String> resolveSidebarHostFilters(AppStore store, JsonMap prefs) {
  final stored = prefs.array('hostFilters').map((item) => item.toString()).toSet();
  return {
    for (final host in store.hosts)
      if (stored.contains(host.serverId)) host.serverId,
  };
}

/// The stored label filter keys; legacy `__unlabelled__` reads as the Unlabelled key.
Set<String> sidebarLabelFilters(JsonMap prefs) => {
  for (final item in prefs.array('labelFilters'))
    item.toString() == '__unlabelled__' ? sidebarUnlabelledLabelKey : sidebarLabelKey(item.toString()),
};

/// Original `filterWorkspacesByLabels`: any selected label matches; Unlabelled matches a
/// workspace that carries no real label.
bool sidebarWorkspaceMatchesLabels(JsonMap workspace, Set<String> filters) {
  if (filters.isEmpty) return true;
  final keys = {
    for (final label in workspace.array('labels'))
      if (sidebarLabelKey(label.toString()).isNotEmpty) sidebarLabelKey(label.toString()),
  };
  return filters.any(
    (key) => key == sidebarUnlabelledLabelKey ? keys.isEmpty : keys.contains(key),
  );
}

/// Original `SidebarDisplayPreferencesMenu` (components/sidebar/display-preferences/
/// menu.tsx): the "Display" sheet (a 232px popover on wide layouts) with one row per
/// decision — Host and Project filters (only with more than one to choose from), Labels
/// (only when there are labels or an active label filter) and Show background — whose
/// options live a page down. Every choice applies immediately.
Future<void> showSidebarDisplayPreferences(
  BuildContext context, {
  required AppStore store,
  Rect? anchor,
}) => showPaseoMenuSurface<void>(
  context,
  anchor: anchor,
  width: 232,
  alignEnd: true,
  builder: (_) => _DisplayPreferences(store: store),
);

enum _Page { hosts, projects, labels }

class _DisplayPreferences extends StatefulWidget {
  const _DisplayPreferences({required this.store});
  final AppStore store;
  @override
  State<_DisplayPreferences> createState() => _DisplayPreferencesState();
}

class _DisplayPreferencesState extends State<_DisplayPreferences> {
  _Page? _page;

  /// The merged cross-host label catalog (`useWorkspaceLabelProjection().labels`), keyed by
  /// label key; the first host to report a label decides its colour.
  final Map<String, JsonMap> _catalog = {};

  AppStore get store => widget.store;

  @override
  void initState() {
    super.initState();
    unawaited(_loadLabels());
  }

  Future<void> _loadLabels() async {
    for (final host in List<HostState>.of(store.hosts)) {
      if (host.client.status != ConnectionStatus.connected) continue;
      try {
        final response = await navigationRequest(
          store,
          host.serverId,
          'workspace.label.list.request',
          {},
          'workspace.label.list.response',
        );
        if (!mounted) return;
        setState(() {
          for (final label in response.array('labels').map(jsonMap)) {
            final key = sidebarLabelKey(label.text('name'));
            if (key.isNotEmpty) _catalog.putIfAbsent(key, () => label);
          }
        });
      } catch (_) {
        // A host without the label catalog contributes only the labels its workspaces carry.
      }
    }
  }

  JsonMap get _prefs => {...jsonMap(store.preferences['navigation'])};

  Future<void> _save(JsonMap prefs) async {
    store.preferences['navigation'] = prefs;
    await store.savePreferences();
  }

  Future<void> _toggle(String field, String value) {
    final prefs = _prefs;
    final values = prefs.array(field).map((item) => item.toString()).toList();
    if (!values.remove(value)) values.add(value);
    return _save({...prefs, field: values});
  }

  Future<void> _clear(String field) => _save({..._prefs, field: <String>[]});

  Future<void> _toggleLabel(String key) {
    final prefs = _prefs;
    final values = sidebarLabelFilters(prefs).toList();
    if (!values.remove(key)) values.add(key);
    return _save({...prefs, 'labelFilters': values});
  }

  /// Every label any host knows, merged by key, then any label only a workspace carries.
  List<(String, String, String)> _labels() {
    final labels = <String, (String, String, String)>{
      for (final entry in _catalog.entries)
        entry.key: (entry.key, entry.value.text('name'), entry.value.text('color')),
    };
    for (final host in store.hosts) {
      for (final workspace in host.workspaces) {
        for (final raw in workspace.array('labels')) {
          final name = raw.toString();
          final key = sidebarLabelKey(name);
          if (key.isNotEmpty) labels.putIfAbsent(key, () => (key, name.trim(), ''));
        }
      }
    }
    return labels.values.toList();
  }

  /// Original "Manage labels…": closes the menu and opens the label manager for a host.
  Future<void> _manageLabels() async {
    final navigator = Navigator.of(context);
    final host = await showHostChooser(
      context,
      store,
      filter: (host) => host.client.status == ConnectionStatus.connected,
    );
    if (host == null || !mounted) return;
    navigator.pop();
    await showWorkspaceLabelManager(navigator.context, store: store, serverId: host.serverId);
  }

  Widget _hostDot(HostState host) {
    final colors = PaseoColors.of(context);
    final status = host.client.status;
    return Container(
      width: 8,
      height: 8,
      decoration: BoxDecoration(
        shape: BoxShape.circle,
        color: status == ConnectionStatus.connected && host.error == null
            ? colors.success
            : status == ConnectionStatus.disconnected || status == ConnectionStatus.error
            ? colors.danger
            : colors.warning,
      ),
    );
  }

  Widget _labelDot(String color) => color.isEmpty
      ? Icon(LucideIcons.circle300, size: 11, color: PaseoColors.of(context).muted)
      : Container(
          width: 10,
          height: 10,
          decoration: BoxDecoration(shape: BoxShape.circle, color: labelColor(context, color)),
        );

  List<Widget> _hostPage(JsonMap prefs) {
    final selected = resolveSidebarHostFilters(store, prefs);
    return [
      PaseoMenuItem(
        label: tr(context, 'sidebar.display.hostFilter.all'),
        selected: prefs.array('hostFilters').isEmpty,
        onTap: () => _clear('hostFilters'),
      ),
      for (final host in store.hosts)
        PaseoMenuItem(
          label: hostDisplayLabel(host),
          leading: _hostDot(host),
          selected: selected.contains(host.serverId),
          onTap: () => _toggle('hostFilters', host.serverId),
        ),
    ];
  }

  List<Widget> _projectPage(JsonMap prefs) {
    final resolved = resolveSidebarProjectFilters(store, prefs);
    return [
      PaseoMenuItem(
        label: tr(context, 'sidebar.display.projectFilter.all'),
        selected: resolved.isEmpty,
        onTap: () => _clear('projectFilters'),
      ),
      for (final host in store.hosts)
        for (final project in sidebarProjects(host))
          PaseoMenuItem(
            label: project.text('projectDisplayName', project.text('projectRootPath')),
            leading: ProjectIconSquare(
              name: project.text('projectDisplayName', project.text('projectRootPath')),
              viewKey: sidebarProjectKey(host, project),
              size: 14,
            ),
            selected: resolved.contains(sidebarProjectKey(host, project)),
            onTap: () => _toggle('projectFilters', sidebarProjectKey(host, project)),
          ),
    ];
  }

  List<Widget> _labelPage(JsonMap prefs) {
    final selected = sidebarLabelFilters(prefs);
    return [
      for (final (key, name, color) in _labels())
        PaseoMenuItem(
          label: name,
          leading: _labelDot(color),
          selected: selected.contains(key),
          onTap: () => _toggleLabel(key),
        ),
      PaseoMenuItem(
        label: tr(context, 'workspaceLabels.unlabelled'),
        leading: _labelDot(''),
        selected: selected.contains(sidebarUnlabelledLabelKey),
        onTap: () => _toggleLabel(sidebarUnlabelledLabelKey),
      ),
      if (selected.isNotEmpty) ...[
        const PaseoMenuSeparator(),
        PaseoMenuItem(
          label: tr(context, 'workspaceLabels.filter.clear'),
          onTap: () => _clear('labelFilters'),
        ),
      ],
      const PaseoMenuSeparator(),
      PaseoMenuItem(
        label: tr(context, 'workspaceLabels.manage.open'),
        onTap: _manageLabels,
      ),
    ];
  }

  List<Widget> _rootPage(JsonMap prefs) {
    final showHostFilter = store.hosts.length > 1;
    final showProjectFilter =
        store.hosts.fold<int>(0, (count, host) => count + sidebarProjects(host).length) > 1;
    final labelFilters = sidebarLabelFilters(prefs);
    final showLabelFilter = _labels().isNotEmpty || labelFilters.isNotEmpty;
    return [
      if (showHostFilter) ...[
        const PaseoMenuSeparator(),
        PaseoMenuSubTrigger(
          label: tr(context, 'sidebar.display.hostFilter.label'),
          indicator: prefs.array('hostFilters').isNotEmpty,
          onOpen: () => setState(() => _page = _Page.hosts),
        ),
      ],
      if (showProjectFilter) ...[
        // Host and Project narrow the same list, so they read as one block.
        if (!showHostFilter) const PaseoMenuSeparator(),
        PaseoMenuSubTrigger(
          label: tr(context, 'sidebar.display.projectFilter.label'),
          indicator: resolveSidebarProjectFilters(store, prefs).isNotEmpty,
          onOpen: () => setState(() => _page = _Page.projects),
        ),
      ],
      if (showLabelFilter) ...[
        const PaseoMenuSeparator(),
        PaseoMenuSubTrigger(
          label: tr(context, 'workspaceLabels.title'),
          indicator: labelFilters.isNotEmpty,
          onOpen: () => setState(() => _page = _Page.labels),
        ),
      ],
      const PaseoMenuSeparator(),
      PaseoMenuItem(
        label: tr(context, 'sidebar.display.showBackground'),
        selected: prefs.flag('showBackground'),
        onTap: () => _save({..._prefs, 'showBackground': !prefs.flag('showBackground')}),
      ),
    ];
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: store,
    builder: (context, _) {
      final prefs = _prefs;
      final page = _page;
      final (title, rows) = switch (page) {
        null => (tr(context, 'sidebar.display.heading'), _rootPage(prefs)),
        _Page.hosts => (tr(context, 'sidebar.display.hostFilter.label'), _hostPage(prefs)),
        _Page.projects => (
          tr(context, 'sidebar.display.projectFilter.label'),
          _projectPage(prefs),
        ),
        _Page.labels => (tr(context, 'workspaceLabels.title'), _labelPage(prefs)),
      };
      // Sheets always carry a header; the popover root has none, its pages lead back.
      final header = isCompactLayout(context) || page != null;
      return Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (header)
            Padding(
              padding: EdgeInsets.only(top: isCompactLayout(context) ? 0 : 8),
              child: PaseoMenuSheetHeader(
                title: title,
                onBack: page == null ? null : () => setState(() => _page = null),
              ),
            ),
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 4),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: rows,
            ),
          ),
        ],
      );
    },
  );
}
