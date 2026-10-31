import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import 'remote_file_service.dart';

/// Original `PluginFileConflictError`: the file changed on disk since the
/// handle last read or wrote it.
class FileConflictError implements Exception {
  const FileConflictError();
  @override
  String toString() => 'The file changed on disk.';
}

/// Original `PluginFileDeletedError`: the file was deleted on disk.
class FileDeletedError implements Exception {
  const FileDeletedError();
  @override
  String toString() => 'The file was deleted.';
}

/// Original protocol `FileVersion` (`ready` | `missing` | `error`), with the
/// `kind`/`mimeType` classification the session attaches to ready versions.
class FileVersion {
  const FileVersion.ready({
    required this.size,
    required this.modifiedAt,
    this.revision,
    this.kind,
    this.mimeType,
  }) : status = 'ready',
       error = null;
  const FileVersion.missing()
    : status = 'missing',
      size = 0,
      modifiedAt = '',
      revision = null,
      kind = null,
      mimeType = null,
      error = null;
  const FileVersion.error(String this.error)
    : status = 'error',
      size = 0,
      modifiedAt = '',
      revision = null,
      kind = null,
      mimeType = null;

  factory FileVersion.fromJson(JsonMap json) => switch (json.text('status')) {
    'ready' => FileVersion.ready(
      size: json.integer('size'),
      modifiedAt: json.text('modifiedAt'),
      revision: json['revision'] is String ? json['revision'] as String : null,
      kind: json['kind'] is String ? json['kind'] as String : null,
      mimeType: json['mimeType'] is String ? json['mimeType'] as String : null,
    ),
    'missing' => const FileVersion.missing(),
    _ => FileVersion.error(json.text('error', 'File observation failed')),
  };

  final String status;
  final int size;
  final String modifiedAt;
  final String? revision;

  /// `text` | `image` | `binary`.
  final String? kind;
  final String? mimeType;
  final String? error;

  bool get ready => status == 'ready';

  /// Original `versionKey`.
  String get key => revision ?? '$size:$modifiedAt';

  FileVersion classified(String kind, String mimeType) => FileVersion.ready(
    size: size,
    modifiedAt: modifiedAt,
    revision: revision,
    kind: kind,
    mimeType: mimeType,
  );
}

/// One read of the file: its bytes and the version they are.
class FileContents {
  const FileContents({
    required this.bytes,
    required this.size,
    required this.modifiedAt,
    this.revision,
    this.kind = 'binary',
    this.mimeType = 'application/octet-stream',
  });
  final Uint8List bytes;
  final int size;
  final String modifiedAt;
  final String? revision;
  final String kind;
  final String mimeType;
  String get key => revision ?? '$size:$modifiedAt';
}

/// What a handle needs from the host (original `FileSession`).
abstract interface class FileHost {
  Future<({FileVersion initial, void Function() unsubscribe})> observe(
    void Function(FileVersion observation) onObservation,
  );
  Future<FileContents> read();

  /// The `fs.file.write.response` result: `written` | `conflict` | `error`.
  Future<JsonMap> write({
    required String content,
    required String expectedModifiedAt,
    String? expectedRevision,
  });
}

/// Original `createDaemonFileSession` for one target.
class DaemonFileHost implements FileHost {
  DaemonFileHost(this.client, {required this.cwd, required this.path});
  final PaseoClient client;
  final String cwd, path;

  // COMPAT(workspaceFileEditing): the host cannot observe or write files.
  bool get _workspaceFileEditing =>
      client.serverInfo.object('features').flag('workspaceFileEditing');

