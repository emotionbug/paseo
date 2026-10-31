import 'dart:async';

import 'package:flutter/foundation.dart';

import 'file_handle.dart';

/// Original `FileEditorStatus`.
enum FileEditorStatus { loading, failed, clean, dirty, saving, conflict, error }

/// Original `AUTOSAVE_DELAY_MS`.
const fileAutosaveDelay = Duration(milliseconds: 800);

typedef FileEditorTimer = Timer Function(Duration delay, void Function() callback);

/// The text a [FileEditorModel] reads and writes: a [FileHandle], or a test
/// double.
abstract interface class FileEditorFile {
  Future<String> readText();
  Future<void> writeText(String text, {bool force = false});
  void Function() watch(FileEventListener listener);
}

class FileHandleEditorFile implements FileEditorFile {
  FileHandleEditorFile(this.handle);
  final FileHandle handle;
  @override
  Future<String> readText() => handle.readText();
  @override
  Future<void> writeText(String text, {bool force = false}) =>
      handle.writeText(text, force: force);
  @override
  void Function() watch(FileEventListener listener) => handle.watch(listener);
}

/// Original `detectLineSeparator`: the file's first line break, `\n` without one.
String detectLineSeparator(String content) {
  for (var index = 0; index < content.length; index += 1) {
    final character = content.codeUnitAt(index);
    if (character == 10) return '\n';
    if (character == 13) {
      return index + 1 < content.length && content.codeUnitAt(index + 1) == 10 ? '\r\n' : '\r';
    }
  }
  return '\n';
}

/// Source's buffer over a file (original `file-panels/source/editor/model.ts`):
/// reads the file, autosaves edits, and reconciles changes on disk. A clean
/// buffer follows the disk; a modified one conflicts and the user overwrites
/// or reloads. The buffer holds `\n` line breaks; writes restore the file's
/// detected line separator, as the original editor's `lineSeparator` does.
class FileEditorModel extends ChangeNotifier {
  FileEditorModel(this.file, {FileEditorTimer? timer}) : _timer = timer ?? Timer.new;
  final FileEditorFile file;
  final FileEditorTimer _timer;

  FileEditorStatus _status = FileEditorStatus.loading;
  String _content = '';
  String _lineSeparator = '\n';
  bool _modified = false;

  /// `changed` | `deleted` while in conflict.
  String? _conflict;
  Object? _error;
  String _persisted = '';
  Timer? _autosave;
  bool _saving = false;
  int _saveSequence = 0;
  int _readSequence = 0;
  String? _eventDuringSave;
  void Function()? _unwatch;
  bool _disposed = false;

  FileEditorStatus get status => _status;
  String get content => _content;

  /// The buffer differs from what this editor last read or saved.
  bool get modified => _modified;
  String? get conflict => _conflict;

  /// The failed load's or save's message.
  Object? get error => _error;

  /// The buffer as the file stores it.
  String get fileContent => _withSeparator(_content);

  /// Reads the file and follows its changes on disk.
  void open() {
    if (_disposed || _unwatch != null) return;
    _unwatch = file.watch(_receiveFileEvent);
    unawaited(reload());
  }

  void edit(String content) {
    if (_disposed || !_isLoaded || content == _content) return;
    final modified = content != _persisted;
    var status = modified ? FileEditorStatus.dirty : FileEditorStatus.clean;
    if (_status == FileEditorStatus.conflict) status = FileEditorStatus.conflict;
    _status = status;
    _content = content;
    _modified = modified;
    _error = null;
    _changed();
    if (status == FileEditorStatus.dirty) {
      _scheduleAutosave();
    } else {
      _clearAutosave();
    }
  }

  Future<void> save() async {
    if (_status != FileEditorStatus.dirty && _status != FileEditorStatus.error) return;
    await _write(force: false);
  }

  /// Writes the buffer over a file that changed on disk.
  Future<void> overwrite() async {
    if (_conflict != 'changed') return;
    await _write(force: true);
  }

  /// Replaces the buffer with the file on disk.
  Future<void> reload() async {
    if (_disposed) return;
    final sequence = ++_readSequence;
    try {
      final content = await file.readText();
      if (_disposed || sequence != _readSequence) return;
      _applyFile(content);
    } catch (error) {
      if (_disposed || sequence != _readSequence) return;
      if (_isLoaded) return;
      _status = FileEditorStatus.failed;
      _error = error;
      _changed();
    }
  }

