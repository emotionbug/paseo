import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:xterm/core.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import 'terminal_snapshot.dart';

/// Read-only observation of an existing daemon execution. xterm is used only as
/// a headless ANSI parser: CR, erasure, cursor movement and alternate screens must
/// be interpreted before presenting output as selectable log lines. No input,
/// focus, mouse, resize ownership or emulator response is sent to the process.
class ExecutionLogSession extends ChangeNotifier {
  ExecutionLogSession({
    required this.client,
    required this.terminalId,
    this.scrollback = 10000,
  }) {
    if (terminalId.isEmpty) {
      throw ArgumentError.value(
        terminalId,
        'terminalId',
        'An existing execution is required',
      );
    }
    terminal = _newTerminal();
    _resetDecoder();
    // Bind the slot in the synchronous response event, before the next binary
    // bootstrap frame, not in the observe Future's continuation.
    _events = client.events.listen(_event, onError: _fail);
    _binary = client.binaryEvents.listen(_frame, onError: _fail);
    client.addListener(_connectionChanged);
  }

  final PaseoClient client;
  final String terminalId;
  final int scrollback;
  late Terminal terminal;
  String title = '';
  Object? error;
  bool attaching = false;
  bool exited = false;
  int revision = 0;
  int _lineCount = 0;
  int get lineCount => math.min(_lineCount, terminal.buffer.height);
  bool _visible = false;
  bool _closed = false;
  bool _hasDimensions = false;
  int _generation = 0;
  int? _slot;
  String? _subscriptionId;
  OwnedObservation? _observation;
  late final StreamSubscription<JsonMap> _events;
  late final StreamSubscription<Uint8List> _binary;
  late ByteConversionSink _decoder;
  Timer? _outputNotification;

  bool get connected =>
      client.status == ConnectionStatus.connected &&
      _slot != null &&
      !attaching &&
      !exited &&
      error == null;

  Terminal _newTerminal({int rows = 24}) => Terminal(
    maxLines: math.max(24, scrollback + rows),
    // Deliberately leave onOutput/onResize unset. Even terminal status queries
    // in process output cannot turn this observer into an interactive client.
    onTitleChange: (value) {
      title = value;
    },
  );

  /// The cells [lineAt] presents for one buffer row: trailing blanks are trimmed
  /// unless the row soft-wraps into the next one.
  int _presentedLength(int index) {
    final buffer = terminal.buffer;
    final line = buffer.lines[index];
    final continues =
        index + 1 < buffer.height && buffer.lines[index + 1].isWrapped;
    return continues ? line.length : line.getTrimmedLength();
  }

  String lineAt(int index) {
    final line = terminal.buffer.lines[index];
    final length = _presentedLength(index);
    final text = StringBuffer();
    for (var column = 0; column < length; column++) {
      final code = line.getCodePoint(column);
      if (code == 0 && column > 0 && line.getWidth(column - 1) == 2) continue;
      // BufferLine.getText omits erased cells entirely. Logs must preserve gaps
      // produced by cursor movement/erasure instead of joining separate words.
      text.writeCharCode(code == 0 ? 32 : code);
    }
    return text.toString();
  }

  /// [lineAt] split into runs of identical SGR state, so the panel can paint the
  /// process's ANSI colours and attributes as the original xterm surface did. The
  /// run texts concatenate to exactly `lineAt(index)`.
  List<ExecutionLogRun> lineRuns(int index) {
    final line = terminal.buffer.lines[index];
    final length = _presentedLength(index);
    final runs = <ExecutionLogRun>[];
    final text = StringBuffer();
    var foreground = 0, background = 0, attributes = 0;
    void flush() {
      if (text.isEmpty) return;
      runs.add((
        text: text.toString(),
        foreground: foreground,
        background: background,
        attributes: attributes,
      ));
      text.clear();
    }

    for (var column = 0; column < length; column++) {
      final code = line.getCodePoint(column);
      if (code == 0 && column > 0 && line.getWidth(column - 1) == 2) continue;
      final fg = line.getForeground(column);
      final bg = line.getBackground(column);
      final attrs = line.getAttributes(column);
      if (fg != foreground || bg != background || attrs != attributes) {
        flush();
        foreground = fg;
        background = bg;
        attributes = attrs;
      }
      text.writeCharCode(code == 0 ? 32 : code);
    }
    flush();
    return runs;
  }

