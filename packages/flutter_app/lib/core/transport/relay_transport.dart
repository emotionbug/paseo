import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'io_transport.dart';
import 'relay_crypto.dart';
import 'relay_records.dart';

/// Security failures are terminal, not ordinary network reconnect signals.
class RelaySecurityException implements Exception {
  const RelaySecurityException(this.closeCode, this.message);

  final int closeCode;
  final String message;

  @override
  String toString() => 'RelaySecurityException ($closeCode): $message';
}

/// The caller opens [inner] without daemon credentials in the relay upgrade.
/// Credentials may only be sent through the returned, handshaken transport.
Future<DaemonTransport> protectRelayTransport(
  DaemonTransport inner, {
  required String daemonPublicKey,
  required bool requireProtected,
  required void Function() onProtected,
}) {
  final transport = RelayTransport._(inner, requireProtected, onProtected);
  // Attach the returned future before starting async key derivation so early
  // socket errors are delivered to the factory's caller, not an orphan future.
  final ready = transport._ready.future;
  unawaited(transport._start(daemonPublicKey));
  return ready;
}

class RelayTransport implements DaemonTransport {
  RelayTransport._(this._inner, this._requireProtected, this._onProtected) {
    _messages.onCancel = close;
    _subscription = _inner.messages.listen(
      _enqueue,
      onError: (Object error, StackTrace stack) {
        _fail(error, stack, 1000, 'Relay transport failed');
      },
      onDone: () {
        // Socket completion must not overtake records already being handled.
        _receiveQueue = _receiveQueue.then((_) async {
          await _initialized.future;
          if (_closed) return;
          if (!_open) {
            _fail(
              StateError('Relay closed during handshake'),
              StackTrace.current,
              1000,
              'Relay closed during handshake',
            );
          } else {
            unawaited(
              close().catchError((Object error, StackTrace stack) {
                // Completion already closed the output stream; the physical
                // connection was closed by the peer, so no work can be retried.
              }),
            );
          }
        });
      },
    );
    _deadline = Timer(const Duration(seconds: 15), () {
      _fail(
        TimeoutException('Relay security handshake timed out'),
        StackTrace.current,
        1000,
        'Relay handshake timed out',
      );
    });
  }

  final DaemonTransport _inner;
  final bool _requireProtected;
  final void Function() _onProtected;
  // Single subscription controller deliberately buffers before the client can
  // subscribe, including records immediately following ready in the same turn.
  final _messages = StreamController<Object>();
  final _ready = Completer<DaemonTransport>();
  final _initialized = Completer<void>();
  StreamSubscription<Object>? _subscription;
  Future<void> _receiveQueue = Future<void>.value();
  Future<void>? _closeFuture;
  Timer? _retry;
  Timer? _deadline;
  RelayNoiseInitiator? _noise;
  RelayLegacyBox? _legacy;
  RelayNoiseCipher? _sender;
  RelayNoiseCipher? _receiver;
  bool _open = false;
  bool _closed = false;
  bool _protectedMode = false;
  bool _binaryCiphertext = false;

  bool get protectedMode => _protectedMode;

  @override
  Stream<Object> get messages => _messages.stream;

  Future<void> _start(String daemonPublicKey) async {
    try {
      final remote = importRelayPublicKey(daemonPublicKey);
      final legacy = await RelayLegacyBox.create(remote);
      if (_closed) {
        legacy.clear();
        return;
      }
      _legacy = legacy;
      final noise = await RelayNoiseInitiator.create(remote);
      if (_closed) {
        noise.clear();
        return;
      }
      _noise = noise;
      final hello = jsonEncode({
        'type': 'e2ee_hello',
        'key': base64.encode(legacy.publicKey),
        'capabilities': {'binaryCiphertext': true},
        'protected': {'v': 1, 'message': base64.encode(noise.helloMessage)},
      });
      void sendHello() {
        if (_closed || _open) return;
        try {
          _inner.send(
            hello,
          ); // Retry byte-identical text; never regenerate keys.
        } catch (error, stack) {
          _fail(error, stack, 1000, 'Relay hello send failed');
        }
      }

      sendHello();
      if (!_closed) {
        _retry = Timer.periodic(const Duration(seconds: 1), (_) => sendHello());
      }
    } catch (error, stack) {
      _securityFailure(error, stack, 4002);
    } finally {
      _initialized.complete();
    }
  }

  void _enqueue(Object data) {
    _receiveQueue = _receiveQueue.then((_) async {
      await _initialized.future;
      if (_closed) return;
      try {
        await _receive(data);
      } catch (error, stack) {
        _securityFailure(error, stack, _open ? 4003 : 4002);
      }
    });
  }

