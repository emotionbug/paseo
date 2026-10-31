import '../../core/json.dart';

/// Port of protocol/src/terminal-snapshot.ts. Opcode 4 is a JSON cell grid,
/// not serialized ANSI. Opcode 5 is a separate ANSI restore operation.
String renderTerminalSnapshot(JsonMap state) {
  final grid = state.array('grid');
  final history = state.array('scrollback');
  final rows = [...history, ...grid];
  final wraps = [
    ...state.array('scrollbackWrapped'),
    ...state.array('gridWrapped'),
  ];
  final hasWraps = wraps.length == rows.length;
  final out = StringBuffer(hasWraps ? '' : '\x1b[?7l');
  for (var i = 0; i < rows.length; i++) {
    final row = jsonList(rows[i]);
    final continues = hasWraps && wraps[i] == true;
    var length = row.length;
    while (length > 0) {
      final cell = jsonMap(row[length - 1]);
      if (cell['char'] != ' ' || _styled(cell)) break;
      length--;
    }
    if (continues && length < state.integer('cols')) {
      length = state.integer('cols');
    }
    var previous = '\x1b[0m';
    for (var col = 0; col < length; col++) {
      final cell = col < row.length
          ? jsonMap(row[col])
          : <String, dynamic>{'char': ' '};
      if (cell['char'] == '') continue; // Wide-glyph continuation, not a blank.
      final style = _style(cell);
      if (style != previous) {
        out.write(style);
        previous = style;
      }
      out.write(cell.text('char', ' '));
    }
    if (previous != '\x1b[0m') out.write('\x1b[0m');
    if (i < rows.length - 1 && !continues) out.write('\r\n');
  }
  final cursor = state.object('cursor');
  out.write('\x1b[0m');
  final style = cursor.text('style');
  if (style.isNotEmpty) {
    final base = switch (style) {
      'block' => 1,
      'underline' => 3,
      _ => 5,
    };
    out.write('\x1b[${base + (cursor['blink'] == false ? 1 : 0)} q');
  }
  out.write('\x1b[${cursor.integer('row') + 1};${cursor.integer('col') + 1}H');
  out.write(cursor.flag('hidden') ? '\x1b[?25l' : '\x1b[?25h');
  if (!hasWraps) out.write('\x1b[?7h');
  return out.toString();
}

bool _styled(JsonMap cell) =>
    ['fg', 'bg', 'fgMode', 'bgMode'].any((key) => cell[key] != null) ||
    [
      'bold',
      'dim',
      'italic',
      'underline',
      'inverse',
      'strikethrough',
    ].any((key) => cell[key] == true);

String _style(JsonMap cell) {
  final codes = <int>[0];
  for (final entry in const {
    'bold': 1,
    'dim': 2,
    'italic': 3,
    'underline': 4,
    'inverse': 7,
    'strikethrough': 9,
  }.entries) {
    if (cell.flag(entry.key)) codes.add(entry.value);
  }
  for (final background in [false, true]) {
    final key = background ? 'bg' : 'fg';
    if (cell[key] == null || cell['${key}Mode'] == null) continue;
    final value = cell.integer(key);
    switch (cell.integer('${key}Mode')) {
      case 1:
        codes.add(
          value >= 8
              ? (background ? 100 : 90) + value - 8
              : (background ? 40 : 30) + value,
        );
      case 2:
        codes.addAll([background ? 48 : 38, 5, value]);
      case 3:
        codes.addAll([
          background ? 48 : 38,
          2,
          (value >> 16) & 255,
          (value >> 8) & 255,
          value & 255,
        ]);
    }
  }
  return '\x1b[${codes.join(';')}m';
}