  String get renderedText {
    final text = StringBuffer();
    final count = _countLines();
    for (var row = 0; row < count; row++) {
      if (row > 0 && !terminal.buffer.lines[row].isWrapped) text.write('\n');
      text.write(lineAt(row));
    }
    return text.toString();
  }

  int _countLines() {
    final buffer = terminal.buffer;
    var count = buffer.height;
    final minimum = math.min(buffer.height, buffer.absoluteCursorY + 1);
    while (count > minimum && lineAt(count - 1).trim().isEmpty) {
      count--;
    }
    return count;
  }

  void _resetDecoder() {
    _decoder = const Utf8Decoder(allowMalformed: true)
        .startChunkedConversion(_OutputSink(_output));
  }

  void _output(String text) {
    if (_closed || text.isEmpty) return;
    terminal.write(text);
    // Coalesce output bursts without rebuilding the widget for every frame.
    _outputNotification ??= Timer(
      const Duration(milliseconds: 33),
      _publishOutput,
    );
  }

  void _publishOutput() {
    _outputNotification = null;
    if (_closed) return;
    _lineCount = _countLines();
    revision++;
    if (_visible) notifyListeners();
  }

  void setVisible(bool value) {
    if (_closed || value == _visible) return;
    _visible = value;
    if (value) {
      unawaited(attach());
    } else {
      _outputNotification?.cancel();
      _outputNotification = null;
      unawaited(_release());
    }
  }

  Future<void> attach() async {
    if (_closed ||
        !_visible ||
        exited ||
        attaching ||
        _observation != null ||
        client.status != ConnectionStatus.connected) {
      return;
    }
    final generation = ++_generation;
    attaching = true;
    error = null;
    _slot = null;
    notifyListeners();
    try {
      final observation = await client.observe(
        type: 'subscribe_terminal_request',
        // The default JSON snapshot includes authoritative PTY dimensions. An
        // observer must not request a guessed size or take resize ownership.
        fields: {'terminalId': terminalId},
        responseType: 'subscribe_terminal_response',
        onEvent: _observationEvent,
      );
      if (_closed || generation != _generation || !_visible) {
        await observation.close();
        return;
      }
      _observation = observation;
      _acknowledge(observation.snapshot);
    } catch (failure) {
      if (!_closed && generation == _generation) _fail(failure);
    }
  }

  void _acknowledge(JsonMap payload) {
    if (_closed || !_visible || payload.text('terminalId') != terminalId) {
      return;
    }
    if (payload['error'] != null) {
      _fail(payload['error']!);
      return;
    }
    final slot = payload['slot'];
    if (slot is! int || slot < 0 || slot > 255) {
      _fail(const FormatException('Invalid execution subscription slot'));
      return;
    }
    _slot = slot;
    _subscriptionId = payload['subscriptionId'] as String?;
    attaching = false;
    error = null;
    notifyListeners();
  }

  void _event(JsonMap event) {
    if (_closed || !_visible) return;
    if (event.text('type') == 'subscribe_terminal_response') {
      _acknowledge(event.object('payload'));
    }
  }

