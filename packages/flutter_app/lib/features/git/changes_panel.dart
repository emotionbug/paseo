import 'dart:async';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:uuid/uuid.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../files/file_icons.dart';
import '../files/pane_chrome.dart';
import 'diff_view.dart';
import 'git_actions.dart';
import 'git_controller.dart';
import 'review_panel.dart';

/// Original Changes presentations: the explorer `tree`, the `combined` working
/// diff pane (document plus a trailing tree rail) and the bare commit `diff`.
enum _Presentation { tree, combined, diff }

class ChangesPanel extends StatefulWidget {
  const ChangesPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<ChangesPanel> createState() => _ChangesPanelState();
}

class _ChangesPanelState extends State<ChangesPanel> {
  GitController? _git;
  CheckoutGitState? _checkout;
  CheckoutGitActions? _actions;
  ReviewDraft? _review;
  late AppStore _store;
  final Set<String> _collapsedFolders = {};
  final Set<String> _collapsedFiles = {};
  final _document = GlobalKey<DiffDocumentState>();
  String? _selectedTreePath;
  // Original `defaultChangesState`: the desktop tree rail starts hidden and the
  // commits footer collapsed.
  bool _treeVisible = false;
  bool _commitsCollapsed = true;
  bool _commitsLoading = false;
  Object? _commitsError;
  bool _initialRevealed = false;
  GitController get git => _git!;
  String get _serverId => widget.target.serverId;
  String get _cwd => widget.target.cwd;

  /// Original `useChangesPreferences`: `DEFAULT_CHANGES_PREFERENCES` under the
  /// persisted `@paseo:changes-preferences` values, shared by every Changes view.
  JsonMap get _preferences => {
    'layout': 'unified',
    'wrapLines': false,
    'hideWhitespace': false,
    'inlineDiff': false,
    ...jsonMap(_store.preferences['changesPreferences']),
  };
  bool get _wrap => _preferences['wrapLines'] == true;
  bool get _hideWhitespace => _preferences['hideWhitespace'] == true;
  bool get _inlineDiff => _preferences['inlineDiff'] == true;

  /// Original `canUseSplitLayout` (web/desktop, not compact) with
  /// `resolveDiffLayout`: split only where it can be used.
  bool get _canUseSplitLayout => !isCompactLayout(context) && !_native;
  bool get _split => _canUseSplitLayout && _preferences['layout'] == 'split';

  Future<void> _updatePreferences(JsonMap updates) async {
    _store.preferences['changesPreferences'] = {..._preferences, ...updates};
    await _store.savePreferences();
  }

  /// Original `resolveChangesPresentation` (panels/changes/presentation.ts): the
  /// Explorer's Changes view is the bare tree only on wide web/desktop layouts
  /// without the inline-diff preference; compact and native (Android/iOS)
  /// layouts render it `combined`, each changed file's diff inline.
  _Presentation get _presentation => switch (widget.target.kind) {
    'changes_tree' => isCompactLayout(context) || _native || _inlineDiff
        ? _Presentation.combined
        : _Presentation.tree,
    'commit_diff' => _Presentation.diff,
    _ => _Presentation.combined,
  };

