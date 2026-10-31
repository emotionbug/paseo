import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/legacy_leveldb.dart';

List<int> _fixed(int value, int count) => [for (var i = 0; i < count; i++) (value >> (8 * i)) & 255];
List<int> _varint(int value) {
  final result = <int>[];
  while (value >= 128) { result.add((value & 127) | 128); value >>= 7; }
  return [...result, value];
}
List<int> _slice(List<int> bytes) => [..._varint(bytes.length), ...bytes];
int _crc(List<int> bytes) {
  var crc = 0xffffffff;
  for (final byte in bytes) {
    crc ^= byte;
    for (var bit = 0; bit < 8; bit++) { crc = (crc >>> 1) ^ ((crc & 1) == 1 ? 0x82f63b78 : 0); }
  }
  crc = (crc ^ 0xffffffff) & 0xffffffff;
  return (((crc >>> 15) | (crc << 17)) + 0xa282ead8) & 0xffffffff;
}
List<int> _record(List<int> data, [int type = 1]) => [
  ..._fixed(_crc([type, ...data]), 4), ..._fixed(data.length, 2), type, ...data,
];
List<int> _journal(List<int> data) {
  final out = <int>[];
  var at = 0;
  while (at < data.length) {
    final count = data.length - at > 32761 ? 32761 : data.length - at;
    final last = at + count == data.length;
    final type = at == 0 ? (last ? 1 : 2) : (last ? 4 : 3);
    out.addAll(_record(data.sublist(at, at + count), type));
    at += count;
  }
  return out;
}
List<int> _string(String value) => [0, for (final unit in value.codeUnits) ..._fixed(unit, 2)];
List<int> _key(String name, [String origin = 'paseo://app']) => [95, ...utf8.encode(origin), 0, ..._string(name)];
List<int> _internal(List<int> key, int sequence) => [...key, ..._fixed((sequence << 8) | 1, 8)];
List<int> _entries(List<(List<int>, List<int>)> entries) => [
  for (final entry in entries) ...[0, ..._varint(entry.$1.length), ..._varint(entry.$2.length), ...entry.$1, ...entry.$2],
  ..._fixed(0, 4), ..._fixed(1, 4),
];
List<int> _literal(List<int> bytes) {
  if (bytes.length <= 60) return [(bytes.length - 1) << 2, ...bytes];
  final count = bytes.length <= 256 ? 1 : bytes.length <= 65536 ? 2 : 3;
  return [(59 + count) << 2, ..._fixed(bytes.length - 1, count), ...bytes];
}
List<int> _snappy(List<int> data) {
  final out = [..._varint(data.length)];
  var start = 0;
  var at = 1;
  while (at < data.length) {
    var run = 0;
    while (at + run < data.length && data[at + run] == data[at - 1] && run < 64) { run++; }
    if (run < 4) { at++; continue; }
    if (at > start) out.addAll(_literal(data.sublist(start, at)));
    out.addAll([((run - 1) << 2) | 2, 1, 0]);
    at += run;
    start = at;
  }
  if (start < data.length) out.addAll(_literal(data.sublist(start)));
  return out;
}
List<int> _block(List<int> data, {bool compressed = false}) {
  final encoded = compressed ? _snappy(data) : data;
  final type = compressed ? 1 : 0;
  return [...encoded, type, ..._fixed(_crc([...encoded, type]), 4)];
}
List<int> _table(List<(List<int>, List<int>)> entries) {
  final data = _block(_entries(entries), compressed: true);
  final indexOffset = data.length;
  final index = _block(_entries([(entries.last.$1, [..._varint(0), ..._varint(data.length - 5)])]));
  final handles = [..._varint(0), ..._varint(0), ..._varint(indexOffset), ..._varint(index.length - 5)];
  return [...data, ...index, ...handles, ...List<int>.filled(40 - handles.length, 0),
    ..._fixed(0x8b80fb57, 4), ..._fixed(0xdb477524, 4)];
}
Future<void> _write(Directory dir, String name, List<int> bytes) => File('${dir.path}/$name').writeAsBytes(bytes).then((_) {});
Future<void> _manifest(Directory dir, {List<int>? table}) async {
  await File('${dir.path}/CURRENT').writeAsString('MANIFEST-000001\n');
  await _write(dir, 'MANIFEST-000001', _record([
    1, ..._slice(utf8.encode('leveldb.BytewiseComparator')), 2, 3, 3, 8, 4, 50,
    if (table != null) ...[7, 0, 2, ..._varint(table.length), ..._slice([0]), ..._slice([255])],
  ]));
  if (table != null) await _write(dir, '000002.ldb', table);
}

void main() {
  late Directory directory;
  setUp(() async { directory = await Directory.systemTemp.createTemp('paseo-legacy-leveldb-test-'); });
  tearDown(() async { await directory.delete(recursive: true); });

  test('manifest tables, Snappy, live fragmented WAL and tombstones recover exact Chromium strings', () async {
    final oldSettings = jsonEncode({'language': 'en', 'padding': List.filled(200, 'a').join()});
    final updated = jsonEncode({'language': 'ko', 'password': '  密碼\n  ', 'padding': List.filled(20000, 'z').join()});
    final table = _table([
      (_internal(_key('@paseo:app-settings'), 1), _string(oldSettings)),
      (_internal(_key('deleted-setting'), 2), _string('removed')),
    ]);
    await _manifest(directory, table: table);
    // Unlisted SSTs are obsolete, even if their sequence is larger.
    await _write(directory, '000009.ldb', _table([
      (_internal(_key('@paseo:app-settings'), 100), _string('must not resurrect')),
    ]));
    final batch = [..._fixed(20, 8), ..._fixed(3, 4),
      1, ..._slice(_key('@paseo:app-settings')), ..._slice(_string(updated)),
      0, ..._slice(_key('deleted-setting')),
      1, ..._slice(_key('private-other-site', 'https://unrelated.example')), ..._slice(_string('not imported')),
    ];
    await _write(directory, '000003.log', _journal(batch));
    final before = await File('${directory.path}/000003.log').readAsBytes();
    final values = await readLegacyLocalStorage(directory);
    expect(values, {'@paseo:app-settings': updated});
    expect(await File('${directory.path}/000003.log').readAsBytes(), before);
  });

  test('bad checksums and incomplete WAL fail instead of dropping newest settings', () async {
    await _manifest(directory);
    final batch = [..._fixed(1, 8), ..._fixed(1, 4), 1,
      ..._slice(_key('@paseo:app-settings')), ..._slice(_string('{"language":"ko"}'))];
    final record = _record(batch);
    record[0] ^= 1;
    await _write(directory, '000003.log', record);
    await expectLater(readLegacyLocalStorage(directory), throwsFormatException);
    await _write(directory, '000003.log', _record(batch).sublist(0, 10));
    await expectLater(readLegacyLocalStorage(directory), throwsFormatException);
    expect(await File('${directory.path}/000003.log').length(), 10);
  });

  test('manifest deletion prevents resurrecting obsolete table values', () async {
    final table = _table([(_internal(_key('obsolete'), 1), _string('secret'))]);
    await _manifest(directory, table: table);
    await File('${directory.path}/MANIFEST-000001').writeAsBytes(_record([6, 0, 2]), mode: FileMode.append);
    await _write(directory, '000003.log', []);
    expect(await readLegacyLocalStorage(directory), isEmpty);
  });
}
