import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/foundation.dart';
import 'package:paseo_flutter_audio/paseo_flutter_audio.dart';
import 'package:uuid/uuid.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';

/// Original session-context `BufferedAudioChunk`.
typedef AudioOutputChunk = ({int chunkIndex, String audio, String format, String id});

/// Original `buildAudioPlaybackSource` MIME rule: daemon formats are bare
/// names; `pcm` is 24 kHz PCM16.
String audioOutputMimeType(String format) => switch (format) {
  'pcm' => 'audio/pcm;rate=24000;bits=16',
  'mp3' => 'audio/mpeg',
  _ => 'audio/$format',
};

/// Original `buildAudioPlaybackSource`: the group's chunks in index order,
/// concatenated into one source typed by the first chunk's format.
({Uint8List bytes, String mimeType}) buildAudioPlaybackSource(List<AudioOutputChunk> chunks) {
  final ordered = [...chunks]..sort((a, b) => a.chunkIndex - b.chunkIndex);
  final builder = BytesBuilder(copy: false);
  for (final chunk in ordered) {
    builder.add(base64Decode(chunk.audio));
  }
  return (
    bytes: builder.takeBytes(),
    mimeType: audioOutputMimeType(ordered.firstOrNull?.format ?? 'pcm'),
  );
}

/// Original `audio/pcm` `parsePcmSampleRate`.
int? parsePcmSampleRate(String mimeType) {
  final match = RegExp(r'rate=(\d+)', caseSensitive: false).firstMatch(mimeType);
  final rate = match == null ? null : int.tryParse(match.group(1)!);
  return rate != null && rate > 0 ? rate : null;
}

/// Original `audio/pcm` `resamplePcm16`: linear interpolation of
/// little-endian PCM16 between sample rates.
Uint8List resamplePcm16(Uint8List pcm, int fromRate, int toRate) {
  if (fromRate == toRate) return pcm;
  final inputSamples = pcm.length ~/ 2;
  final outputSamples = inputSamples * toRate ~/ fromRate;
  final out = Uint8List(outputSamples * 2);
  final ratio = fromRate / toRate;
  int read(int sampleIndex) {
    final i = sampleIndex * 2;
    if (i + 1 >= pcm.length) return 0;
    final value = (pcm[i + 1] << 8) | pcm[i];
    return value & 0x8000 != 0 ? value - 0x10000 : value;
  }

  for (var i = 0; i < outputSamples; i++) {
    final position = i * ratio;
    final i0 = position.floor();
    final fraction = position - i0;
    final s0 = read(i0);
    final s1 = read(math.min(inputSamples - 1, i0 + 1));
    final value = (s0 + (s1 - s0) * fraction).round().clamp(-32768, 32767);
    out[i * 2] = value & 0xff;
    out[i * 2 + 1] = (value >> 8) & 0xff;
  }
  return out;
}

/// Host-scoped output playback (original session-context `audio_output`
/// handling). Every host shares the one FIFO engine, as the original shared
/// its one audio engine. Microphone capture and voice mode are deliberately
/// not part of the client.
class AudioService extends ChangeNotifier {
  AudioService._(this.client) {
    _messages = client.events.listen(_onMessage);
  }

  static final _services = <PaseoClient, AudioService>{};
  static final _engine = FlutterAudioEngine();
  static VoidCallback? _removeOpenHook, _removeCloseHook;
  static StreamSubscription<AudioEngineEvent>? _audioEvents;
  static AudioService forClient(PaseoClient client) =>
      _services.putIfAbsent(client, () => AudioService._(client));

  /// The original listens on every host session, not only open agents.
  static void initialize(AppStore store) {
    _removeOpenHook?.call();
    _removeCloseHook?.call();
    _audioEvents ??= _engine.events.listen(_onAudioEvent);
    _removeOpenHook = store.registerClientOpenHook((client, _) => forClient(client));
    _removeCloseHook = store.registerClientCloseHook((client) async {
      await _services[client]?.close();
    });
  }

  static Future<void> shutdown() async {
    _removeOpenHook?.call();
    _removeCloseHook?.call();
    _removeOpenHook = _removeCloseHook = null;
    await _audioEvents?.cancel();
    _audioEvents = null;
    for (final service in _services.values.toList()) {
      await service.close();
    }
    _services.clear();
    await _engine.dispose();
  }

  final PaseoClient client;
  late final StreamSubscription<JsonMap> _messages;
  Object? error;
  bool _disposed = false;
  final _buffers = <String, List<AudioOutputChunk>>{};

  void _failure(Object failure) {
    if (_disposed) return;
    error = failure;
    notifyListeners();
  }

  /// The platform took the audio session away: nothing queued may resume.
  static void _onAudioEvent(AudioEngineEvent event) {
    if ((event.type == 'audioFocus' || event.type == 'interruption') && event.state == 'blocked') {
      unawaited(_engine.cancelPlayback().catchError((Object _) {}));
    }
  }

  /// Original `audio_output` handler: chunks buffer per group until the final
  /// one (`isLastChunk` defaults to true), then the group plays as one source
  /// and every chunk id is confirmed, whether or not playback succeeded.
  /// Voice-mode audio belongs to the voice runtime this client excludes, so it
  /// is confirmed without playing (the original's no-voice-runtime branch).
  void _onMessage(JsonMap message) {
    if (_disposed || message['type'] != 'audio_output') return;
    final payload = jsonMap(message['payload']);
    final id = stringValue(payload['id']);
    final groupId = stringValue(payload['groupId'], id);
    final chunks = _buffers.putIfAbsent(groupId, () => []);
    chunks.add((
      chunkIndex: intValue(payload['chunkIndex']),
      audio: stringValue(payload['audio']),
      format: stringValue(payload['format']),
      id: id,
    ));
    if (payload['isLastChunk'] == false) return;
    unawaited(_playGroup(groupId, [...chunks], voiceMode: payload['isVoiceMode'] == true));
  }

  Future<void> _playGroup(String groupId, List<AudioOutputChunk> chunks, {required bool voiceMode}) async {
    try {
      if (!voiceMode) {
        final source = buildAudioPlaybackSource(chunks);
        await playAudio(source.bytes, mimeType: source.mimeType);
      }
    } catch (failure) {
      _failure(failure);
    } finally {
      _buffers.remove(groupId);
      if (!_disposed && client.status == ConnectionStatus.connected) {
        for (final chunk in chunks) {
          client.send('audio_played', {'id': chunk.id});
        }
      }
    }
  }

  /// Plays one source after everything already queued; completes when it has
  /// finished playing. PCM is resampled to the engine's 16 kHz (original
  /// `playPcm16`, 24 kHz when the type names no rate).
  static Future<void> playAudio(Uint8List bytes, {required String mimeType}) async {
    final id = const Uuid().v4();
    if (mimeType.toLowerCase().startsWith('audio/pcm')) {
      final pcm = resamplePcm16(bytes, parsePcmSampleRate(mimeType) ?? 24000, 16000);
      await _engine.enqueuePlayback(id: id, pcm16: pcm, sampleRate: 16000);
    } else {
      await _engine.playEncoded(id: id, bytes: bytes, mimeType: mimeType);
    }
  }

  /// Stops the playing source and drops everything queued behind it.
  static Future<void> cancelPlayback() => _engine.cancelPlayback();

  Future<void> close() async {
    if (_disposed) return;
    _disposed = true;
    _buffers.clear();
    _services.remove(client);
    await _messages.cancel();
    super.dispose();
  }
}
