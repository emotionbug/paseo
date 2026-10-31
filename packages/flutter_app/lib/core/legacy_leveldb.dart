import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:path/path.dart' as p;

/// Read-only LevelDB recovery for Chromium localStorage. Only live tables from
/// CURRENT's manifest and eligible journals participate; obsolete files must not
/// resurrect deleted hosts. No LevelDB open/repair/compaction touches the source.
Future<Map<String, String>> readLegacyLocalStorage(Directory directory) async {
  if (!await directory.exists()) return {};
  final observed = <String, FileStat>{};
  Future<Uint8List> bytes(String name) async {
    final file = File(p.join(directory.path, name));
    observed[file.path] = await file.stat();
    return file.readAsBytes();
  }

  final current = utf8.decode(await bytes('CURRENT')).trim();
  if (!RegExp(r'^MANIFEST-\d+$').hasMatch(current)) {
    throw const FormatException('Invalid legacy LevelDB CURRENT');
  }
  final tables = <int, int>{};
  int? logNumber;
  var previousLog = 0;
  for (final record in _records(await bytes(current))) {
    final input = _Input(record);
    while (!input.done) {
      switch (input.varint()) {
        case 1:
          if (utf8.decode(input.slice()) != 'leveldb.BytewiseComparator') {
            throw const FormatException('Unsupported legacy LevelDB comparator');
          }
        case 2:
          logNumber = input.varint();
        case 3:
        case 4:
          input.varint();
        case 5:
          input.varint();
          input.slice();
        case 6:
          input.varint();
          tables.remove(input.varint());
        case 7:
          input.varint();
          final number = input.varint();
          tables[number] = input.varint();
          input.slice();
          input.slice();
        case 9:
          previousLog = input.varint();
        default:
          throw const FormatException('Unsupported legacy LevelDB manifest field');
      }
    }
  }
  if (logNumber == null) throw const FormatException('Legacy LevelDB has no journal identity');
  final versions = <String, ({Uint8List key, Uint8List? value, int sequence})>{};
  void accept(Uint8List key, Uint8List? value, int sequence) {
    final identity = base64.encode(key);
    final before = versions[identity];
    if (before == null || sequence > before.sequence) {
      versions[identity] = (key: key, value: value, sequence: sequence);
    } else if (sequence == before.sequence &&
        (value == null ? before.value != null :
          before.value == null || base64.encode(value) != base64.encode(before.value!))) {
      throw const FormatException('Conflicting legacy LevelDB entries');
    }
  }

  for (final table in tables.entries) {
    final prefix = table.key.toString().padLeft(6, '0');
    final name = await File(p.join(directory.path, '$prefix.ldb')).exists()
        ? '$prefix.ldb' : '$prefix.sst';
    final data = await bytes(name);
    if (data.length != table.value || data.length < 48 ||
        _Input(Uint8List.sublistView(data, data.length - 8)).fixed(4) != 0x8b80fb57 ||
        _Input(Uint8List.sublistView(data, data.length - 4)).fixed(4) != 0xdb477524) {
      throw const FormatException('Invalid legacy LevelDB table');
    }
    final footer = _Input(Uint8List.sublistView(data, data.length - 48, data.length - 8));
    footer.varint();
    footer.varint();
    final index = _block(data, footer.varint(), footer.varint());
    for (final entry in _entries(index)) {
      final handle = _Input(entry.value);
      final block = _block(data, handle.varint(), handle.varint());
      for (final row in _entries(block)) {
        if (row.key.length < 8) throw const FormatException('Invalid internal LevelDB key');
        final tag = _Input(Uint8List.sublistView(row.key, row.key.length - 8)).fixed(8);
        final type = tag & 255;
        if (type != 0 && type != 1) throw const FormatException('Unknown LevelDB value type');
        accept(Uint8List.sublistView(row.key, 0, row.key.length - 8),
          type == 0 ? null : row.value, tag >> 8);
      }
    }
  }
  final journals = <String>[];
  await for (final entry in directory.list(followLinks: false)) {
    final name = p.basename(entry.path);
    if (!RegExp(r'^\d+\.log$').hasMatch(name)) continue;
    final number = int.parse(name.split('.').first);
    if (number >= logNumber || number == previousLog) journals.add(name);
  }
  for (final journal in journals) {
    for (final record in _records(await bytes(journal))) {
      final input = _Input(record);
      final sequence = input.fixed(8);
      final count = input.fixed(4);
      for (var i = 0; i < count; i++) {
        final type = input.fixed(1);
        final key = input.slice();
        if (type != 0 && type != 1) throw const FormatException('Unknown journal entry');
        accept(key, type == 0 ? null : input.slice(), sequence + i);
      }
      if (!input.done) throw const FormatException('Invalid legacy journal count');
    }
  }
  for (final entry in observed.entries) {
    final after = await File(entry.key).stat();
    if (after.size != entry.value.size || after.modified != entry.value.modified) {
      throw const FileSystemException('Original desktop settings changed during import; close the old app and retry');
    }
  }
  final origins = <String, Map<String, String>>{};
  for (final row in versions.values) {
    if (row.value == null || row.key.isEmpty || row.key.first != 95) continue;
    final separator = row.key.indexOf(0, 1);
    if (separator < 0) continue;
    final origin = utf8.decode(row.key.sublist(1, separator));
    final key = _chromiumString(Uint8List.sublistView(row.key, separator + 1));
    // Never import unrelated webview sites' credentials. A recognizable Paseo
    // profile at an unsupported origin is an explicit error, not "no old data".
    if (origin != 'paseo://app' && origin != 'file://' &&
        origin != 'http://localhost:8081') {
      if (const {'@paseo:settings', '@paseo:app-settings', '@paseo:daemon-registry'}.contains(key)) {
        throw const FormatException('Original Paseo settings use an unsupported desktop origin; source retained.');
      }
      continue;
    }
    (origins[origin] ??= {})[key] = _chromiumString(row.value!);
  }
  // Production first. Development storage is a distinct profile, not a fallback
  // for individual missing keys (which may have deliberately been deleted).
  return origins['paseo://app'] ?? origins['file://'] ?? origins['http://localhost:8081'] ?? {};
}

