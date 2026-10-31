import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/features/files/file_editor_model.dart';
import 'package:paseo_sessions/features/files/file_editor_session.dart';
import 'package:paseo_sessions/features/files/file_handle.dart';
import 'package:paseo_sessions/features/files/file_panel_resolve.dart';

/// A host file: its bytes and version; writes compare the expected version.
class _FakeHost implements FileHost {
  _FakeHost(String text) : bytes = utf8.encode(text), revision = 'r1';
  Uint8List bytes;
  String? revision;
  bool exists = true;
  int writes = 0;
  void Function(FileVersion)? _emit;

  FileVersion get version => exists
      ? FileVersion.ready(
          size: bytes.length,
          modifiedAt: 't-$revision',
          revision: revision,
          kind: 'text',
          mimeType: 'text/plain',
        )
      : const FileVersion.missing();

  /// Another program changes the file on disk.
  void change(String text, String nextRevision) {
    bytes = utf8.encode(text);
    revision = nextRevision;
    exists = true;
    _emit?.call(version);
  }

  void delete() {
    exists = false;
    _emit?.call(version);
  }

  @override
  Future<({FileVersion initial, void Function() unsubscribe})> observe(
    void Function(FileVersion) onObservation,
  ) async {
    _emit = onObservation;
    return (initial: version, unsubscribe: () => _emit = null);
  }

  @override
  Future<FileContents> read() async {
    if (!exists) throw StateError('missing');
    return FileContents(
      bytes: bytes,
      size: bytes.length,
      modifiedAt: 't-$revision',
      revision: revision,
      kind: 'text',
      mimeType: 'text/plain',
    );
  }

  @override
  Future<JsonMap> write({
    required String content,
    required String expectedModifiedAt,
    String? expectedRevision,
  }) async {
    writes += 1;
    if (!exists) {
      return {'status': 'conflict', 'version': {'status': 'missing'}};
    }
    if (expectedRevision != revision) {
      return {
        'status': 'conflict',
        'version': {
          'status': 'ready',
          'size': bytes.length,
          'modifiedAt': 't-$revision',
          'revision': revision,
        },
      };
    }
    bytes = utf8.encode(content);
    revision = 'w$writes';
    final written = {
      'status': 'written',
      'size': bytes.length,
      'modifiedAt': 't-$revision',
      'revision': revision,
    };
    // The daemon's own observation of the write arrives too.
    _emit?.call(version);
    return written;
  }
}

/// Autosave timers the test fires by hand.
class _Timers {
  final pending = <void Function()>[];
  Timer call(Duration delay, void Function() callback) {
    pending.add(callback);
    return _ManualTimer(() => pending.remove(callback));
  }

  void fire() {
    final callbacks = List.of(pending);
    pending.clear();
    for (final callback in callbacks) {
      callback();
    }
  }
}

class _ManualTimer implements Timer {
  _ManualTimer(this._cancel);
  final void Function() _cancel;
  bool _active = true;
  @override
  void cancel() {
    if (_active) _cancel();
    _active = false;
  }

  @override
  bool get isActive => _active;
  @override
  int get tick => 0;
}

Future<void> _settle() => Future<void>.delayed(Duration.zero).then((_) => Future<void>.delayed(Duration.zero));

