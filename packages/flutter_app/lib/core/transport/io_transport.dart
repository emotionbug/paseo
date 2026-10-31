import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import 'binary_frames.dart' show maxWebSocketMessageBytes;

/// A connected, single-subscription daemon message stream.
/// Text is String; binary is Uint8List. Unsubscribed messages remain buffered.
abstract class DaemonTransport {
  Stream<Object> get messages;
  void send(Object data);
  Future<void> close([int? code, String? reason]);
}

final class DaemonTransportClosedException implements IOException {
  const DaemonTransportClosedException(this.closeCode, [this.reason]);

  final int closeCode;
  final String? reason;

  @override
  String toString() => reason == null || reason!.isEmpty
      ? 'Daemon transport closed ($closeCode)'
      : 'Daemon transport closed ($closeCode): $reason';
}

/// Shared application-message limit. Counts UTF-8 without allocating a second
/// copy of text; malformed surrogate code units encode as replacement characters.
void validateDaemonTransportMessage(Object data) {
  if (data is Uint8List) {
    if (data.length <= maxWebSocketMessageBytes) return;
  } else if (data is String) {
    // One UTF-16 code unit needs at most three UTF-8 bytes.
    if (data.length <= maxWebSocketMessageBytes ~/ 3) return;
    if (data.length <= maxWebSocketMessageBytes) {
      var bytes = 0;
      for (var index = 0; index < data.length; index++) {
        final unit = data.codeUnitAt(index);
        if (unit <= 0x7f) {
          bytes++;
        } else if (unit <= 0x7ff) {
          bytes += 2;
        } else if (unit >= 0xd800 &&
            unit <= 0xdbff &&
            index + 1 < data.length &&
            data.codeUnitAt(index + 1) >= 0xdc00 &&
            data.codeUnitAt(index + 1) <= 0xdfff) {
          bytes += 4;
          index++;
        } else {
          bytes += 3;
        }
        if (bytes > maxWebSocketMessageBytes) {
          throw RangeError('Daemon WebSocket message exceeds 100 MiB');
        }
      }
      return;
    }
  } else {
    throw ArgumentError('Transport data must be String or Uint8List');
  }
  throw RangeError('Daemon WebSocket message exceeds 100 MiB');
}

/// Connects already-normalized endpoints. Credentials for relay endpoints must
/// be omitted by the caller; relay protection is applied above this transport.
Future<DaemonTransport> connectDaemonTransport(
  Uri endpoint, {
  Map<String, String> headers = const {},
  List<String>? protocols,
}) async {
  final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
  _SocketResource? resource;
  var websocketEndpoint = endpoint;
  try {
    switch (endpoint.scheme) {
      case 'ws':
      case 'wss':
        break;
      case 'unix':
        if (Platform.isWindows) {
          throw UnsupportedError('Unix sockets are not available on Windows');
        }
        if (endpoint.path.isEmpty) {
          throw const FormatException('Unix socket path is required');
        }
        final socket = await Socket.connect(
          InternetAddress(endpoint.path, type: InternetAddressType.unix),
          0,
          timeout: const Duration(seconds: 10),
        );
        resource = _SocketResource(socket);
        websocketEndpoint = Uri.parse('ws://localhost/ws');
      case 'pipe':
        resource = await _openWindowsPipe(endpoint.path);
        websocketEndpoint = Uri.parse('ws://localhost/ws');
      case 'ssh':
        resource = await _openSsh(SshTransportTarget.fromUri(endpoint));
        websocketEndpoint = Uri.parse('ws://localhost/ws');
      default:
        throw UnsupportedError('Unsupported daemon transport scheme');
    }
    if (resource != null) {
      final connection = resource;
      var claimed = false;
      client.findProxy = (_) => 'DIRECT';
      client.connectionFactory = (_, _, _) async {
        if (claimed) throw StateError('Daemon socket has already been claimed');
        claimed = true;
        return ConnectionTask.fromSocket(
          Future.value(connection.socket),
          () => unawaited(connection.close()),
        );
      };
    }
    final socket = await WebSocket.connect(
      websocketEndpoint.toString(),
      headers: headers,
      protocols: protocols,
      customClient: client,
      maxPayloadLength: maxWebSocketMessageBytes,
    ).timeout(const Duration(seconds: 15));
    return _WebSocketTransport(socket, client, resource);
  } catch (error) {
    final failure = resource?.failure;
    client.close(force: true);
    await resource?.close();
    if (failure != null) throw failure;
    rethrow;
  }
}

