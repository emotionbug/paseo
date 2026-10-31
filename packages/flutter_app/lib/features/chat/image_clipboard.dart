import 'dart:convert';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:pasteboard/pasteboard.dart';

import '../../core/json.dart';

class ChatClipboardContent {
  const ChatClipboardContent(this.images, this.text);
  final List<JsonMap> images;
  final String? text;
}

Future<ChatClipboardContent> readChatClipboard({
  bool imagesOnly = false,
}) async {
  var bytes = await Pasteboard.image;
  if (bytes != null && bytes.isNotEmpty) {
    var mime = _imageMime(bytes);
    if (mime == null) {
      // The native clipboard can expose a platform-specific image encoding.
      // Decode real image bytes and transcode, rather than mislabeling the data.
      final codec = await ui.instantiateImageCodec(bytes);
      try {
        final frame = await codec.getNextFrame();
        try {
          final encoded = await frame.image.toByteData(
            format: ui.ImageByteFormat.png,
          );
          if (encoded == null) {
            throw StateError('Could not encode clipboard image as PNG.');
          }
          bytes = encoded.buffer.asUint8List(
            encoded.offsetInBytes,
            encoded.lengthInBytes,
          );
          mime = 'image/png';
        } finally {
          frame.image.dispose();
        }
      } finally {
        codec.dispose();
      }
    }
    return ChatClipboardContent([
      {'data': base64Encode(bytes), 'mimeType': mime},
    ], null);
  }
  if (imagesOnly) throw StateError('No image in the current clipboard.');
  return ChatClipboardContent(const [], await Pasteboard.text);
}

String? _imageMime(Uint8List bytes) {
  if (bytes.length >= 8 &&
      bytes[0] == 0x89 &&
      bytes[1] == 0x50 &&
      bytes[2] == 0x4e &&
      bytes[3] == 0x47 &&
      bytes[4] == 13 &&
      bytes[5] == 10 &&
      bytes[6] == 26 &&
      bytes[7] == 10) {
    return 'image/png';
  }
  if (bytes.length >= 3 &&
      bytes[0] == 0xff &&
      bytes[1] == 0xd8 &&
      bytes[2] == 0xff) {
    return 'image/jpeg';
  }
  if (bytes.length >= 6 &&
      bytes[0] == 71 &&
      bytes[1] == 73 &&
      bytes[2] == 70 &&
      bytes[3] == 56) {
    return 'image/gif';
  }
  if (bytes.length >= 12 &&
      bytes[0] == 82 &&
      bytes[1] == 73 &&
      bytes[2] == 70 &&
      bytes[3] == 70 &&
      bytes[8] == 87 &&
      bytes[9] == 69 &&
      bytes[10] == 66 &&
      bytes[11] == 80) {
    return 'image/webp';
  }
  return null;
}
