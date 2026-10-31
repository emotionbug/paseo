/// Native audio output for Paseo: FIFO playback of server-provided PCM16 and
/// encoded audio. The package never captures audio and never requests
/// microphone permission.
///
/// Integration: add a path dependency on this package. Android minimum SDK is 24
/// (compile SDK 35, Java 17); iOS requires deployment target 13; macOS requires
/// 10.15. No microphone usage descriptions, entitlements, or runtime
/// permissions are needed on any platform.
///
/// Android and iOS play PCM16 natively and own the platform audio session
/// (Android audio focus, iOS AVAudioSession playback category with
/// interruption, route and media-services-reset handling). Encoded audio on
/// every platform, and PCM16 on desktop, uses flutter_soloud's native engine.
/// Desktop operating systems do not expose mobile-style audio focus through
/// these packages, so no synthetic focus-granted events are emitted.
library;

import 'dart:async';
import 'dart:collection';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/services.dart';
import 'package:flutter_soloud/flutter_soloud.dart';

/// Event types: `playbackStarted`/`playbackEnded` (with [id], [cancelled]),
/// `audioVolume` (output [level] 0..1, [state] `output`), `audioFocus` and
/// `interruption` (with [state]), and `error` (with [error]).
class AudioEngineEvent {
  const AudioEngineEvent({
    required this.type,
    this.level,
    this.id,
    this.cancelled = false,
    this.error,
    this.state,
  });
  final String type;
  final double? level;
  final String? id;
  final bool cancelled;
  final String? error;
  final String? state;

  factory AudioEngineEvent.fromMap(Map<Object?, Object?> map) =>
      AudioEngineEvent(
        type: map['type'] as String,
        level: (map['level'] as num?)?.toDouble(),
        id: map['id'] as String?,
        cancelled: map['cancelled'] == true,
        error: map['error'] as String?,
        state: map['state'] as String?,
      );
}

/// One process-wide audio session. Playback is FIFO across PCM and encoded data.
/// Futures resolve only when the native playback head/voice has ended; cancellation
/// resolves false for every outstanding item. Method/decoder errors throw;
/// asynchronous native device errors are also delivered through [events].
class FlutterAudioEngine {
  factory FlutterAudioEngine() => _shared ??= FlutterAudioEngine._();
  FlutterAudioEngine._();
  static FlutterAudioEngine? _shared;
  static const _methods = MethodChannel('app.paseo/audio');
  static const _nativeEvents = EventChannel('app.paseo/audio/events');
  final _events = StreamController<AudioEngineEvent>.broadcast();
  final _queue = Queue<_Playback>();
  final _ids = <String>{};
  StreamSubscription<dynamic>? _nativeSubscription;
  Future<void>? _initializing;
  Future<void>? _drainTask;
  Future<void>? _soloudInitializing;
  Completer<bool>? _nativeCompletion;
  _Playback? _current;
  SoundHandle? _handle;
  bool _initialized = false;
  bool _disposed = false;
  bool _ownsSoloud = false;
  bool _playbackSuspended = false;
  Completer<void>? _playbackResume;
  int _generation = 0;
  int _sourceSerial = 0;
  Stream<AudioEngineEvent> get events => _events.stream;
  bool get _mobile => Platform.isAndroid || Platform.isIOS;

  Future<void> initialize() {
    if (_disposed) throw StateError('Audio engine disposed');
    if (_initialized) return Future.value();
    return _initializing ??= _initialize().whenComplete(
      () => _initializing = null,
    );
  }