final class _WebSocketTransport implements DaemonTransport {
  _WebSocketTransport(this._socket, this._client, this._resource) {
    _messages = StreamController<Object>(
      onPause: () => _subscription.pause(),
      onResume: () => _subscription.resume(),
      onCancel: close,
    );
    _subscription = _socket.listen(
      (dynamic data) {
        if (_disposed) return;
        if (data is String || data is Uint8List || data is List<int>) {
          try {
            if (data is List<int> && data.length > maxWebSocketMessageBytes) {
              throw RangeError('Daemon WebSocket message exceeds 100 MiB');
            }
            final Object message = data is List<int> && data is! Uint8List
                ? Uint8List.fromList(data)
                : data as Object;
            validateDaemonTransportMessage(message);
            _messages.add(message);
          } on RangeError catch (error, stack) {
            _messages.addError(error, stack);
            unawaited(close(1009, 'Message exceeds 100 MiB'));
          }
        } else {
          _messages.addError(
            const FormatException('Invalid WebSocket message'),
          );
          unawaited(close(1002, 'Invalid WebSocket message'));
        }
      },
      onError: (Object error, StackTrace stack) {
        if (!_disposed) _messages.addError(error, stack);
        unawaited(close());
      },
      onDone: () {
        if (!_disposed && _closing == null) {
          final code = _socket.closeCode ?? 1006;
          if (code != 1000 && code != 1001) {
            _messages.addError(
              DaemonTransportClosedException(code, _socket.closeReason),
            );
          }
        }
        unawaited(_dispose());
      },
    );
    _resourceSubscription = _resource?.failures.stream.listen((error) {
      if (!_disposed) _messages.addError(error);
      unawaited(close());
    });
  }

  final WebSocket _socket;
  final HttpClient _client;
  final _SocketResource? _resource;
  late final StreamController<Object> _messages;
  late final StreamSubscription<dynamic> _subscription;
  StreamSubscription<Object>? _resourceSubscription;
  Future<void>? _closing;
  Future<void>? _disposing;
  bool _disposed = false;

  @override
  Stream<Object> get messages => _messages.stream;

  @override
  void send(Object data) {
    validateDaemonTransportMessage(data);
    if (_disposed || _closing != null || _socket.readyState != WebSocket.open) {
      throw StateError('Daemon transport is closed');
    }
    _socket.add(data);
  }

  @override
  Future<void> close([int? code, String? reason]) =>
      _closing ??= _close(code, reason);

  Future<void> _close(int? code, String? reason) async {
    try {
      if (!_disposed) {
        await _socket.close(code, reason).timeout(const Duration(seconds: 2));
      }
    } on TimeoutException {
      // Tear down the owned socket/process even when a peer never answers close.
    } finally {
      await _dispose();
    }
  }

  Future<void> _dispose() => _disposing ??= _disposeResources();

  Future<void> _disposeResources() async {
    _disposed = true;
    await _resourceSubscription?.cancel();
    await _subscription.cancel();
    _client.close(force: true);
    await _resource?.close();
    // A single-subscription controller's close future waits for a listener.
    // Teardown must also complete when nobody ever subscribed.
    unawaited(_messages.close());
  }
}