  @override
  Future<({FileVersion initial, void Function() unsubscribe})> observe(
    void Function(FileVersion observation) onObservation,
  ) async {
    if (!_workspaceFileEditing) {
      // COMPAT(workspaceFileEditing): its read is the stat.
      return (initial: _statFromRead(await read()), unsubscribe: () {});
    }
    ({String kind, String mimeType})? kind;
    FileVersion classify(FileVersion version) {
      if (!version.ready) return version;
      if (version.kind != null && version.mimeType != null) {
        kind = (kind: version.kind!, mimeType: version.mimeType!);
      }
      // COMPAT(fileVersionKind): older hosts classify a file only when it is
      // read; the first read's kind stands for later versions.
      final known = kind;
      if (known == null) return const FileVersion.error('The file has no kind.');
      return version.classified(known.kind, known.mimeType);
    }

    final observation = await client.observe(
      type: 'fs.file.subscribe.request',
      fields: {'cwd': cwd, 'path': path},
      responseType: 'fs.file.subscribe.response',
      onEvent: (event) {
        final payload = event['payload'] is Map ? jsonMap(event['payload']) : event;
        // `fs.file.update` carries a version; a restored subscription's snapshot
        // repairs what a reconnect missed.
        final version = switch (event.text('type')) {
          'fs.file.update' => payload['version'],
          'subscription.restored' => payload['initial'],
          _ => null,
        };
        if (version is Map) onObservation(classify(FileVersion.fromJson(jsonMap(version))));
      },
    );
    final initial = FileVersion.fromJson(observation.snapshot.object('initial'));
    if (initial.ready && initial.kind == null) {
      try {
        final contents = await read();
        kind = (kind: contents.kind, mimeType: contents.mimeType);
      } catch (_) {
        unawaited(observation.close());
        rethrow;
      }
    }
    return (initial: classify(initial), unsubscribe: () => unawaited(observation.close()));
  }

  @override
  Future<FileContents> read() async {
    final data = await RemoteFileService(client).read(cwd, path, maxBytes: null);
    return FileContents(
      bytes: data.bytes,
      size: data.metadata.integer('size', data.bytes.length),
      modifiedAt: data.modifiedAt,
      revision: data.revision,
      kind: data.kind,
      mimeType: data.mime,
    );
  }

  @override
  Future<JsonMap> write({
    required String content,
    required String expectedModifiedAt,
    String? expectedRevision,
  }) async {
    if (!_workspaceFileEditing) throw StateError('Update the host to edit files.');
    try {
      final response = await client.request('fs.file.write.request', {
        'cwd': cwd,
        'path': path,
        'content': content,
        'expectedModifiedAt': expectedModifiedAt,
        'expectedRevision': ?expectedRevision,
      }, responseType: 'fs.file.write.response');
      return response.object('result');
    } on DaemonException catch (failure) {
      final result = jsonMap(failure.payload).object('result');
      if (result['status'] is String) return result;
      rethrow;
    }
  }

  static FileVersion _statFromRead(FileContents file) => FileVersion.ready(
    size: file.size,
    modifiedAt: file.modifiedAt,
    revision: file.revision,
    kind: file.kind,
    mimeType: file.mimeType,
  );
}

/// Original `FileHandleSnapshot`: `loading`, the ready [stat], `missing` or
/// `error`.
class FileHandleSnapshot {
  const FileHandleSnapshot._(this.status, {this.stat, this.error});
  static const loading = FileHandleSnapshot._('loading');
  static const missing = FileHandleSnapshot._('missing');
  final String status;
  final FileVersion? stat;
  final Object? error;
}

typedef FileEventListener = void Function(String event);

/// One file tab target's file (original `file-panels/file/handle.ts`): its
/// stat for choosing a panel, and the reads and writes every panel goes
/// through.
///
/// The handle remembers the version it last read or wrote, and writes only
/// over that version. Changes on disk reach [watch] listeners once per
/// version (`changed` / `deleted`); versions this handle read or wrote itself
/// do not. Reads wait for this handle's writes. Writes run one at a time, and
/// wait while the tab holds them. Once the file has a stat, the snapshot keeps
/// it; a deletion is reported to watchers only after a read confirms it.
class FileHandle extends ChangeNotifier {
  FileHandle(this.host);
  final FileHost host;
  final _watchers = <FileEventListener>{};
  FileHandleSnapshot _snapshot = FileHandleSnapshot.loading;
  FileVersion? _stat;
  FileVersion? _latest;
  FileContents? _base;
  final _seen = <String>{};
  bool _deleted = false;
  bool _confirmingDeletion = false;
  bool _hasBom = false;
  Future<void> _writes = Future<void>.value();
  int _writing = 0;
  int _holds = 0;
  Completer<void>? _held;
  ({int generation, void Function()? unsubscribe})? _observing;
  int _generation = 0;
  bool _disposed = false;