/// Original file-panels/{resolve,catalog}.ts, file/handle.ts,
/// source/editor/model.ts and source/readonly/presentation.ts.
void main() {
  group('resolveFilePanel', () {
    test('built-ins claim their extensions; Source is the text floor', () {
      expect(resolveFilePanel(path: 'docs/README.MD', kind: 'text').panel, markdownFilePanel);
      expect(resolveFilePanel(path: 'a/index.htm', kind: 'text').panel, htmlFilePanel);
      expect(resolveFilePanel(path: 'logo.svg', kind: 'image').panel, imageFilePanel);
      expect(resolveFilePanel(path: 'main.rs', kind: 'text').panel, sourceFilePanel);
      // `.mkd` is not a Markdown extension in the original catalog.
      expect(resolveFilePanel(path: 'notes.mkd', kind: 'text').panel, sourceFilePanel);
    });

    test('a file of another kind than the panel reads has no built-in', () {
      expect(resolveFilePanel(path: 'icon.bmp', kind: 'image').panel, isNull);
      expect(resolveFilePanel(path: 'blob.bin', kind: 'binary').panel, isNull);
      // An `.svg` the host classifies as text opens in Source.
      expect(resolveFilePanel(path: 'logo.svg', kind: 'text').panel, sourceFilePanel);
    });

    test('associations use ext:/name: keys and only pick a candidate', () {
      final resolution = resolveFilePanel(
        path: 'README.md',
        kind: 'text',
        associations: {'ext:md': 'builtin:source', 'md': 'source'},
      );
      expect(resolution.selectorKey, 'ext:md');
      // Source is not a candidate beside Markdown, so the association is ignored.
      expect(resolution.candidates, [markdownFilePanel]);
      expect(resolution.associatedPanelId, isNull);
      expect(resolution.panel, markdownFilePanel);
    });
  });

  test('fileTextPresentation follows the platform budgets', () {
    expect(fileTextPresentation(size: 1024 * 1024, editorPlatform: true), FileTextPresentation.editable);
    expect(fileTextPresentation(size: 1024 * 1024 + 1, editorPlatform: true), FileTextPresentation.highlighted);
    expect(fileTextPresentation(size: 20 * 1024 * 1024, editorPlatform: true), FileTextPresentation.plain);
    expect(fileTextPresentation(size: 50 * 1024 * 1024 + 1, editorPlatform: true), FileTextPresentation.unsupported);
    expect(fileTextPresentation(size: 10, editorPlatform: false), FileTextPresentation.highlighted);
    expect(fileTextPresentation(size: 2 * 1024 * 1024, editorPlatform: false), FileTextPresentation.plain);
    expect(fileTextPresentation(size: 10 * 1024 * 1024 + 1, editorPlatform: false), FileTextPresentation.unsupported);
  });

  group('FileHandle', () {
    test('a file missing from the start is the missing load state', () async {
      final host = _FakeHost('x')..exists = false;
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      expect(handle.snapshot.status, 'missing');
    });

    test('own writes do not notify; other versions notify once', () async {
      final host = _FakeHost('one');
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      final events = <String>[];
      handle.watch(events.add);
      expect(await handle.readText(), 'one');
      await handle.writeText('two');
      await _settle();
      expect(events, isEmpty);
      host.change('three', 'r9');
      host.change('three', 'r9');
      await _settle();
      expect(events, ['changed']);
    });

    test('a deletion is reported only after a read confirms it', () async {
      final host = _FakeHost('one');
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      final events = <String>[];
      handle.watch(events.add);
      host.delete();
      await _settle();
      expect(events, ['deleted']);
      // The stat is kept for the mounted panel.
      expect(handle.snapshot.status, 'ready');
    });

    test('writes over an unread version conflict; force retries over the disk version', () async {
      final host = _FakeHost('one');
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      await handle.readText();
      host.revision = 'other';
      await expectLater(handle.writeText('mine'), throwsA(isA<FileConflictError>()));
      await handle.writeText('mine', force: true);
      expect(utf8.decode(host.bytes), 'mine');
    });

    test('a byte order mark is dropped on read and written back', () async {
      final host = _FakeHost('\ufeffhello');
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      expect(await handle.readText(), 'hello');
      await handle.writeText('bye');
      expect(host.bytes.take(3), [0xef, 0xbb, 0xbf]);
    });

    test('held writes wait until resumed and are dropped when disposed', () async {
      final host = _FakeHost('one');
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      await handle.readText();
      final resume = handle.holdWrites();
      var done = false;
      unawaited(handle.writeText('two').then((_) => done = true));
      await _settle();
      expect(host.writes, 0);
      resume();
      await _settle();
      expect(done, isTrue);
      expect(host.writes, 1);

      handle.holdWrites();
      final dropped = handle.writeText('three');
      handle.dispose();
      await expectLater(dropped, throwsA(isA<StateError>()));
      expect(host.writes, 1);
    });
  });

  group('FileEditorModel', () {
    Future<(FileEditorModel, _FakeHost, _Timers)> open(String text) async {
      final host = _FakeHost(text);
      final handle = FileHandle(host)..setObserving(true);
      await _settle();
      final timers = _Timers();
      final model = FileEditorModel(FileHandleEditorFile(handle), timer: timers.call)..open();
      await _settle();
      return (model, host, timers);
    }

    test('edits autosave after the delay and return to clean', () async {
      final (model, host, timers) = await open('a');
      expect(model.status, FileEditorStatus.clean);
      model.edit('ab');
      expect(model.status, FileEditorStatus.dirty);
      expect(timers.pending, hasLength(1));
      timers.fire();
      await _settle();
      expect(utf8.decode(host.bytes), 'ab');
      expect(model.status, FileEditorStatus.clean);
      expect(model.modified, isFalse);
    });

    test('a clean buffer follows the disk; a modified one conflicts', () async {
      final (model, host, _) = await open('a');
      host.change('b', 'r2');
      await _settle();
      expect(model.content, 'b');
      expect(model.status, FileEditorStatus.clean);
      model.edit('mine');
      host.change('theirs', 'r3');
      await _settle();
      expect(model.status, FileEditorStatus.conflict);
      expect(model.conflict, 'changed');
      expect(model.content, 'mine');
      await model.overwrite();
      await _settle();
      expect(utf8.decode(host.bytes), 'mine');
      expect(model.status, FileEditorStatus.clean);
    });

    test('a deleted file keeps the buffer in a deleted conflict', () async {
      final (model, host, _) = await open('a');
      model.edit('mine');
      host.delete();
      await _settle();
      expect(model.status, FileEditorStatus.conflict);
      expect(model.conflict, 'deleted');
      expect(model.content, 'mine');
    });

    test('saving over a version changed on disk conflicts instead of overwriting', () async {
      final (model, host, _) = await open('a');
      model.edit('b');
      host.revision = 'gone';
      await model.save();
      expect(model.status, FileEditorStatus.conflict);
      expect(utf8.decode(host.bytes), 'a');
    });

    test('line separators are restored on save', () async {
      final (model, host, timers) = await open('a\r\nb');
      expect(model.content, 'a\nb');
      model.edit('a\nb\nc');
      timers.fire();
      await _settle();
      expect(utf8.decode(host.bytes), 'a\r\nb\r\nc');
    });
  });
}