/// Mirrors protocol/src/ssh-transport.ts. SSH configuration and agent keys are
/// used by the system ssh executable; no command shell or remote shell is used.
final class SshTransportTarget {
  SshTransportTarget({
    required String host,
    this.sshPort,
    this.daemonPort = 6767,
  }) : host = _validateSshHost(host) {
    if (sshPort != null) _validatePort(sshPort!, 'SSH port');
    _validatePort(daemonPort, 'Daemon port');
  }

  factory SshTransportTarget.fromUri(Uri uri) {
    if (uri.scheme != 'ssh' ||
        (uri.path.isNotEmpty && uri.path != '/') ||
        uri.fragment.isNotEmpty) {
      throw const FormatException('Invalid SSH host URI');
    }
    final colon = uri.userInfo.indexOf(':');
    if (colon >= 0 && colon < uri.userInfo.length - 1) {
      throw const FormatException(
        'SSH passwords in host URIs are not supported',
      );
    }
    for (final key in uri.queryParametersAll.keys) {
      if (key != 'daemonPort') {
        throw const FormatException('Unsupported SSH host option');
      }
    }
    final ports = uri.queryParametersAll['daemonPort'];
    if (ports != null && ports.length != 1) {
      throw const FormatException('daemonPort may only be specified once');
    }
    final daemonPort = ports == null ? 6767 : int.tryParse(ports.single);
    if (daemonPort == null) throw const FormatException('Invalid daemon port');
    final host = _validateSshHost(uri.host);
    final username = Uri.decodeComponent(
      colon < 0 ? uri.userInfo : uri.userInfo.substring(0, colon),
    );
    return SshTransportTarget(
      host: username.isEmpty ? host : '$username@$host',
      sshPort: uri.hasPort ? uri.port : null,
      daemonPort: daemonPort,
    );
  }

  final String host;
  final int? sshPort;
  final int daemonPort;
}

List<String> buildSshTunnelArgs(SshTransportTarget target) => [
  '-T',
  '-o',
  'BatchMode=yes',
  '-o',
  'ConnectTimeout=10',
  '-o',
  'ClearAllForwardings=yes',
  '-o',
  'ExitOnForwardFailure=yes',
  if (target.sshPort != null) ...['-p', '${target.sshPort}'],
  '-W',
  '127.0.0.1:${target.daemonPort}',
  target.host,
];

String _validateSshHost(String host) {
  final normalized = host.trim();
  if (normalized.isEmpty ||
      normalized.startsWith('-') ||
      RegExp(r'\s').hasMatch(normalized) ||
      normalized.contains('\u0000')) {
    throw const FormatException('Invalid SSH host');
  }
  return normalized;
}

void _validatePort(int port, String label) {
  if (port < 1 || port > 65535) {
    throw FormatException('$label must be between 1 and 65535');
  }
}

final class _SocketResource {
  _SocketResource(this.socket, {this.peer, this.cleanup});

  final Socket socket;
  final Socket? peer;
  final Future<void> Function()? cleanup;
  final StreamController<Object> failures = StreamController<Object>();
  Object? failure;
  Future<void>? _closing;
  bool get closed => _closing != null;

  void fail(Object error) {
    if (closed) return;
    failure ??= error;
    failures.add(error);
    unawaited(close());
  }

  Future<void> close() => _closing ??= _close();

  Future<void> _close() async {
    socket.destroy();
    peer?.destroy();
    try {
      await cleanup?.call();
    } finally {
      unawaited(failures.close());
    }
  }
}

