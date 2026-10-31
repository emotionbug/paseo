import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show RenderEditable;
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:yaml/yaml.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../platform/desktop/desktop_integrations.dart';
import '../../platform/desktop/desktop_host.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/panel_activity.dart';
import '../../ui/theme.dart';
import '../chat/chat_drafts.dart';
import '../chat/chat_markdown.dart';
import 'file_editor_model.dart';
import 'file_editor_session.dart';
import 'file_panel_resolve.dart';
import 'html_preview.dart';
import 'native_file_actions.dart';
import 'pane_chrome.dart';
import 'remote_directory_browser.dart';
import 'remote_file_service.dart';
import 'source_controller.dart';
import 'vim_engine.dart';
import 'vim_command_line.dart';

/// Original `palette.amber[500]`: the file conflict alert's glyph, title and rule.
const _amber500 = Color(0xfff59e0b);

/// Original `palette.red[300]`: the file toolbar's "Save failed".
const _red300 = Color(0xfffca5a5);

/// The code font size range of the appearance settings.
const _minCodeFont = 9.0, _maxCodeFont = 22.0;

class FilePanel extends StatefulWidget {
  const FilePanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<FilePanel> createState() => _FilePanelState();
}

class _FilePanelState extends State<FilePanel> {
  FileEditorSession? _session;
  late SourceController _editor;
  final _focus = FocusNode();
  final _find = TextEditingController();
  final _replacement = TextEditingController();
  final _scroll = ScrollController();
  final _sourceFieldKey = GlobalKey();
  final _sourcePaintKey = GlobalKey();
  VoidCallback? _removeGuard;
  bool _syncing = false;
  bool _findVisible = false;
  bool _replaceExpanded = false;
  bool _matchCase = false;
  bool _wrap = true;
  bool _initialLocationApplied = false;
  double _fontSize = 12;
  /// The mounted text panel's view (original `useTextFileView`): `preview` or
  /// `source`, reset for each panel mount.
  String _view = 'source';
  String? _viewMount;
  int _matchCount = 0;
  int _matchIndex = 0;
  String? _markdownSource;
  String _markdownBody = '';
  List<(String, String)> _frontMatter = const [];
  RemoteHtmlPreview? _htmlPreview;
  bool _htmlBusy = false;
  final Map<String, Future<RemoteFileData>> _images = {};
  FileTransferCancellation _assetCancellation = FileTransferCancellation();
  bool _observing = false;
  VimEngine? _vim;
  bool _vimStarting = false;
  Object? _vimError;
  int _vimEpoch = 0;
  AppStore get _store => PaseoScope.of(context);
  FileEditorSession get session => _session!;
  String get _name => remoteBasename(session.path, cwd: widget.target.cwd);

  /// Original TextFilePanel (file-panels/source/panel.tsx): only the web/desktop app mounts
  /// the editable CodeMirror source; native shows every text file in the read-only
  /// FileSourceView — no caret, keyboard, save state or cursor position.
  static final bool _editorPlatform = !(Platform.isAndroid || Platform.isIOS);

  /// The editable buffer, when the mounted panel edits its file.
  FileEditorModel? get _model => session.model;

  /// Whether this panel edits the file (original `EditableTextFile`).
  bool get _editable => _model != null;

  /// The mounted text panel's text, once read.
  String? get _textValue => _model != null
      ? (_model!.status == FileEditorStatus.loading || _model!.status == FileEditorStatus.failed
            ? null
            : _model!.content)
      : session.text?.value;
  bool get _hasText => _textValue != null;
  String get _currentText => _textValue ?? '';

  /// The panel shows a Preview beside its Source (Markdown, HTML).
  bool get _hasPreview =>
      session.panel == markdownFilePanel || session.panel == htmlFilePanel;
  bool get _showPreview => _hasPreview && _view == 'preview';