  FileHandleSnapshot get snapshot => _snapshot;

  /// The latest stat this handle knows, or null before the first one.
  FileVersion? get stat => _stat;

  void Function() watch(FileEventListener listener) {
    _watchers.add(listener);
    return () => _watchers.remove(listener);
  }

  /// Observes the file while the tab is on screen. Coming back reports what
  /// changed meanwhile.
  void setObserving(bool observing) {
    if (_disposed || observing == (_observing != null)) return;
    if (!observing) {
      _stopObserving();
      return;
    }
    final generation = ++_generation;
    _observing = (generation: generation, unsubscribe: null);
    unawaited(_observe(generation));
  }

  Future<void> _observe(int generation) async {
    final ({FileVersion initial, void Function() unsubscribe}) subscription;
    try {
      subscription = await host.observe((observation) {
        if (_observing?.generation == generation) _receive(observation);
      });
    } catch (error) {
      if (_observing?.generation != generation) return;
      _observing = null;
      _setSnapshot(FileHandleSnapshot._('error', error: error));
      return;
    }
    if (_observing?.generation != generation) {
      subscription.unsubscribe();
      return;
    }
    _observing = (generation: generation, unsubscribe: subscription.unsubscribe);
    _receive(subscription.initial);
  }

  /// Observes again after a failure.
  void retry() {
    _stopObserving();
    setObserving(true);
  }

  /// Holds this file's writes while the tab asks whether to discard unsaved
  /// changes. The returned function lets them run; disposing drops them.
  void Function() holdWrites() {
    _holds += 1;
    if (_holds == 1) _held = Completer<void>();
    var resumed = false;
    return () {
      if (resumed) return;
      resumed = true;
      _holds -= 1;
      if (_holds > 0) return;
      final held = _held;
      _held = null;
      if (held != null && !held.isCompleted) held.complete();
    };
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _stopObserving();
    final held = _held;
    if (held != null && !held.isCompleted) held.complete();
    _watchers.clear();
    super.dispose();
  }

  void _stopObserving() {
    _observing?.unsubscribe?.call();
    _observing = null;
  }

  void _receive(FileVersion observation) {
    if (_disposed) return;
    _latest = observation;
    if (observation.ready) {
      _stat = observation;
      if (_base == null) _adopt(observation.size, observation.modifiedAt, observation.revision);
      _setSnapshot(FileHandleSnapshot._('ready', stat: _stat));
    } else if (_stat == null) {
      _setSnapshot(
        observation.status == 'missing'
            ? FileHandleSnapshot.missing
            : FileHandleSnapshot._('error', error: StateError(observation.error ?? '')),
      );
    }
    _notifyChange(observation);
  }

  /// Tells watchers about a disk version this handle did not read or write.
  void _notifyChange(FileVersion observation) {
    if (_writing > 0 || observation.status == 'error') return;
    if (observation.status == 'missing') {
      if (!_deleted) unawaited(_confirmDeletion());
      return;
    }
    _notifyVersion(observation.key);
  }

  void _notifyVersion(String key) {
    final restored = _deleted;
    _deleted = false;
    if (_seen.contains(key) && !restored) return;
    _seen.add(key);
    _emit('changed');
  }

  Future<void> _confirmDeletion() async {
    if (_confirmingDeletion) return;
    _confirmingDeletion = true;
    FileContents? contents;
    try {
      contents = await host.read();
    } catch (_) {
      // The read failing is the confirmation.
    } finally {
      _confirmingDeletion = false;
    }
    if (_disposed || _writing > 0 || _deleted) return;
    if (contents != null) {
      _notifyVersion(contents.key);
      return;
    }
    if (_latest?.status != 'missing') return;
    _deleted = true;
    _emit('deleted');
  }

  void _emit(String event) {
    for (final watcher in List.of(_watchers)) {
      watcher(event);
    }
  }

  Future<FileContents> _readContents() async {
    _assertOpen();
    await _writes;
    final contents = await host.read();
    _assertOpen();
    _adopt(contents.size, contents.modifiedAt, contents.revision);
    return contents;
  }