/// A socket pair lets dart:io own the HTTP upgrade and RFC6455 implementation
/// while the other end is a full-duplex subprocess or Windows pipe.
Future<(Socket, Socket)> _socketPair() async {
  final listener = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  Socket? client;
  Socket? peer;
  final accepted = Completer<Socket>();
  final subscription = listener.listen(
    (socket) {
      if (accepted.isCompleted) {
        socket.destroy();
      } else {
        peer = socket;
        accepted.complete(socket);
      }
    },
    onError: (Object error, StackTrace stack) {
      if (!accepted.isCompleted) accepted.completeError(error, stack);
    },
  );
  try {
    client = await Socket.connect(InternetAddress.loopbackIPv4, listener.port);
    final server = await accepted.future;
    if (server.remotePort != client.port) {
      throw const SocketException(
        'Unexpected connection to local transport bridge',
      );
    }
    client.setOption(SocketOption.tcpNoDelay, true);
    server.setOption(SocketOption.tcpNoDelay, true);
    return (client, server);
  } catch (_) {
    client?.destroy();
    peer?.destroy();
    rethrow;
  } finally {
    await subscription.cancel();
    await listener.close();
  }
}

Future<_SocketResource> _openSsh(SshTransportTarget target) async {
  final (socket, peer) = await _socketPair();
  Process process;
  try {
    process = await Process.start('ssh', buildSshTunnelArgs(target));
  } catch (_) {
    socket.destroy();
    peer.destroy();
    rethrow;
  }
  var stderr = '';
  late final StreamSubscription<String> stderrSubscription;
  late final _SocketResource resource;
  late final Future<void> inputPump;
  resource = _SocketResource(
    socket,
    peer: peer,
    cleanup: () async {
      process.kill();
      try {
        await process.exitCode.timeout(const Duration(seconds: 2));
      } on TimeoutException {
        process.kill(ProcessSignal.sigkill);
        await process.exitCode;
      } finally {
        await inputPump.catchError((Object _) {});
        try {
          await process.stdin.close();
        } finally {
          await stderrSubscription.cancel();
        }
      }
    },
  );
  stderrSubscription = process.stderr
      .transform(const Utf8Decoder(allowMalformed: true))
      .listen((chunk) {
        stderr += chunk;
        if (stderr.length > 8192) {
          stderr = stderr.substring(stderr.length - 8192);
        }
      }, onError: (Object error) => resource.fail(error));
  unawaited(
    process.exitCode.then((code) {
      if (!resource.closed && code != 0) {
        final detail = stderr.trim();
        resource.fail(
          ProcessException(
            'ssh',
            const [],
            detail.isEmpty ? 'SSH exited with code $code' : detail,
            code,
          ),
        );
      }
    }),
  );
  unawaited(
    peer.addStream(process.stdout).then<void>((_) async {
      if (resource.closed) return;
      try {
        await peer.flush();
        await process.exitCode;
        await resource.close();
      } catch (error) {
        resource.fail(error);
      }
    }, onError: (Object error) => resource.fail(error)),
  );
  inputPump = process.stdin.addStream(peer);
  unawaited(
    inputPump.then<void>((_) {
      unawaited(resource.close());
    }, onError: (Object error) => resource.fail(error)),
  );
  return resource;
}

Future<_SocketResource> _openWindowsPipe(String path) async {
  if (!Platform.isWindows) {
    throw UnsupportedError('Windows named pipes are only available on Windows');
  }
  final pipe = await _WindowsPipe.open(path);
  Socket socket;
  Socket peer;
  try {
    (socket, peer) = await _socketPair();
  } catch (_) {
    await pipe.close();
    rethrow;
  }
  final resource = _SocketResource(socket, peer: peer, cleanup: pipe.close);
  unawaited(
    pipe.pumpTo(peer).then<void>((_) {
      unawaited(resource.close());
    }, onError: (Object error) => resource.fail(error)),
  );
  unawaited(
    pipe.pumpFrom(peer).then<void>((_) {
      unawaited(resource.close());
    }, onError: (Object error) => resource.fail(error)),
  );
  return resource;
}

