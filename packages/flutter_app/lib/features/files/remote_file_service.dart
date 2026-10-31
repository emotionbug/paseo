import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:uuid/uuid.dart';

import '../../core/client.dart';
import '../../core/json.dart';

class RemoteFileData {
  const RemoteFileData({
    required this.bytes,
    required this.metadata,
    required this.path,
  });
  final Uint8List bytes;
  final JsonMap metadata;
  final String path;
  String get mime => metadata.text('mime', 'application/octet-stream');
  String get kind => mime.startsWith('image/')
      ? 'image'
      : metadata.text('encoding') == 'utf-8'
      ? 'text'
      : 'binary';
  String get modifiedAt => metadata.text('modifiedAt');
  String? get revision => metadata['revision'] as String?;
  String get text => utf8.decode(bytes, allowMalformed: false);
}

class FileTransferCancelled implements Exception {
  const FileTransferCancelled();
  @override
  String toString() => 'File transfer cancelled';
}

class FileTransferCancellation {
  bool cancelled = false;
  final _listeners = <void Function()>{};
  void cancel() {
    if (cancelled) return;
    cancelled = true;
    for (final listener in _listeners.toList()) {
      listener();
    }
    _listeners.clear();
  }

  void Function() listen(void Function() listener) {
    if (cancelled) {
      listener();
    } else {
      _listeners.add(listener);
    }
    return () {
      _listeners.remove(listener);
    };
  }
}

/// Files use the daemon's framed stream, including over relay/routed transports.
/// A binary read ends with FileEnd, not a JSON file_explorer_response.
class RemoteFileService {
  RemoteFileService(this.client);
  final PaseoClient client;

  Future<RemoteFileData> read(
    String cwd,
    String path, {
    int? maxBytes = 32 * 1024 * 1024,
    void Function(int received, int total)? onProgress,
    FileTransferCancellation? cancellation,
  }) async {
    final bytes = BytesBuilder(copy: false);
    final metadata = await _receive(
      cwd,
      path,
      maxBytes: maxBytes,
      onChunk: bytes.add,
      onProgress: onProgress,
      cancellation: cancellation,
    );
    return RemoteFileData(
      bytes: bytes.takeBytes(),
      metadata: metadata,
      path: path,
    );
  }

  Future<JsonMap> download(
    String cwd,
    String path,
    File destination, {
    void Function(int received, int total)? onProgress,
    FileTransferCancellation? cancellation,
  }) async {
    final sink = destination.openWrite();
    Object? sinkError;
    // Observe asynchronous disk errors immediately, before a network transfer ends.
    final sinkDone = sink.done.catchError((Object error) {
      sinkError = error;
    });
    try {
      final metadata = await _receive(
        cwd,
        path,
        onChunk: (bytes) {
          if (sinkError != null) throw sinkError!;
          sink.add(bytes);
        },
        onProgress: onProgress,
        cancellation: cancellation,
      );
      await sink.flush();
      await sink.close();
      await sinkDone;
      if (sinkError != null) throw sinkError!;
      return metadata;
    } catch (_) {
      await sink.close().catchError((Object _) {});
      await sinkDone;
      if (await destination.exists()) await destination.delete();
      rethrow;
    }
  }

