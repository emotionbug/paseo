import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:path/path.dart' as p;

import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'file_icons.dart';
import 'pane_chrome.dart';

final _windowsDrive = RegExp(r'^[A-Za-z]:');
p.Context _remoteStyle(String path) =>
    _windowsDrive.hasMatch(path) || path.startsWith('\\\\')
    ? p.windows
    : p.posix;
String remoteBasename(String path, {String? cwd}) =>
    _remoteStyle(cwd ?? path).basename(path);
String remoteParent(String path) => _remoteStyle(path).dirname(path);
String remoteAbsolute(String cwd, String path) {
  final style = _remoteStyle(cwd);
  return style.normalize(style.isAbsolute(path) ? path : style.join(cwd, path));
}

String remoteRelative(String cwd, String path) {
  final style = _remoteStyle(cwd);
  return style.isAbsolute(path)
      ? style.relative(path, from: cwd)
      : style.normalize(path);
}

/// Original `isHiddenExplorerPath`: any segment other than `.`/`..` starting
/// with a dot.
bool isHiddenExplorerPath(String path) =>
    path.split('/').any((segment) => segment != '.' && segment != '..' && segment.startsWith('.'));

/// Original `isExplorerPathWithin`.
bool isExplorerPathWithin(String candidate, String parent) =>
    candidate == parent || candidate.startsWith('$parent/');

/// Original `parentExplorerPath`.
String parentExplorerPath(String entryPath) {
  final separator = entryPath.lastIndexOf('/');
  return separator > 0 ? entryPath.substring(0, separator) : '.';
}

/// Original `MAX_AUTO_EXPANDED_DIRECTORY_DEPTH`.
const maxAutoExpandedDirectoryDepth = 5;

/// Original `reconcileRestoredExpandedPaths`: the restored paths, minus what
/// the user collapsed meanwhile, plus what the user expanded meanwhile.
List<String> reconcileRestoredExpandedPaths({
  required Set<String> persisted,
  required Set<String> current,
  required List<String> restored,
}) {
  final reconciled = {...restored};
  for (final path in persisted) {
    if (!current.contains(path)) reconciled.remove(path);
  }
  for (final path in current) {
    if (!persisted.contains(path)) reconciled.add(path);
  }
  return reconciled.toList();
}

/// Original `restoreExpandedDirectories`: re-lists the persisted expanded
/// directories reachable through visible entries, at most
/// [maxAutoExpandedDirectoryDepth] levels deep. Returns `.` and every
/// directory that listed.
Future<List<String>> restoreExpandedDirectories({
  required List<JsonMap> rootEntries,
  required Set<String> persisted,
  required bool showHiddenFiles,
  required Future<List<JsonMap>?> Function(String path) list,
}) async {
  final restored = <String>['.'];
  final requested = <String>{'.'};
  var parents = [rootEntries];
  for (var depth = 1; depth <= maxAutoExpandedDirectoryDepth; depth += 1) {
    final paths = <String>[];
    for (final entries in parents) {
      for (final entry in entries) {
        if (!showHiddenFiles && entry.text('name').startsWith('.')) continue;
        final path = entry.text('path');
        if (entry.text('kind') == 'directory' && persisted.contains(path) && requested.add(path)) {
          paths.add(path);
        }
      }
    }
    if (paths.isEmpty) break;
    final listings = await Future.wait(paths.map(list));
    parents = [];
    for (final (index, listing) in listings.indexed) {
      if (listing == null) continue;
      restored.add(paths[index]);
      parents.add(listing);
    }
  }
  return restored;
}

/// Original panel-store explorer state, persisted on the device: the sort,
/// whether hidden files show, and each workspace's expanded directories
/// (`expandedPathsByWorkspace[workspaceKey]`).
class ExplorerPreferences {
  const ExplorerPreferences({
    required this.values,
    required this.save,
    required this.workspaceKey,
  });
  final Map<String, dynamic> values;
  final Future<void> Function() save;

  /// Original `buildWorkspaceExplorerStateKey`: `workspace:<id>` or `root:<dir>`.
  final String workspaceKey;
}