// OVERLAPPED contains pointer-sized status words and an offset union. The two
// DWORD fields cover that union on both 32- and 64-bit Windows.
final class _Overlapped extends Struct {
  @UintPtr()
  external int internal;
  @UintPtr()
  external int internalHigh;
  @Uint32()
  external int offset;
  @Uint32()
  external int offsetHigh;
  @IntPtr()
  external int event;
}

final class _WindowsApi {
  _WindowsApi() {
    final library = DynamicLibrary.open('kernel32.dll');
    createFile = library
        .lookupFunction<
          IntPtr Function(
            Pointer<Utf16>,
            Uint32,
            Uint32,
            Pointer<Void>,
            Uint32,
            Uint32,
            IntPtr,
          ),
          int Function(Pointer<Utf16>, int, int, Pointer<Void>, int, int, int)
        >('CreateFileW');
    createEvent = library
        .lookupFunction<
          IntPtr Function(Pointer<Void>, Int32, Int32, Pointer<Utf16>),
          int Function(Pointer<Void>, int, int, Pointer<Utf16>)
        >('CreateEventW');
    closeHandle = library
        .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
          'CloseHandle',
        );
    resetEvent = library
        .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
          'ResetEvent',
        );
    readFile = library
        .lookupFunction<
          Int32 Function(
            IntPtr,
            Pointer<Void>,
            Uint32,
            Pointer<Uint32>,
            Pointer<_Overlapped>,
          ),
          int Function(
            int,
            Pointer<Void>,
            int,
            Pointer<Uint32>,
            Pointer<_Overlapped>,
          )
        >('ReadFile');
    writeFile = library
        .lookupFunction<
          Int32 Function(
            IntPtr,
            Pointer<Void>,
            Uint32,
            Pointer<Uint32>,
            Pointer<_Overlapped>,
          ),
          int Function(
            int,
            Pointer<Void>,
            int,
            Pointer<Uint32>,
            Pointer<_Overlapped>,
          )
        >('WriteFile');
    getOverlappedResult = library
        .lookupFunction<
          Int32 Function(IntPtr, Pointer<_Overlapped>, Pointer<Uint32>, Int32),
          int Function(int, Pointer<_Overlapped>, Pointer<Uint32>, int)
        >('GetOverlappedResult');
    cancelIo = library
        .lookupFunction<
          Int32 Function(IntPtr, Pointer<_Overlapped>),
          int Function(int, Pointer<_Overlapped>)
        >('CancelIoEx');
    lastError = library.lookupFunction<Uint32 Function(), int Function()>(
      'GetLastError',
    );
    wait = library
        .lookupFunction<
          Uint32 Function(IntPtr, Uint32),
          int Function(int, int)
        >('WaitForSingleObject');
  }

  static final instance = _WindowsApi();
  late final int Function(
    Pointer<Utf16>,
    int,
    int,
    Pointer<Void>,
    int,
    int,
    int,
  )
  createFile;
  late final int Function(Pointer<Void>, int, int, Pointer<Utf16>) createEvent;
  late final int Function(int) closeHandle;
  late final int Function(int) resetEvent;
  late final int Function(
    int,
    Pointer<Void>,
    int,
    Pointer<Uint32>,
    Pointer<_Overlapped>,
  )
  readFile;
  late final int Function(
    int,
    Pointer<Void>,
    int,
    Pointer<Uint32>,
    Pointer<_Overlapped>,
  )
  writeFile;
  late final int Function(int, Pointer<_Overlapped>, Pointer<Uint32>, int)
  getOverlappedResult;
  late final int Function(int, Pointer<_Overlapped>) cancelIo;
  late final int Function() lastError;
  late final int Function(int, int) wait;
}

final class _PipeIo {
  _PipeIo() {
    event = _WindowsApi.instance.createEvent(nullptr, 1, 0, nullptr);
    if (event == 0) {
      final code = _WindowsApi.instance.lastError();
      close();
      throw FileSystemException(
        'Creating pipe I/O event failed',
        '',
        OSError('', code),
      );
    }
    overlapped.ref.event = event;
  }

