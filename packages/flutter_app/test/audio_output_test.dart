import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/features/audio/audio_service.dart';

void main() {
  group('audio output (session-context buildAudioPlaybackSource)', () {
    test('a group plays as one source in chunk order, typed by the daemon format', () {
      final source = buildAudioPlaybackSource([
        (chunkIndex: 1, audio: base64Encode([3, 4]), format: 'pcm', id: 'g:1'),
        (chunkIndex: 0, audio: base64Encode([1, 2]), format: 'pcm', id: 'g:0'),
      ]);
      expect(source.bytes, [1, 2, 3, 4]);
      expect(source.mimeType, 'audio/pcm;rate=24000;bits=16');
      expect(audioOutputMimeType('mp3'), 'audio/mpeg');
      expect(audioOutputMimeType('wav'), 'audio/wav');
    });

    test('PCM without a declared rate is 24 kHz and is resampled to 16 kHz', () {
      expect(parsePcmSampleRate('audio/pcm;bits=16'), isNull);
      expect(parsePcmSampleRate('audio/pcm;rate=16000;bits=16'), 16000);
      final samples = Int16List.fromList([0, 300, 600, -300, -600, 0]);
      final pcm = samples.buffer.asUint8List();
      final out = resamplePcm16(pcm, 24000, 16000).buffer.asInt16List();
      // 6 samples at 24 kHz are 4 at 16 kHz, linearly interpolated.
      expect(out, [0, 450, -300, -300]);
      expect(identical(resamplePcm16(pcm, 16000, 16000), pcm), isTrue);
    });
  });
}