/// Directory entries are flattened only when expanded; rows are created lazily.
class RemoteDirectoryBrowser extends StatefulWidget {
  const RemoteDirectoryBrowser({
    super.key,
    required this.client,
    required this.cwd,
    this.initialPath = '.',
    this.directoriesOnly = false,
    this.onDirectoryChanged,
    this.onDirectorySelected,
    this.onFileSelected,
    this.entryTrailing,
    this.onEntryMenu,
    this.onBackgroundMenu,
    this.toolbarActions = const [],
    this.navigationMenu = true,
    this.preferences,
  });
  final PaseoClient client;
  final String cwd;
  final String initialPath;
  final bool directoriesOnly;
  final ValueChanged<String>? onDirectoryChanged;
  final ValueChanged<String>? onDirectorySelected;
  final ValueChanged<JsonMap>? onFileSelected;
  final Widget Function(BuildContext, JsonMap)? entryTrailing;

  /// Original row context menu: long press (compact) or right click (desktop, with the
  /// pointer's global [position]) on an entry; [rowContext] anchors a popover.
  final void Function(BuildContext rowContext, JsonMap entry, Offset? position)? onEntryMenu;

  /// Original `RootCreationContextTarget`: the same gestures on the tree's empty area.
  final void Function(Offset? position)? onBackgroundMenu;
  final List<Widget> toolbarActions;

  /// The trailing "…" back / forward / parent / go-to menu of the directory picker. The
  /// original workspace file explorer toolbar has no such control.
  final bool navigationMenu;

  /// The workspace explorer's persisted state; null for a directory picker.
  final ExplorerPreferences? preferences;
  @override
  State<RemoteDirectoryBrowser> createState() => RemoteDirectoryBrowserState();
}

class RemoteDirectoryBrowserState extends State<RemoteDirectoryBrowser> {
  final _pathInput = TextEditingController();
  final _directories = <String, List<JsonMap>>{};
  final _expanded = <String>{};
  final _loading = <String>{};
  final _inflight = <String, Future<List<JsonMap>?>>{};
  final _errors = <String, Object>{};
  final _history = <String>[];
  int _historyIndex = -1;
  int _generation = 0;
  String _path = '.';
  bool _hidden = true;
  String _sort = 'name';
  bool _refreshing = false;
  String? _selected;
  bool _showLocation = false;
  List<(JsonMap, int)> _rows = [];

  @override
  void initState() {
    super.initState();
    _path = widget.initialPath;
    final preferences = widget.preferences;
    if (preferences == null) {
      navigate(_path);
      return;
    }
    _hidden = preferences.values['explorerShowHiddenFiles'] != false;
    final sort = preferences.values['explorerSortOption'];
    if (const ['name', 'modified', 'size'].contains(sort)) _sort = sort as String;
    unawaited(_initialize());
  }

  /// The persisted expanded directories of this workspace (always with `.`).
  Set<String> get _persistedExpanded {
    final preferences = widget.preferences!;
    final paths = jsonMap(preferences.values['expandedPathsByWorkspace'])[preferences.workspaceKey];
    final list = paths is List ? paths.whereType<String>().toList() : const <String>[];
    return {if (list.isEmpty) '.', ...list};
  }

  /// Original `initializeExplorer`: lists the root, then restores the
  /// persisted expanded directories.
  Future<void> _initialize() async {
    final persisted = _persistedExpanded;
    setState(() {
      _expanded
        ..clear()
        ..addAll(persisted.where((path) => path != '.'));
    });
    final root = await navigate(_path);
    if (root == null || !mounted) return;
    final restored = await restoreExpandedDirectories(
      rootEntries: root,
      persisted: persisted,
      showHiddenFiles: _hidden,
      list: _load,
    );
    if (!mounted) return;
    final hiddenPersisted = _hidden ? const <String>[] : persisted.where(isHiddenExplorerPath);
    _setExpanded(reconcileRestoredExpandedPaths(
      persisted: persisted,
      current: {'.', ..._expanded},
      restored: [...restored, ...hiddenPersisted],
    ));
  }

  void _setExpanded(Iterable<String> paths) {
    setState(() {
      _expanded
        ..clear()
        ..addAll(paths.where((path) => path != '.'));
      _flatten();
    });
    final preferences = widget.preferences;
    if (preferences == null) return;
    preferences.values['expandedPathsByWorkspace'] = {
      ...jsonMap(preferences.values['expandedPathsByWorkspace']),
      preferences.workspaceKey: ['.', ..._expanded],
    };
    unawaited(preferences.save().catchError((Object _) {}));
  }

  void _savePreference(String key, Object value) {
    final preferences = widget.preferences;
    if (preferences == null) return;
    preferences.values[key] = value;
    unawaited(preferences.save().catchError((Object _) {}));
  }