  Future<void> _initialize() async {
    if (_mobile) {
      _nativeSubscription = _nativeEvents.receiveBroadcastStream().listen(
        (dynamic value) {
          final event = AudioEngineEvent.fromMap(
            (value as Map).cast<Object?, Object?>(),
          );
          if (event.type == 'playbackPaused' ||
              event.type == 'playbackResumed') {
            _playbackSuspended = event.type == 'playbackPaused';
            if (!_playbackSuspended) {
              _playbackResume?.complete();
              _playbackResume = null;
            }
            final handle = _handle;
            if (handle != null && SoLoud.instance.isInitialized) {
              try {
                SoLoud.instance.setPause(handle, _playbackSuspended);
              } catch (error) {
                _report(error);
              }
            }
            return;
          }
          if (event.type == 'playbackEnded') {
            if (event.id == _current?.id &&
                !(_nativeCompletion?.isCompleted ?? true)) {
              _nativeCompletion!.complete(!event.cancelled);
            }
            return;
          }
          _emit(event);
          if (event.type == 'interruption' &&
              (event.state == 'began' || event.state == 'blocked')) {
            unawaited(cancelPlayback().catchError(_report));
          }
        },
        onError: (Object error, StackTrace stack) {
          _report(error, stack);
          unawaited(cancelPlayback().catchError(_report));
        },
      );
      try {
        await _methods.invokeMethod<void>('initialize');
      } catch (_) {
        await _nativeSubscription?.cancel();
        _nativeSubscription = null;
        rethrow;
      }
    }
    _initialized = true;
  }

  void _emit(AudioEngineEvent event) {
    if (_events.isClosed) return;
    if (event.type == 'outputVolume') {
      event = AudioEngineEvent(
        type: 'audioVolume',
        level: event.level,
        state: 'output',
      );
    }
    _events.add(event);
  }

  void _report(Object error, [StackTrace? stack]) =>
      _emit(AudioEngineEvent(type: 'error', error: error.toString()));

  Future<bool> enqueuePlayback({
    required String id,
    required Uint8List pcm16,
    int sampleRate = 16000,
  }) {
    if (sampleRate != 16000 || pcm16.isEmpty || pcm16.length.isOdd) {
      throw ArgumentError('Playback requires nonempty PCM16 mono 16000 Hz');
    }
    return _enqueue(_Playback(id, pcm16, null));
  }

  /// Uses SoLoud's native decoder. WAV/MP3/FLAC/OGG support is determined by
  /// the decoder, never by a guessed duration. Unsupported bytes throw.
  Future<bool> playEncoded({
    required String id,
    required Uint8List bytes,
    required String mimeType,
  }) {
    if (bytes.isEmpty) throw ArgumentError('Encoded audio is empty');
    return _enqueue(_Playback(id, bytes, mimeType));
  }

  Future<bool> _enqueue(_Playback playback) async {
    await initialize();
    if (!_ids.add(playback.id))
      throw ArgumentError('Duplicate playback id: ${playback.id}');
    _queue.addLast(playback);
    _startDrain();
    return playback.completion.future;
  }

  void _startDrain() {
    if (_drainTask != null || _disposed) return;
    _drainTask = _drain().whenComplete(() {
      _drainTask = null;
      if (_queue.isNotEmpty && !_disposed) _startDrain();
    });
  }

  Future<void> _drain() async {
    while (_queue.isNotEmpty && !_disposed) {
      final playback = _queue.removeFirst();
      _current = playback;
      final generation = _generation;
      try {
        bool played;
        if (_mobile && playback.mimeType == null) {
          final completion = Completer<bool>();
          _nativeCompletion = completion;
          await _methods.invokeMethod<void>('enqueuePlayback', {
            'id': playback.id,
            'pcm16': playback.bytes,
            'sampleRate': 16000,
          });
          played = await completion.future;
          _nativeCompletion = null;
        } else {
          played = await _playWithSoloud(playback, generation);
        }
        _finish(playback, played && generation == _generation);
      } catch (error, stack) {
        if (generation == _generation && !playback.completion.isCompleted) {
          _report(error, stack);
          playback.completion.completeError(error, stack);
          _ids.remove(playback.id);
          _emit(
            AudioEngineEvent(
              type: 'playbackEnded',
              id: playback.id,
              cancelled: true,
              error: error.toString(),
            ),
          );
        }
      } finally {
        _nativeCompletion = null;
        _current = null;
      }
    }
  }

  Future<void> _ensureSoloud() => _soloudInitializing ??=
      (() async {
        if (!SoLoud.instance.isInitialized) {
          await SoLoud.instance.init();
          _ownsSoloud = true;
        }
        SoLoud.instance.setVisualizationEnabled(true);
      })().catchError((Object error) {
        _soloudInitializing = null;
        throw error;
      });

