import 'dart:convert';
import 'dart:typed_data';

// Wire formats: packages/protocol/src/binary-frames/*.ts.
const int maxWebSocketMessageBytes = 100 * 1024 * 1024;
const int hostRouteOpcode = 0x30;
const int maxHostRouteIdBytes = 256;
const int maxHostRouteHeaderBytes = 3 + maxHostRouteIdBytes;
const int maxRoutedPayloadBytes =
    maxWebSocketMessageBytes - maxHostRouteHeaderBytes;
final Uint8List _emptyBytes = Uint8List(0);

abstract final class TerminalStreamOpcode {
  static const int output = 0x01;
  static const int input = 0x02;
  static const int resize = 0x03;
  static const int snapshot = 0x04;
  static const int restore = 0x05;
}

final class TerminalStreamFrame {
  const TerminalStreamFrame({
    required this.opcode,
    required this.slot,
    required this.payload,
  });

  final int opcode;
  final int slot;
  // Decoders return views of the original frame, not copies.
  final Uint8List payload;
}

TerminalStreamFrame? decodeTerminalStreamFrame(Uint8List bytes) {
  if (bytes.length < 2 ||
      bytes[0] < TerminalStreamOpcode.output ||
      bytes[0] > TerminalStreamOpcode.restore) {
    return null;
  }
  return TerminalStreamFrame(
    opcode: bytes[0],
    slot: bytes[1],
    payload: Uint8List.sublistView(bytes, 2),
  );
}

Map<String, dynamic>? decodeTerminalSnapshotPayload(Uint8List bytes) {
  final value = _decodeJson(bytes);
  return value != null && _validSnapshot(value) ? value : null;
}

abstract final class FileTransferOpcode {
  static const int fileBegin = 0x10;
  static const int fileChunk = 0x11;
  static const int fileEnd = 0x12;
}

final class FileTransferFrame {
  const FileTransferFrame({
    required this.opcode,
    required this.requestId,
    required this.payload,
    this.metadata,
  });

  final int opcode;
  final String requestId;
  final Uint8List payload;
  final Map<String, dynamic>? metadata;
}

Uint8List encodeFileTransferFrame({
  required int opcode,
  required String requestId,
  Map<String, dynamic>? metadata,
  Object? payload,
}) {
  if (opcode < FileTransferOpcode.fileBegin ||
      opcode > FileTransferOpcode.fileEnd) {
    throw ArgumentError.value(opcode, 'opcode', 'Invalid file transfer opcode');
  }
  final id = utf8.encode(requestId);
  if (id.isEmpty || id.length > 255) {
    throw RangeError('File transfer requestId must contain 1–255 UTF-8 bytes');
  }
  if (opcode == FileTransferOpcode.fileBegin) {
    if (metadata == null || !_validFileMetadata(metadata)) {
      throw ArgumentError('Invalid file begin metadata');
    }
    if (payload != null) {
      throw ArgumentError('File begin cannot include a chunk payload');
    }
    final body = utf8.encode(jsonEncode(metadata));
    if (body.length > 0xffff) {
      throw RangeError('File begin metadata is too long');
    }
    final bytes = Uint8List(4 + id.length + body.length);
    bytes[0] = opcode;
    bytes[1] = id.length;
    bytes.setRange(2, 2 + id.length, id);
    ByteData.sublistView(bytes).setUint16(2 + id.length, body.length);
    bytes.setRange(4 + id.length, bytes.length, body);
    return bytes;
  }
  if (metadata != null) {
    throw ArgumentError('Only file begin can include metadata');
  }
  final body = _payloadBytes(payload);
  if (opcode == FileTransferOpcode.fileEnd && body.isNotEmpty) {
    throw ArgumentError('File end cannot include a payload');
  }
  final bytes = Uint8List(2 + id.length + body.length);
  bytes[0] = opcode;
  bytes[1] = id.length;
  bytes.setRange(2, 2 + id.length, id);
  bytes.setRange(2 + id.length, bytes.length, body);
  return bytes;
}

FileTransferFrame? decodeFileTransferFrame(Uint8List bytes) {
  if (bytes.length < 2 ||
      bytes[0] < FileTransferOpcode.fileBegin ||
      bytes[0] > FileTransferOpcode.fileEnd) {
    return null;
  }
  final opcode = bytes[0];
  final idLength = bytes[1];
  if (idLength == 0 || idLength > bytes.length - 2) return null;
  // The reference codec deliberately uses replacement decoding for request IDs.
  final requestId = utf8.decode(
    Uint8List.sublistView(bytes, 2, 2 + idLength),
    allowMalformed: true,
  );
  final body = Uint8List.sublistView(bytes, 2 + idLength);
  if (opcode == FileTransferOpcode.fileBegin) {
    if (body.length < 2 ||
        ByteData.sublistView(body).getUint16(0) != body.length - 2) {
      return null;
    }
    final metadata = _decodeJson(Uint8List.sublistView(body, 2));
    if (metadata == null || !_validFileMetadata(metadata)) return null;
    return FileTransferFrame(
      opcode: opcode,
      requestId: requestId,
      metadata: metadata,
      payload: _emptyBytes,
    );
  }
  if (opcode == FileTransferOpcode.fileEnd && body.isNotEmpty) return null;
  return FileTransferFrame(opcode: opcode, requestId: requestId, payload: body);
}