  @override
  void didUpdateWidget(RemoteDirectoryBrowser oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.client != widget.client || oldWidget.cwd != widget.cwd) {
      _generation++;
      _directories.clear();
      _expanded.clear();
      _loading.clear();
      _inflight.clear();
      _errors.clear();
      _history.clear();
      _historyIndex = -1;
      navigate(widget.initialPath);
    } else if (oldWidget.initialPath != widget.initialPath &&
        _path != widget.initialPath) {
      navigate(widget.initialPath);
    }
  }

  @override
  void dispose() {
    _generation++;
    _pathInput.dispose();
    super.dispose();
  }

  void _flatten() {
    final rows = <(JsonMap, int)>[];
    void visit(String path, int depth, Set<String> ancestors) {
      if (ancestors.contains(path)) return;
      final next = {...ancestors, path};
      for (final entry in _directories[path] ?? const <JsonMap>[]) {
        if (widget.directoriesOnly && entry.text('kind') != 'directory') {
          continue;
        }
        if (!_hidden && entry.text('name').startsWith('.')) continue;
        rows.add((entry, depth));
        if (_expanded.contains(entry.text('path'))) {
          visit(entry.text('path'), depth + 1, next);
        }
      }
    }

    visit(_path, 0, {});
    _rows = rows;
  }

  /// Original `sortExplorerEntries`: directories first, then name A→Z, newest first or
  /// largest first.
  int _compare(JsonMap a, JsonMap b) {
    final ad = a.text('kind') == 'directory',
        bd = b.text('kind') == 'directory';
    if (ad != bd) return ad ? -1 : 1;
    final result = switch (_sort) {
      'size' => b.integer('size').compareTo(a.integer('size')),
      'modified' => b.text('modifiedAt').compareTo(a.text('modifiedAt')),
      _ => a.text('name').toLowerCase().compareTo(b.text('name').toLowerCase()),
    };
    return result == 0 ? a.text('name').compareTo(b.text('name')) : result;
  }

  /// Original `handleSortCycle`: the sort label cycles Name → Modified → Size.
  void _cycleSort() {
    const sorts = ['name', 'modified', 'size'];
    setState(() {
      _sort = sorts[(sorts.indexOf(_sort) + 1) % sorts.length];
      for (final entries in _directories.values) {
        entries.sort(_compare);
      }
      _flatten();
    });
    _savePreference('explorerSortOption', _sort);
  }

  /// Original `handleToggleHiddenFiles`: showing hidden files again lists the
  /// expanded hidden directories the tree now shows.
  Future<void> _toggleHidden() async {
    final willShow = !_hidden;
    setState(() {
      _hidden = willShow;
      _flatten();
    });
    _savePreference('explorerShowHiddenFiles', willShow);
    final root = _directories[_path];
    if (!willShow || root == null || widget.preferences == null) return;
    final persisted = {'.', ..._expanded};
    final restored = await restoreExpandedDirectories(
      rootEntries: root,
      persisted: persisted,
      showHiddenFiles: true,
      list: _load,
    );
    if (!mounted) return;
    _setExpanded(reconcileRestoredExpandedPaths(
      persisted: persisted,
      current: {'.', ..._expanded},
      restored: restored,
    ));
  }

  /// Lists [path]; concurrent requests for one path share the listing.
  /// Returns null when it failed.
  Future<List<JsonMap>?> _load(String path) =>
      _inflight[path] ??= _list(path).whenComplete(() => _inflight.remove(path));

  Future<List<JsonMap>?> _list(String path) async {
    final generation = _generation;
    setState(() {
      _loading.add(path);
      _errors.remove(path);
    });
    try {
      final result = await widget.client.request('file_explorer_request', {
        'cwd': widget.cwd,
        'path': path,
        'mode': 'list',
      }, responseType: 'file_explorer_response');
      if (result['error'] != null) throw StateError(result.text('error'));
      if (result['directory'] == null) {
        throw StateError('Directory listing unavailable');
      }
      final entries = result
          .object('directory')
          .array('entries')
          .map(jsonMap)
          .toList();
      entries.sort(_compare);
      if (!mounted || generation != _generation) return null;
      setState(() {
        _directories[path] = entries;
        _flatten();
      });
      return entries;
    } catch (error) {
      if (mounted && generation == _generation) {
        setState(() {
          _errors[path] = error;
        });
      }
      return null;
    } finally {
      if (mounted && generation == _generation) {
        setState(() {
          _loading.remove(path);
        });
      }
    }
  }

  Future<List<JsonMap>?> navigate(String path, {bool recordHistory = true}) async {
    if (path.trim().isEmpty) return null;
    setState(() {
      _path = path;
      _pathInput.text = path;
      _selected = null;
      if (recordHistory &&
          (_historyIndex < 0 || _history[_historyIndex] != path)) {
        _history.removeRange(_historyIndex + 1, _history.length);
        _history.add(path);
        _historyIndex = _history.length - 1;
      }
      _flatten();
    });
    widget.onDirectoryChanged?.call(path);
    return _load(path);
  }

  /// Original `refreshExplorerDirectories`: re-lists the root and every
  /// expanded directory the tree shows.
  Future<void> refresh() async {
    setState(() => _refreshing = true);
    try {
      await Future.wait([
        _load(_path),
        for (final path in _expanded)
          if (_hidden || !isHiddenExplorerPath(path)) _load(path),
      ]);
    } finally {
      if (mounted) setState(() => _refreshing = false);
    }
  }

  /// Re-lists one directory (after an entry changed in it).
  Future<void> reload(String path) async => _load(path);

  /// Original `toggleDirectory`: lists a directory the first time it expands.
  Future<void> _toggle(JsonMap entry) async {
    final path = entry.text('path');
    setState(() => _selected = path);
    final expand = !_expanded.contains(path);
    _setExpanded(expand ? {..._expanded, path} : ({..._expanded}..remove(path)));
    if (expand && !_directories.containsKey(path)) await _load(path);
  }

  bool isExpanded(String path) => _expanded.contains(path);

  /// Original "Collapse folder" file action: collapses the folder and every
  /// folder inside it.
  void collapse(String path) {
    _setExpanded(_expanded.where((expanded) => !isExplorerPathWithin(expanded, path)));
  }

  /// Original `handleNewEntry`: a new entry's parent folder expands (and lists
  /// once).
  Future<void> expandForCreate(String parentPath) async {
    if (parentPath == '.' || parentPath == _path) return;
    _setExpanded({..._expanded, parentPath});
    if (!_directories.containsKey(parentPath)) await _load(parentPath);
  }

  /// Original `handleRenameCommit`: a renamed folder's expanded paths follow
  /// it, and re-list.
  Future<void> renamed(String from, String to) async {
    String remap(String path) =>
        isExplorerPathWithin(path, from) ? '$to${path.substring(from.length)}' : path;
    final moved = _expanded.where((path) => isExplorerPathWithin(path, from)).map(remap).toList();
    _setExpanded(_expanded.map(remap));
    await Future.wait(moved.map(_load));
  }

  /// Original `revealExplorerDirectory`: lists and expands the path to a
  /// directory selected outside the tree, showing hidden files for a hidden one.
  Future<void> reveal(String directoryPath) async {
    if (isHiddenExplorerPath(directoryPath) && !_hidden) {
      setState(() {
        _hidden = true;
        _flatten();
      });
      _savePreference('explorerShowHiddenFiles', true);
    }
    final paths = ['.'];
    if (directoryPath != '.') {
      final segments = directoryPath.split('/').where((segment) => segment.isNotEmpty).toList();
      for (var length = 1; length <= segments.length; length += 1) {
        paths.add(segments.take(length).join('/'));
      }
    }
    for (final path in paths) {
      if (await _load(path) == null) return;
    }
    if (!mounted) return;
    _setExpanded({..._expanded, ...paths});
  }

  Future<void> _navigationMenu(BuildContext anchor) async {
    final action = await showPaseoMenu<String>(
      anchor,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      width: 200,
      entries: [
        PaseoMenuEntry.item(
          value: 'back',
          label: 'Back',
          icon: LucideIcons.arrowLeft300,
          enabled: _historyIndex > 0,
        ),
        PaseoMenuEntry.item(
          value: 'forward',
          label: 'Forward',
          icon: LucideIcons.arrowRight300,
          enabled: _historyIndex + 1 < _history.length,
        ),
        const PaseoMenuEntry.item(
          value: 'parent',
          label: 'Parent directory',
          icon: LucideIcons.cornerLeftUp300,
        ),
        const PaseoMenuEntry.separator(),
        const PaseoMenuEntry.item(
          value: 'location',
          label: 'Go to directory…',
          icon: LucideIcons.folderSearch300,
        ),
      ],
    );
    if (!mounted) return;
    switch (action) {
      case 'back':
        navigate(_history[--_historyIndex], recordHistory: false);
      case 'forward':
        navigate(_history[++_historyIndex], recordHistory: false);
      case 'parent':
        navigate(remoteParent(remoteAbsolute(widget.cwd, _path)));
      case 'location':
        setState(() => _showLocation = !_showLocation);
    }
  }

  /// Original explorer error state: destructive base text over outline pill actions.
  Widget _errorState(BuildContext context, Object error) {
    final theme = Theme.of(context);
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(mainAxisSize: MainAxisSize.min, spacing: 8, children: [
          Text(
            errorMessage(error),
            textAlign: TextAlign.center,
            style: theme.textTheme.bodyMedium!.copyWith(color: theme.colorScheme.error),
          ),
          Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: [
            if (_historyIndex > 0)
              _PillButton(
                label: tr(context, 'workspace.fileExplorer.actions.back'),
                onPressed: () => navigate(_history[--_historyIndex], recordHistory: false),
              ),
            _PillButton(
              label: tr(context, 'workspace.fileExplorer.actions.retry'),
              onPressed: () => _load(_path),
            ),
          ]),
        ]),
      ),
    );
  }

  Widget _centerState(BuildContext context, List<Widget> children) => Center(
    child: Padding(
      padding: const EdgeInsets.all(16),
      child: Column(mainAxisSize: MainAxisSize.min, spacing: 8, children: children),
    ),
  );

  Widget _row(BuildContext context, JsonMap entry, int depth) {
    final directory = entry.text('kind') == 'directory', path = entry.text('path');
    final menu = widget.onEntryMenu;
    return Builder(
      builder: (rowContext) => FileTreeRow(
        depth: depth,
        selected: _selected == path,
        name: entry.text('name'),
        tooltip: '${entry.text('path')}\n${entry.integer('size')} bytes · ${entry.text('modifiedAt')}${_errors[path] == null ? '' : '\n${_errors[path]}'}',
        icon: _loading.contains(path)
            ? const TreeLoadingIcon()
            : directory
            ? TreeChevron(expanded: _expanded.contains(path))
            : MaterialFileIcon(fileName: entry.text('name')),
        trailing: widget.entryTrailing?.call(context, entry),
        onTap: () {
          if (directory) {
            _toggle(entry);
          } else {
            setState(() => _selected = path);
            widget.onFileSelected?.call(entry);
          }
        },
        onLongPress: menu != null
            ? () {
                setState(() => _selected = path);
                menu(rowContext, entry, null);
              }
            : directory && widget.onDirectorySelected != null
            ? () => widget.onDirectorySelected!(remoteAbsolute(widget.cwd, path))
            : null,
        onSecondaryTapDown: menu == null
            ? null
            : (position) {
                setState(() => _selected = path);
                menu(rowContext, entry, position);
              },
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final muted = theme.textTheme.bodyMedium!.copyWith(color: colors.muted);
    final error = _errors[_path];
    final initialLoading =
        _rows.isEmpty && _loading.contains(_path) && !_directories.containsKey(_path);
    final background = widget.onBackgroundMenu;
    Widget tree;
    if (error != null && !_directories.containsKey(_path)) {
      tree = _errorState(context, error);
    } else if (initialLoading) {
      tree = _centerState(context, [
        SizedBox.square(
          dimension: 20,
          child: CircularProgressIndicator(strokeWidth: 2, color: colors.muted),
        ),
        Text(tr(context, 'workspace.fileExplorer.states.loading'), style: muted),
      ]);
    } else if (_rows.isEmpty) {
      tree = _centerState(context, [
        Text(
          tr(context, _hidden
              ? 'workspace.fileExplorer.empty.noFiles'
              : 'workspace.fileExplorer.empty.noVisibleFiles'),
          textAlign: TextAlign.center,
          style: muted,
        ),
      ]);
    } else {
      tree = ListView.builder(
        itemCount: _rows.length,
        padding: const EdgeInsets.only(top: 8, bottom: 16),
        prototypeItem: FileTreeRow(
          name: 'M',
          icon: const SizedBox.shrink(),
          onTap: () {},
        ),
        itemBuilder: (context, index) {
          final (entry, depth) = _rows[index];
          return _row(context, entry, depth);
        },
      );
    }
    if (background != null) {
      tree = GestureDetector(
        behavior: HitTestBehavior.translucent,
        onLongPress: () => background(null),
        onSecondaryTapDown: (details) => background(details.globalPosition),
        child: tree,
      );
    }
    return Column(
      children: [
        FilePaneToolbar(
          sidebar: true,
          padding: EdgeInsets.only(right: paneToolbarTrailingPadding(context)),
          children: [
            _SortTrigger(
              label: tr(context, 'workspace.fileExplorer.sort.$_sort'),
              onPressed: _cycleSort,
            ),
            const Spacer(),
            PaneToolbarControls(children: [
              ...widget.toolbarActions,
              PaneToolbarButton(
                tooltip: tr(context, _hidden
                    ? 'workspace.fileExplorer.actions.hideHiddenFiles'
                    : 'workspace.fileExplorer.actions.showHiddenFiles'),
                icon: _hidden ? LucideIcons.eye300 : LucideIcons.eyeOff300,
                selected: !_hidden,
                onPressed: _toggleHidden,
              ),
              PaneToolbarButton(
                tooltip: tr(context, _refreshing
                    ? 'workspace.fileExplorer.actions.refreshing'
                    : 'workspace.fileExplorer.actions.refresh'),
                onPressed: _refreshing ? null : refresh,
                child: _refreshing
                    ? SizedBox.square(
                        dimension: paneToolbarIconSize(context),
                        child: CircularProgressIndicator(
                          strokeWidth: 1.5,
                          color: colors.extraMuted,
                        ),
                      )
                    : const Icon(LucideIcons.rotateCw300),
              ),
              if (widget.navigationMenu)
                Builder(
                  builder: (anchor) => PaneToolbarButton(
                    tooltip: 'Directory navigation',
                    icon: LucideIcons.ellipsis300,
                    onPressed: () => _navigationMenu(anchor),
                  ),
                ),
            ]),
          ],
        ),
        if (_showLocation || widget.directoriesOnly)
          Padding(
            padding: const EdgeInsets.fromLTRB(8, 8, 8, 0),
            child: TextField(
              controller: _pathInput,
              style: theme.textTheme.bodySmall,
              decoration: paseoInputDecoration(
                context,
                hintText: 'Remote directory',
                size: PaseoControlSize.sm,
              ),
              onSubmitted: navigate,
            ),
          ),
        if (widget.onDirectorySelected != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(8, 8, 8, 0),
            child: Align(
              alignment: Alignment.centerLeft,
              child: PaseoButton(
                icon: LucideIcons.check300,
                label: 'Use this directory',
                variant: PaseoButtonVariant.outline,
                size: PaseoControlSize.sm,
                onPressed: () => widget.onDirectorySelected!(remoteAbsolute(widget.cwd, _path)),
              ),
            ),
          ),
        if (error != null && _directories.containsKey(_path))
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: Text(
              errorMessage(error),
              style: theme.textTheme.bodySmall!.copyWith(color: theme.colorScheme.error),
            ),
          ),
        Expanded(child: tree),
      ],
    );
  }
}