  static const capacity = 64 * 1024;
  final Pointer<Uint8> buffer = calloc<Uint8>(capacity);
  final Pointer<_Overlapped> overlapped = calloc<_Overlapped>();
  final Pointer<Uint32> transferred = calloc<Uint32>();
  late final Uint8List bytes = buffer.asTypedList(capacity);
  late final int event;

  void close() {
    if (event != 0) _WindowsApi.instance.closeHandle(event);
    calloc.free(transferred);
    calloc.free(overlapped);
    calloc.free(buffer);
  }
}

/// Two persistent wait workers permit simultaneous read and write without
/// blocking Flutter's isolate or polling the pipe. Only integer HANDLE values
/// cross isolates; buffers remain owned until overlapped I/O has completed.
void _windowsWaitWorker(SendPort parent) {
  final commands = ReceivePort();
  parent.send(commands.sendPort);
  commands.listen((dynamic event) {
    final api = _WindowsApi.instance;
    final result = api.wait(event as int, 0xffffffff);
    parent.send(result == 0 ? 0 : api.lastError());
  });
}

final class _WindowsEventWaiter {
  _WindowsEventWaiter._(this._responses);

  final ReceivePort _responses;
  late final Isolate _isolate;
  late final SendPort _commands;
  Completer<void>? _pending;

  static Future<_WindowsEventWaiter> start() async {
    final waiter = _WindowsEventWaiter._(ReceivePort());
    final ready = Completer<void>();
    waiter._responses.listen((dynamic message) {
      if (message is SendPort) {
        waiter._commands = message;
        ready.complete();
      } else {
        final pending = waiter._pending;
        waiter._pending = null;
        if (message == 0) {
          pending?.complete();
        } else {
          pending?.completeError(
            FileSystemException(
              'Waiting for Windows pipe I/O failed',
              '',
              OSError('', message as int),
            ),
          );
        }
      }
    });
    try {
      waiter._isolate = await Isolate.spawn(
        _windowsWaitWorker,
        waiter._responses.sendPort,
      );
      await ready.future;
      return waiter;
    } catch (_) {
      waiter._responses.close();
      rethrow;
    }
  }

  Future<void> wait(int event) {
    if (_pending != null) {
      throw StateError('Overlapping waits on one pipe direction');
    }
    final completer = Completer<void>();
    _pending = completer;
    _commands.send(event);
    return completer.future;
  }

  void close() {
    _isolate.kill(priority: Isolate.immediate);
    _responses.close();
  }
}

final class _WindowsPipe {
  _WindowsPipe._(this._handle, this._readWaiter, this._writeWaiter);

  final int _handle;
  final _WindowsEventWaiter _readWaiter;
  final _WindowsEventWaiter _writeWaiter;
  Future<void>? _reading;
  Future<void>? _writing;
  Future<void>? _closing;
  bool _closed = false;

  static Future<_WindowsPipe> open(String path) async {
    final normalized = path.replaceAll('/', '\\');
    if (!normalized.toLowerCase().startsWith('\\\\.\\pipe\\') ||
        normalized.contains('\u0000')) {
      throw const FormatException(
        'An absolute Windows named pipe path is required',
      );
    }
    final api = _WindowsApi.instance;
    final name = normalized.toNativeUtf16(allocator: calloc);
    int handle;
    var openError = 0;
    try {
      handle = api.createFile(
        name,
        0xc0000000,
        0,
        nullptr,
        3,
        0x40000000,
        0,
      ); // GENERIC_READ|WRITE, OPEN_EXISTING, FILE_FLAG_OVERLAPPED.
      if (handle == -1) openError = api.lastError();
    } finally {
      calloc.free(name);
    }
    if (handle == -1) {
      throw FileSystemException(
        'Opening Windows named pipe failed',
        '',
        OSError('', openError),
      );
    }
    _WindowsEventWaiter? reader;
    try {
      reader = await _WindowsEventWaiter.start();
      final writer = await _WindowsEventWaiter.start();
      return _WindowsPipe._(handle, reader, writer);
    } catch (_) {
      reader?.close();
      api.closeHandle(handle);
      rethrow;
    }
  }