  static bool get _native =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _store = PaseoScope.of(context);
    if (_git != null) {
      // The shared whitespace preference keys the diff request.
      if (!git.isCommit && git.ignoreWhitespace != _hideWhitespace) {
        git.ignoreWhitespace = _hideWhitespace;
        unawaited(git.refresh());
      }
      return;
    }
    final client = _store.clientFor(_serverId);
    _git = GitController(client, widget.target)
      ..ignoreWhitespace = _hideWhitespace
      ..addListener(_changed);
    if (!git.isCommit) {
      final state = CheckoutGitState.acquire(client, _serverId, _cwd)..addListener(_checkoutChanged);
      _checkout = state;
      _actions = CheckoutGitActions(
        store: _store,
        serverId: _serverId,
        workspaceId: widget.target.workspaceId,
        cwd: _cwd,
        state: state,
      );
      CheckoutGitActionsStore.instance.addListener(_checkoutChanged);
      _bindReview();
    }
    unawaited(git.refresh());
  }

  void _changed() {
    if (!mounted) return;
    _bindReview();
    _ensureCommentPatches();
    setState(() {});
  }

  void _checkoutChanged() {
    if (mounted) setState(() {});
  }

  /// Original `buildReviewDraftKey`: the draft follows the comparison, base ref
  /// and whitespace mode.
  void _bindReview() {
    if (git.isCommit) return;
    final key = buildReviewDraftKey(
      serverId: _serverId,
      workspaceId: widget.target.workspaceId,
      cwd: _cwd,
      mode: git.mode,
      baseRef: git.baseRef,
      ignoreWhitespace: git.ignoreWhitespace,
    );
    if (_review?.key == key) return;
    _review?.removeListener(_reviewChanged);
    _review?.release();
    _review = ReviewDraft.acquire(key)..addListener(_reviewChanged);
  }

  void _reviewChanged() {
    if (!mounted) return;
    _ensureCommentPatches();
    setState(() {});
  }

  /// The feedback attachment resolves comments against the current patches, so
  /// the files that carry comments load theirs.
  void _ensureCommentPatches() {
    final review = _review;
    if (review == null || git.tooLarge) return;
    for (final comment in review.comments) {
      final path = comment.text('filePath');
      if (git.patches.containsKey(path) ||
          git.loadingPatches.contains(path) ||
          git.patchErrors.containsKey(path) ||
          !git.files.any((file) => file.text('path') == path)) {
        continue;
      }
      scheduleMicrotask(() {
        if (mounted) unawaited(git.ensurePatch(path));
      });
    }
  }

  ReviewAttachmentSnapshot? get _reviewAttachment {
    final review = _review;
    if (review == null) return null;
    return buildReviewAttachmentSnapshot(
      cwd: _cwd,
      mode: git.mode,
      baseRef: git.baseRef,
      comments: review.comments,
      diffFile: (path) => git.patches[path],
    );
  }

  /// Original `readRecipients`: the agent tabs open in this workspace.
  List<FeedbackRecipient> get _feedbackRecipients {
    final agents = _store.hostFor(_serverId)?.agents ?? const <JsonMap>[];
    String name(String id) {
      for (final row in agents) {
        final agent = row['agent'] is Map ? jsonMap(row['agent']) : row;
        if (agent.text('id') == id) return agent.text('title', agent.text('name'));
      }
      return '';
    }

    return [
      for (final tab in _store.tabs)
        if (tab.kind == 'agent' &&
            tab.serverId == _serverId &&
            tab.workspaceId == widget.target.workspaceId &&
            (tab.id ?? '').isNotEmpty)
          FeedbackRecipient(
            tab.id!,
            [name(tab.id!), tab.title ?? '', tr(context, 'sessionNavigation.untitled')]
                .firstWhere((label) => label.trim().isNotEmpty),
          ),
    ];
  }

  /// Original review feedback `send`: the review prompt with the attachment,
  /// following the send-behavior preference for a running turn.
  Future<void> _sendFeedback(FeedbackRecipient recipient, JsonMap attachment) async {
    final client = git.client;
    if (client.status != ConnectionStatus.connected) {
      throw StateError(tr(context, 'workspace.terminal.hostDisconnected'));
    }
    final sendBehavior = stringValue(_store.preferences['sendBehavior'], 'steer');
    final failed = tr(context, 'review.feedback.failed');
    final response = await client.request(
      'send_agent_message_request',
      {
        'agentId': recipient.agentId,
        'text': tr(context, 'review.feedback.prompt'),
        'messageId': const Uuid().v4(),
        'activeTurnBehavior': sendBehavior == 'steer' ? 'steer' : 'interrupt',
        'images': const [],
        'attachments': [attachment],
      },
      responseType: 'send_agent_message_response',
    );
    final error = response['error'];
    if (error != null || response['accepted'] == false || response['success'] == false) {
      throw StateError(
        error is Map ? stringValue(error['message'], failed) : stringValue(error, failed),
      );
    }
  }

  @override
  void dispose() {
    _git?.removeListener(_changed);
    _git?.dispose();
    _checkout?.removeListener(_checkoutChanged);
    _checkout?.release();
    if (_actions != null) CheckoutGitActionsStore.instance.removeListener(_checkoutChanged);
    _review?.removeListener(_reviewChanged);
    _review?.release();
    super.dispose();
  }

  Future<void> _loadCommits() async {
    setState(() {
      _commitsLoading = true;
      _commitsError = null;
    });
    try {
      await git.loadCommits();
    } catch (e) {
      if (mounted) setState(() => _commitsError = e);
    } finally {
      if (mounted) setState(() => _commitsLoading = false);
    }
  }

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  void _openFile(String path, [int? line]) {
    PaseoScope.of(context).open(
      PanelTarget(
        kind: 'file',
        serverId: widget.target.serverId,
        workspaceId: widget.target.workspaceId,
        cwd: widget.target.cwd,
        id: path,
        title: path.split('/').last,
        data: {'path': path, 'line': ?line},
      ),
    );
  }

  void _openPullRequest() => PaseoScope.of(context).open(
    PanelTarget(
      kind: 'pull_request',
      serverId: widget.target.serverId,
      workspaceId: widget.target.workspaceId,
      cwd: widget.target.cwd,
      title: tr(context, 'panels.pullRequest.label'),
    ),
  );

  /// Original `useCheckoutGitActionsStore.refresh`: the host re-reads the
  /// checkout (`checkout.refresh`), then the checkout's queries invalidate.
  bool get _refreshSupported => git.client.supports('checkoutRefresh');
  bool get _refreshing =>
      CheckoutGitActionsStore.instance.status(_serverId, _cwd, 'refresh') == GitActionStatus.pending;

  void _refreshCheckout() {
    if (_refreshing) return;
    final failed = tr(context, 'workspace.git.diff.failedRefresh');
    unawaited(
      CheckoutGitActionsStore.instance
          .run(
            serverId: _serverId,
            cwd: _cwd,
            actionId: 'refresh',
            body: () => gitRequest(git.client, 'checkout.refresh.request', {'cwd': _cwd}),
          )
          .catchError((Object error) {
            if (!mounted) return;
            final message = errorMessage(error);
            showError(context, StateError(message.trim().isEmpty ? failed : message));
          }),
    );
  }

  /// Original `BranchSwitcher` combobox (hooks/use-branch-switcher.ts):
  /// searchable, 280px wide on desktop, every suggested branch led by a 14px
  /// muted git-branch glyph; picking one switches to it.
  Future<void> _branch(BuildContext anchor) async {
    final current = _currentBranchName;
    final client = git.client;
    if (client.status != ConnectionStatus.connected) return;
    final response = await client.request(
      'branch_suggestions_request',
      {'cwd': _cwd, 'limit': 200},
      responseType: 'branch_suggestions_response',
    );
    final failure = response['error'];
    if (failure != null) {
      throw StateError(failure is Map ? stringValue(failure['message']) : failure.toString());
    }
    if (!mounted || !anchor.mounted) return;
    final muted = PaseoColors.of(context).muted;
    final branch = await showPaseoSelect<String>(
      context: anchor,
      title: tr(context, 'branchSwitcher.title'),
      value: current,
      searchable: true,
      searchPlaceholder: tr(context, 'branchSwitcher.searchPlaceholder'),
      emptyText: tr(context, 'branchSwitcher.empty'),
      desktopMinWidth: 280,
      options: [
        for (final branch in response.array('branches'))
          PaseoSelectOption(
            branch.toString(),
            branch.toString(),
            leading: Icon(LucideIcons.gitBranch300, size: 14, color: muted),
          ),
      ],
    );
    if (branch == null || branch == current || !mounted) return;
    await _switchBranch(branch);
  }

  String? _payloadError(JsonMap payload) {
    final error = payload['error'];
    if (error == null) return null;
    return error is Map ? stringValue(error['message'], error.toString()) : error.toString();
  }

  void _invalidateCheckout() => CheckoutGitActionsStore.instance.invalidate(_serverId, _cwd);

  /// Original `handleBranchSelect`: switch; an "uncommitted" refusal offers the
  /// stash-and-switch dialog; a successful switch offers to restore the target
  /// branch's Paseo stash.
  Future<void> _switchBranch(String branch) async {
    final failed = tr(context, 'branchSwitcher.failedToSwitch');
    try {
      final payload = await checkoutPayload(
        git.client,
        'checkout_switch_branch_request',
        {'cwd': _cwd, 'branch': branch},
      );
      final error = _payloadError(payload);
      if (error != null) {
        if (error.toLowerCase().contains('uncommitted')) {
          await _stashAndSwitch(branch);
          return;
        }
        if (mounted) showError(context, StateError(error));
        return;
      }
      _invalidateCheckout();
      await _maybeRestoreStash(branch);
    } catch (error) {
      if (!mounted) return;
      final message = errorMessage(error);
      showError(context, StateError(message.trim().isEmpty ? failed : message));
    }
  }

  /// Original `stashAndSwitch`.
  Future<void> _stashAndSwitch(String branch) async {
    if (!mounted) return;
    final shouldStash = await confirmAction(
      context,
      title: tr(context, 'branchSwitcher.uncommittedTitle'),
      message: tr(context, 'branchSwitcher.uncommittedMessage'),
      confirmLabel: tr(context, 'branchSwitcher.stashAndSwitch'),
    );
    if (!shouldStash || !mounted) return;
    final failed = tr(context, 'branchSwitcher.failedToStash');
    try {
      final stash = await checkoutPayload(
        git.client,
        'stash_save_request',
        {'cwd': _cwd, 'branch': ?_currentBranchName},
      );
      final stashError = _payloadError(stash);
      if (stashError != null) {
        if (mounted) showError(context, StateError(stashError));
        return;
      }
      _invalidateCheckout();
      final switched = await checkoutPayload(
        git.client,
        'checkout_switch_branch_request',
        {'cwd': _cwd, 'branch': branch},
      );
      final switchError = _payloadError(switched);
      if (switchError != null) {
        if (mounted) showError(context, StateError(switchError));
        return;
      }
      _invalidateCheckout();
    } catch (error) {
      if (!mounted) return;
      final message = errorMessage(error);
      showError(context, StateError(message.trim().isEmpty ? failed : message));
    }
  }

  /// Original `maybeRestoreStashForBranch`: non-critical; failures are ignored.
  Future<void> _maybeRestoreStash(String branch) async {
    try {
      final list = await checkoutPayload(
        git.client,
        'stash_list_request',
        {'cwd': _cwd, 'paseoOnly': true},
      );
      final target = list.array('entries').map(jsonMap).where((e) => e['branch'] == branch).firstOrNull;
      if (target == null || !mounted) return;
      final restore = await confirmAction(
        context,
        title: tr(context, 'branchSwitcher.restoreStashTitle'),
        message: tr(context, 'branchSwitcher.restoreStashMessage'),
        confirmLabel: tr(context, 'branchSwitcher.restore'),
      );
      if (!restore || !mounted) return;
      final restoredLabel = tr(context, 'branchSwitcher.stashRestored');
      final popped = await checkoutPayload(
        git.client,
        'stash_pop_request',
        {'cwd': _cwd, 'stashIndex': target.integer('index')},
      );
      final error = _payloadError(popped);
      if (mounted) {
        error != null ? showError(context, StateError(error)) : showToast(context, restoredLabel);
      }
      _invalidateCheckout();
    } catch (_) {
      // Non-critical: the user can restore on the next branch switch.
    }
  }

  /// Original `useDiscardChangesAction`: offered only for the uncommitted
  /// comparison on hosts that support it.
  bool get _discardSupported =>
      !git.isCommit && git.mode == 'uncommitted' && git.client.supports('checkoutDiscardChanges');

  /// Original `discardPath`: confirm, then discard the path (with a rename's
  /// old path) through the shared action store.
  Future<void> _discard(String path, [String? oldPath]) async {
    final confirmed = await confirmAction(
      context,
      title: tr(context, 'workspace.fileActions.confirmRevert.title'),
      message: tr(context, 'workspace.fileActions.confirmRevert.message', args: {'name': path}),
      confirmLabel: tr(context, 'workspace.fileActions.confirmRevert.confirm'),
      destructive: true,
    );
    if (!confirmed || !mounted) return;
    final failed = tr(context, 'workspace.fileActions.confirmRevert.failed');
    try {
      await CheckoutGitActionsStore.instance.run(
        serverId: _serverId,
        cwd: _cwd,
        actionId: 'discard-changes',
        body: () async {
          final payload = await checkoutPayload(
            git.client,
            'checkout.discard_changes.request',
            {
              'cwd': _cwd,
              'paths': [path, if (oldPath != null && oldPath.isNotEmpty) oldPath],
            },
          );
          if (payload['success'] != true) {
            throw StateError(_payloadError(payload) ?? failed);
          }
        },
      );
    } catch (error) {
      if (!mounted) return;
      final message = errorMessage(error);
      showError(context, StateError(message.trim().isEmpty ? failed : message));
    }
  }

  /// Original `GitActionsSplitButton menuOnly`: the primary action then every
  /// other action, separated by group, run through the git action runner.
  Future<void> _gitActionsMenu(BuildContext anchor) async {
    final binding = _actions;
    if (binding == null) return;
    final actions = binding.build(context);
    final list = [?actions.primary, ...actionsWithoutPrimary(actions)];
    if (list.isEmpty) return;
    final id = await showPaseoMenu<String>(
      context,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      title: tr(context, 'workspace.header.actions.workspaceActions'),
      entries: gitActionMenuEntries(context, binding, list),
    );
    final action = list.where((a) => a.id == id).firstOrNull;
    if (action != null && mounted) binding.run(context, action);
  }

  /// Selects [path] in the tree and scrolls the document to its header; the
  /// explorer tree hands the file to the working diff pane instead.
  void _selectFile(String path) {
    if (_presentation == _Presentation.tree) {
      PaseoScope.of(context).open(
        PanelTarget(
          kind: 'working_diff',
          serverId: widget.target.serverId,
          workspaceId: widget.target.workspaceId,
          cwd: widget.target.cwd,
          id: path,
          title: 'Diff',
        ),
      );
      return;
    }
    setState(() => _collapsedFiles.remove(path));
    unawaited(git.selectFile(path));
    _document.currentState?.reveal(path);
  }

  /// Original `buildAbsoluteExplorerPath`.
  String _absolutePath(String entry) {
    final root = _cwd.trim().replaceFirst(RegExp(r'[\\/]+$'), '');
    final path = entry.trim();
    if (root.isEmpty) return path;
    if (path.isEmpty || path == '.') return root;
    if (path.startsWith('/') || RegExp(r'^[A-Za-z]:[\\/]').hasMatch(path)) return path;
    final separator = root.contains(r'\') ? r'\' : '/';
    return [root, ...path.split(RegExp(r'[\\/]+')).where((s) => s.isNotEmpty)].join(separator);
  }

  /// Original `FileActionsContextMenu` for a changed file: open, copy absolute
  /// and relative paths, duplicate (where supported) and the uncommitted
  /// comparison's discard, each group behind a separator.
  Future<void> _fileMenu(String path, Rect? anchor, {required bool deleted, String? oldPath}) async {
    final duplicate = git.client.supports('fsEntryDuplicate');
    final action = await showPaseoMenu<String>(
      context,
      anchor: anchor,
      title: path.split('/').last,
      entries: [
        if (!deleted)
          PaseoMenuEntry.item(
            value: 'open',
            label: tr(context, 'workspace.fileActions.openFile'),
            icon: LucideIcons.fileText300,
          ),
        if (!deleted) const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'copy',
          label: tr(context, 'workspace.fileActions.copyPath'),
          icon: LucideIcons.copy300,
        ),
        PaseoMenuEntry.item(
          value: 'copy-relative',
          label: tr(context, 'workspace.fileActions.copyRelativePath'),
          icon: LucideIcons.copy300,
        ),
        if (duplicate) ...[
          const PaseoMenuEntry.separator(),
          PaseoMenuEntry.item(
            value: 'duplicate',
            label: tr(context, 'workspace.fileActions.duplicate'),
            icon: LucideIcons.copyPlus300,
          ),
        ],
        if (_discardSupported) ...[
          const PaseoMenuEntry.separator(),
          PaseoMenuEntry.item(
            value: 'revert',
            label: tr(context, 'workspace.fileActions.revert'),
            icon: LucideIcons.undo2300,
            destructive: true,
          ),
        ],
      ],
    );
    switch (action) {
      case 'open':
        _openFile(path);
      case 'copy':
        await Clipboard.setData(ClipboardData(text: _absolutePath(path)));
      case 'copy-relative':
        await Clipboard.setData(ClipboardData(text: path));
      case 'duplicate':
        await _duplicate(path);
      case 'revert':
        await _discard(path, oldPath);
    }
  }

  /// Original `handleDuplicatePath`.
  Future<void> _duplicate(String path) async {
    final failed = tr(context, 'workspace.fileExplorer.errors.duplicateFailed');
    try {
      final payload = await checkoutPayload(
        git.client,
        'fs.entry.duplicate.request',
        {'cwd': _cwd, 'path': path},
      );
      if (payload['success'] != true && mounted) {
        showError(context, StateError(stringValue(payload['error'], failed)));
      }
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  Future<void> _folderMenu(String path, Rect? anchor, Set<String> collapsed, VoidCallback changed) async {
    final duplicate = git.client.supports('fsEntryDuplicate');
    final action = await showPaseoMenu<String>(
      context,
      anchor: anchor,
      title: path.split('/').last,
      entries: [
        if (!collapsed.contains(path)) ...[
          PaseoMenuEntry.item(
            value: 'collapse',
            label: tr(context, 'workspace.fileActions.collapseFolder'),
            icon: LucideIcons.folderMinus300,
          ),
          const PaseoMenuEntry.separator(),
        ],
        PaseoMenuEntry.item(
          value: 'copy',
          label: tr(context, 'workspace.fileActions.copyPath'),
          icon: LucideIcons.copy300,
        ),
        PaseoMenuEntry.item(
          value: 'copy-relative',
          label: tr(context, 'workspace.fileActions.copyRelativePath'),
          icon: LucideIcons.copy300,
        ),
        if (duplicate) ...[
          const PaseoMenuEntry.separator(),
          PaseoMenuEntry.item(
            value: 'duplicate',
            label: tr(context, 'workspace.fileActions.duplicate'),
            icon: LucideIcons.copyPlus300,
          ),
        ],
        if (_discardSupported) ...[
          const PaseoMenuEntry.separator(),
          PaseoMenuEntry.item(
            value: 'revert',
            label: tr(context, 'workspace.fileActions.revert'),
            icon: LucideIcons.undo2300,
            destructive: true,
          ),
        ],
      ],
    );
    switch (action) {
      case 'collapse':
        // Original handleCollapseFolder: the folder and every folder under it.
        collapsed.add(path);
        for (final file in git.files) {
          final segments = file.text('path').split('/');
          for (var i = 1; i < segments.length; i++) {
            final folder = segments.sublist(0, i).join('/');
            if (folder.startsWith('$path/')) collapsed.add(folder);
          }
        }
        changed();
      case 'copy':
        await Clipboard.setData(ClipboardData(text: _absolutePath(path)));
      case 'copy-relative':
        await Clipboard.setData(ClipboardData(text: path));
      case 'duplicate':
        await _duplicate(path);
      case 'revert':
        // Original DiffFolderRow revert: the folder path itself.
        await _discard(path);
    }
  }

  /// Original `ChangedFilesTree`: compressed single-child folder chains, folders
  /// before files, folder rows with a chevron and subtree stat, file rows with the
  /// material file icon, stat and change glyph 8px apart.
  Widget _tree({
    required Set<String> collapsed,
    required VoidCallback changed,
    required ValueChanged<String> onSelect,
    EdgeInsets padding = const EdgeInsets.only(bottom: 32),
  }) {
    final root = _ChangeFolder('', '');
    final folders = <String, _ChangeFolder>{'': root};
    _ChangeFolder ensure(String path) => folders[path] ??= () {
      final slash = path.lastIndexOf('/');
      final parent = ensure(slash < 0 ? '' : path.substring(0, slash));
      final folder = _ChangeFolder(path, path.substring(slash + 1));
      parent.folders.add(folder);
      return folder;
    }();
    for (final file in git.files) {
      final path = file.text('path');
      final slash = path.lastIndexOf('/');
      ensure(slash < 0 ? '' : path.substring(0, slash)).files.add(file);
    }
    final rows = <(JsonMap?, _ChangeFolder?, int)>[];
    void walk(_ChangeFolder node, int depth) {
      node.folders.sort((a, b) => a.name.compareTo(b.name));
      node.files.sort((a, b) => a.text('path').compareTo(b.text('path')));
      for (var folder in node.folders) {
        var name = folder.name;
        while (folder.folders.length == 1 && folder.files.isEmpty) {
          folder = folder.folders.single;
          name = '$name/${folder.name}';
        }
        rows.add((null, folder..display = name, depth));
        if (!collapsed.contains(folder.path)) walk(folder, depth + 1);
      }
      for (final file in node.files) {
        rows.add((file, null, depth));
      }
    }

    root.stats();
    walk(root, 0);
    return ListView.builder(
      itemCount: rows.length,
      padding: padding,
      itemBuilder: (context, index) {
        final (file, folder, depth) = rows[index];
        if (folder != null) {
          final directory = folder.path;
          final isCollapsed = collapsed.contains(directory);
          return Builder(
            builder: (anchor) => FileTreeRow(
              name: folder.display,
              tooltip: directory,
              depth: depth,
              selected: _selectedTreePath == directory,
              icon: TreeChevron(expanded: !isCollapsed),
              trailing: FileDiffStat(additions: folder.additions, deletions: folder.deletions),
              onTap: () {
                isCollapsed ? collapsed.remove(directory) : collapsed.add(directory);
                setState(() => _selectedTreePath = directory);
                changed();
              },
              onLongPress: () {
                setState(() => _selectedTreePath = directory);
                _folderMenu(directory, paseoAnchorRect(anchor), collapsed, changed);
              },
              onSecondaryTapDown: (position) {
                setState(() => _selectedTreePath = directory);
                _folderMenu(directory, position & Size.zero, collapsed, changed);
              },
            ),
          );
        }
        final path = file!.text('path');
        final deleted = fileChangeKind(file) == 'deleted';
        return Builder(
          builder: (anchor) => FileTreeRow(
            depth: depth,
            name: path.split('/').last,
            tooltip: path,
            selected: _selectedTreePath == path,
            icon: MaterialFileIcon(fileName: path.split('/').last),
            trailing: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 8,
              children: [
                FileDiffStat(additions: file.integer('additions'), deletions: file.integer('deletions')),
                FileChangeIcon(change: fileChangeKind(file)),
              ],
            ),
            onTap: () {
              setState(() => _selectedTreePath = path);
              onSelect(path);
            },
            onLongPress: () {
              setState(() => _selectedTreePath = path);
              _fileMenu(path, paseoAnchorRect(anchor), deleted: deleted, oldPath: file.text('oldPath'));
            },
            onSecondaryTapDown: (position) {
              setState(() => _selectedTreePath = path);
              _fileMenu(path, position & Size.zero, deleted: deleted, oldPath: file.text('oldPath'));
            },
          ),
        );
      },
    );
  }

  /// Original `JumpToFile` sheet: a transient, fully expanded changed-files tree
  /// whose rows land on the sheet header's rails.
  Future<void> _jumpToFile() async {
    final collapsed = <String>{};
    final path = await showAdaptiveSheet<String>(
      context: context,
      builder: (sheet) => SheetContents(
        title: tr(context, 'workspace.git.diff.jumpToFile.title'),
        scroll: false,
        child: SizedBox(
          height: MediaQuery.sizeOf(sheet).height * .6,
          child: StatefulBuilder(
            builder: (context, update) => _tree(
              collapsed: collapsed,
              changed: () => update(() {}),
              onSelect: (path) => Navigator.pop(sheet, path),
              padding: const EdgeInsets.fromLTRB(12, 8, 24, 32),
            ),
          ),
        ),
      ),
    );
    if (path != null && mounted) _selectFile(path);
  }

  String get _baseRefLabel => formatBaseRefLabel(git.baseRef, tr(context, 'workspace.git.diff.base'));

  /// The branch the switcher names: none for a detached HEAD.
  String? get _currentBranchName {
    final branch = git.status?.text('currentBranch') ?? '';
    return branch.isEmpty || branch == 'HEAD' ? null : branch;
  }

  /// Original `resolveBranchLabel` + `computeCommittedDiffDescription`.
  String? get _committedDescription {
    final branch = _currentBranchName ??
        tr(context, git.status?.flag('isGit') == false ? 'workspace.git.diff.notRepository' : 'workspace.git.diff.branchUnknown');
    final base = _baseRefLabel;
    if (branch.isEmpty || base.isEmpty || branch == base) return null;
    return '$branch -> $base';
  }

  static String _modeLabel(String mode) => switch (mode) {
    'all' => 'workspace.git.diff.all',
    'base' => 'workspace.git.diff.committed',
    _ => 'workspace.git.diff.uncommitted',
  };

  /// Original `selectComparison`: shared by every Changes view of the workspace.
  void _setMode(String mode) => git.selectComparison(mode);

  /// Original `DiffModeMenu`: a 260px menu of All / Uncommitted / Committed with
  /// separators, the committed row describing `branch -> base`.
  Future<void> _modeMenu(BuildContext anchor) async {
    final mode = await showPaseoMenu<String>(
      context,
      anchor: paseoAnchorRect(anchor),
      width: 260,
      title: tr(context, 'workspace.git.diff.diffMode'),
      entries: [
        PaseoMenuEntry.item(
          value: 'all',
          label: tr(context, 'workspace.git.diff.all'),
          selected: git.mode == 'all',
        ),
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'uncommitted',
          label: tr(context, 'workspace.git.diff.uncommitted'),
          selected: git.mode == 'uncommitted',
        ),
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'base',
          label: tr(context, 'workspace.git.diff.committed'),
          hint: _committedDescription,
          selected: git.mode == 'base',
        ),
      ],
    );
    if (mode != null) _setMode(mode);
  }

  bool get _allFilesCollapsed =>
      git.files.isNotEmpty && git.files.every((file) => _collapsedFiles.contains(file.text('path')));

  void _toggleCollapseAll() => setState(() {
    _allFilesCollapsed
        ? _collapsedFiles.clear()
        : _collapsedFiles.addAll(git.files.map((file) => file.text('path')));
  });

  void _toggleWhitespace() => unawaited(_updatePreferences({'hideWhitespace': !_hideWhitespace}));
  void _toggleSplit() => unawaited(_updatePreferences({'layout': _split ? 'unified' : 'split'}));
  void _toggleWrap() => unawaited(_updatePreferences({'wrapLines': !_wrap}));

  /// Original `ChangesOptionsMenu`: a 240px end-aligned menu. The tree offers the
  /// diff tab; the combined pane its collapse / split / whitespace / wrap
  /// options. Wide layouts end with the inline-diff preference.
  Future<void> _optionsMenu(BuildContext anchor) async {
    final compact = isCompactLayout(context);
    final hasChanges = git.files.isNotEmpty;
    final allCollapsed = _allFilesCollapsed;
    final inlineDiff = [
      const PaseoMenuEntry<String>.separator(),
      PaseoMenuEntry.item(
        value: 'inline-diff',
        label: tr(context, 'workspace.git.diff.inlineDiff'),
        selected: _inlineDiff,
      ),
    ];
    final action = await showPaseoMenu<String>(
      context,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      width: 240,
      title: tr(context, 'workspace.git.diff.options'),
      entries: _presentation == _Presentation.tree
          ? [
              PaseoMenuEntry.item(
                value: 'open-diff',
                label: tr(context, 'workspace.git.diff.openDiffTab'),
                icon: LucideIcons.maximize300,
              ),
              if (!compact) ...inlineDiff,
            ]
          : [
              if (hasChanges) ...[
                PaseoMenuEntry.item(
                  value: 'collapse',
                  label: tr(
                    context,
                    allCollapsed
                        ? 'workspace.git.diff.expandAllFiles'
                        : 'workspace.git.diff.collapseAllFiles',
                  ),
                  icon: allCollapsed
                      ? LucideIcons.listChevronsUpDown300
                      : LucideIcons.listChevronsDownUp300,
                ),
                const PaseoMenuEntry.separator(),
              ],
              if (_canUseSplitLayout)
                PaseoMenuEntry.item(
                  value: 'split',
                  label: tr(context, 'workspace.git.diff.split'),
                  icon: LucideIcons.columns2300,
                  selected: _split,
                ),
              PaseoMenuEntry.item(
                value: 'whitespace',
                label: tr(
                  context,
                  _hideWhitespace
                      ? 'workspace.git.diff.showWhitespace'
                      : 'workspace.git.diff.hideWhitespace',
                ),
                icon: LucideIcons.pilcrow300,
                selected: _hideWhitespace,
              ),
              PaseoMenuEntry.item(
                value: 'wrap',
                label: tr(
                  context,
                  _wrap ? 'workspace.git.diff.scrollLongLines' : 'workspace.git.diff.wrapLongLines',
                ),
                icon: LucideIcons.wrapText300,
                selected: _wrap,
              ),
              if (!compact) ...inlineDiff,
            ],
    );
    switch (action) {
      case 'open-diff':
        if (!mounted) return;
        PaseoScope.of(context).open(
          PanelTarget(
            kind: 'working_diff',
            serverId: widget.target.serverId,
            workspaceId: widget.target.workspaceId,
            cwd: widget.target.cwd,
            title: 'Diff',
          ),
        );
      case 'collapse':
        _toggleCollapseAll();
      case 'split':
        _toggleSplit();
      case 'whitespace':
        _toggleWhitespace();
      case 'wrap':
        _toggleWrap();
      case 'inline-diff':
        unawaited(_updatePreferences({'inlineDiff': !_inlineDiff}));
    }
  }

  /// Original `ChangesRefreshButton`: shown only on hosts with `checkoutRefresh`.
  Widget _refreshButton() {
    final refreshing = _refreshing;
    return PaneToolbarButton(
      tooltip: tr(context, refreshing ? 'workspace.git.diff.refreshing' : 'workspace.git.diff.refresh'),
      onPressed: refreshing ? null : _refreshCheckout,
      icon: refreshing ? null : LucideIcons.rotateCw300,
      child: refreshing
          ? SizedBox.square(
              dimension: paneToolbarIconSize(context),
              child: CircularProgressIndicator(
                strokeWidth: 1.5,
                color: PaseoColors.of(context).extraMuted,
              ),
            )
          : null,
    );
  }

  Widget _optionsButton() => Builder(
    builder: (anchor) => PaneToolbarButton(
      tooltip: tr(context, 'workspace.git.diff.options'),
      icon: LucideIcons.ellipsis300,
      onPressed: () => _optionsMenu(anchor),
    ),
  );

  /// Original `ChangesToolbarRow`: the pane toolbar with an 8px leading inset, an
  /// 8px gap between the flexible identity and the trailing controls, and the
  /// trailing padding resolved from what the last control paints.
  Widget _toolbarRow({
    required List<Widget> leading,
    required List<Widget> trailing,
    bool framed = false,
  }) => FilePaneToolbar(
    sidebar: _presentation == _Presentation.tree,
    padding: EdgeInsets.only(left: 8, right: paneToolbarTrailingPadding(context, framed: framed)),
    children: [
      Expanded(child: Row(spacing: 4, children: leading)),
      const SizedBox(width: 8),
      PaneToolbarControls(children: trailing),
    ],
  );

  Widget _repositoryToolbar() {
    final branch = _currentBranchName;
    final hint = selectPrHint(_checkout?.prPayload);
    final presentation = forgePresentation(hint?.forge);
    final compact = isCompactLayout(context);
    final actions = _actions;
    // Original `buildChangesHeaderModel`: the git actions menu lives here only
    // on compact layouts; wide layouts carry it in the workspace header.
    final gitActions = compact && actions != null ? actions.build(context) : null;
    final showActions = gitActions != null && (gitActions.primary != null || actionsWithoutPrimary(gitActions).isNotEmpty);
    return _toolbarRow(
      framed: showActions,
      leading: [
        if (branch != null)
          Flexible(
            child: Builder(
              builder: (anchor) => _ToolbarLabelTrigger(
                label: branch,
                chevron: true,
                tooltip: tr(context, 'branchSwitcher.triggerTooltip'),
                semanticLabel: tr(context, 'branchSwitcher.currentBranch', args: {'branchName': branch}),
                onPressed: () => _run(() => _branch(anchor)),
              ),
            ),
          ),
      ],
      trailing: [
        if (hint != null) ...[
          _ToolbarLabelTrigger(
            label: '${presentation.numberPrefix}${hint.number}',
            leading: _PullRequestStateIcon(state: hint.state),
            tooltip: tr(context, 'panels.pullRequest.label'),
            onPressed: () async => _openPullRequest(),
          ),
          PaneToolbarButton(
            tooltip: tr(context, 'workspace.git.pr.actions.openOn', args: {'brand': presentation.brandLabel}),
            icon: LucideIcons.externalLink300,
            onPressed: () => _run(() async {
              final uri = Uri.tryParse(hint.url);
              if (uri == null || !await launchUrl(uri, mode: LaunchMode.externalApplication)) {
                throw StateError('Unable to open ${hint.url}');
              }
            }),
          ),
        ],
        if (showActions) ...[
          const SizedBox(width: 4),
          Builder(
            builder: (anchor) => _GitActionsTrigger(
              onPressed: () => _run(() => _gitActionsMenu(anchor)),
            ),
          ),
        ],
      ],
    );
  }

  Widget _comparisonToolbar() {
    final hasChanges = git.files.isNotEmpty;
    final combined = _presentation == _Presentation.combined;
    return _toolbarRow(
      leading: [
        Flexible(
          child: Builder(
            builder: (anchor) => _ToolbarLabelTrigger(
              label: tr(context, _modeLabel(git.mode)),
              chevron: true,
              semanticLabel: tr(context, 'workspace.git.diff.diffMode'),
              onPressed: () => _modeMenu(anchor),
            ),
          ),
        ),
        if (!git.loading)
          FileDiffStat(
            additions: git.files.fold<int>(0, (sum, f) => sum + f.integer('additions')),
            deletions: git.files.fold<int>(0, (sum, f) => sum + f.integer('deletions')),
          ),
      ],
      trailing: [
        if (combined && !isCompactLayout(context) && hasChanges)
          PaneToolbarButton(
            tooltip: tr(
              context,
              _treeVisible ? 'workspace.tree.hideFolderTree' : 'workspace.tree.showFolderTree',
            ),
            icon: LucideIcons.folderTree300,
            selected: _treeVisible,
            onPressed: () => setState(() => _treeVisible = !_treeVisible),
          ),
        if (_refreshSupported) _refreshButton(),
        _optionsButton(),
      ],
    );
  }

  /// Original `ChangesDiffOnlyToolbar` with the bare `ChangesDiffToolbar` toggles.
  Widget _diffOnlyToolbar() {
    final allCollapsed = _allFilesCollapsed;
    return _toolbarRow(
      leading: [
        Flexible(
          child: Padding(
            padding: const EdgeInsets.all(4),
            child: Text(
              widget.target.title ?? tr(context, 'panels.diff.commitSubtitle'),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ),
      ],
      trailing: [
        if (_refreshSupported) _refreshButton(),
        if (git.files.isNotEmpty)
          PaneToolbarButton(
            tooltip: tr(
              context,
              allCollapsed ? 'workspace.git.diff.expandAllFiles' : 'workspace.git.diff.collapseAllFiles',
            ),
            icon: allCollapsed ? LucideIcons.listChevronsUpDown300 : LucideIcons.listChevronsDownUp300,
            onPressed: _toggleCollapseAll,
          ),
        if (_canUseSplitLayout)
          PaneToolbarButton(
            tooltip: tr(
              context,
              _split ? 'workspace.git.diff.switchToUnified' : 'workspace.git.diff.switchToSplit',
            ),
            icon: LucideIcons.columns2300,
            selected: _split,
            onPressed: _toggleSplit,
          ),
        PaneToolbarButton(
          tooltip: tr(
            context,
            _hideWhitespace ? 'workspace.git.diff.showWhitespace' : 'workspace.git.diff.hideWhitespace',
          ),
          icon: LucideIcons.pilcrow300,
          selected: _hideWhitespace,
          onPressed: _toggleWhitespace,
        ),
        PaneToolbarButton(
          tooltip: tr(
            context,
            _wrap ? 'workspace.git.diff.scrollLongLines' : 'workspace.git.diff.wrapLongLines',
          ),
          icon: LucideIcons.wrapText300,
          selected: _wrap,
          onPressed: _toggleWrap,
        ),
      ],
    );
  }

  /// Original `DiffBodyContent` states: 64px from the top, centred, base muted
  /// copy (destructive for errors) under a large spinner.
  Widget _state({String? text, bool spinner = false, bool error = false, Widget? action}) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 64, 24, 0),
      child: Align(
        alignment: Alignment.topCenter,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          spacing: spinner ? 16 : 8,
          children: [
            if (spinner)
              SizedBox.square(
                dimension: 32,
                child: CircularProgressIndicator(strokeWidth: 2, color: colors.muted),
              ),
            if (text != null)
              Text(
                text,
                textAlign: TextAlign.center,
                style: theme.textTheme.bodyMedium!.copyWith(
                  color: error ? theme.colorScheme.error : colors.muted,
                ),
              ),
            ?action,
          ],
        ),
      ),
    );
  }

  /// Original `computeChangesEmptyAction`.
  Widget? _emptyAction() {
    final status = git.status;
    if (_hideWhitespace || status == null || git.isCommit) return null;
    final (label, mode) = git.mode == 'base' && status.flag('isDirty')
        ? ('workspace.git.diff.seeUncommittedChanges', 'uncommitted')
        : git.mode == 'uncommitted' && jsonMap(status['aheadBehind']).integer('ahead') > 0
        ? ('workspace.git.diff.seeCommittedChanges', 'base')
        : (null, null);
    if (label == null) return null;
    return PaseoButton(
      label: tr(context, label),
      variant: PaseoButtonVariant.ghost,
      size: PaseoControlSize.xs,
      onPressed: () => _setMode(mode!),
    );
  }

  Widget _diff(double bottomInset) {
    final comments = <String, Map<String, List<JsonMap>>>{};
    for (final comment in _review?.comments ?? <JsonMap>[]) {
      ((comments[comment.text('filePath')] ??= {})['${comment.text('side')}:${comment.integer('lineNumber')}'] ??= [])
          .add(comment);
    }
    if (!_initialRevealed && git.selectedPath.isNotEmpty) {
      _initialRevealed = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _document.currentState?.reveal(git.selectedPath);
      });
    }
    return DiffDocument(
      key: _document,
      git: git,
      collapsed: _collapsedFiles,
      split: _split,
      onFileMenu: (file, anchor) => _fileMenu(
        file.text('path'),
        anchor,
        deleted: fileChangeKind(file) == 'deleted',
        oldPath: file.text('oldPath'),
      ),
      wrap: _wrap,
      comments: comments,
      bottomInset: bottomInset,
      onToggleCollapsed: (path) => setState(() {
        _collapsedFiles.contains(path) ? _collapsedFiles.remove(path) : _collapsedFiles.add(path);
      }),
      onOpen: _openFile,
      onEditComment: (comment) => _run(() async {
        final body = await promptReviewComment(
          context,
          path: comment.text('filePath'),
          side: comment.text('side'),
          line: comment.integer('lineNumber'),
          initialValue: comment.text('body'),
        );
        if (body == null || body.trim().isEmpty) return;
        await _review!.saveComment(
          id: comment.text('id'),
          filePath: comment.text('filePath'),
          side: comment.text('side'),
          lineNumber: comment.integer('lineNumber'),
          body: body,
        );
      }),
      onDeleteComment: (comment) => _run(() => _review!.delete(comment)),
      onComment: (path, line, side, lines) => _run(() async {
        final lineNumber = side == 'old' ? line.oldNumber : line.newNumber;
        if (git.isCommit || _review == null || lineNumber == null) {
          _openFile(path, line.newNumber ?? line.oldNumber);
          return;
        }
        final body = await promptReviewComment(context, path: path, side: side, line: lineNumber);
        if (body == null || body.trim().isEmpty) return;
        await _review!.saveComment(filePath: path, side: side, lineNumber: lineNumber, body: body);
      }),
    );
  }

  Widget _body() {
    final status = git.status;
    if (status == null) {
      if (git.error != null && !git.loading) {
        return _state(text: errorMessage(git.error!), error: true);
      }
      return _state(spinner: true, text: tr(context, 'workspace.git.diff.checkingRepository'));
    }
    if (!status.flag('isGit')) {
      return _state(text: tr(context, 'workspace.git.diff.notRepository'));
    }
    final hasChanges = git.files.isNotEmpty;
    if (!hasChanges && git.loading) return _state(spinner: true);
    // Original DiffBodyContent: the whole-diff limit, then the snapshot's error.
    if (git.tooLarge && !git.isCommit) return _tooLargeState();
    if (git.diffError != null) return _state(text: git.diffError, error: true);
    if (!hasChanges) {
      return _state(text: tr(context, 'diffViewer.empty'), action: _emptyAction());
    }
    final compact = isCompactLayout(context);
    switch (_presentation) {
      case _Presentation.tree:
        return ColoredBox(
          color: PaseoColors.of(context).sidebar,
          child: _tree(
            collapsed: _collapsedFolders,
            changed: () => setState(() {}),
            onSelect: _selectFile,
          ),
        );
      case _Presentation.diff:
        return _diff(0);
      case _Presentation.combined:
        final jump = compact ? gitFloatingActionClearance : 0.0;
        final hasFeedback = (_review?.comments.isNotEmpty ?? false);
        final feedback = hasFeedback ? (compact ? gitFloatingActionClearance : 32.0 + 16) : 0.0;
        final diff = Stack(
          children: [
            Positioned.fill(child: _diff(jump + feedback)),
            if (compact)
              GitFloatingActionButton(
                icon: LucideIcons.folderTree300,
                semanticLabel: tr(context, 'workspace.git.diff.jumpToFile.title'),
                bottomInset: MediaQuery.paddingOf(context).bottom,
                onPressed: _jumpToFile,
              ),
            if (_review != null)
              ReviewFeedbackAction(
                draft: _review!,
                recipients: _feedbackRecipients,
                connected: git.client.status == ConnectionStatus.connected,
                attachment: _reviewAttachment,
                onSend: _sendFeedback,
                bottomOffset: jump,
              ),
          ],
        );
        if (compact || !_treeVisible) return diff;
        // Original TreeRail: content left, a 220px tree right of a 1px border.
        return Row(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Expanded(child: diff),
            Container(
              width: 220,
              decoration: BoxDecoration(
                border: Border(left: BorderSide(color: Theme.of(context).dividerColor)),
              ),
              child: _tree(
                collapsed: _collapsedFolders,
                changed: () => setState(() {}),
                onSelect: _selectFile,
              ),
            ),
          ],
        );
    }
  }

  /// Original `DiffTooLargeState`: a muted warning glyph over the title and
  /// description, centred 64px from the top.
  Widget _tooLargeState() {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(24, 64, 24, 0),
      child: Align(
        alignment: Alignment.topCenter,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          spacing: 12,
          children: [
            Icon(LucideIcons.fileWarning300, size: 24, color: colors.muted),
            Column(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [
                Text(
                  tr(context, 'workspace.git.diff.previewTooLargeTitle'),
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500),
                ),
                Text(
                  tr(context, 'workspace.git.diff.previewTooLargeDescription'),
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// Original forge setup callout (`computeForgeSetupAction`).
  String? get _forgeSetupMessage {
    final payload = _checkout?.prPayload;
    if (payload == null) return null;
    return forgeSetupMessage(
      context,
      forge: payload.text('forge'),
      authState: payload.text('authState'),
      forgeProvidersSupported: git.client.supports('forgeProviders'),
      remoteUrl: git.status?.text('remoteUrl'),
    );
  }

  /// Original `computePrErrorMessage`: the PR status error while forge
  /// features are on.
  String? get _prErrorMessage {
    final payload = _checkout?.prPayload;
    if (payload == null || !payload.flag('githubFeaturesEnabled', true)) return null;
    final error = payload['error'];
    if (error == null) return null;
    final message = error is Map ? stringValue(error['message']) : error.toString();
    return message.isEmpty ? null : message;
  }

  @override
  Widget build(BuildContext context) {
    if (_git == null) return const LoadingState();
    final theme = Theme.of(context);
    final isGit = git.status?.flag('isGit') == true;
    final hasStatus = git.status != null;
    return ColoredBox(
      color: _presentation == _Presentation.tree
          ? PaseoColors.of(context).sidebar
          : PaseoColors.of(context).surface0,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (isGit || git.isCommit) ...[
            if (_presentation == _Presentation.diff)
              _diffOnlyToolbar()
            else ...[
              _repositoryToolbar(),
              _comparisonToolbar(),
            ],
          ],
          if (isGit && _forgeSetupMessage != null)
            Container(
              margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              decoration: BoxDecoration(
                color: PaseoColors.of(context).surface1,
                border: Border.all(color: theme.dividerColor),
                borderRadius: BorderRadius.circular(6),
              ),
              child: Text(
                _forgeSetupMessage!,
                style: theme.textTheme.bodySmall!.copyWith(color: PaseoColors.of(context).muted),
              ),
            ),
          // Original actionErrorText: 12px insets, small destructive copy.
          for (final message in [
            ?_prErrorMessage,
            if (hasStatus && git.error != null) errorMessage(git.error!),
          ])
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 0, 12, 4),
              child: Text(
                message,
                style: theme.textTheme.bodySmall!.copyWith(color: theme.colorScheme.error),
              ),
            ),
          Expanded(child: _body()),
          if (isGit && _presentation != _Presentation.diff)
            _CommitsSection(
              commits: git.commits.where((commit) => !commit.flag('isOnBase')).toList(),
              baseRef: _baseRefLabel,
              collapsed: _commitsCollapsed,
              loading: _commitsLoading,
              error: _commitsError,
              onToggle: () {
                setState(() => _commitsCollapsed = !_commitsCollapsed);
                if (!_commitsCollapsed) unawaited(_loadCommits());
              },
              onOpen: (commit) => PaseoScope.of(context).open(
                PanelTarget(
                  kind: 'commit_diff',
                  serverId: widget.target.serverId,
                  workspaceId: widget.target.workspaceId,
                  cwd: widget.target.cwd,
                  id: commit.text('sha'),
                  title: commit.text('subject'),
                  data: {'sha': commit.text('sha')},
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `ToolbarLabelSelectTrigger` / toolbar label trigger: 4px padding, 4px
/// gap, 6px radius, a small normal-weight muted label (foreground and the
/// interaction highlight while hovered, pressed or open) with an optional
/// leading glyph and a trailing 12px extraMuted chevron.
class _ToolbarLabelTrigger extends StatefulWidget {
  const _ToolbarLabelTrigger({
    required this.label,
    required this.onPressed,
    this.leading,
    this.chevron = false,
    this.tooltip,
    this.semanticLabel,
  });
  final String label;
  final Widget? leading;
  final bool chevron;
  final String? tooltip, semanticLabel;
  final Future<void> Function()? onPressed;

  @override
  State<_ToolbarLabelTrigger> createState() => _ToolbarLabelTriggerState();
}

class _ToolbarLabelTriggerState extends State<_ToolbarLabelTrigger> {
  bool _hovered = false, _pressed = false, _open = false;

  Future<void> _press() async {
    setState(() => _open = true);
    try {
      await widget.onPressed!();
    } finally {
      if (mounted) setState(() => _open = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final enabled = widget.onPressed != null;
    final highlighted = enabled && (_hovered || _pressed || _open);
    Widget trigger = Semantics(
      button: true,
      enabled: enabled,
      label: widget.semanticLabel ?? widget.label,
      excludeSemantics: true,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: enabled ? _press : null,
          child: Container(
            padding: const EdgeInsets.all(4),
            decoration: BoxDecoration(
              color: highlighted ? paseoInteractionHighlight(context) : null,
              borderRadius: BorderRadius.circular(6),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [
                ?widget.leading,
                Flexible(
                  child: Text(
                    widget.label,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodySmall!.copyWith(
                      fontWeight: FontWeight.normal,
                      color: highlighted ? theme.colorScheme.onSurface : colors.muted,
                    ),
                  ),
                ),
                if (widget.chevron)
                  Icon(LucideIcons.chevronDown300, size: 12, color: colors.extraMuted),
              ],
            ),
          ),
        ),
      ),
    );
    if (widget.tooltip != null && !isCompactLayout(context)) {
      trigger = Tooltip(
        message: widget.tooltip,
        waitDuration: const Duration(milliseconds: 300),
        child: trigger,
      );
    }
    return trigger;
  }
}

/// Original `GitActionsSplitButton menuOnly` trigger: a 42x26 (compact 48x28)
/// borderAccent-framed 6px-radius button with a 16px muted git-branch glyph and
/// a 12px extraMuted chevron 4px apart; surface2 while hovered, pressed or open.
class _GitActionsTrigger extends StatefulWidget {
  const _GitActionsTrigger({required this.onPressed});
  final Future<void> Function()? onPressed;

  @override
  State<_GitActionsTrigger> createState() => _GitActionsTriggerState();
}

class _GitActionsTriggerState extends State<_GitActionsTrigger> {
  bool _hovered = false, _pressed = false, _open = false;

  Future<void> _press() async {
    setState(() => _open = true);
    try {
      await widget.onPressed!();
    } finally {
      if (mounted) setState(() => _open = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final enabled = widget.onPressed != null;
    return Semantics(
      button: true,
      enabled: enabled,
      label: tr(context, 'workspace.header.actions.workspaceActions'),
      excludeSemantics: true,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: enabled ? _press : null,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: compact ? 48 : 42,
              height: compact ? 28 : 26,
              decoration: BoxDecoration(
                color: enabled && (_hovered || _pressed || _open) ? colors.surface2 : null,
                border: Border.all(color: colors.borderAccent),
                borderRadius: BorderRadius.circular(6),
              ),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.center,
                spacing: 4,
                children: [
                  Icon(LucideIcons.gitBranch300, size: 16, color: colors.muted),
                  Icon(LucideIcons.chevronDown300, size: 12, color: colors.extraMuted),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `PullRequestStateIcon`: git-pull-request in success, git-merge in the
/// merged purple, git-pull-request-closed in danger, 14px.
class _PullRequestStateIcon extends StatelessWidget {
  const _PullRequestStateIcon({required this.state});

  /// Original `PrHint.state`: open, merged or closed.
  final String state;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final (icon, color) = switch (state) {
      'merged' => (LucideIcons.gitMerge300, statusMergedColor(context)),
      'closed' => (LucideIcons.gitPullRequestClosed300, colors.danger),
      _ => (LucideIcons.gitPullRequest300, colors.success),
    };
    return Icon(icon, size: 14, color: color);
  }
}

/// Original `statusMerged` (styles/theme.ts light / dark status colours).
Color statusMergedColor(BuildContext context) => Theme.of(context).brightness == Brightness.dark
    ? const Color(0xffa890d5)
    : const Color(0xff7347af);

/// Original `CommitsSection`: a collapsible footer under a 1px border. The header
/// shares a depth-0 tree row's leading rail (12px) with a 14px muted chevron in a
/// 16px slot, the base title and a small muted count; rows list the workspace
/// commits (those not on base) with their graph node, mono sha, subject, age
/// and caret.
class _CommitsSection extends StatelessWidget {
  const _CommitsSection({
    required this.commits,
    required this.baseRef,
    required this.collapsed,
    required this.loading,
    required this.error,
    required this.onToggle,
    required this.onOpen,
  });
  final List<JsonMap> commits;
  final String baseRef;
  final bool collapsed, loading;
  final Object? error;
  final VoidCallback onToggle;
  final ValueChanged<JsonMap> onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(color: colors.muted);
    final loaded = !loading && error == null && (commits.isNotEmpty || !collapsed);
    Widget content;
    if (error != null) {
      content = Padding(
        padding: const EdgeInsets.fromLTRB(8, 8, 12, 8),
        child: Text(
          tr(context, 'workspace.git.diff.commits.loadError'),
          style: theme.textTheme.bodySmall!.copyWith(color: colors.danger),
        ),
      );
    } else if (loading && commits.isEmpty) {
      content = const _CommitsSkeleton();
    } else if (commits.isEmpty) {
      content = Padding(
        padding: const EdgeInsets.fromLTRB(8, 4, 12, 8),
        child: Text(
          tr(context, 'workspace.git.diff.commits.noneAhead', args: {'baseRef': baseRef}),
          style: small,
        ),
      );
    } else {
      content = ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * .3),
        child: ListView.builder(
          shrinkWrap: true,
          padding: const EdgeInsets.only(bottom: 4),
          itemCount: commits.length,
          itemBuilder: (context, index) => _CommitRow(
            commit: commits[index],
            isFirst: index == 0,
            isLast: index == commits.length - 1,
            onPressed: () => onOpen(commits[index]),
          ),
        ),
      );
    }
    return Container(
      padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(context).bottom),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: theme.dividerColor)),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Semantics(
            button: true,
            expanded: !collapsed,
            child: GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: onToggle,
              child: MouseRegion(
                cursor: SystemMouseCursors.click,
                child: Padding(
                  padding: EdgeInsets.fromLTRB(treeRowPaddingLeft(0), 8, 12, 8),
                  child: Row(
                    spacing: 8,
                    children: [
                      SizedBox.square(
                        dimension: 16,
                        child: RotatedBox(
                          quarterTurns: collapsed ? 0 : 1,
                          child: Icon(LucideIcons.chevronRight300, size: 14, color: colors.muted),
                        ),
                      ),
                      Text(
                        tr(context, 'workspace.git.diff.commits.title'),
                        style: theme.textTheme.bodyMedium!.copyWith(
                          color: theme.colorScheme.onSurface,
                        ),
                      ),
                      Expanded(
                        child: loaded && commits.isNotEmpty
                            ? Text(
                                '${commits.length}',
                                style: small,
                                semanticsLabel: tr(
                                  context,
                                  'workspace.git.diff.commits.countLabel',
                                  args: {'count': '${commits.length}'},
                                ),
                              )
                            : const SizedBox.shrink(),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
          if (!collapsed) content,
        ],
      ),
    );
  }
}

class _CommitsSkeleton extends StatelessWidget {
  const _CommitsSkeleton();

  @override
  Widget build(BuildContext context) {
    final fill = PaseoColors.of(context).surface2;
    Widget bar(double? width, double height, {double radius = 4}) => Container(
      width: width,
      height: height,
      decoration: BoxDecoration(color: fill, borderRadius: BorderRadius.circular(radius)),
    );
    return Semantics(
      label: tr(context, 'workspace.git.diff.commits.loading'),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(8, 4, 8, 8),
        child: SizedBox(
          height: 20,
          child: Row(
            spacing: 8,
            children: [
              bar(8, 8, radius: 4),
              bar(48, 10),
              Expanded(child: bar(null, 12)),
              bar(40, 10),
              const SizedBox(width: 16),
            ],
          ),
        ),
      ),
    );
  }
}

/// Original `CommitRow` + `CommitGraphNode`: 8px insets, 4px vertical padding, an
/// 8px graph column (2px rail and 8px ring marker, accent for workspace commits,
/// filled once pushed), a 70px small mono muted sha 4px before the base subject,
/// the small muted age and a 14px muted caret; surfaceSidebarHover when active.
class _CommitRow extends StatefulWidget {
  const _CommitRow({
    required this.commit,
    required this.isFirst,
    required this.isLast,
    required this.onPressed,
  });
  final JsonMap commit;
  final bool isFirst, isLast;
  final VoidCallback onPressed;

  @override
  State<_CommitRow> createState() => _CommitRowState();
}

class _CommitRowState extends State<_CommitRow> {
  bool _hovered = false, _pressed = false;

  static String _timeAgo(DateTime date) {
    final elapsed = DateTime.now().difference(date);
    if (elapsed.inMinutes < 1) return 'just now';
    if (elapsed.inHours < 1) return '${elapsed.inMinutes}m ago';
    if (elapsed.inDays < 1) return '${elapsed.inHours}h ago';
    if (elapsed.inDays < 7) return '${elapsed.inDays}d ago';
    const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    final local = date.toLocal();
    return '${months[local.month - 1]} ${local.day}';
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final commit = widget.commit;
    final rail = commit.flag('isOnBase') ? colors.muted : colors.accent;
    final date = DateTime.tryParse(commit.text('authorDate'));
    final small = theme.textTheme.bodySmall!.copyWith(color: colors.muted);
    return Semantics(
      button: true,
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
            color: _hovered || _pressed ? sidebarHoverColor(context) : null,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: IntrinsicHeight(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 8,
                children: [
                  SizedBox(
                    width: 8,
                    child: Stack(
                      alignment: Alignment.center,
                      children: [
                        if (!(widget.isFirst && widget.isLast))
                          Positioned.fill(
                            child: Column(
                              children: [
                                Expanded(
                                  child: Container(
                                    width: 2,
                                    color: widget.isFirst ? Colors.transparent : rail,
                                  ),
                                ),
                                Expanded(
                                  child: Container(
                                    width: 2,
                                    color: widget.isLast ? Colors.transparent : rail,
                                  ),
                                ),
                              ],
                            ),
                          ),
                        Container(
                          width: 8,
                          height: 8,
                          decoration: BoxDecoration(
                            shape: BoxShape.circle,
                            color: commit.flag('isOnRemote') ? rail : colors.surface0,
                            border: Border.all(color: rail, width: 2),
                          ),
                        ),
                      ],
                    ),
                  ),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 4),
                      child: Row(
                        spacing: 4,
                        children: [
                          SizedBox(
                            width: 70,
                            child: Text(
                              commit.text('shortSha'),
                              maxLines: 1,
                              overflow: TextOverflow.clip,
                              style: small.copyWith(
                                fontFamily: typography.monoFontFamily,
                                fontFamilyFallback: typography.monoFontFamilyFallback,
                              ),
                            ),
                          ),
                          Expanded(
                            child: Text(
                              commit.text('subject'),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.bodyMedium!.copyWith(
                                color: theme.colorScheme.onSurface,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                  Center(child: Text(date == null ? '' : _timeAgo(date), style: small)),
                  Center(
                    child: SizedBox.square(
                      dimension: 16,
                      child: Icon(LucideIcons.chevronRight300, size: 14, color: colors.muted),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// One directory of the changed-files tree; rows carry full subtree stats.
class _ChangeFolder {
  _ChangeFolder(this.path, this.name) : display = name;
  final String path, name;
  String display;
  final folders = <_ChangeFolder>[];
  final files = <JsonMap>[];
  int additions = 0, deletions = 0;

  void stats() {
    additions = 0;
    deletions = 0;
    for (final folder in folders) {
      folder.stats();
      additions += folder.additions;
      deletions += folder.deletions;
    }
    for (final file in files) {
      additions += file.integer('additions');
      deletions += file.integer('deletions');
    }
  }
}