String _chromiumString(Uint8List bytes) {
  if (bytes.isEmpty) throw const FormatException('Missing Chromium string encoding');
  if (bytes.first == 1) return latin1.decode(bytes.sublist(1));
  if (bytes.first != 0 || bytes.length.isEven) {
    throw const FormatException('Invalid Chromium UTF-16 string');
  }
  return String.fromCharCodes([
    for (var i = 1; i < bytes.length; i += 2) bytes[i] | bytes[i + 1] << 8,
  ]);
}

class _Input {
  _Input(this.bytes);
  final Uint8List bytes;
  int at = 0;
  bool get done => at == bytes.length;
  int fixed(int count) {
    if (count < 0 || at + count > bytes.length) throw const FormatException('Truncated legacy LevelDB');
    var value = 0;
    for (var i = 0; i < count; i++) {
      value |= bytes[at++] << (i * 8);
    }
    return value;
  }
  int varint() {
    var value = 0;
    for (var shift = 0; shift < 64; shift += 7) {
      final byte = fixed(1);
      value |= (byte & 127) << shift;
      if (byte < 128) return value;
    }
    throw const FormatException('Invalid legacy LevelDB integer');
  }
  Uint8List take(int size) {
    if (size < 0 || size > bytes.length - at) throw const FormatException('Truncated legacy LevelDB value');
    final result = Uint8List.sublistView(bytes, at, at + size);
    at += size;
    return result;
  }
  Uint8List slice() => take(varint());
}

Iterable<Uint8List> _records(Uint8List bytes) sync* {
  final fragments = BytesBuilder(copy: false);
  var fragmented = false;
  var at = 0;
  while (at < bytes.length) {
    final remaining = 32768 - at % 32768;
    if (remaining < 7) {
      final end = at + remaining < bytes.length ? at + remaining : bytes.length;
      if (bytes.sublist(at, end).any((byte) => byte != 0)) throw const FormatException('Invalid journal padding');
      at = end;
      continue;
    }
    if (bytes.length - at < 7) throw const FormatException('Truncated journal header');
    final header = _Input(Uint8List.sublistView(bytes, at, at + 7));
    final checksum = header.fixed(4);
    final size = header.fixed(2);
    final type = header.fixed(1);
    if (checksum == 0 && size == 0 && type == 0) {
      final end = at + remaining < bytes.length ? at + remaining : bytes.length;
      if (bytes.sublist(at, end).any((byte) => byte != 0)) throw const FormatException('Invalid journal padding');
      at = end;
      continue;
    }
    if (size + 7 > remaining || at + size + 7 > bytes.length) throw const FormatException('Truncated journal record');
    final data = Uint8List.sublistView(bytes, at + 7, at + 7 + size);
    _checkCrc(checksum, data, prefix: type);
    at += 7 + size;
    if (type == 1 && !fragmented) {
      yield data;
    } else if (type == 2 && !fragmented) {
      fragmented = true;
      fragments.add(data);
    } else if ((type == 3 || type == 4) && fragmented) {
      fragments.add(data);
      if (type == 4) { yield fragments.takeBytes(); fragmented = false; }
    } else {
      throw const FormatException('Invalid legacy journal fragmentation');
    }
  }
  if (fragmented) throw const FormatException('Incomplete legacy journal record');
}