  void _observationEvent(JsonMap event) {
    if (_closed || !_visible) return;
    final payload = event.object('payload');
    switch (event.text('type')) {
      case 'subscription.restored':
        _acknowledge(payload);
      case 'subscription.error':
        _slot = null;
        _fail(
          payload['error'] ?? 'Execution observation could not be restored',
        );
      case 'terminal_stream_exit':
        if (payload.text('terminalId') != terminalId) return;
        if (_subscriptionId != null &&
            payload['subscriptionId'] != null &&
            payload['subscriptionId'] != _subscriptionId) {
          return;
        }
        if (payload['error'] != null) {
          _fail(
            payload['error']!,
          ); // An observer failure does not mean process exit.
        } else {
          exited = true;
          attaching = false;
          _slot = null;
          _outputNotification?.cancel();
          _publishOutput();
        }
        unawaited(_release());
    }
  }

  void _frame(Uint8List bytes) {
    if (_closed || !_visible || _slot == null) return;
    final frame = decodeTerminalStreamFrame(bytes);
    if (frame == null || frame.slot != _slot) return;
    try {
      switch (frame.opcode) {
        case TerminalStreamOpcode.output:
          _decoder.add(frame.payload);
        case TerminalStreamOpcode.snapshot:
          final state = decodeTerminalSnapshotPayload(frame.payload);
          if (state == null) {
            throw const FormatException('Invalid execution snapshot');
          }
          _replace(text: renderTerminalSnapshot(state), state: state);
        case TerminalStreamOpcode.restore:
          // Opcode 5 is ANSI; opcode 4 above is a structured cell grid. ANSI
          // restore has no size fields, so retain the last authoritative size.
          if (!_hasDimensions) {
            throw const FormatException(
              'Execution restore arrived without snapshot dimensions',
            );
          }
          _replace(text: utf8.decode(frame.payload, allowMalformed: true));
      }
    } catch (failure) {
      _fail(failure);
    }
  }

  void _replace({required String text, JsonMap? state}) {
    final cols = state?.integer('cols') ?? terminal.viewWidth;
    final rows = state?.integer('rows') ?? terminal.viewHeight;
    _outputNotification?.cancel();
    _outputNotification = null;
    terminal = _newTerminal(rows: rows);
    terminal.resize(cols, rows); // Local parser only; no onResize handler.
    _hasDimensions = true;
    _resetDecoder();
    terminal.write(text);
    if (state != null && state.text('title').isNotEmpty) {
      title = state.text('title');
    }
    _publishOutput();
  }

  void _connectionChanged() {
    if (_closed) return;
    if (client.status != ConnectionStatus.connected) {
      _slot = null;
      _subscriptionId = null;
      attaching = _visible && !exited;
      if (_visible) notifyListeners();
    } else if (_observation == null && _visible && !exited) {
      attaching = false;
      unawaited(attach());
    }
    // Existing owned observations restore automatically. Their response remaps
    // the slot synchronously before the binary snapshot is delivered.
  }

  void _fail(Object failure) {
    if (_closed) return;
    error = failure;
    attaching = false;
    if (_visible) notifyListeners();
  }

  Future<void> retry() async {
    await _release();
    if (_closed) return;
    exited = false;
    await attach();
  }

  Future<void> _release() async {
    ++_generation;
    final previous = _observation;
    _observation = null;
    _slot = null;
    _subscriptionId = null;
    attaching = false;
    try {
      await previous?.close();
    } catch (failure) {
      if (!_closed && _visible) _fail(failure);
    }
  }

  @override
  void dispose() {
    if (_closed) return;
    _closed = true;
    _outputNotification?.cancel();
    client.removeListener(_connectionChanged);
    unawaited(_events.cancel());
    unawaited(_binary.cancel());
    unawaited(_release());
    super.dispose();
  }
}

/// One run of identically styled cells: xterm `CellColor` foreground/background
/// values and `CellAttr` flags.
typedef ExecutionLogRun = ({
  String text,
  int foreground,
  int background,
  int attributes,
});

class _OutputSink implements Sink<String> {
  _OutputSink(this.receive);
  final void Function(String) receive;
  @override
  void add(String data) => receive(data);
  @override
  void close() {}
}