  Future<void> _receive(Object data) async {
    if (!_open) {
      if (data is! String) return;
      Object? parsed;
      try {
        parsed = jsonDecode(data);
      } on FormatException {
        return; // Non-handshake traffic while waiting for ready.
      }
      if (parsed is! Map || parsed['type'] != 'e2ee_ready') return;
      final capabilities = parsed['capabilities'];
      if (parsed.containsKey('capabilities') &&
          (capabilities is! Map ||
              (capabilities.containsKey('binaryCiphertext') &&
                  capabilities['binaryCiphertext'] is! bool))) {
        throw const FormatException('Malformed relay ready capabilities');
      }
      // Presence (including null) commits to protected; malformed NEVER falls
      // through to the legacy box, regardless of the configured security floor.
      if (parsed.containsKey('protected')) {
        final envelope = parsed['protected'];
        if (envelope is! Map ||
            envelope['v'] != 1 ||
            envelope['message'] is! String ||
            (envelope['message'] as String).isEmpty) {
          throw const FormatException('Malformed protected relay ready');
        }
        final keys = await _noise!.finish(
          decodeRelayBase64(envelope['message'] as String),
        );
        if (_closed) {
          keys.send.clear();
          keys.receive.clear();
          return;
        }
        _sender = keys.send;
        _receiver = keys.receive;
        _binaryCiphertext = keys.binaryCiphertext;
        _protectedMode = true;
        _legacy?.clear();
        _legacy = null;
        _onProtected();
      } else {
        if (_requireProtected) {
          throw const FormatException(
            'Daemon offered only legacy endpoint security',
          );
        }
        _binaryCiphertext =
            capabilities is Map && capabilities['binaryCiphertext'] == true;
      }
      _noise?.clear();
      _noise = null;
      if (_closed) return;
      _open = true;
      _retry?.cancel();
      _deadline?.cancel();
      _ready.complete(this);
      return;
    }

    if (data is String && data.trimLeft().startsWith('{')) {
      final parsed = jsonDecode(data);
      if (parsed is Map &&
          (parsed['type'] == 'e2ee_ready' || parsed['type'] == 'e2ee_hello')) {
        return; // Duplicated ready never resets keys or counters.
      }
      throw const FormatException('Received plaintext on encrypted relay');
    }
    if (data is! String && data is! Uint8List) {
      throw const FormatException('Unsupported relay WebSocket frame');
    }

    Uint8List record;
    if (data is String) {
      record = decodeRelayBase64(data);
    } else {
      record = data as Uint8List;
      if (!_protectedMode && !_binaryCiphertext) {
        // Historical adapters lost the opcode; only this legacy mode tries
        // base64 first. Protected framing always authenticates the frame kind.
        try {
          record = decodeRelayBase64(utf8.decode(record));
        } on FormatException {
          // Original bytes are the old adapter's binary ciphertext.
        }
      }
    }
    final Object plaintext;
    if (_protectedMode) {
      final decoded = decodeRelayRecord(record, _receiver!);
      plaintext = decoded.binary ? decoded.bytes : utf8.decode(decoded.bytes);
    } else {
      final bytes = _legacy!.decrypt(record);
      if (_binaryCiphertext) {
        plaintext = data is Uint8List ? bytes : utf8.decode(bytes);
      } else {
        // Pre-binaryCiphertext peers have no kind marker. Match their historical
        // strict-UTF8-else-bytes behavior, rather than guessing from JSON content.
        Object legacyPlaintext;
        try {
          legacyPlaintext = utf8.decode(bytes);
        } on FormatException {
          legacyPlaintext = bytes;
        }
        plaintext = legacyPlaintext;
      }
    }
    if (!_closed) _messages.add(plaintext);
  }

  @override
  void send(Object data) {
    if (_closed || !_open) throw StateError('Relay transport is not open');
    if (data is! String && data is! Uint8List) {
      throw ArgumentError.value(data, 'data', 'Expected String or Uint8List');
    }
    final binary = data is Uint8List;
    final bytes = binary
        ? data
        : Uint8List.fromList(utf8.encode(data as String));
    final Uint8List record;
    try {
      record = _protectedMode
          ? encodeRelayRecord(bytes, binary: binary, sender: _sender!)
          : _legacy!.encrypt(bytes);
    } catch (error, stack) {
      _securityFailure(error, stack, 4003);
      throw RelaySecurityException(
        4003,
        'Relay record encryption failed: $error',
      );
    }
    // Encryption and the physical send are synchronous. Multiple callers
    // cannot reorder records or reuse/interleave any reserved chunk nonces.
    try {
      _inner.send(binary && _binaryCiphertext ? record : base64.encode(record));
    } catch (error, stack) {
      _fail(error, stack, 1000, 'Relay record send failed');
      rethrow;
    }
  }

  void _securityFailure(Object error, StackTrace stack, int code) {
    final phase = code == 4002 ? 'handshake' : 'record';
    _fail(
      RelaySecurityException(code, 'Relay $phase failed: $error'),
      stack,
      code,
      'Relay $phase authentication failed',
    );
  }

  void _fail(Object error, StackTrace stack, int code, String reason) {
    if (_closed) return;
    if (!_ready.isCompleted) {
      _ready.completeError(error, stack);
    } else {
      _messages.addError(error, stack);
    }
    // The original error has already been reported. A secondary physical-close
    // failure must neither replace it nor become an unhandled async exception.
    unawaited(close(code, reason).catchError((Object _) {}));
  }

  @override
  Future<void> close([int? code, String? reason]) {
    final pending = _closeFuture;
    if (pending != null) return pending;
    _closed = true;
    _retry?.cancel();
    _deadline?.cancel();
    _noise?.clear();
    _noise = null;
    _legacy?.clear();
    _legacy = null;
    _sender?.clear();
    _sender = null;
    _receiver?.clear();
    _receiver = null;
    if (!_ready.isCompleted) {
      _ready.completeError(
        StateError('Relay closed before handshake completed'),
      );
    }
    // Do not await: a single-subscription stream's close future waits for its
    // eventual listener, and the factory may have failed before one can attach.
    unawaited(_messages.close());
    final subscription = _subscription;
    _subscription = null;
    return _closeFuture = () async {
      try {
        await subscription?.cancel();
      } finally {
        await _inner.close(code, reason);
      }
    }();
  }
}