/// Original explorer sort trigger: a 24px chip 8px in from the toolbar's edge, 4px
/// padding and gap, 4px radius, small muted label plus a 12px chevron, surfaceSidebarHover
/// while hovered or pressed.
class _SortTrigger extends StatefulWidget {
  const _SortTrigger({required this.label, required this.onPressed});
  final String label;
  final VoidCallback onPressed;

  @override
  State<_SortTrigger> createState() => _SortTriggerState();
}

class _SortTriggerState extends State<_SortTrigger> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Semantics(
      button: true,
      label: widget.label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => _pressed = true),
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: widget.onPressed,
          child: Container(
            height: 24,
            margin: const EdgeInsets.only(left: 8),
            padding: const EdgeInsets.symmetric(horizontal: 4),
            decoration: BoxDecoration(
              color: _hovered || _pressed ? sidebarHoverColor(context) : null,
              borderRadius: BorderRadius.circular(4),
            ),
            child: Row(mainAxisSize: MainAxisSize.min, spacing: 4, children: [
              Text(
                widget.label,
                style: Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.muted),
              ),
              Icon(LucideIcons.chevronDown300, size: 12, color: colors.muted),
            ]),
          ),
        ),
      ),
    );
  }
}

/// Original explorer `retryButton`: a fully rounded 1px border pill, 12/4 padding, base
/// semibold muted label.
class _PillButton extends StatelessWidget {
  const _PillButton({required this.label, required this.onPressed});
  final String label;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Semantics(
      button: true,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: onPressed,
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
          decoration: BoxDecoration(
            border: Border.all(color: theme.dividerColor),
            borderRadius: BorderRadius.circular(999),
          ),
          child: Text(
            label,
            style: theme.textTheme.bodyMedium!.copyWith(
              color: PaseoColors.of(context).muted,
              fontWeight: FontWeight.w600,
            ),
          ),
        ),
      ),
    );
  }
}