  /// Stops following the file. With [flush], a modified buffer is written
  /// first; the tab holds that write while it asks to discard it.
  void close({required bool flush}) {
    if (_disposed) return;
    if (flush && _modified && _status != FileEditorStatus.conflict) {
      unawaited(file.writeText(fileContent).catchError((Object _) {}));
    }
    _disposed = true;
    _clearAutosave();
    _unwatch?.call();
    _unwatch = null;
  }

  @override
  void dispose() {
    close(flush: false);
    super.dispose();
  }

  void _receiveFileEvent(String event) {
    if (_disposed) return;
    if (!_isLoaded) {
      if (event == 'changed') unawaited(reload());
      return;
    }
    if (_saving) {
      _eventDuringSave = event;
      return;
    }
    if (event == 'deleted') {
      _enterConflict('deleted');
      return;
    }
    unawaited(_reconcile());
  }

  /// Follows a clean buffer to the disk; a modified one conflicts unless the
  /// disk matches it.
  Future<void> _reconcile() async {
    final sequence = ++_readSequence;
    String content;
    try {
      content = await file.readText();
    } catch (_) {
      return;
    }
    if (_disposed || sequence != _readSequence) return;
    if (_saving) {
      _eventDuringSave = 'changed';
      return;
    }
    if (content == _withSeparator(_persisted)) {
      _adoptUnchangedFile();
      return;
    }
    if (!_modified) {
      _applyFile(content);
      return;
    }
    _enterConflict('changed');
  }

  Future<void> _write({required bool force}) async {
    if (_disposed) return;
    _clearAutosave();
    final sequence = ++_saveSequence;
    final content = _content;
    _saving = true;
    _eventDuringSave = null;
    _status = FileEditorStatus.saving;
    _conflict = null;
    _error = null;
    _changed();
    try {
      await file.writeText(_withSeparator(content), force: force);
    } catch (error) {
      _saving = false;
      if (_disposed || sequence != _saveSequence) return;
      if (error is FileConflictError) {
        _enterConflict('changed');
      } else if (error is FileDeletedError) {
        _enterConflict('deleted');
      } else {
        _status = FileEditorStatus.error;
        _error = error;
        _changed();
      }
      _receivePendingEvent();
      return;
    }
    _saving = false;
    if (_disposed || sequence != _saveSequence) return;
    _persisted = content;
    _modified = _content != _persisted;
    _status = _modified ? FileEditorStatus.dirty : FileEditorStatus.clean;
    _changed();
    if (_modified) _scheduleAutosave();
    _receivePendingEvent();
  }

  void _receivePendingEvent() {
    final event = _eventDuringSave;
    _eventDuringSave = null;
    if (event != null) _receiveFileEvent(event);
  }

  void _applyFile(String raw) {
    _clearAutosave();
    _saveSequence += 1;
    _lineSeparator = detectLineSeparator(raw);
    final content = _normalize(raw);
    _persisted = content;
    _status = FileEditorStatus.clean;
    _content = content;
    _modified = false;
    _conflict = null;
    _error = null;
    _changed();
  }

  void _adoptUnchangedFile() {
    if (_status != FileEditorStatus.conflict) return;
    _status = _modified ? FileEditorStatus.dirty : FileEditorStatus.clean;
    _conflict = null;
    _error = null;
    _changed();
    if (_status == FileEditorStatus.dirty) _scheduleAutosave();
  }

  void _enterConflict(String conflict) {
    _clearAutosave();
    _status = FileEditorStatus.conflict;
    _conflict = conflict;
    _error = null;
    _changed();
  }

  bool get _isLoaded =>
      _status != FileEditorStatus.loading && _status != FileEditorStatus.failed;

  void _scheduleAutosave() {
    _clearAutosave();
    _autosave = _timer(fileAutosaveDelay, () {
      _autosave = null;
      unawaited(save());
    });
  }

  void _clearAutosave() {
    _autosave?.cancel();
    _autosave = null;
  }

  String _normalize(String raw) => raw.replaceAll('\r\n', '\n').replaceAll('\r', '\n');

  String _withSeparator(String content) =>
      _lineSeparator == '\n' ? content : content.replaceAll('\n', _lineSeparator);

  void _changed() {
    if (!_disposed) notifyListeners();
  }
}