  @override
  void initState() {
    super.initState();
    _editor = SourceController(
      path: widget.target.data.text('path', widget.target.id ?? ''),
    );
    _editor.addListener(_onEditor);
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_session == null) _start();
    if (_session != null) unawaited(_syncVim());
    _syncObserving();
  }

  /// Original FilePane: a hidden tab or a backgrounded app stops observing;
  /// observing again reports what changed meanwhile.
  void _syncObserving() {
    final activity = PanelActivity.of(context);
    _observing = activity.visible && activity.appActive;
    _session?.setObserving(_observing);
  }

  @override
  void didUpdateWidget(FilePanel oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.target.key != widget.target.key) {
      _assetCancellation.cancel();
      _assetCancellation = FileTransferCancellation();
      _stopVim();
      _vimError = null;
      _removeGuard?.call();
      modifiedFileTabs.set(oldWidget.target.key, modified: false);
      _session?.removeListener(_onSession);
      _session?.dispose();
      _editor.removeListener(_onEditor);
      _editor.dispose();
      _editor = SourceController(
        path: widget.target.data.text('path', widget.target.id ?? ''),
      )..addListener(_onEditor);
      _images.clear();
      _initialLocationApplied = false;
      _viewMount = null;
      final preview = _htmlPreview;
      _htmlPreview = null;
      if (preview != null) unawaited(preview.close());
      _start();
    } else if (oldWidget.target.data['line'] != widget.target.data['line'] ||
        oldWidget.target.data['lineEnd'] != widget.target.data['lineEnd'] ||
        oldWidget.target.data['column'] != widget.target.data['column']) {
      // Original useTextFileView: each later line target switches to Source.
      if (widget.target.data['line'] != null) _view = 'source';
      _goToTargetLocation();
    }
  }

  /// The target's `line` (or a `line..lineEnd` range). The original reveal ignores the
  /// column (readonly/view.native.tsx, editor/view.web.tsx).
  void _goToTargetLocation() => _goToLocation(
    widget.target.data.integer('line', 1),
    lineEnd: widget.target.data['lineEnd'] is int ? widget.target.data['lineEnd'] as int : null,
  );

  void _start() {
    final store = _store;
    _fontSize = (store.preferences['codeFontSize'] as num?)?.toDouble() ?? 12;
    _wrap = store.preferences['fileWordWrap'] != false;
    _session = FileEditorSession(
      client: store.clientFor(widget.target.serverId),
      cwd: widget.target.cwd,
      path: widget.target.data.text('path', widget.target.id ?? ''),
      editorPlatform: _editorPlatform,
      associations: () => jsonMap(store.preferences['filePanelAssociations']),
    )..addListener(_onSession);
    _removeGuard = store.registerCloseGuard(widget.target.key, _guardClose);
    session.setObserving(_observing);
  }

  void _onSession() {
    if (!mounted) return;
    // Original usePublishPanelInstanceAttributes({ modified }): the tab's dot.
    modifiedFileTabs.set(widget.target.key, modified: session.modified);
    // Original useTextFileView: each panel mount opens on Preview, or on
    // Source for a line target.
    final mount = session.panel == null ? null : '${session.panel!.id}:${session.presentation}';
    if (mount != _viewMount) {
      _viewMount = mount;
      _view = widget.target.data['line'] != null ? 'source' : 'preview';
      // Original `selectSourcePresentation`: text past the highlighting budget is plain.
      _editor.highlighting = session.presentation != FileTextPresentation.plain;
    }
    final text = _currentText;
    if (_vim != null && _vim!.text != text && !_vim!.pendingEdits) {
      unawaited(_vim!.synchronize(text));
    }
    if (_vim != null && _model?.status == FileEditorStatus.clean && _vim!.text == text) {
      unawaited(_vim!.markSaved(text));
    }
    if (_editor.text != text &&
        !(_vim?.pendingEdits ?? false) &&
        !(_editor.value.composing.isValid &&
            !_editor.value.composing.isCollapsed)) {
      final selection = _editor.selection;
      _syncing = true;
      _editor.value = TextEditingValue(
        text: text,
        selection: TextSelection(
          baseOffset: selection.baseOffset.clamp(0, text.length),
          extentOffset: selection.extentOffset.clamp(0, text.length),
        ),
      );
      _syncing = false;
    }
    if (!_initialLocationApplied && _hasText) {
      _initialLocationApplied = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && widget.target.data['line'] != null) _goToTargetLocation();
      });
    }
    _htmlPreview?.update(text);
    unawaited(_syncVim());
    setState(() {});
  }

  void _onEditor() {
    if (_syncing || _session == null) return;
    if (_vim != null && _vim!.ready) {
      unawaited(_vim!.applyFlutterValue(_editor.value));
    } else {
      _model?.edit(_editor.text);
    }
    if (mounted) setState(() {});
  }

  Future<void> _syncVim() async {
    if (!mounted || _session == null) return;
    final enabled =
        _store.preferences['vimKeybindings'] == true && VimEngine.supported;
    if (!enabled) {
      if (_vim != null) await _commitVimInput();
      if (!mounted) return;
      if (_vim != null || _vimStarting) _stopVim();
      _vimError = null;
      return;
    }
    final model = _model;
    if (_vim != null || _vimStarting || _vimError != null || model == null || !_hasText) {
      return;
    }
    _vimStarting = true;
    final epoch = ++_vimEpoch;
    final store = _store, targetKey = widget.target.key;
    try {
      final engine = await VimEngine.start(
        text: model.content,
        onText: model.edit,
        onSave: () async {
          await model.save();
          return model.status == FileEditorStatus.clean;
        },
        onClose: () async {
          await store.closeTab(targetKey);
        },
      );
      if (!mounted || epoch != _vimEpoch) {
        await engine.close();
        return;
      }
      _vim = engine;
      engine.addListener(_onVim);
      await engine.setSelection(_editor.selection);
      _onVim();
    } catch (error) {
      if (mounted && epoch == _vimEpoch) _vimError = error;
    } finally {
      if (mounted && epoch == _vimEpoch) {
        setState(() {
          _vimStarting = false;
        });
      }
    }
  }

  void _stopVim() {
    _vimEpoch++;
    _vimStarting = false;
    final engine = _vim;
    _vim = null;
    _editor.visualBlocks = const [];
    if (engine != null) {
      engine.removeListener(_onVim);
      unawaited(engine.close());
    }
  }

  void _onVim() {
    final engine = _vim;
    if (!mounted || engine == null) return;
    if (engine.error != null) {
      _vimError = engine.error;
      // Retain any committed platform text still awaiting engine processing.
      final retained = _editor.text;
      _stopVim();
      _model?.edit(retained);
      setState(() {});
      return;
    }
    _editor.visualBlocks = List<TextRange>.of(engine.visualBlocks);
    if (!engine.pendingEdits &&
        !(_editor.value.composing.isValid &&
            !_editor.value.composing.isCollapsed)) {
      _syncing = true;
      _editor.value = TextEditingValue(
        text: engine.text,
        selection: engine.selection,
      );
      _syncing = false;
    }
    setState(() {});
  }

  Future<void> _commitVimInput() async {
    final engine = _vim;
    if (engine == null) return;
    if (_editor.value.composing.isValid &&
        !_editor.value.composing.isCollapsed) {
      _editor.value = _editor.value.copyWith(composing: TextRange.empty);
    }
    await engine.flush();
  }

  Future<void> _saveFile() async {
    await _commitVimInput();
    await _model?.save();
  }

  Future<void> _setVim(bool enabled) async {
    await _commitVimInput();
    if (!mounted) return;
    _store.preferences['vimKeybindings'] = enabled;
    _vimError = null;
    try {
      await _store.savePreferences();
      await _syncVim();
    } catch (error) {
      if (mounted) showError(context, error);
    }
    if (mounted) setState(() {});
  }

  /// Original `confirmDiscardModifiedTab`: a tab with unsaved changes asks
  /// before closing, holding its pending save meanwhile; closing drops it.
  Future<bool> _guardClose() async {
    if (!mounted) return !session.modified;
    await _commitVimInput();
    if (!session.modified || !mounted) return true;
    final resumePendingSave = session.holdWrites();
    final confirmed = await confirmAction(
      context,
      title: tr(context, 'workspace.tabs.confirmations.unsavedTitle'),
      message: tr(context, 'workspace.tabs.confirmations.unsavedMessage'),
      confirmLabel: tr(context, 'workspace.tabs.confirmations.closeWithoutSaving'),
      destructive: true,
    );
    if (!confirmed) resumePendingSave();
    return confirmed;
  }

  /// Original `handleReload`: replaces the buffer with the file on disk,
  /// confirming first when it has unsaved changes.
  Future<void> _reload() async {
    await _commitVimInput();
    final model = _model;
    if (!mounted || model == null) return;
    if (model.modified &&
        !await confirmAction(
          context,
          title: tr(context, 'panels.file.editor.reloadTitle'),
          message: tr(context, 'panels.file.editor.reloadMessage'),
          confirmLabel: tr(context, 'panels.file.editor.reload'),
          destructive: true,
        )) {
      return;
    }
    await model.reload();
  }

  /// Original `handleOverwrite`.
  Future<void> _overwrite() async {
    await _commitVimInput();
    await _model?.overwrite();
  }

  /// Original line reveal: the source view scrolls the line to the middle of the view
  /// (readonly/view.native.tsx `scrollToIndex` with `viewPosition: 0.5`; editor/view.web.tsx
  /// `scrollIntoView(from, { y: "center" })`). Only the editor also selects it — the line's
  /// start, or the whole `line..lineEnd` range — and neither takes focus; [focus] is for the
  /// explicit "Go to line" command, which also honours [column].
  void _goToLocation(int line, {int column = 1, int? lineEnd, bool focus = false}) {
    setState(() {
      _view = 'source';
    });
    final start = line < 1 ? 1 : line;
    if (_editable) {
      _editor.selection = lineEnd != null && lineEnd > start
          ? TextSelection(
              baseOffset: _editor.offsetAt(start, 1),
              extentOffset: _editor.offsetAt(lineEnd, 1 << 30),
            )
          : TextSelection.collapsed(offset: _editor.offsetAt(start, column < 1 ? 1 : column));
      if (focus) _focus.requestFocus();
    }
    WidgetsBinding.instance.addPostFrameCallback((_) => _revealLine(start));
  }

  /// Scrolls [line] to the middle of the source view, measured with the editor's own
  /// caret geometry so wrapped lines and text scaling stay exact.
  void _revealLine(int line) {
    if (!mounted || !_scroll.hasClients) return;
    final position = _scroll.position;
    final editable = _sourceEditable(_sourceFieldKey);
    double top, height;
    if (editable != null && editable.hasSize) {
      final caret = editable.getLocalRectForCaret(
        TextPosition(offset: _editor.offsetAt(line, 1)),
      );
      // The caret rect carries the current scroll as its paint offset.
      top = caret.top + position.pixels;
      height = caret.height;
    } else {
      height = _fontSize * 1.45;
      top = (line - 1) * height;
    }
    _scroll.jumpTo(
      (top + height / 2 - position.viewportDimension / 2).clamp(0, position.maxScrollExtent),
    );
  }

  List<int> _matches() {
    final needle = _matchCase ? _find.text : _find.text.toLowerCase();
    final haystack = _matchCase ? _editor.text : _editor.text.toLowerCase();
    final matches = <int>[];
    if (needle.isEmpty) return matches;
    for (var offset = 0; offset <= haystack.length - needle.length;) {
      final found = haystack.indexOf(needle, offset);
      if (found < 0) break;
      matches.add(found);
      offset = found + needle.length;
    }
    return matches;
  }

  /// Live match count while the query changes; navigation picks the current match.
  void _countMatches() {
    setState(() {
      _matchCount = _matches().length;
      _matchIndex = 0;
    });
  }

  void _findNext({bool previous = false}) {
    final matches = _matches();
    setState(() {
      _matchCount = matches.length;
      _matchIndex = 0;
      if (matches.isNotEmpty) _view = 'source';
    });
    if (matches.isEmpty) return;
    final selection = _editor.selection;
    final next = previous
        ? matches.where((index) => index < selection.start).lastOrNull ??
              matches.last
        : matches.where((index) => index >= selection.end).firstOrNull ??
              matches.first;
    _editor.selection = TextSelection(
      baseOffset: next,
      extentOffset: next + _find.text.length,
    );
    setState(() {
      _matchIndex = matches.indexOf(next) + 1;
    });
    _focus.requestFocus();
  }

  void _replaceSelection() {
    if (!_editable || !_hasText) return;
    final selection = _editor.selection;
    if (!selection.isValid || selection.isCollapsed) {
      _findNext();
      return;
    }
    final selected = selection.textInside(_editor.text);
    if ((_matchCase ? selected : selected.toLowerCase()) !=
        (_matchCase ? _find.text : _find.text.toLowerCase())) {
      _findNext();
      return;
    }
    _editor.value = TextEditingValue(
      text: _editor.text.replaceRange(
        selection.start,
        selection.end,
        _replacement.text,
      ),
      selection: TextSelection.collapsed(
        offset: selection.start + _replacement.text.length,
      ),
    );
    _findNext();
  }

  Future<void> _replaceAll() async {
    if (!_editable || !_hasText || _find.text.isEmpty) return;
    await _commitVimInput();
    if (!mounted) return;
    final pattern = RegExp(
      RegExp.escape(_find.text),
      caseSensitive: _matchCase,
      unicode: true,
    );
    final replaced = _editor.text.replaceAllMapped(
      pattern,
      (_) => _replacement.text,
    );
    if (replaced == _editor.text) return;
    final offset = _editor.selection.extentOffset.clamp(0, replaced.length);
    _editor.value = TextEditingValue(
      text: replaced,
      selection: TextSelection.collapsed(offset: offset),
    );
    await _commitVimInput();
    if (mounted) {
      setState(() {
        _matchCount = pattern.allMatches(_editor.text).length;
      });
    }
  }

  void _attach({bool selectionOnly = false}) {
    JsonMap selection = {'kind': 'whole_file'};
    if (selectionOnly &&
        _editor.selection.isValid &&
        !_editor.selection.isCollapsed) {
      final range = _editor.selection;
      selection = {
        'kind': 'line_range',
        'startLine': _editor.locationAt(range.start).$1,
        'endLine': _editor
            .locationAt((range.end - 1).clamp(range.start, range.end))
            .$1,
      };
    }
    ChatDrafts.instance.addAttachments(
      widget.target.serverId,
      widget.target.workspaceId,
      [
        {
          'kind': 'workspace_file',
          'path': remoteRelative(widget.target.cwd, session.path),
          'selection': selection,
        },
      ],
    );
    showToast(context, 'File reference added to chat; unsaved edits are not uploaded');
  }

  Future<void> _download({bool share = false, bool open = false}) async {
    await _commitVimInput();
    if (!mounted) return;
    await saveRemoteFile(
      context,
      client: session.client,
      cwd: widget.target.cwd,
      path: session.path,
      share: share,
      open: open,
      currentBytes: _model?.modified ?? false
          ? Uint8List.fromList(utf8.encode(_model!.fileContent))
          : null,
      mimeType: session.stat?.mimeType,
    );
  }

  Future<void> _openHtml() async {
    if (_htmlBusy) return;
    _htmlBusy = true;
    final targetKey = widget.target.key;
    try {
      await _commitVimInput();
      if (!mounted) return;
      if (_htmlPreview == null) {
        final preview = await RemoteHtmlPreview.create(
          client: session.client,
          cwd: widget.target.cwd,
          path: session.path,
          content: _currentText,
          onError: (error) {
            if (mounted) showError(context, error);
          },
        );
        if (!mounted || widget.target.key != targetKey) {
          await preview.close();
          return;
        }
        _htmlPreview = preview;
      }
      _htmlPreview!.update(_currentText);
      await _htmlPreview!.open();
    } catch (error) {
      if (mounted) showError(context, error);
    } finally {
      _htmlBusy = false;
    }
  }

  Future<void> _openLocalEditor(BuildContext anchor) async {
    try {
      final desktop = DesktopHost.instance;
      if (desktop == null) {
        throw StateError('Local desktop host is not available');
      }
      final status = jsonMap(await desktop.invoke('desktop_daemon_status'));
      if (status.text('serverId') != widget.target.serverId) {
        throw StateError(
          'This file belongs to a different host. Open a downloaded copy instead.',
        );
      }
      final targets = await DesktopIntegrations.instance.listEditorTargets();
      if (!mounted || !anchor.mounted) return;
      final target = await showPaseoMenu<DesktopEditorTarget>(
        anchor,
        title: 'Open with',
        anchor: paseoAnchorRect(anchor),
        alignEnd: true,
        entries: [
          for (final target in targets)
            PaseoMenuEntry.item(value: target, label: target.label),
        ],
      );
      if (target == null) return;
      final (line, column) = _editor.locationAt(_editor.selection.extentOffset);
      await DesktopIntegrations.instance.openEditor(
        editorId: target.id,
        workspacePath: widget.target.cwd,
        filePath: remoteAbsolute(widget.target.cwd, session.path),
        line: line,
        column: column,
      );
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  bool get _localHost {
    if (!(Platform.isLinux || Platform.isMacOS || Platform.isWindows)) {
      return false;
    }
    final host = _store.hostFor(widget.target.serverId);
    if (host == null ||
        host.originServerId != null ||
        !session.client.serverInfo.flag('desktopManaged')) {
      return false;
    }
    final endpoint = host.config.endpoint;
    if (endpoint.startsWith('unix:') ||
        endpoint.startsWith('pipe:') ||
        endpoint.startsWith('/')) {
      return true;
    }
    final uri = Uri.tryParse(
      endpoint.contains('://') ? endpoint : 'tcp://$endpoint',
    );
    return ['localhost', '127.0.0.1', '::1'].contains(uri?.host);
  }

  Future<void> _link(String? href) async {
    if (href == null || href.isEmpty) return;
    try {
      final uri = Uri.parse(href);
      if (['https', 'http', 'mailto'].contains(uri.scheme)) {
        if (!await launchUrl(uri, mode: LaunchMode.externalApplication)) {
          throw StateError('Could not open $href');
        }
      } else if (uri.scheme.isEmpty || uri.scheme == 'file') {
        final filePath = uri.scheme == 'file'
            ? Uri(scheme: 'file', host: uri.host, path: uri.path).toFilePath(
                windows: RegExp(r'^[A-Za-z]:|^\\\\')
                    .hasMatch(widget.target.cwd),
              )
            : remoteAbsolute(
                remoteParent(remoteAbsolute(widget.target.cwd, session.path)),
                uri.path,
              );
        final location = RegExp(r'^L?(\d+)(?::(\d+))?')
            .firstMatch(uri.fragment);
        _store.open(
          PanelTarget(
            kind: 'file',
            serverId: widget.target.serverId,
            workspaceId: widget.target.workspaceId,
            cwd: widget.target.cwd,
            id: filePath,
            title: remoteBasename(filePath),
            data: {
              'path': filePath,
              if (location != null) 'line': int.parse(location[1]!),
              if (location?[2] != null) 'column': int.parse(location![2]!),
            },
          ),
        );
      } else {
        throw StateError('This link scheme is not allowed: ${uri.scheme}');
      }
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  Widget _markdownImage(Uri uri, String? title, String? alt) {
    if (uri.scheme == 'http' || uri.scheme == 'https') {
      return Image.network(
        uri.toString(),
        errorBuilder: (_, error, _) => Text(alt ?? error.toString()),
      );
    }
    if (uri.scheme.isNotEmpty && uri.scheme != 'file') {
      return Text(alt ?? 'Unsupported image URI');
    }
    final path = uri.scheme == 'file'
        ? Uri(scheme: 'file', host: uri.host, path: uri.path).toFilePath(
            windows: RegExp(r'^[A-Za-z]:|^\\\\').hasMatch(widget.target.cwd),
          )
        : remoteAbsolute(
            remoteParent(remoteAbsolute(widget.target.cwd, session.path)),
            uri.path,
          );
    final future = _images.putIfAbsent(
      path,
      () =>
          RemoteFileService(session.client)
              .read(widget.target.cwd, path, cancellation: _assetCancellation),
    );
    return FutureBuilder<RemoteFileData>(
      future: future,
      builder: (context, snapshot) {
        if (snapshot.hasError) return Text(alt ?? snapshot.error.toString());
        if (!snapshot.hasData) {
          return SizedBox.square(
            dimension: 16,
            child: CircularProgressIndicator(
              strokeWidth: 1.5,
              color: PaseoColors.of(context).muted,
            ),
          );
        }
        return _image(snapshot.data!.bytes, snapshot.data!.mime);
      },
    );
  }

  void _setView(String value) {
    setState(() {
      _view = value;
    });
  }

  /// Source editor in the original code font at 1.45 line height. Compact layouts keep the
  /// native reader's geometry (56px gutter, numbers 12px before the text, no rule); desktop
  /// keeps CodeMirror's (`.cm-gutters` rule, 5/3px number padding, foreground active line
  /// number, 6px line inset and 16px content padding).
  Widget _source() {
    _editor.syntaxTheme = stringValue(_store.preferences['syntaxTheme'], 'one');
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final scaler = MediaQuery.textScalerOf(context);
    final style = PaseoTypography.of(context).codeStyle.copyWith(
      fontSize: _fontSize,
      height: 1.45,
      fontWeight: FontWeight.normal,
    );
    final double gutter;
    if (compact) {
      gutter = 56;
    } else {
      final digits = '${_editor.locationAt(_editor.text.length).$1}'.length;
      final measure = TextPainter(
        text: TextSpan(text: '9' * digits, style: style),
        textDirection: TextDirection.ltr,
        textScaler: scaler,
      )..layout();
      gutter = (measure.width < 20 ? 20 : measure.width).ceilToDouble() + 8 + 1;
      measure.dispose();
    }
    final padding = compact
        ? EdgeInsets.zero
        : const EdgeInsets.fromLTRB(6, 16, 2, 16);
    final field = Focus(
      onKeyEvent: (_, event) {
        final keyboard = HardwareKeyboard.instance;
        if ((keyboard.isControlPressed || keyboard.isMetaPressed) &&
            (event.logicalKey == LogicalKeyboardKey.keyS ||
                event.logicalKey == LogicalKeyboardKey.keyF)) {
          return KeyEventResult.ignored;
        }
        return _vim?.handleKey(event) ?? KeyEventResult.ignored;
      },
      child: TextField(
        key: _sourceFieldKey,
        controller: _editor,
        focusNode: _focus,
        scrollController: _scroll,
        readOnly:
            !_editable ||
            _vimStarting ||
            (_vim != null && !_vim!.acceptsText),
        showCursor: _editable,
        cursorWidth: _vim != null && !_vim!.acceptsText ? _fontSize * 0.6 : 2,
        cursorColor: theme.colorScheme.onSurface,
        expands: true,
        textAlignVertical: TextAlignVertical.top,
        maxLines: null,
        minLines: null,
        keyboardType: TextInputType.multiline,
        autocorrect: false,
        enableSuggestions: false,
        smartDashesType: SmartDashesType.disabled,
        smartQuotesType: SmartQuotesType.disabled,
        style: style,
        textDirection: TextDirection.ltr,
        decoration: InputDecoration(
          border: InputBorder.none,
          enabledBorder: InputBorder.none,
          focusedBorder: InputBorder.none,
          filled: false,
          isDense: true,
          contentPadding: padding,
        ),
      ),
    );
    final source = _wrap
        ? field
        : LayoutBuilder(
            builder: (context, constraints) {
              final width =
                  (_editor.intrinsicWidth(style, scaler) + padding.horizontal + 2)
                      .clamp(constraints.maxWidth, double.infinity);
              return SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: SizedBox(
                  width: width,
                  height: constraints.maxHeight,
                  child: field,
                ),
              );
            },
          );
    return CustomPaint(
      key: _sourcePaintKey,
      foregroundPainter: _SourceLineNumbers(
        editor: _editor,
        scroll: _scroll,
        fieldKey: _sourceFieldKey,
        paintKey: _sourcePaintKey,
        style: style.copyWith(color: colors.muted),
        scaler: scaler,
        gutter: gutter,
        numberPadding: compact ? 12 : 3,
        activeColor: compact ? null : theme.colorScheme.onSurface,
        border: compact ? null : theme.dividerColor,
      ),
      child: Row(children: [
        SizedBox(width: gutter),
        Expanded(child: source),
      ]),
    );
  }

  Widget _image(Uint8List bytes, String mime) {
    if (mime.split(';').first == 'image/svg+xml') return SvgPicture.memory(bytes);
    return Image.memory(
      bytes,
      errorBuilder: (_, error, _) => ErrorState(error: error),
    );
  }

  Widget _markdownPreview() {
    final text = _currentText;
    if (_markdownSource != text) {
      _markdownSource = text;
      _markdownBody = text;
      _frontMatter = const [];
      final match = RegExp(r'^\uFEFF?---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)')
          .firstMatch(text);
      if (match != null) {
        try {
          final metadata = loadYaml(match.group(1)!);
          if (metadata is Map) {
            bool scalar(Object? value) => value == null || value is String || value is num || value is bool;
            String format(Object? value) {
              if (scalar(value)) return value?.toString() ?? 'null';
              if (value is List && value.every(scalar)) {
                return value.map((item) => item?.toString() ?? 'null').join(', ');
              }
              return const JsonEncoder.withIndent('  ').convert(value);
            }
            _frontMatter = metadata.entries.map((entry) => (entry.key.toString(), format(entry.value))).toList();
            _markdownBody = text.substring(match.end);
          }
        } catch (_) {
          // Invalid or non-representable metadata remains visible as source.
          _frontMatter = const [];
        }
      }
    }
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final base = theme.textTheme.bodyMedium!.copyWith(
      fontSize: typography.uiBaseFontSize,
      height: 20 / typography.uiBaseFontSize,
    );
    return LayoutBuilder(builder: (context, constraints) {
      final outer = constraints.maxWidth < 768 ? 12.0 : 16.0;
      final frameWidth =
          (constraints.maxWidth - outer * 2).clamp(0.0, typography.contentMaxWidth) - 16;
      final keyWidth = (frameWidth * .3).clamp(0.0, 180.0);
      return SingleChildScrollView(
        padding: EdgeInsets.symmetric(horizontal: outer, vertical: 16),
        child: Align(
          alignment: Alignment.topCenter,
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: typography.contentMaxWidth),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 8),
              child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
                if (_frontMatter.isNotEmpty)
                  Container(
                    margin: const EdgeInsets.only(bottom: 24),
                    clipBehavior: Clip.antiAlias,
                    decoration: BoxDecoration(
                      border: Border.all(color: theme.dividerColor),
                      borderRadius: BorderRadius.circular(8),
                    ),
                    child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
                      for (final (index, row) in _frontMatter.indexed)
                        DecoratedBox(
                          decoration: BoxDecoration(
                            border: index == 0
                                ? null
                                : Border(top: BorderSide(color: theme.dividerColor)),
                          ),
                          child: IntrinsicHeight(
                            child: Row(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
                              SizedBox(
                                width: keyWidth,
                                  child: Container(
                                    color: colors.surface2,
                                    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                                    child: SelectableText(
                                      row.$1,
                                      style: base.copyWith(
                                        fontFamily: typography.monoFontFamily,
                                        fontFamilyFallback: typography.monoFontFamilyFallback,
                                        color: colors.muted,
                                      ),
                                    ),
                                  ),
                              ),
                              Expanded(
                                child: Padding(
                                  padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                                  child: SelectableText(row.$2, style: base),
                                ),
                              ),
                            ]),
                          ),
                        ),
                    ]),
                  ),
                PaseoMarkdown(
                  data: _markdownBody,
                  onTapLink: _link,
                  imageBuilder: _markdownImage,
                ),
              ]),
            ),
          ),
        ),
      );
    });
  }

  Widget _content() {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final base = theme.textTheme.bodyMedium!;
    final muted = base.copyWith(color: colors.muted);
    final danger = base.copyWith(color: colors.danger);
    final stat = session.stat!;
    final panel = session.panel;
    // Original FilePanelError / the text panels' failed read: destructive base text.
    Widget failure(Object? error, {VoidCallback? retry}) => _centerState([
      Text(
        error != null ? errorMessage(error) : tr(context, 'panels.file.failedToLoad'),
        textAlign: TextAlign.center,
        style: danger,
      ),
      if (retry != null)
        PaseoButton(
          label: tr(context, 'common.actions.retry'),
          variant: PaseoButtonVariant.outline,
          size: PaseoControlSize.sm,
          onPressed: retry,
        ),
    ]);
    if (panel == imageFilePanel) {
      final reader = session.bytes!;
      if (reader.loading) return LoadingState(label: tr(context, 'panels.file.loading'));
      if (reader.error != null) return failure(reader.error);
      return InteractiveViewer(
        minScale: 0.1,
        maxScale: 16,
        child: Center(child: _image(reader.value!, stat.mimeType ?? '')),
      );
    }
    if (panel == null) {
      // Original FilePane without a panel: muted base "Binary preview unavailable" over the
      // size, centred with 12px gaps and 16px padding.
      return _centerState([
        Text(tr(context, 'panels.file.binaryPreviewUnavailable'), textAlign: TextAlign.center, style: muted),
        Text(_fileSize(stat.size), textAlign: TextAlign.center, style: muted),
        Wrap(spacing: 8, runSpacing: 8, alignment: WrapAlignment.center, children: [
          PaseoButton(
            icon: LucideIcons.download300,
            label: 'Save / Download',
            variant: PaseoButtonVariant.outline,
            size: PaseoControlSize.sm,
            onPressed: _download,
          ),
          PaseoButton(
            icon: LucideIcons.externalLink300,
            label: 'Open a downloaded copy',
            variant: PaseoButtonVariant.outline,
            size: PaseoControlSize.sm,
            onPressed: () => _download(open: true),
          ),
        ]),
      ]);
    }
    // Original TooLargeTextFile: refused before reading.
    if (session.presentation == FileTextPresentation.unsupported) {
      return _centerState([
        Text(tr(context, 'panels.file.tooLargeToDisplay'), textAlign: TextAlign.center, style: muted),
        Text(_fileSize(stat.size), textAlign: TextAlign.center, style: muted),
      ]);
    }
    final model = _model;
    if (model != null) {
      if (model.status == FileEditorStatus.loading) {
        return LoadingState(label: tr(context, 'panels.file.loading'));
      }
      if (model.status == FileEditorStatus.failed) {
        return failure(model.error, retry: () => unawaited(model.reload()));
      }
    } else {
      final reader = session.text!;
      if (reader.loading) return LoadingState(label: tr(context, 'panels.file.loading'));
      if (reader.error != null) return failure(reader.error);
    }
    if (_showPreview && panel == markdownFilePanel) return _markdownPreview();
    if (_showPreview && panel == htmlFilePanel) {
      return _centerState([
        Icon(LucideIcons.fileCode300, size: 32, color: colors.muted),
        Text('HTML previews open in your system browser.', textAlign: TextAlign.center, style: base),
        Text(
          'Relative workspace assets are served locally; scripts are disabled.',
          textAlign: TextAlign.center,
          style: muted,
        ),
        Wrap(spacing: 8, runSpacing: 8, alignment: WrapAlignment.center, children: [
          PaseoButton(
            icon: LucideIcons.externalLink300,
            label: 'Open HTML preview',
            variant: PaseoButtonVariant.primary,
            size: PaseoControlSize.sm,
            onPressed: _openHtml,
          ),
          PaseoButton(
            label: tr(context, 'panels.file.editor.source'),
            variant: PaseoButtonVariant.outline,
            size: PaseoControlSize.sm,
            onPressed: () => _setView('source'),
          ),
        ]),
      ]);
    }
    return _source();
  }

  /// Original FilePane (file-pane/pane.tsx) before a panel mounts: Loading, or the
  /// destructive base "File not found" / observation error over an outline sm Retry,
  /// centred with 12px gaps and 16px padding — without the panel's toolbar.
  Widget _loadState() {
    final status = session.status;
    if (status.status == 'loading') {
      return LoadingState(label: tr(context, 'panels.file.loading'));
    }
    final error = status.error;
    return _centerState([
      Text(
        status.status == 'missing'
            ? tr(context, 'panels.file.notFound')
            : error != null
            ? errorMessage(error)
            : tr(context, 'panels.file.failedToLoad'),
        textAlign: TextAlign.center,
        style: Theme.of(context).textTheme.bodyMedium!.copyWith(color: PaseoColors.of(context).danger),
      ),
      PaseoButton(
        label: tr(context, 'common.actions.retry'),
        variant: PaseoButtonVariant.outline,
        size: PaseoControlSize.sm,
        onPressed: session.retry,
      ),
    ]);
  }

  Widget _centerState(List<Widget> children) => Center(
    child: SingleChildScrollView(
      padding: const EdgeInsets.all(16),
      child: Column(mainAxisSize: MainAxisSize.min, spacing: 12, children: children),
    ),
  );

  void _openFind() {
    setState(() {
      _findVisible = true;
    });
  }

  void _closeFind() {
    setState(() {
      _findVisible = false;
    });
    _focus.requestFocus();
  }

  /// Original `PaneFind` (pane-find/index.tsx): a floating card at most 340px wide (surface1,
  /// 1px border, 8px radius, md shadow, 6px padding, 4px gaps) with a replace disclosure rail
  /// spanning both rows, surface2 fields (28px desktop / 32px compact, 6px radius, 8px inset)
  /// carrying the small muted match status, and toolbar buttons; Enter finds the next match,
  /// Shift+Enter the previous one and Escape closes.
  Widget _findWidget() {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final control = paneToolbarButtonSize(context);
    final rowHeight = control > 28 ? control : 28.0;
    final dark = theme.brightness == Brightness.dark;
    final canReplace = _editable;
    final canNavigate = _matchCount > 0;
    final status = _find.text.isEmpty
        ? ''
        : _matchCount == 0
        ? tr(context, 'paneFind.noMatches')
        : _matchIndex > 0
        ? tr(context, 'paneFind.position', args: {'current': '$_matchIndex', 'total': '$_matchCount'})
        : tr(context, 'paneFind.total', args: {'total': '$_matchCount'});
    final text = theme.textTheme.bodyMedium!.copyWith(height: 1.4);
    Widget field(
      TextEditingController controller,
      String label, {
      bool autofocus = false,
      Widget? trailing,
      ValueChanged<String>? onChanged,
      ValueChanged<String>? onSubmitted,
    }) => SizedBox(
      height: rowHeight,
      child: TextField(
        controller: controller,
        autofocus: autofocus,
        autocorrect: false,
        enableSuggestions: false,
        style: text,
        textInputAction: onSubmitted == null ? TextInputAction.done : TextInputAction.search,
        decoration: paseoInputDecoration(context, hintText: label, size: PaseoControlSize.sm)
            .copyWith(
              contentPadding: EdgeInsets.symmetric(
                horizontal: 8,
                vertical: (rowHeight - (text.fontSize! * 1.4) - 2) / 2,
              ),
              suffixIcon: trailing == null
                  ? null
                  : Padding(padding: const EdgeInsets.only(right: 8), child: trailing),
              suffixIconConstraints: const BoxConstraints(),
            ),
        onChanged: onChanged,
        onSubmitted: onSubmitted,
      ),
    );
    final replaceAction = compact ? PaseoControlSize.sm : PaseoControlSize.xs;
    return Focus(
      canRequestFocus: false,
      skipTraversal: true,
      onKeyEvent: (_, event) {
        if (event is KeyDownEvent && event.logicalKey == LogicalKeyboardKey.escape) {
          _closeFind();
          return KeyEventResult.handled;
        }
        return KeyEventResult.ignored;
      },
      child: Semantics(
        label: tr(context, 'paneFind.title'),
        container: true,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 340),
          child: Container(
            padding: const EdgeInsets.all(6),
            decoration: BoxDecoration(
              color: colors.surface1,
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
              boxShadow: [
                BoxShadow(
                  color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                  offset: const Offset(0, 4),
                  blurRadius: dark ? 8 : 16,
                ),
              ],
            ),
            child: Row(spacing: 4, children: [
              if (canReplace)
                PaneToolbarButton(
                  tooltip: tr(context, 'paneFind.toggleReplace'),
                  icon: _replaceExpanded ? LucideIcons.chevronDown300 : LucideIcons.chevronRight300,
                  onPressed: () => setState(() {
                    _replaceExpanded = !_replaceExpanded;
                  }),
                ),
              Expanded(
                child: Column(mainAxisSize: MainAxisSize.min, spacing: 4, children: [
                  Row(spacing: 4, children: [
                    Expanded(
                      child: field(
                        _find,
                        tr(context, 'paneFind.placeholder'),
                        autofocus: true,
                        trailing: status.isEmpty
                            ? null
                            : Text(
                                status,
                                semanticsLabel: tr(context, 'paneFind.matches'),
                                style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                              ),
                        onChanged: (_) => _countMatches(),
                        onSubmitted: (_) => _findNext(
                          previous: HardwareKeyboard.instance.isShiftPressed,
                        ),
                      ),
                    ),
                    PaneToolbarButton(
                      tooltip: 'Match case',
                      icon: LucideIcons.caseSensitive300,
                      selected: _matchCase,
                      onPressed: () {
                        setState(() {
                          _matchCase = !_matchCase;
                        });
                        _countMatches();
                      },
                    ),
                    PaneToolbarButton(
                      tooltip: tr(context, 'paneFind.previous'),
                      icon: LucideIcons.arrowUp300,
                      onPressed: canNavigate ? () => _findNext(previous: true) : null,
                    ),
                    PaneToolbarButton(
                      tooltip: tr(context, 'paneFind.next'),
                      icon: LucideIcons.arrowDown300,
                      onPressed: canNavigate ? _findNext : null,
                    ),
                    PaneToolbarButton(
                      tooltip: tr(context, 'paneFind.close'),
                      icon: LucideIcons.x300,
                      onPressed: _closeFind,
                    ),
                  ]),
                  if (canReplace && _replaceExpanded)
                    Row(spacing: 4, children: [
                      Expanded(child: field(_replacement, tr(context, 'paneFind.replaceWith'))),
                      PaseoButton(
                        label: tr(context, 'paneFind.replace'),
                        variant: PaseoButtonVariant.ghost,
                        size: replaceAction,
                        onPressed: canNavigate ? _replaceSelection : null,
                      ),
                      PaseoButton(
                        label: tr(context, 'paneFind.replaceAll'),
                        variant: PaseoButtonVariant.ghost,
                        size: replaceAction,
                        onPressed: canNavigate ? _replaceAll : null,
                      ),
                    ]),
                ]),
              ),
            ]),
          ),
        ),
      ),
    );
  }

  /// Editor preferences in the original adaptive sheet: setting rows with a base label, an
  /// optional small muted hint and a [PaseoSwitch], and a code font size stepper.
  Future<void> _settings() async {
    await showAdaptiveSheet<void>(
      context: context,
      builder: (sheetContext) => StatefulBuilder(
        builder: (sheetContext, update) {
          final theme = Theme.of(sheetContext);
          final colors = PaseoColors.of(sheetContext);
          Widget row(String label, Widget trailing, {String? hint}) => Row(
            spacing: 12,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  spacing: 2,
                  children: [
                    Text(label, style: theme.textTheme.bodyMedium),
                    if (hint != null)
                      Text(hint, style: theme.textTheme.bodySmall!.copyWith(color: colors.muted)),
                  ],
                ),
              ),
              trailing,
            ],
          );
          void setFont(double value) {
            setState(() {
              _fontSize = value;
            });
            update(() {});
          }
          return SheetContents(
            title: 'Editor settings',
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 16,
              children: [
                row('Word wrap', PaseoSwitch(
                  value: _wrap,
                  semanticLabel: 'Word wrap',
                  onChanged: (value) {
                    setState(() {
                      _wrap = value;
                    });
                    update(() {});
                  },
                )),
                row('Syntax highlighting', PaseoSwitch(
                  value: _editor.highlighting,
                  semanticLabel: 'Syntax highlighting',
                  onChanged: (value) {
                    setState(() {
                      _editor.highlighting = value;
                    });
                    update(() {});
                  },
                )),
                if (VimEngine.supported)
                  row(
                    'Vim keybindings',
                    PaseoSwitch(
                      value: _store.preferences['vimKeybindings'] == true,
                      semanticLabel: 'Vim keybindings',
                      onChanged: (value) async {
                        await _setVim(value);
                        if (sheetContext.mounted) update(() {});
                      },
                    ),
                    hint: 'Native editor powered by the bundled Neovim engine',
                  ),
                row('Code font size', Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: [
                  PaseoButton(
                    icon: LucideIcons.minus300,
                    tooltip: 'Smaller',
                    variant: PaseoButtonVariant.outline,
                    size: PaseoControlSize.sm,
                    onPressed: _fontSize > _minCodeFont ? () => setFont(_fontSize - 1) : null,
                  ),
                  SizedBox(
                    width: 28,
                    child: Text(
                      '${_fontSize.round()}',
                      textAlign: TextAlign.center,
                      style: theme.textTheme.bodyMedium,
                    ),
                  ),
                  PaseoButton(
                    icon: LucideIcons.plus300,
                    tooltip: 'Larger',
                    variant: PaseoButtonVariant.outline,
                    size: PaseoControlSize.sm,
                    onPressed: _fontSize < _maxCodeFont ? () => setFont(_fontSize + 1) : null,
                  ),
                ])),
              ],
            ),
          );
        },
      ),
    );
    if (!mounted) return;
    _store.preferences['codeFontSize'] = _fontSize;
    _store.preferences['fileWordWrap'] = _wrap;
    try {
      await _store.savePreferences();
    } catch (error) {
      if (mounted) showError(context, error);
    }
  }

  static String _fileSize(int bytes) {
    if (bytes < 1024) return '$bytes B';
    if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
    return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
  }

  /// Original `TextFileToolbar` metadata: the file size and line count as small extraMuted
  /// whispers 8px apart (the file name and size for files without text).
  Widget _toolbarMetadata() {
    final whisper = Theme.of(context).textTheme.bodySmall!.copyWith(
      color: PaseoColors.of(context).extraMuted,
    );
    Widget text(String value, {String? semantics}) => Text(
      value,
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: whisper,
      semanticsLabel: semantics,
    );
    final stat = session.stat;
    if (!_hasText) {
      return Row(spacing: 8, children: [
        Flexible(child: Tooltip(message: session.path, child: text(_name))),
        if (stat != null) text(_fileSize(stat.size)),
      ]);
    }
    final size = _fileSize(stat?.size ?? 0);
    final lines = _editor.locationAt(_editor.text.length).$1;
    return Row(spacing: 8, children: [
      Flexible(
        child: text(size, semantics: tr(context, 'panels.file.editor.fileSize', args: {'size': size})),
      ),
      Flexible(
        child: text(tr(context, 'panels.file.editor.lines', args: {'count': '$lines'})),
      ),
    ]);
  }

  /// Original `TextFileToolbar` status: the unsaved 6px dot, a 14px spinner and muted
  /// "Saving…", a red "Save failed", the mono vim mode and the extraMuted cursor, 8px apart.
  Widget _toolbarStatus((int, int) location) {
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final small = Theme.of(context).textTheme.bodySmall!;
    final status = _model?.status;
    return Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: [
      if (status == FileEditorStatus.dirty)
        Semantics(
          label: tr(context, 'panels.file.editor.unsavedChanges'),
          child: Container(
            width: 6,
            height: 6,
            decoration: BoxDecoration(color: colors.extraMuted, shape: BoxShape.circle),
          ),
        ),
      if (status == FileEditorStatus.saving) ...[
        SizedBox.square(
          dimension: 14,
          child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.muted),
        ),
        Text(tr(context, 'panels.file.editor.saving'), style: small.copyWith(color: colors.muted)),
      ],
      if (status == FileEditorStatus.error)
        Text(tr(context, 'panels.file.editor.saveFailed'), style: small.copyWith(color: _red300)),
      if (_vim != null && !_showPreview)
        Text(
          _vim!.modeLabel,
          semanticsLabel: tr(context, 'panels.file.editor.vimMode', args: {'mode': _vim!.modeLabel}),
          style: small.copyWith(
            color: colors.muted,
            fontFamily: typography.monoFontFamily,
            fontFamilyFallback: typography.monoFontFamilyFallback,
          ),
        ),
      if (!_showPreview)
      Text(
        'Ln ${location.$1}, Col ${location.$2}',
        semanticsLabel: tr(
          context,
          'panels.file.editor.cursor',
          args: {'line': '${location.$1}', 'column': '${location.$2}'},
        ),
        style: small.copyWith(color: colors.extraMuted),
      ),
    ]);
  }

  /// The more-actions menu of the file toolbar, anchored under its button.
  Future<void> _moreMenu(BuildContext anchor, (int, int) location) async {
    final action = await showPaseoMenu<String>(
      anchor,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      width: 240,
      entries: [
        if (_hasText) ...[
          // The HTML panel's Preview opens in the system browser (no embedded browser).
          if (session.panel == htmlFilePanel) ...[
            const PaseoMenuEntry.item(
              value: 'html',
              label: 'Preview HTML in system browser',
              icon: LucideIcons.globe300,
            ),
            const PaseoMenuEntry.separator(),
          ],
          const PaseoMenuEntry.item(
            value: 'find',
            label: 'Find / Replace',
            icon: LucideIcons.search300,
          ),
          const PaseoMenuEntry.item(
            value: 'goto',
            label: 'Go to line',
            icon: LucideIcons.hash300,
          ),
          const PaseoMenuEntry.item(
            value: 'settings',
            label: 'Editor settings',
            icon: LucideIcons.settings2300,
          ),
          const PaseoMenuEntry.separator(),
        ],
        const PaseoMenuEntry.item(
          value: 'download',
          label: 'Save a copy / Download',
          icon: LucideIcons.download300,
        ),
        PaseoMenuEntry.item(
          value: 'share',
          label: tr(context, 'downloads.shareFile'),
          icon: LucideIcons.share2300,
        ),
        const PaseoMenuEntry.item(
          value: 'external',
          label: 'Open downloaded copy externally',
          icon: LucideIcons.externalLink300,
        ),
        if (_localHost)
          const PaseoMenuEntry.item(
            value: 'local',
            label: 'Open in editor / file manager',
            icon: LucideIcons.squarePen300,
          ),
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'chat',
          label: tr(context, 'workspace.fileActions.addToChat'),
          icon: LucideIcons.messageSquarePlus300,
        ),
        if (_hasText && !_editor.selection.isCollapsed)
          const PaseoMenuEntry.item(
            value: 'selection',
            label: 'Add selected lines to chat',
            icon: LucideIcons.textSelect300,
          ),
        PaseoMenuEntry.item(
          value: 'relative',
          label: tr(context, 'workspace.fileActions.copyRelativePath'),
          icon: LucideIcons.copy300,
        ),
        PaseoMenuEntry.item(
          value: 'absolute',
          label: tr(context, 'workspace.fileActions.copyPath'),
          icon: LucideIcons.copy300,
        ),
      ],
    );
    if (!mounted) return;
    switch (action) {
      case 'find':
        _openFind();
      case 'settings':
        await _settings();
      case 'html':
        _setView('preview');
        await _openHtml();
      case 'download':
        await _download();
      case 'share':
        await _download(share: true);
      case 'external':
        await _download(open: true);
      case 'local':
        if (anchor.mounted) await _openLocalEditor(anchor);
      case 'chat':
        _attach();
      case 'selection':
        _attach(selectionOnly: true);
      case 'absolute':
      case 'relative':
        final path = action == 'absolute'
            ? remoteAbsolute(widget.target.cwd, session.path)
            : remoteRelative(widget.target.cwd, session.path);
        await Clipboard.setData(ClipboardData(text: path));
        if (mounted) showCopiedToast(context);
      case 'goto':
        final line = await promptText(
          context,
          title: 'Go to line:column',
          initialValue: '${location.$1}:${location.$2}',
        );
        if (line != null && mounted) {
          final parts = line.split(':');
          _goToLocation(
            int.tryParse(parts.first) ?? 1,
            column: parts.length > 1 ? int.tryParse(parts[1]) ?? 1 : 1,
            focus: true,
          );
        }
    }
  }

  /// Original `FileConflictAlert` strip (file-panels/source/conflict-alert.tsx): at least
  /// 48px, 12/8 padding and 12px gaps on surface1 over a 1px bottom rule in [tone], an
  /// optional sm glyph, a base medium title over a small muted description, and small outline
  /// actions at the end (wrapping under the message on narrow panes).
  Widget _alert({
    required String title,
    String? description,
    Color? tone,
    IconData? icon,
    List<Widget> actions = const [],
  }) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final message = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      spacing: 4,
      children: [
        Text(
          title,
          style: theme.textTheme.bodyMedium!.copyWith(
            color: tone ?? theme.colorScheme.onSurface,
            fontWeight: FontWeight.w500,
          ),
        ),
        if (description != null)
          Text(description, style: theme.textTheme.bodySmall!.copyWith(color: colors.muted)),
      ],
    );
    final glyph = icon == null
        ? null
        : Icon(icon, size: densityIcon(context, 'sm'), color: tone ?? colors.muted);
    final buttons = Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: actions);
    return Container(
      constraints: const BoxConstraints(minHeight: 48),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: colors.surface1,
        border: Border(bottom: BorderSide(color: tone ?? theme.dividerColor)),
      ),
      child: LayoutBuilder(builder: (context, constraints) {
        final lead = Row(spacing: 12, children: [?glyph, Expanded(child: message)]);
        if (actions.isEmpty) return lead;
        if (constraints.maxWidth >= 420) {
          return Row(spacing: 12, children: [
            ?glyph,
            Expanded(child: message),
            buttons,
          ]);
        }
        return Column(crossAxisAlignment: CrossAxisAlignment.end, spacing: 8, children: [
          lead,
          buttons,
        ]);
      }),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_session == null) return const LoadingState();
    if (session.status.status != 'ready') {
      return ColoredBox(color: PaseoColors.of(context).surface0, child: _loadState());
    }
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final location = _editor.locationAt(_editor.selection.extentOffset);
    final model = _model;
    final status = model?.status;
    final preview = _hasText && _hasPreview;
    Widget outline(String label, VoidCallback onPressed) => PaseoButton(
      label: label,
      variant: PaseoButtonVariant.outline,
      size: PaseoControlSize.sm,
      onPressed: onPressed,
    );
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.keyS, control: true): () {
          unawaited(_saveFile());
        },
        const SingleActivator(LogicalKeyboardKey.keyS, meta: true): () {
          unawaited(_saveFile());
        },
        const SingleActivator(LogicalKeyboardKey.keyF, control: true): _openFind,
        const SingleActivator(LogicalKeyboardKey.keyF, meta: true): _openFind,
      },
      child: ColoredBox(
        color: colors.surface0,
        child: Column(
          children: [
            FilePaneToolbar(
              padding: EdgeInsets.only(left: 12, right: paneToolbarTrailingPadding(context)),
              children: [
                Expanded(child: _toolbarMetadata()),
                if (_hasText && _editable) ...[
                  const SizedBox(width: 12),
                  _toolbarStatus(location),
                ],
                if (preview) ...[
                  const SizedBox(width: 12),
                  PaseoSegmentedControl<String>(
                    size: PaseoControlSize.xs,
                    value: _view,
                    segments: [
                      PaseoSegment('preview', tr(context, 'panels.file.editor.preview')),
                      PaseoSegment('source', tr(context, 'panels.file.editor.source')),
                    ],
                    onChanged: _setView,
                  ),
                ],
                const SizedBox(width: 12),
                PaneToolbarControls(children: [
                  if (_hasText && model != null) ...[
                    PaneToolbarButton(
                      tooltip: tr(context, 'common.actions.save'),
                      icon: LucideIcons.save300,
                      onPressed: status == FileEditorStatus.dirty || status == FileEditorStatus.error
                          ? _saveFile
                          : null,
                    ),
                    PaneToolbarButton(
                      tooltip: tr(context, 'panels.file.editor.reload'),
                      icon: LucideIcons.rotateCw300,
                      onPressed: status == FileEditorStatus.saving ? null : _reload,
                    ),
                  ],
                  Builder(
                    builder: (anchor) => PaneToolbarButton(
                      tooltip: tr(context, 'workspace.fileActions.moreActions'),
                      icon: LucideIcons.ellipsis300,
                      onPressed: () => _moreMenu(anchor, location),
                    ),
                  ),
                ]),
              ],
            ),
            // Original FileConflictAlert: "changed" offers Overwrite (with unsaved
            // changes) and Reload; "deleted" only says the open copy is preserved.
            if (status == FileEditorStatus.conflict && model!.conflict == 'deleted')
              _alert(
                title: tr(context, 'panels.file.editor.deletedTitle'),
                description: tr(context, 'panels.file.editor.preservedDescription'),
                tone: _amber500,
                icon: LucideIcons.triangleAlert300,
              )
            else if (status == FileEditorStatus.conflict)
              _alert(
                title: tr(context, 'panels.file.editor.changedOnDisk'),
                description: model!.modified
                    ? tr(context, 'panels.file.editor.conflictDescription')
                    : null,
                tone: _amber500,
                icon: LucideIcons.triangleAlert300,
                actions: [
                  if (model.modified)
                    outline(tr(context, 'panels.file.editor.overwrite'), _overwrite),
                  outline(tr(context, 'panels.file.editor.reload'), _reload),
                ],
              ),
            if (_vimStarting)
              LinearProgressIndicator(
                minHeight: 2,
                color: colors.accent,
                backgroundColor: Colors.transparent,
              ),
            if (_vimError != null)
              _alert(
                title: 'Vim engine failed',
                description: errorMessage(_vimError!),
                tone: colors.danger,
                actions: [
                  outline(tr(context, 'common.actions.retry'), () {
                    setState(() {
                      _vimError = null;
                    });
                    unawaited(_syncVim());
                  }),
                  outline('Use normal keybindings', () => _setVim(false)),
                ],
              ),
            Expanded(
              child: Stack(children: [
                Positioned.fill(child: _content()),
                if (_findVisible && _hasText)
                  Positioned(
                    top: 8,
                    left: 8,
                    right: 8,
                    child: Align(alignment: Alignment.topRight, child: _findWidget()),
                  ),
              ]),
            ),
            if (_vim != null &&
                (_vim!.commandLine.isNotEmpty || _vim!.message.isNotEmpty))
              // CodeMirror `.cm-vim-panel`: 0/10px padding, 1.3em minimum height, in the
              // editor background with the bottom panel's 1px top border.
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 10),
                constraints: BoxConstraints(
                  minHeight: (PaseoTypography.of(context).codeStyle.fontSize ?? 13) * 1.3,
                ),
                alignment: Alignment.centerLeft,
                decoration: BoxDecoration(
                  color: colors.surface0,
                  border: Border(top: BorderSide(color: theme.dividerColor)),
                ),
                child: VimCommandLine(engine: _vim!, sourceFocus: _focus),
              ),
          ],
        ),
      ),
    );
  }

  @override
  void dispose() {
    _removeGuard?.call();
    modifiedFileTabs.set(widget.target.key, modified: false);
    _assetCancellation.cancel();
    _stopVim();
    _session?.removeListener(_onSession);
    _session?.dispose();
    _editor.removeListener(_onEditor);
    _editor.dispose();
    _find.dispose();
    _replacement.dispose();
    _focus.dispose();
    _scroll.dispose();
    final preview = _htmlPreview;
    if (preview != null) unawaited(preview.close());
    super.dispose();
  }
}

