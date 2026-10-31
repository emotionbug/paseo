import 'dart:typed_data';

import 'relay_crypto.dart';

const relayMaxChunkDataBytes = 65513;
const _maxFrameBytes = 0xffffffff;

int _chunkCount(int length) => length == 0
    ? 1
    : (length + relayMaxChunkDataBytes - 1) ~/ relayMaxChunkDataBytes;

int _wireLength(int length) => length + _chunkCount(length) * 24;

/// Exactly one application frame per WebSocket record. All chunk nonces are
/// consumed synchronously before returning any ciphertext to the transport.
Uint8List encodeRelayRecord(
  Uint8List frame, {
  required bool binary,
  required RelayNoiseCipher sender,
}) {
  if (frame.length > _maxFrameBytes) {
    throw const FormatException('Protected relay frame too large');
  }
  final chunks = _chunkCount(frame.length);
  final wire = Uint8List(_wireLength(frame.length));
  final wireData = ByteData.sublistView(wire);
  var offset = 0;
  for (var index = 0; index < chunks; index++) {
    final start = index * relayMaxChunkDataBytes;
    final remaining = frame.length - start;
    final length = remaining < relayMaxChunkDataBytes
        ? remaining
        : relayMaxChunkDataBytes;
    final plaintext = Uint8List(length + 6);
    plaintext[0] = binary ? 1 : 0;
    plaintext[1] = (index == 0 ? 1 : 0) | (index == chunks - 1 ? 2 : 0);
    ByteData.sublistView(plaintext).setUint32(2, frame.length, Endian.big);
    plaintext.setRange(6, plaintext.length, frame, start);
    final ciphertext = sender.encrypt(plaintext);
    wireData.setUint16(offset, ciphertext.length, Endian.big);
    wire.setRange(offset + 2, offset + 2 + ciphertext.length, ciphertext);
    offset += ciphertext.length + 2;
  }
  return wire;
}

/// Authenticates and validates the whole record before exposing any plaintext.
/// A caller MUST close after any error because authenticated chunks may already
/// have consumed receive nonces.
({bool binary, Uint8List bytes}) decodeRelayRecord(
  Uint8List wire,
  RelayNoiseCipher receiver,
) {
  final wireData = ByteData.sublistView(wire);
  Uint8List? frame;
  var offset = 0;
  var index = 0;
  var expectedChunks = 0;
  var total = 0;
  var kind = -1;
  var written = 0;
  while (offset < wire.length) {
    if (frame != null && index >= expectedChunks) {
      throw const FormatException('Unexpected protected chunk after FINAL');
    }
    if (wire.length - offset < 2) {
      throw const FormatException('Truncated protected record');
    }
    final length = wireData.getUint16(offset, Endian.big);
    offset += 2;
    if (length < 22 || wire.length - offset < length) {
      throw const FormatException('Invalid protected chunk length');
    }
    final plaintext = receiver.decrypt(
      Uint8List.sublistView(wire, offset, offset + length),
    );
    offset += length;
    final chunkKind = plaintext[0];
    final flags = plaintext[1];
    final claimedLength = ByteData.sublistView(plaintext)
        .getUint32(2, Endian.big);
    if (chunkKind > 1 || (flags & ~3) != 0) {
      throw const FormatException('Invalid protected chunk metadata');
    }
    final first = (flags & 1) != 0;
    final last = (flags & 2) != 0;
    if (frame == null) {
      if (!first) throw const FormatException('Protected record missing FIRST');
      // Validate exact received wire size before allocating the claimed frame.
      if (_wireLength(claimedLength) != wire.length) {
        throw const FormatException('Protected record length mismatch');
      }
      total = claimedLength;
      kind = chunkKind;
      expectedChunks = _chunkCount(total);
      frame = Uint8List(total);
    } else if (first || chunkKind != kind || claimedLength != total) {
      throw const FormatException('Protected chunk metadata changed');
    }
    final finalExpected = index == expectedChunks - 1;
    if (last != finalExpected) {
      throw const FormatException('Protected record FINAL mismatch');
    }
    final expectedLength = finalExpected
        ? total - written
        : relayMaxChunkDataBytes;
    if (plaintext.length - 6 != expectedLength) {
      throw const FormatException('Protected chunk size mismatch');
    }
    frame.setRange(written, written + expectedLength, plaintext, 6);
    written += expectedLength;
    index++;
  }
  if (frame == null || index != expectedChunks || written != total) {
    throw const FormatException('Incomplete protected record');
  }
  return (binary: kind == 1, bytes: frame);
}