  Future<bool> _playWithSoloud(_Playback playback, int generation) async {
    await _ensureSoloud();
    if (generation != _generation || _disposed) return false;
    while (_playbackSuspended) {
      await (_playbackResume ??= Completer<void>()).future;
      if (generation != _generation || _disposed) return false;
    }
    var sessionHeld = false;
    AudioSource? source;
    try {
      if (_mobile) {
        await _methods.invokeMethod<void>('beginEncodedPlayback');
        sessionHeld = true;
      }
      if (generation != _generation || _disposed) return false;
      if (playback.mimeType == null) {
        source = SoLoud.instance.setBufferStream(
          maxBufferSizeBytes: math.max(playback.bytes.length * 4, 4096),
          sampleRate: 16000,
          channels: Channels.mono,
          format: BufferType.s16le,
          bufferingTimeNeeds: 0,
        );
        SoLoud.instance.addAudioDataStream(source, playback.bytes);
        SoLoud.instance.setDataIsEnded(source);
      } else {
        source = await SoLoud.instance.loadMem(
          'paseo_audio_${_sourceSerial++}',
          playback.bytes,
        );
      }
      if (generation != _generation || _disposed) return false;
      final handle = SoLoud.instance.play(source, paused: _playbackSuspended);
      if (handle.id <= 0)
        throw StateError(
          'Native audio decoder could not create a playback voice',
        );
      _handle = handle;
      _emit(AudioEngineEvent(type: 'playbackStarted', id: playback.id));
      // Query the real native voice lifetime. This is not a chunk-duration timer;
      // paused or device-delayed playback cannot complete just because time elapsed.
      while (SoLoud.instance.getIsValidVoiceHandle(handle)) {
        if (generation != _generation || _disposed) return false;
        _emit(
          AudioEngineEvent(
            type: 'outputVolume',
            level: SoLoud.instance.getApproximateVolume(0).clamp(0.0, 1.0),
          ),
        );
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      return generation == _generation;
    } finally {
      _handle = null;
      try {
        if (source != null) await SoLoud.instance.disposeSource(source);
      } finally {
        _emit(const AudioEngineEvent(type: 'outputVolume', level: 0));
        if (sessionHeld)
          await _methods.invokeMethod<void>('endEncodedPlayback');
      }
    }
  }

  void _finish(_Playback playback, bool played) {
    if (playback.completion.isCompleted) return;
    _ids.remove(playback.id);
    playback.completion.complete(played);
    _emit(
      AudioEngineEvent(
        type: 'playbackEnded',
        id: playback.id,
        cancelled: !played,
      ),
    );
  }

  Future<void> cancelPlayback() async {
    _generation++;
    _playbackResume?.complete();
    _playbackResume = null;
    final current = _current;
    if (current != null) _finish(current, false);
    while (_queue.isNotEmpty) {
      _finish(_queue.removeFirst(), false);
    }
    final native = _nativeCompletion;
    if (native != null && !native.isCompleted) native.complete(false);
    if (_initialized && _mobile)
      await _methods.invokeMethod<void>('cancelPlayback');
    final handle = _handle;
    if (handle != null && SoLoud.instance.isInitialized)
      await SoLoud.instance.stop(handle);
  }

  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    Object? firstError;
    StackTrace? firstStack;
    Future<void> cleanup(Future<void> Function() action) async {
      try {
        await action();
      } catch (error, stack) {
        firstError ??= error;
        firstStack ??= stack;
      }
    }

    await cleanup(() async {
      await _initializing;
    });
    await cleanup(cancelPlayback);
    await cleanup(() async {
      await _drainTask;
    });
    await cleanup(() async {
      if (_mobile && _initialized) await _methods.invokeMethod<void>('dispose');
    });
    await cleanup(() async {
      await _nativeSubscription?.cancel();
    });
    await cleanup(() async {
      if (_ownsSoloud && SoLoud.instance.isInitialized)
        SoLoud.instance.deinit();
    });
    _initialized = false;
    if (identical(_shared, this)) _shared = null;
    await _events.close();
    if (firstError != null) Error.throwWithStackTrace(firstError!, firstStack!);
  }
}

class _Playback {
  _Playback(this.id, this.bytes, this.mimeType);
  final String id;
  final Uint8List bytes;
  final String? mimeType;
  final completion = Completer<bool>();
}