/// The render object of the source TextField at [fieldKey]: its caret geometry measures
/// lines for the gutter numbers and for line reveals.
RenderEditable? _sourceEditable(GlobalKey fieldKey) {
  EditableTextState? editableState;
  void findEditable(Element element) {
    if (element is StatefulElement && element.state is EditableTextState) {
      editableState = element.state as EditableTextState;
    } else if (editableState == null) {
      element.visitChildren(findEditable);
    }
  }
  fieldKey.currentContext?.visitChildElements(findEditable);
  return editableState?.renderEditable;
}

/// Paint only visible logical line numbers using the editor's own caret geometry.
/// This stays aligned with wrapped lines, text scaling, and the native IME. Numbers sit
/// right-aligned [numberPadding] before the [gutter]'s end; the caret's line takes
/// [activeColor] and a 1px [border] closes the gutter when given (CodeMirror `.cm-gutters`).
class _SourceLineNumbers extends CustomPainter {
  _SourceLineNumbers({
    required this.editor,
    required ScrollController scroll,
    required this.fieldKey,
    required this.paintKey,
    required this.style,
    required this.scaler,
    required this.gutter,
    required this.numberPadding,
    this.activeColor,
    this.border,
  }) : super(repaint: Listenable.merge([editor, scroll]));

  final SourceController editor;
  final GlobalKey fieldKey, paintKey;
  final TextStyle style;
  final TextScaler scaler;
  final double gutter, numberPadding;
  final Color? activeColor, border;