  Future<void> pumpTo(Socket sink) => _reading ??= _readTo(sink);
  Future<void> pumpFrom(Socket source) => _writing ??= _writeFrom(source);

  Future<void> _readTo(Socket sink) async {
    final io = _PipeIo();
    try {
      while (!_closed) {
        final count = await _operation(io, 0, _PipeIo.capacity, write: false);
        if (count == 0) return;
        sink.add(io.buffer.asTypedList(count));
        // Do not reuse the native buffer until the socket has consumed it.
        await sink.flush();
      }
    } finally {
      io.close();
    }
  }

  Future<void> _writeFrom(Socket source) async {
    final io = _PipeIo();
    try {
      await for (final chunk in source) {
        if (_closed) return;
        var offset = 0;
        while (offset < chunk.length && !_closed) {
          final remaining = chunk.length - offset;
          final length = remaining < _PipeIo.capacity
              ? remaining
              : _PipeIo.capacity;
          io.bytes.setRange(0, length, chunk, offset);
          var sent = 0;
          while (sent < length && !_closed) {
            final count = await _operation(
              io,
              sent,
              length - sent,
              write: true,
            );
            if (count == 0) return;
            sent += count;
          }
          offset += length;
        }
      }
    } finally {
      io.close();
    }
  }

  Future<int> _operation(
    _PipeIo io,
    int offset,
    int length, {
    required bool write,
  }) async {
    if (_closed) return 0;
    final api = _WindowsApi.instance;
    final overlapped = io.overlapped;
    final transferred = io.transferred;
    overlapped.ref
      ..internal = 0
      ..internalHigh = 0
      ..offset = 0
      ..offsetHigh = 0;
    transferred.value = 0;
    if (api.resetEvent(io.event) == 0) {
      throw FileSystemException(
        'Resetting pipe I/O event failed',
        '',
        OSError('', api.lastError()),
      );
    }
    final start = write ? api.writeFile : api.readFile;
    final succeeded = start(
      _handle,
      (io.buffer + offset).cast(),
      length,
      transferred,
      overlapped,
    );
    if (succeeded == 0) {
      final code = api.lastError();
      if (code == 109 || code == 232 || (_closed && code == 995)) return 0;
      if (code != 997) {
        throw FileSystemException(
          'Windows pipe I/O failed',
          '',
          OSError('', code),
        );
      }
      try {
        await (write ? _writeWaiter : _readWaiter).wait(io.event);
      } catch (_) {
        // Never release native memory while the kernel may still write it.
        api.cancelIo(_handle, overlapped);
        api.getOverlappedResult(_handle, overlapped, transferred, 1);
        rethrow;
      }
    }
    if (api.getOverlappedResult(_handle, overlapped, transferred, 0) == 0) {
      final code = api.lastError();
      if (code == 109 || code == 232 || (_closed && code == 995)) return 0;
      throw FileSystemException(
        'Windows pipe I/O failed',
        '',
        OSError('', code),
      );
    }
    return transferred.value;
  }

  Future<void> close() => _closing ??= _close();

  Future<void> _close() async {
    _closed = true;
    final api = _WindowsApi.instance;
    api.cancelIo(_handle, nullptr);
    // The bridge destroys both sockets before invoking this method, unblocking
    // the write pump's stream and any read pump waiting on socket backpressure.
    await Future.wait([
      if (_reading != null) _reading!.catchError((Object _) {}),
      if (_writing != null) _writing!.catchError((Object _) {}),
    ]);
    api.closeHandle(_handle);
    _readWaiter.close();
    _writeWaiter.close();
  }
}