  Future<JsonMap> _receive(
    String cwd,
    String path, {
    int? maxBytes,
    required void Function(Uint8List) onChunk,
    void Function(int received, int total)? onProgress,
    FileTransferCancellation? cancellation,
  }) async {
    if (client.status != ConnectionStatus.connected) {
      throw StateError('Host is disconnected');
    }
    if (cancellation?.cancelled == true) throw const FileTransferCancelled();
    final id = const Uuid().v4();
    final complete = Completer<JsonMap>();
    JsonMap? metadata;
    int received = 0;
    void fail(Object error) {
      if (!complete.isCompleted) complete.completeError(error);
    }

    void connectionChanged() {
      if (client.status != ConnectionStatus.connected) {
        fail(StateError('Connection lost during file download'));
      }
    }

    final binary = client.binaryEvents.listen(
      (bytes) {
        if (complete.isCompleted) return;
        try {
          final frame = decodeFileTransferFrame(bytes);
          if (frame == null || frame.requestId != id) return;
          if (frame.opcode == FileTransferOpcode.fileBegin) {
            if (metadata != null) {
              throw StateError('Duplicate file transfer header');
            }
            metadata = frame.metadata!;
            if (maxBytes != null && metadata!.integer('size') > maxBytes) {
              throw StateError(
                'File exceeds preview limit ($maxBytes bytes). Download it to open externally.',
              );
            }
            onProgress?.call(0, metadata!.integer('size'));
          } else if (frame.opcode == FileTransferOpcode.fileChunk) {
            if (metadata == null) {
              throw StateError('File data arrived before its header');
            }
            received += frame.payload.length;
            if (received > metadata!.integer('size') ||
                (maxBytes != null && received > maxBytes)) {
              throw StateError('File transfer exceeded declared size');
            }
            onChunk(frame.payload);
            onProgress?.call(received, metadata!.integer('size'));
          } else if (frame.opcode == FileTransferOpcode.fileEnd) {
            if (metadata == null || received != metadata!.integer('size')) {
              throw StateError('Incomplete file transfer ($received bytes)');
            }
            complete.complete(metadata!);
          }
        } catch (error) {
          fail(error);
        }
      },
      onError: fail,
      onDone: () => fail(StateError('File stream closed')),
    );
    final events = client.events.listen((event) {
      final payload = event['payload'] is Map
          ? jsonMap(event['payload'])
          : event;
      if (payload.text('requestId') != id || complete.isCompleted) return;
      try {
        if (event.text('type') == 'rpc_error' || payload['error'] != null) {
          throw StateError(payload.text('error', 'File read failed'));
        }
        if (event.text('type') != 'file_explorer_response') return;
        final file = payload.object('file');
        if (file.isEmpty) throw StateError('File unavailable');
        final encoding = file.text('encoding');
        final content = file['content'];
        if (content is! String || !['utf-8', 'base64'].contains(encoding)) {
          throw StateError('Host did not return file bytes');
        }
        final bytes = encoding == 'base64'
            ? base64Decode(content)
            : Uint8List.fromList(utf8.encode(content));
        if (maxBytes != null && bytes.length > maxBytes) {
          throw StateError('File exceeds preview limit ($maxBytes bytes)');
        }
        if (bytes.length != file.integer('size')) {
          throw StateError('Incomplete legacy file response');
        }
        onChunk(bytes);
        onProgress?.call(bytes.length, bytes.length);
        complete.complete({
          'mime': file.text('mimeType', 'application/octet-stream'),
          'size': bytes.length,
          'encoding': file.text('kind') == 'text' ? 'utf-8' : 'binary',
          'modifiedAt': file.text('modifiedAt'),
          if (file['revision'] != null) 'revision': file['revision'],
        });
      } catch (error) {
        fail(error);
      }
    }, onError: fail);
    client.addListener(connectionChanged);
    final releaseCancellation = cancellation?.listen(
      () => fail(const FileTransferCancelled()),
    );
    try {
      client.send('file_explorer_request', {
        'requestId': id,
        'cwd': cwd,
        'path': path,
        'mode': 'file',
        'acceptBinary': true,
        'maxBytes': ?maxBytes,
      });
      return await complete.future;
    } finally {
      releaseCancellation?.call();
      client.removeListener(connectionChanged);
      await binary.cancel();
      await events.cancel();
    }
  }

  Future<JsonMap> upload({
    required Uint8List bytes,
    required String fileName,
    required String mimeType,
    String? modifiedAt,
    void Function(int sent, int total)? onProgress,
  }) => uploadStream(
    stream: Stream.value(bytes),
    size: bytes.length,
    fileName: fileName,
    mimeType: mimeType,
    modifiedAt: modifiedAt,
    onProgress: onProgress,
  );