  @override
  void paint(Canvas canvas, Size size) {
    final editable = _sourceEditable(fieldKey);
    final box = paintKey.currentContext?.findRenderObject();
    if (editable == null || box is! RenderBox || !editable.hasSize) return;
    final origin = box.localToGlobal(Offset.zero);
    final first = editable.getPositionForPoint(Offset(
      editable.localToGlobal(Offset.zero).dx,
      origin.dy,
    ));
    var line = editor.locationAt(first.offset).$1;
    final last = editor.locationAt(editor.text.length).$1;
    final active = activeColor != null && editor.selection.isValid
        ? editor.locationAt(editor.selection.extentOffset).$1
        : -1;
    final right = gutter - (border == null ? 0 : 1) - numberPadding;
    final painter = TextPainter(textDirection: TextDirection.ltr, textScaler: scaler);
    canvas.save();
    canvas.clipRect(Rect.fromLTWH(0, 0, gutter, size.height));
    if (border != null) {
      canvas.drawLine(
        Offset(gutter - .5, 0),
        Offset(gutter - .5, size.height),
        Paint()..color = border!,
      );
    }
    while (line <= last) {
      final caret = editable.getLocalRectForCaret(TextPosition(offset: editor.offsetAt(line, 1)));
      final y = editable.localToGlobal(caret.topLeft).dy - origin.dy;
      if (y >= size.height) break;
      painter.text = TextSpan(
        text: '$line',
        style: line == active ? style.copyWith(color: activeColor) : style,
      );
      painter.layout();
      if (y + caret.height >= 0) {
        painter.paint(canvas, Offset(right - painter.width, y + (caret.height - painter.height) / 2));
      }
      line++;
    }
    canvas.restore();
    painter.dispose();
  }

  @override
  bool shouldRepaint(covariant _SourceLineNumbers oldDelegate) =>
      editor != oldDelegate.editor || style != oldDelegate.style ||
      scaler != oldDelegate.scaler || border != oldDelegate.border ||
      gutter != oldDelegate.gutter || numberPadding != oldDelegate.numberPadding ||
      activeColor != oldDelegate.activeColor;
}