final class HostRouteFrame {
  const HostRouteFrame({required this.routeId, required this.payload});

  final String routeId;
  final Uint8List payload;
}

Uint8List encodeHostRouteFrame(String routeId, Uint8List payload) {
  final id = utf8.encode(routeId);
  if (id.isEmpty || id.length > maxHostRouteIdBytes) {
    throw RangeError('Host route ID must contain 1–256 UTF-8 bytes');
  }
  if (payload.length > maxRoutedPayloadBytes) {
    throw RangeError('Host frame is too large');
  }
  final bytes = Uint8List(3 + id.length + payload.length);
  bytes[0] = hostRouteOpcode;
  ByteData.sublistView(bytes).setUint16(1, id.length);
  bytes.setRange(3, 3 + id.length, id);
  bytes.setRange(3 + id.length, bytes.length, payload);
  return bytes;
}

HostRouteFrame? decodeHostRouteFrame(Uint8List bytes) {
  if (bytes.length < 3 || bytes[0] != hostRouteOpcode) return null;
  final length = ByteData.sublistView(bytes).getUint16(1);
  if (length == 0 ||
      length > maxHostRouteIdBytes ||
      bytes.length < 3 + length ||
      bytes.length - 3 - length > maxRoutedPayloadBytes) {
    return null;
  }
  try {
    final routeId = utf8.decode(Uint8List.sublistView(bytes, 3, 3 + length));
    return HostRouteFrame(
      routeId: routeId,
      payload: Uint8List.sublistView(bytes, 3 + length),
    );
  } on FormatException {
    return null;
  }
}

Uint8List _payloadBytes(Object? payload) {
  if (payload == null) return _emptyBytes;
  if (payload is Uint8List) return payload;
  if (payload is String) return utf8.encode(payload);
  if (payload is ByteBuffer) return payload.asUint8List();
  throw ArgumentError(
    'Binary payload must be a String, Uint8List or ByteBuffer',
  );
}

Map<String, dynamic>? _decodeJson(Uint8List bytes) {
  try {
    final value = jsonDecode(utf8.decode(bytes, allowMalformed: true));
    return value is Map<String, dynamic> ? value : null;
  } on FormatException {
    return null;
  }
}

bool _finiteNumber(Object? value) => value is num && value.isFinite;
bool _integer(Object? value) =>
    value is num &&
    value.isFinite &&
    value.abs() <= 9007199254740991 &&
    value == value.truncateToDouble();

bool _optional(Map value, String key, bool Function(Object?) predicate) =>
    !value.containsKey(key) || predicate(value[key]);
bool _string(Object? value) => value is String;
bool _boolean(Object? value) => value is bool;
bool _booleanList(Object? value) => value is List && value.every(_boolean);

bool _validFileMetadata(Map value) =>
    value['mime'] is String &&
    (value['mime'] as String).isNotEmpty &&
    _integer(value['size']) &&
    (value['size'] as num) >= 0 &&
    (value['encoding'] == 'utf-8' || value['encoding'] == 'binary') &&
    value['modifiedAt'] is String &&
    _optional(value, 'revision', _string) &&
    _optional(value, 'fileName', _string);

bool _validCell(Object? value) {
  if (value is! Map || value['char'] is! String) return false;
  for (final key in const ['fg', 'bg', 'fgMode', 'bgMode']) {
    if (!_optional(value, key, _finiteNumber)) return false;
  }
  for (final key in const [
    'bold',
    'italic',
    'underline',
    'dim',
    'inverse',
    'strikethrough',
  ]) {
    if (!_optional(value, key, _boolean)) return false;
  }
  return true;
}

bool _validGrid(Object? value) =>
    value is List && value.every((row) => row is List && row.every(_validCell));

bool _validSnapshot(Map value) {
  final cursor = value['cursor'];
  return _finiteNumber(value['rows']) &&
      _finiteNumber(value['cols']) &&
      _validGrid(value['grid']) &&
      _validGrid(value['scrollback']) &&
      cursor is Map &&
      _finiteNumber(cursor['row']) &&
      _finiteNumber(cursor['col']) &&
      _optional(cursor, 'hidden', _boolean) &&
      _optional(cursor, 'blink', _boolean) &&
      _optional(
        cursor,
        'style',
        (v) => v == 'block' || v == 'underline' || v == 'bar',
      ) &&
      _optional(value, 'title', _string) &&
      _optional(value, 'gridWrapped', _booleanList) &&
      _optional(value, 'scrollbackWrapped', _booleanList);
}