  Future<JsonMap> uploadStream({
    required Stream<List<int>> stream,
    required int size,
    required String fileName,
    required String mimeType,
    String? modifiedAt,
    void Function(int sent, int total)? onProgress,
  }) async {
    if (client.status != ConnectionStatus.connected) {
      throw StateError('Host is disconnected');
    }
    final id = const Uuid().v4();
    final timestamp = modifiedAt ?? DateTime.now().toUtc().toIso8601String();
    final complete = Completer<void>();
    JsonMap? response;
    Object? responseError;
    bool began = false, ended = false;
    void fail(Object error) {
      responseError = error;
      if (!complete.isCompleted) complete.complete();
    }

    void connectionChanged() {
      if (client.status != ConnectionStatus.connected) {
        fail(StateError('Connection lost during upload; outcome unknown'));
      }
    }

    final events = client.events.listen(
      (event) {
        final payload = event['payload'] is Map
            ? jsonMap(event['payload'])
            : event;
        if (payload.text('requestId') != id || complete.isCompleted) return;
        if (event.text('type') == 'rpc_error') {
          fail(StateError(payload.text('error', 'Upload failed')));
        } else if (event.text('type') == 'file.upload.response') {
          response = payload;
          complete.complete();
        }
      },
      onError: fail,
      onDone: () => fail(StateError('Upload connection closed')),
    );
    client.addListener(connectionChanged);
    void checkConnection() {
      if (responseError != null) throw responseError!;
      if (client.status != ConnectionStatus.connected) {
        throw StateError('Connection lost during upload; outcome unknown');
      }
    }

    try {
      client.send('file.upload.request', {
        'requestId': id,
        'fileName': fileName,
        'mimeType': mimeType,
        'size': size,
        'modifiedAt': timestamp,
      });
      checkConnection();
      client.sendBinary(
        encodeFileTransferFrame(
          opcode: FileTransferOpcode.fileBegin,
          requestId: id,
          metadata: {
            'mime': mimeType,
            'size': size,
            'encoding': 'binary',
            'modifiedAt': timestamp,
            'fileName': fileName,
          },
        ),
      );
      began = true;
      var sent = 0;
      await for (final chunk in stream) {
        if (complete.isCompleted) break;
        checkConnection();
        final bytes = chunk is Uint8List ? chunk : Uint8List.fromList(chunk);
        for (var offset = 0; offset < bytes.length; offset += 128 * 1024) {
          await Future<void>.delayed(Duration.zero);
          checkConnection();
          if (complete.isCompleted) break;
          final end = (offset + 128 * 1024).clamp(0, bytes.length);
          final part = Uint8List.sublistView(bytes, offset, end);
          sent += part.length;
          if (sent > size) {
            throw StateError('Upload source exceeded declared size');
          }
          client.sendBinary(
            encodeFileTransferFrame(
              opcode: FileTransferOpcode.fileChunk,
              requestId: id,
              payload: part,
            ),
          );
          onProgress?.call(sent, size);
        }
      }
      if (!complete.isCompleted) {
        if (sent != size) {
          throw StateError('Upload source changed while reading');
        }
        client.sendBinary(
          encodeFileTransferFrame(
            opcode: FileTransferOpcode.fileEnd,
            requestId: id,
          ),
        );
        ended = true;
      }
      await complete.future;
      if (responseError != null) throw responseError!;
      if (response == null ||
          response!['error'] != null ||
          response!['file'] == null) {
        throw StateError(
          response?.text('error') ?? 'Upload was not acknowledged',
        );
      }
      return response!.object('file');
    } finally {
      // There is no transfer-cancel opcode. An early End makes the daemon
      // reject/clean up an incomplete upload rather than retaining its buffer.
      if (began &&
          !ended &&
          !complete.isCompleted &&
          client.status == ConnectionStatus.connected) {
        try {
          client.sendBinary(
            encodeFileTransferFrame(
              opcode: FileTransferOpcode.fileEnd,
              requestId: id,
            ),
          );
        } catch (_) {
          /* Original transfer failure is retained. */
        }
      }
      client.removeListener(connectionChanged);
      await events.cancel();
    }
  }
}