  /// Reads the file as UTF-8. A byte order mark is dropped here and written
  /// back by [writeText].
  Future<String> readText() async {
    final bytes = (await _readContents()).bytes;
    _hasBom =
        bytes.length >= 3 && bytes[0] == 0xef && bytes[1] == 0xbb && bytes[2] == 0xbf;
    return utf8.decode(_hasBom ? Uint8List.sublistView(bytes, 3) : bytes, allowMalformed: true);
  }

  Future<Uint8List> readBytes() async => (await _readContents()).bytes;

  /// Writes the file if it has not changed since this handle last read or
  /// wrote it; otherwise throws [FileConflictError] (`force` writes anyway)
  /// or [FileDeletedError].
  Future<void> writeText(String text, {bool force = false}) =>
      _write(_hasBom ? '\ufeff$text' : text, force: force);

  Future<void> _write(String content, {required bool force}) {
    final write = _writes.then((_) => _performWrite(content, force));
    _writes = write.then<void>((_) {}, onError: (Object _) {});
    return write;
  }

  Future<void> _performWrite(String content, bool force) async {
    _assertOpen();
    final held = _held;
    if (held != null) await held.future;
    _assertOpen();
    _writing += 1;
    try {
      ({int size, String modifiedAt, String? revision})? expected = force
          ? (_stat == null ? _baseVersion : _versionOf(_stat!))
          : _baseVersion;
      for (var attempt = 0; ; attempt += 1) {
        if (expected == null) throw const FileConflictError();
        final result = await host.write(
          content: content,
          expectedModifiedAt: expected.modifiedAt,
          expectedRevision: expected.revision,
        );
        switch (result.text('status')) {
          case 'written':
            _adopt(
              result.integer('size'),
              result.text('modifiedAt'),
              result['revision'] is String ? result['revision'] as String : null,
            );
            return;
          case 'conflict':
            final disk = FileVersion.fromJson(result.object('version'));
            if (force && attempt == 0 && disk.ready) {
              expected = _versionOf(disk);
              continue;
            }
            if (disk.status == 'missing') throw const FileDeletedError();
            throw const FileConflictError();
          default:
            throw StateError(result.text('error', 'Save was not acknowledged'));
        }
      }
    } finally {
      _writing -= 1;
      // Observations that arrived during the write may be its own version.
      final latest = _latest;
      if (latest != null && !_disposed) _notifyChange(latest);
    }
  }

  ({int size, String modifiedAt, String? revision})? get _baseVersion {
    final base = _base;
    if (base == null) return null;
    return (size: base.size, modifiedAt: base.modifiedAt, revision: base.revision);
  }

  static ({int size, String modifiedAt, String? revision}) _versionOf(FileVersion version) =>
      (size: version.size, modifiedAt: version.modifiedAt, revision: version.revision);

  /// Records a version this handle read or wrote, so its observation does not
  /// notify.
  void _adopt(int size, String modifiedAt, String? revision) {
    final base = FileContents(
      bytes: Uint8List(0),
      size: size,
      modifiedAt: modifiedAt,
      revision: revision,
    );
    _base = base;
    _seen.add(base.key);
    final stat = _stat;
    if (stat != null && stat.key != base.key) {
      _stat = FileVersion.ready(
        size: size,
        modifiedAt: modifiedAt,
        revision: revision,
        kind: stat.kind,
        mimeType: stat.mimeType,
      );
      _setSnapshot(FileHandleSnapshot._('ready', stat: _stat));
    }
  }

  void _assertOpen() {
    if (_disposed) throw StateError('The file tab was closed.');
  }

  void _setSnapshot(FileHandleSnapshot next) {
    final current = _snapshot;
    if (current.status == next.status) {
      if (next.status == 'ready' &&
          current.stat!.key == next.stat!.key &&
          current.stat!.kind == next.stat!.kind &&
          current.stat!.mimeType == next.stat!.mimeType) {
        return;
      }
      if (next.status == 'error' && '${current.error}' == '${next.error}') return;
      if (next.status == 'loading' || next.status == 'missing') return;
    }
    _snapshot = next;
    if (!_disposed) notifyListeners();
  }
}
