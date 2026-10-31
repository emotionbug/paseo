import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../core/client.dart';
import 'file_editor_model.dart';
import 'file_handle.dart';
import 'file_panel_resolve.dart';

/// Original `MAX_EDITABLE_BYTES`: the daemon refuses to write larger files.
const maxEditableFileBytes = 1024 * 1024;

/// How a text panel shows its file (original `TextFilePanel` +
/// `selectSourcePresentation`).
enum FileTextPresentation { editable, highlighted, plain, unsupported }

/// Original `SOURCE_PRESENTATION_BUDGETS` and `TextFilePanel`: files above the
/// platform's display budget are refused before reading; the editor platform
/// (original web/desktop) edits files up to 1 MiB; everything else is shown
/// read-only.
FileTextPresentation fileTextPresentation({
  required int size,
  required bool editorPlatform,
}) {
  final highlighted = editorPlatform ? 10 * 1024 * 1024 : 1024 * 1024;
  final plain = editorPlatform ? 50 * 1024 * 1024 : 10 * 1024 * 1024;
  if (size > plain) return FileTextPresentation.unsupported;
  if (editorPlatform && size <= maxEditableFileBytes) return FileTextPresentation.editable;
  return size <= highlighted ? FileTextPresentation.highlighted : FileTextPresentation.plain;
}

/// Original plugin `useFileRead`: the file's text or bytes, re-read when it
/// changes on disk, keeping the current value during a re-read. A deletion is
/// an error holding a [FileDeletedError].
class FileReader<T> extends ChangeNotifier {
  FileReader(this._read, void Function() Function(FileEventListener) watch) {
    _unwatch = watch((event) {
      if (event == 'changed') {
        unawaited(_load());
        return;
      }
      _generation += 1;
      _error = const FileDeletedError();
      _ready = false;
      _changed();
    });
    unawaited(_load());
  }
  final Future<T> Function() _read;
  late final void Function() _unwatch;
  int _generation = 0;
  bool _ready = false;
  bool _disposed = false;
  T? _value;
  Object? _error;

  bool get loading => !_ready && _error == null;
  T? get value => _ready ? _value : null;
  Object? get error => _ready ? null : _error;

  Future<void> _load() async {
    final current = ++_generation;
    try {
      final value = await _read();
      if (_disposed || current != _generation) return;
      _value = value;
      _ready = true;
      _error = null;
    } catch (error) {
      if (_disposed || current != _generation) return;
      _ready = false;
      _error = error;
    }
    _changed();
  }

  void _changed() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _unwatch();
    super.dispose();
  }
}

/// One file tab (original `FilePane`): observes the file through a
/// [FileHandle], resolves its panel from the stat, and mounts that panel's
/// file state — the editor buffer, the read-only text or the image bytes.
class FileEditorSession extends ChangeNotifier {
  FileEditorSession({
    required this.client,
    required this.cwd,
    required this.path,
    required this.editorPlatform,
    FileHost? host,
    this.associations,
  }) : handle = FileHandle(host ?? DaemonFileHost(client, cwd: cwd, path: path)) {
    handle.addListener(_onHandle);
  }

  final PaseoClient client;
  final String cwd, path;
  final bool editorPlatform;
  final FileHandle handle;

  /// The device's `filePanelAssociations` (`ext:`/`name:` key → panel id).
  final Map<String, dynamic> Function()? associations;
  bool _disposed = false;

  FilePanelCandidate? _panel;
  FileTextPresentation? _presentation;
  String? _mounted;
  FileEditorModel? _model;
  FileReader<String>? _text;
  FileReader<Uint8List>? _bytes;

  FileHandleSnapshot get status => handle.snapshot;
  FileVersion? get stat => handle.stat;

  /// The panel the ready file resolves to; null before the stat and for
  /// files no panel reads (binary).
  FilePanelCandidate? get panel => _panel;

  /// For text panels: how the text is shown.
  FileTextPresentation? get presentation => _presentation;

  /// The editable buffer ([FileTextPresentation.editable]).
  FileEditorModel? get model => _model;

  /// The read-only text ([FileTextPresentation.highlighted]/[plain]).
  FileReader<String>? get text => _text;

  /// The image panel's bytes.
  FileReader<Uint8List>? get bytes => _bytes;

  /// Whether the open buffer has unsaved changes (the tab's `modified`).
  bool get modified => _model?.modified ?? false;

  void setObserving(bool observing) => handle.setObserving(observing);

  void retry() => handle.retry();

  /// Holds this file's writes while the tab asks to discard them.
  void Function() holdWrites() => handle.holdWrites();

  void _onHandle() {
    if (_disposed) return;
    final stat = handle.snapshot.stat;
    if (handle.snapshot.status == 'ready' && stat != null) {
      final panel = resolveFilePanel(
        path: path,
        kind: stat.kind ?? 'binary',
        associations: associations?.call() ?? const {},
      ).panel;
      final presentation = panel?.fileKind == 'text'
          ? fileTextPresentation(size: stat.size, editorPlatform: editorPlatform)
          : null;
      _panel = panel;
      _presentation = presentation;
      final editable = presentation == FileTextPresentation.editable;
      final readable = presentation == FileTextPresentation.highlighted ||
          presentation == FileTextPresentation.plain;
      final mount = panel == null
          ? null
          : '${panel.id}:${editable ? 'edit' : readable ? 'read' : presentation?.name ?? ''}';
      if (mount != _mounted) {
        _unmount();
        _mounted = mount;
        if (panel?.fileKind == 'image') {
          _bytes = FileReader<Uint8List>(handle.readBytes, handle.watch)..addListener(_changed);
        } else if (editable) {
          _model = FileEditorModel(FileHandleEditorFile(handle))
            ..addListener(_changed)
            ..open();
        } else if (readable) {
          _text = FileReader<String>(handle.readText, handle.watch)..addListener(_changed);
        }
      }
    }
    _changed();
  }

  /// One panel mount ends: a modified buffer is written first (original
  /// `model.dispose({ flush: true })`), so switching panels keeps the edit.
  void _unmount() {
    final model = _model;
    _model = null;
    if (model != null) {
      model.removeListener(_changed);
      model.close(flush: true);
      model.dispose();
    }
    _text?.removeListener(_changed);
    _text?.dispose();
    _text = null;
    _bytes?.removeListener(_changed);
    _bytes?.dispose();
    _bytes = null;
  }

  void _changed() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    handle.removeListener(_onHandle);
    _unmount();
    // Closing the tab disposes the handle, which drops held and queued writes.
    handle.dispose();
    super.dispose();
  }
}

/// Original panel-instance `modified` attribute (`usePanelInstanceAttributes`):
/// the workspace tab keys whose file buffer has unsaved changes, for the dot
/// on the tab strip and the compact tab switcher. The file panel sets its tab
/// and clears it when it unmounts (the tab closes or changes file).
final modifiedFileTabs = ModifiedFileTabs();

class ModifiedFileTabs extends ChangeNotifier {
  final Set<String> _keys = {};
  bool _notifyScheduled = false;

  bool contains(String key) => _keys.contains(key);

  void set(String key, {required bool modified}) {
    if (!(modified ? _keys.add(key) : _keys.remove(key)) || _notifyScheduled) return;
    // Panels clear their entry while unmounting, when the tree is locked.
    _notifyScheduled = true;
    scheduleMicrotask(() {
      _notifyScheduled = false;
      notifyListeners();
    });
  }
}