Uint8List _block(Uint8List table, int offset, int size) {
  if (offset < 0 || size < 0 || offset + size + 5 > table.length - 48) {
    throw const FormatException('Invalid legacy table block');
  }
  final data = Uint8List.sublistView(table, offset, offset + size);
  final type = table[offset + size];
  final checksum = _Input(Uint8List.sublistView(table, offset + size + 1, offset + size + 5)).fixed(4);
  _checkCrc(checksum, data, suffix: type);
  return switch (type) {
    0 => data,
    1 => _snappy(data),
    _ => throw const FormatException('Unsupported legacy table compression'),
  };
}

Iterable<({Uint8List key, Uint8List value})> _entries(Uint8List bytes) sync* {
  if (bytes.length < 4) throw const FormatException('Invalid legacy table restart array');
  final count = _Input(Uint8List.sublistView(bytes, bytes.length - 4)).fixed(4);
  final end = bytes.length - 4 - count * 4;
  if (count < 1 || end < 0) throw const FormatException('Invalid legacy table restart count');
  final input = _Input(Uint8List.sublistView(bytes, 0, end));
  var previous = Uint8List(0);
  while (!input.done) {
    final shared = input.varint();
    final suffix = input.varint();
    final length = input.varint();
    if (shared > previous.length) throw const FormatException('Invalid legacy table prefix');
    final key = Uint8List(shared + suffix);
    key.setRange(0, shared, previous);
    key.setRange(shared, key.length, input.take(suffix));
    final value = input.take(length);
    yield (key: key, value: value);
    previous = key;
  }
}

Uint8List _snappy(Uint8List bytes) {
  final input = _Input(bytes);
  final size = input.varint();
  if (size < 0 || size > 64 * 1024 * 1024) throw const FormatException('Oversized legacy table block');
  final output = Uint8List(size);
  var at = 0;
  while (!input.done) {
    final tag = input.fixed(1);
    final type = tag & 3;
    int length;
    if (type == 0) {
      final encoded = tag >> 2;
      length = encoded < 60 ? encoded + 1 : input.fixed(encoded - 59) + 1;
      if (length > size - at) throw const FormatException('Invalid Snappy literal');
      output.setRange(at, at + length, input.take(length));
      at += length;
    } else {
      length = type == 1 ? 4 + ((tag >> 2) & 7) : 1 + (tag >> 2);
      final offset = type == 1 ? ((tag & 224) << 3) | input.fixed(1) : input.fixed(type == 2 ? 2 : 4);
      if (offset <= 0 || offset > at || length > size - at) throw const FormatException('Invalid Snappy copy');
      for (var i = 0; i < length; i++) { output[at] = output[at - offset]; at++; }
    }
  }
  if (at != size) throw const FormatException('Truncated Snappy block');
  return output;
}

void _checkCrc(int masked, Uint8List bytes, {int? prefix, int? suffix}) {
  var crc = 0xffffffff;
  for (var index = prefix == null ? 0 : -1;
      index < bytes.length + (suffix == null ? 0 : 1); index++) {
    crc ^= index < 0 ? prefix! : index == bytes.length ? suffix! : bytes[index];
    for (var bit = 0; bit < 8; bit++) {
      crc = (crc >>> 1) ^ ((crc & 1) != 0 ? 0x82f63b78 : 0);
    }
  }
  crc = (crc ^ 0xffffffff) & 0xffffffff;
  final expected = (((crc >>> 15) | (crc << 17)) + 0xa282ead8) & 0xffffffff;
  if (masked != expected) throw const FormatException('Legacy LevelDB checksum mismatch');
}
