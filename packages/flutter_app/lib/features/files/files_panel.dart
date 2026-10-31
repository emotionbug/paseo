import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/chat_drafts.dart';
import 'native_file_actions.dart';
import 'pane_chrome.dart';
import 'remote_directory_browser.dart';

class FilesPanel extends StatefulWidget {
  const FilesPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<FilesPanel> createState() => _FilesPanelState();
}

class _FilesPanelState extends State<FilesPanel> {
  final _browser = GlobalKey<RemoteDirectoryBrowserState>();
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _revealTargetPath());
  }

  @override
  void didUpdateWidget(FilesPanel oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.target.data['path'] != oldWidget.target.data['path']) _revealTargetPath();
  }

  /// An assistant folder link (path-open.ts `openDirectory`) reveals its
  /// folder in the workspace tree (original `revealExplorerDirectory`).
  void _revealTargetPath() {
    final path = widget.target.data['path'];
    if (path is String && path.isNotEmpty && path != '.') {
      _browser.currentState?.reveal(path);
    }
  }

  JsonMap get _features =>
      PaseoScope.of(context).clientFor(widget.target.serverId).serverInfo.object('features');

  // COMPAT(fsEntryOps): create, rename and delete need a host that has them.
  bool get _entryOps => _features.flag('fsEntryOps');

  // COMPAT(fsEntryDuplicate)
  bool get _entryDuplicate => _features.flag('fsEntryDuplicate');

  void _open(String path, {bool? beside}) {
    final store = PaseoScope.of(context);
    store.open(
      PanelTarget(
        kind: 'file',
        serverId: widget.target.serverId,
        workspaceId: widget.target.workspaceId,
        cwd: widget.target.cwd,
        id: path,
        title: remoteBasename(path, cwd: widget.target.cwd),
        data: {'path': path},
      ),
      beside:
          beside ??
          jsonMap(store.preferences['openInSidePane']).flag('explorerFiles'),
    );
  }

  Future<void> _run(Future<void> Function() action) async {
    if (_busy) return;
    setState(() {
      _busy = true;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// Original `handleNewEntry` + `handleDraftCommit`: the parent folder
  /// expands, the new entry's parent re-lists, and a new file opens.
  Future<void> _create(String parent, String kind) => _run(() async {
    await _browser.currentState?.expandForCreate(parent);
    if (!mounted) return;
    final name = (await promptText(
      context,
      title: tr(
        context,
        kind == 'directory'
            ? 'workspace.fileActions.newFolder'
            : 'workspace.fileActions.newFile',
      ),
      hint: tr(
        context,
        kind == 'directory'
            ? 'workspace.fileExplorer.draft.folderPlaceholder'
            : 'workspace.fileExplorer.draft.filePlaceholder',
      ),
    ))?.trim();
    if (name == null || name.isEmpty || !mounted) return;
    final result = await _entryRequest('create', {
      'parentPath': parent,
      'name': name,
      'kind': kind,
    }, 'workspace.fileExplorer.errors.createFailed');
    await _browser.currentState?.reload(parent);
    final path = result['path'];
    if (mounted && kind == 'file' && path is String && path.isNotEmpty) _open(path);
  });

  /// One `fs.entry.<action>` request; an unsuccessful payload is its error or
  /// the original fallback message.
  Future<JsonMap> _entryRequest(String action, JsonMap fields, String failedKey) async {
    final failed = tr(context, failedKey);
    final result = await PaseoScope.of(context)
        .clientFor(widget.target.serverId)
        .request('fs.entry.$action.request', {
          'cwd': widget.target.cwd,
          ...fields,
        }, responseType: 'fs.entry.$action.response');
    if (result['success'] != true) {
      throw StateError(result['error'] is String ? result.text('error') : failed);
    }
    return result;
  }

  Future<void> _action(String action, JsonMap entry) async {
    final path = entry.text('path');
    if (action == 'open') {
      _open(path, beside: true);
      return;
    }
    if (action == 'collapse') {
      _browser.currentState?.collapse(path);
      return;
    }
    if (action == 'absolute' || action == 'relative') {
      await Clipboard.setData(
        ClipboardData(
          text: action == 'absolute'
              ? remoteAbsolute(widget.target.cwd, path)
              : remoteRelative(widget.target.cwd, path),
        ),
      );
      return;
    }
    if (action == 'chat') {
      ChatDrafts.instance.addAttachments(
        widget.target.serverId,
        widget.target.workspaceId,
        [
          {
            'kind': 'workspace_file',
            'path': remoteRelative(widget.target.cwd, path),
            'selection': {'kind': 'whole_file'},
          },
        ],
      );
      if (mounted) showToast(context, 'Added to chat draft');
      return;
    }
    if (action == 'new_file' || action == 'new_directory') {
      await _create(path, action == 'new_file' ? 'file' : 'directory');
      return;
    }
    if (action == 'download' || action == 'share') {
      await saveRemoteFile(
        context,
        client: PaseoScope.of(context).clientFor(widget.target.serverId),
        cwd: widget.target.cwd,
        path: path,
        share: action == 'share',
      );
      return;
    }
    await _run(() async {
      final parent = parentExplorerPath(path);
      if (action == 'rename') {
        final name = (await promptText(
          context,
          title: 'Rename ${entry.text('name')}',
          initialValue: entry.text('name'),
        ))?.trim();
        if (name == null || name.isEmpty || name == entry.text('name') || !mounted) {
          return;
        }
        // Original `handleRenameCommit`: open tabs keep their old path.
        final result = await _entryRequest(
          'rename',
          {'path': path, 'name': name},
          'workspace.fileExplorer.errors.renameFailed',
        );
        final renamed = result['renamedPath'];
        if (renamed is! String || renamed.isEmpty) {
          if (mounted) throw StateError(tr(context, 'workspace.fileExplorer.errors.renameFailed'));
          return;
        }
        await _browser.currentState?.reload(parent);
        if (entry.text('kind') == 'directory') {
          await _browser.currentState?.renamed(path, renamed);
        } else if (mounted) {
          _open(renamed);
        }
        return;
      }
      if (action == 'delete') {
        if (!await confirmAction(
              context,
              title: tr(context, entry.text('kind') == 'directory'
                  ? 'workspace.fileActions.confirmDelete.folderTitle'
                  : 'workspace.fileActions.confirmDelete.fileTitle'),
              message: tr(
                context,
                'workspace.fileActions.confirmDelete.message',
                args: {'name': entry.text('name')},
              ),
              confirmLabel: tr(context, 'workspace.fileActions.confirmDelete.confirm'),
              destructive: true,
            ) ||
            !mounted) {
          return;
        }
        await _entryRequest('delete', {'path': path}, 'workspace.fileExplorer.errors.deleteFailed');
        await _browser.currentState?.reload(parent);
        return;
      }
      if (action == 'duplicate') {
        final result = await _entryRequest(
          'duplicate',
          {'path': path},
          'workspace.fileExplorer.errors.duplicateFailed',
        );
        if (result['duplicatedPath'] is! String && mounted) {
          throw StateError(tr(context, 'workspace.fileExplorer.errors.duplicateFailed'));
        }
        await _browser.currentState?.reload(parent);
      }
    });
  }

  /// Original `FileActionsContextMenuContent` (components/file-actions-menu.tsx): grouped
  /// create / open / reference / manage / destructive rows with 14px muted (destructive red)
  /// glyphs in a 220px menu, a separator between groups.
  Future<void> _entryMenu(BuildContext rowContext, JsonMap entry, Offset? position) async {
    if (_busy) return;
    final file = entry.text('kind') == 'file';
    final expanded = !file && (_browser.currentState?.isExpanded(entry.text('path')) ?? false);
    final colors = PaseoColors.of(context);
    final destructive = Theme.of(context).colorScheme.error;
    Widget glyph(IconData icon, {bool danger = false}) =>
        Icon(icon, size: 14, color: danger ? destructive : colors.muted);
    final groups = <List<PaseoMenuEntry<String>>>[
      if (!file && _entryOps)
        [
          PaseoMenuEntry.item(
            value: 'new_file',
            label: tr(context, 'workspace.fileActions.newFile'),
            leading: glyph(LucideIcons.filePlus300),
          ),
          PaseoMenuEntry.item(
            value: 'new_directory',
            label: tr(context, 'workspace.fileActions.newFolder'),
            leading: glyph(LucideIcons.folderPlus300),
          ),
        ],
      if (expanded)
        [
          PaseoMenuEntry.item(
            value: 'collapse',
            label: tr(context, 'workspace.fileActions.collapseFolder'),
            leading: glyph(LucideIcons.folderMinus300),
          ),
        ],
      if (file)
        [
          PaseoMenuEntry.item(
            value: 'open',
            label: tr(context, 'workspace.fileActions.openToSide'),
            leading: glyph(LucideIcons.arrowRightToLine300),
          ),
        ],
      [
        PaseoMenuEntry.item(
          value: 'absolute',
          label: tr(context, 'workspace.fileActions.copyPath'),
          leading: glyph(LucideIcons.copy300),
        ),
        PaseoMenuEntry.item(
          value: 'relative',
          label: tr(context, 'workspace.fileActions.copyRelativePath'),
          leading: glyph(LucideIcons.copy300),
        ),
        if (file) ...[
          PaseoMenuEntry.item(
            value: 'download',
            label: tr(context, 'workspace.fileActions.download'),
            leading: glyph(LucideIcons.download300),
          ),
          PaseoMenuEntry.item(
            value: 'share',
            label: tr(context, 'downloads.shareFile'),
            leading: glyph(LucideIcons.share2300),
          ),
        ],
        PaseoMenuEntry.item(
          value: 'chat',
          label: tr(context, 'workspace.fileActions.addToChat'),
          leading: glyph(LucideIcons.messageSquarePlus300),
        ),
      ],
      if (_entryOps || _entryDuplicate)
        [
          if (_entryOps)
            PaseoMenuEntry.item(
              value: 'rename',
              label: tr(context, 'workspace.fileActions.rename'),
              leading: glyph(LucideIcons.pencil300),
            ),
          if (_entryDuplicate)
            PaseoMenuEntry.item(
              value: 'duplicate',
              label: tr(context, 'workspace.fileActions.duplicate'),
              leading: glyph(LucideIcons.copyPlus300),
            ),
        ],
      if (_entryOps)
        [
          PaseoMenuEntry.item(
            value: 'delete',
            label: tr(context, 'workspace.fileActions.delete'),
            leading: glyph(LucideIcons.trash2300, danger: true),
            destructive: true,
          ),
        ],
    ];
    final action = await _showMenu(rowContext, position, [
      for (final (index, group) in groups.indexed) ...[
        if (index > 0) const PaseoMenuEntry.separator(),
        ...group,
      ],
    ]);
    if (action != null && mounted) await _action(action, entry);
  }

  /// Original `RootCreationContextTarget` menu: new file / new folder at the root.
  Future<void> _backgroundMenu(Offset? position) async {
    if (_busy || !_entryOps) return;
    final colors = PaseoColors.of(context);
    final action = await _showMenu(context, position, [
      PaseoMenuEntry.item(
        value: 'file',
        label: tr(context, 'workspace.fileActions.newFile'),
        leading: Icon(LucideIcons.filePlus300, size: 14, color: colors.muted),
      ),
      PaseoMenuEntry.item(
        value: 'directory',
        label: tr(context, 'workspace.fileActions.newFolder'),
        leading: Icon(LucideIcons.folderPlus300, size: 14, color: colors.muted),
      ),
    ]);
    if (action != null && mounted) await _create('.', action);
  }

  /// A context menu opens at the pointer on desktop and under the row otherwise.
  Future<String?> _showMenu(
    BuildContext anchor,
    Offset? position,
    List<PaseoMenuEntry<String>> entries,
  ) => showPaseoMenu<String>(
    anchor,
    entries: entries,
    anchor: position != null
        ? Rect.fromLTWH(position.dx, position.dy, 0, 0)
        : paseoAnchorRect(anchor),
  );

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final store = PaseoScope.of(context);
    return ColoredBox(
      // The explorer lives on the original surfaceSidebar.
      color: colors.sidebar,
      child: Column(
        children: [
          Expanded(
            child: RemoteDirectoryBrowser(
              key: _browser,
              client: PaseoScope.of(context).clientFor(widget.target.serverId),
              cwd: widget.target.cwd,
              preferences: ExplorerPreferences(
                values: store.preferences.values,
                save: store.savePreferences,
                workspaceKey: widget.target.workspaceId.trim().isNotEmpty
                    ? 'workspace:${widget.target.workspaceId.trim()}'
                    : 'root:${widget.target.cwd.trim()}',
              ),
              toolbarActions: [
                if (_entryOps) ...[
                  PaneToolbarButton(
                    tooltip: tr(context, 'workspace.fileActions.newFile'),
                    icon: LucideIcons.filePlus300,
                    onPressed: _busy ? null : () => _create('.', 'file'),
                  ),
                  PaneToolbarButton(
                    tooltip: tr(context, 'workspace.fileActions.newFolder'),
                    icon: LucideIcons.folderPlus300,
                    onPressed: _busy ? null : () => _create('.', 'directory'),
                  ),
                ],
              ],
              navigationMenu: false,
              onFileSelected: (entry) => _open(entry.text('path')),
              onEntryMenu: _entryMenu,
              onBackgroundMenu: _backgroundMenu,
            ),
          ),
          if (_busy)
            LinearProgressIndicator(
              minHeight: 2,
              color: colors.accent,
              backgroundColor: Colors.transparent,
            ),
        ],
      ),
    );
  }
}
