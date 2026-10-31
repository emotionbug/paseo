import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:msgpack_stream/msgpack_stream.dart';
import 'package:path/path.dart' as p;

/// One lazy, isolated Neovim process per Vim-enabled remote file. Flutter owns
/// rendering and the daemon remains the only owner of file reads/writes.
class VimEngine extends ChangeNotifier {
  VimEngine._({
    required this.onText,
    required this.onSave,
    required this.onClose,
  });
  final ValueChanged<String> onText;
  final Future<bool> Function() onSave;
  final Future<void> Function() onClose;
  static final _keyNames = <LogicalKeyboardKey, String>{
    LogicalKeyboardKey.escape: 'Esc',
    LogicalKeyboardKey.enter: 'CR',
    LogicalKeyboardKey.numpadEnter: 'CR',
    LogicalKeyboardKey.backspace: 'BS',
    LogicalKeyboardKey.delete: 'Del',
    LogicalKeyboardKey.tab: 'Tab',
    LogicalKeyboardKey.arrowLeft: 'Left',
    LogicalKeyboardKey.arrowRight: 'Right',
    LogicalKeyboardKey.arrowUp: 'Up',
    LogicalKeyboardKey.arrowDown: 'Down',
    LogicalKeyboardKey.home: 'Home',
    LogicalKeyboardKey.end: 'End',
    LogicalKeyboardKey.pageUp: 'PageUp',
    LogicalKeyboardKey.pageDown: 'PageDown',
    LogicalKeyboardKey.f1: 'F1',
    LogicalKeyboardKey.f2: 'F2',
    LogicalKeyboardKey.f3: 'F3',
    LogicalKeyboardKey.f4: 'F4',
    LogicalKeyboardKey.f5: 'F5',
    LogicalKeyboardKey.f6: 'F6',
    LogicalKeyboardKey.f7: 'F7',
    LogicalKeyboardKey.f8: 'F8',
    LogicalKeyboardKey.f9: 'F9',
    LogicalKeyboardKey.f10: 'F10',
    LogicalKeyboardKey.f11: 'F11',
    LogicalKeyboardKey.f12: 'F12',
  };
  Process? _process;
  StreamSubscription<List<int>>? _stdout, _stderr;
  final _decoder = Unpacker();
  final Map<int, Completer<Object?>> _pending = {};
  final List<String> _lines = [''];
  final List<int> _offsets = [0];
  final List<TextRange> visualBlocks = [];
  int _nextId = 0;
  bool _disposed = false;
  bool _closing = false;
  bool ready = false;
  Object? error;
  String mode = 'n';
  String commandLine = '';
  int commandPosition = 0;
  String message = '';
  String text = '';
  TextSelection selection = const TextSelection.collapsed(offset: 0);
  Future<void> _tail = Future.value();
  String _optimisticText = '';
  int _queuedEdits = 0;
  int _queuedCommandEdits = 0;
  bool get pendingCommandEdits => _queuedCommandEdits > 0;
  bool get acceptsText =>
      mode.startsWith('i') || mode.startsWith('R') || mode.startsWith('s');
  bool get pendingEdits => _queuedEdits > 0;
  String get modeLabel => switch (mode[0]) {
    'i' => 'INSERT',
    'R' => 'REPLACE',
    'v' => 'VISUAL',
    'V' => 'VISUAL LINE',
    '\u0016' => 'VISUAL BLOCK',
    's' => 'SELECT',
    'c' => 'COMMAND',
    _ => mode.startsWith('no') ? 'OPERATOR PENDING' : 'NORMAL',
  };

  static bool get supported =>
      Platform.isLinux || Platform.isMacOS || Platform.isWindows;

  static Future<VimEngine> start({
    required String text,
    required ValueChanged<String> onText,
    required Future<bool> Function() onSave,
    required Future<void> Function() onClose,
  }) async {
    if (!supported) throw StateError('The bundled Vim runtime is desktop-only');
    final engine = VimEngine._(
      onText: onText,
      onSave: onSave,
      onClose: onClose,
    );
    final executableDirectory = p.dirname(Platform.resolvedExecutable);
    final root = Platform.isMacOS
        ? p.normalize(
            p.join(executableDirectory, '..', 'Resources', 'runtime', 'neovim'),
          )
        : p.join(executableDirectory, 'runtime', 'neovim');
    final executable = p.join(
      root,
      'bin',
      Platform.isWindows ? 'nvim.exe' : 'nvim',
    );
    final runtime = p.join(root, 'share', 'nvim', 'runtime');
    if (!await File(executable).exists() ||
        !await Directory(runtime).exists()) {
      throw StateError(
        'Bundled Neovim runtime is missing at $root. Install a complete Paseo desktop package; your text is preserved in the native editor.',
      );
    }
    try {
      final environment = Map<String, String>.from(Platform.environment)
        ..remove('NVIM')
        ..remove('NVIM_LISTEN_ADDRESS')
        ..remove('VIMINIT')
        ..remove('EXINIT')
        ..remove('LUA_INIT')
        ..remove('LUA_INIT_5_1')
        ..remove('LUA_PATH')
        ..remove('LUA_CPATH')
        ..['VIMRUNTIME'] = runtime;
      engine._process = await Process.start(
        executable,
        [
          '--clean',
          '-u',
          'NONE',
          '-i',
          'NONE',
          '-n',
          '--noplugin',
          '--embed',
          '--headless',
        ],
        environment: environment,
        includeParentEnvironment: false,
        workingDirectory: root,
      );
      engine._stdout = engine._process!.stdout.listen(
        engine._receive,
        onError: engine._fail,
        onDone: () {
          if (!engine._closing) {
            engine._fail(
              StateError('Vim runtime disconnected; unsaved text is retained'),
            );
          }
        },
      );
      engine._stderr = engine._process!.stderr.listen((bytes) {
        engine.message = utf8.decode(bytes, allowMalformed: true);
        engine._notify();
      }, onError: engine._fail);
      unawaited(
        engine._process!.exitCode.then((code) {
          if (!engine._closing && !engine._disposed) {
            engine._fail(
              StateError(
                'Vim runtime exited ($code); unsaved text is retained',
              ),
            );
          }
        }),
      );
      engine.text = text;
      engine._optimisticText = text;
      engine._replaceLines(text.split('\n'));
      final api = await engine
          ._rpc('nvim_get_api_info', const [])
          .timeout(const Duration(seconds: 15));
      final channel = (api as List).first as int;
      await engine
          ._rpc('nvim_exec_lua', [
            _setup,
            [text, channel],
          ])
          .timeout(const Duration(seconds: 15));
      engine.ready = true;
      engine._notify();
      return engine;
    } catch (_) {
      await engine.close();
      rethrow;
    }
  }

  Future<Object?> _rpc(String method, List<Object?> arguments) {
    if (_disposed || _process == null) {
      return Future.error(StateError('Vim runtime is closed'));
    }
    final id = ++_nextId;
    final complete = Completer<Object?>();
    _pending[id] = complete;
    try {
      _send([0, id, method, arguments]);
    } catch (error, stack) {
      _pending.remove(id);
      complete.completeError(error, stack);
    }
    return complete.future;
  }

  void _send(List<Object?> message) {
    final packer = Packer()..pack(message);
    _process!.stdin.add(packer.takeBytes());
  }

  void _receive(List<int> bytes) {
    if (_disposed) return;
    try {
      _decoder.feed(bytes is Uint8List ? bytes : Uint8List.fromList(bytes));
      Object? value;
      while ((value = _decoder.next()) != null) {
        if (value is! List || value.length < 3) {
          throw const FormatException('Malformed Neovim RPC message');
        }
        if (value[0] == 1 && value.length == 4) {
          final pending = _pending.remove(value[1]);
          if (pending == null) continue;
          if (value[2] != null) {
            pending.completeError(StateError('Vim: ${value[2]}'));
          } else {
            pending.complete(value[3]);
          }
        } else if (value[0] == 2 && value.length == 3) {
          _notification(
            value[1].toString(),
            (value[2] as List).cast<Object?>(),
          );
        } else if (value[0] == 0 && value.length == 4) {
          unawaited(
            _request(
              value[1] as int,
              value[2].toString(),
              (value[3] as List).cast<Object?>(),
            ),
          );
        }
      }
    } catch (error) {
      _fail(error);
    }
  }

  Future<void> _request(int id, String method, List<Object?> arguments) async {
    try {
      final Object? result;
      if (method == 'paseo.save') {
        result = await onSave();
      } else if (method == 'paseo.clipboard.get') {
        final value = await Clipboard.getData(Clipboard.kTextPlain);
        result = [(value?.text ?? '').split('\n'), 'v'];
      } else if (method == 'paseo.clipboard.set') {
        final lines = (arguments.first as List).cast<String>();
        await Clipboard.setData(
          ClipboardData(
            text: lines.join('\n') + (arguments[1] == 'V' ? '\n' : ''),
          ),
        );
        result = true;
      } else {
        throw StateError('Unknown Vim client request: $method');
      }
      if (!_disposed && !_closing) _send([1, id, null, result]);
    } catch (error) {
      if (!_disposed && !_closing) _send([1, id, error.toString(), null]);
    }
  }

  void _notification(String method, List<Object?> arguments) {
    if (_closing || _disposed) return;
    if (method == 'paseo.lines') {
      final first = arguments[0] as int, last = arguments[1] as int;
      final replacement = (arguments[2] as List).cast<String>();
      _lines.replaceRange(first, last, replacement);
      if (_lines.isEmpty) _lines.add('');
      _reindex();
      text = _lines.join('\n');
      if (arguments[3] != true) onText(text);
      if (_queuedEdits == 0) _optimisticText = text;
      _notify();
    } else if (method == 'paseo.state') {
      final state = arguments.first as Map;
      mode = (state['mode'] as String?) ?? 'n';
      if (mode.isEmpty) mode = 'n';
      final cursor = state['cursor'] as List;
      final head = _offset(cursor[0] as int, cursor[1] as int);
      final anchor = state['anchor'] as List;
      final anchorOffset = _offset(anchor[0] as int, (anchor[1] as int) - 1);
      visualBlocks.clear();
      if (mode.startsWith('v') || mode.startsWith('s')) {
        selection = TextSelection(
          baseOffset: anchorOffset <= head
              ? anchorOffset
              : _nextRune(anchorOffset),
          extentOffset: anchorOffset <= head ? _nextRune(head) : head,
        );
      } else if (mode.startsWith('V') || mode.startsWith('S')) {
        final first =
            ((anchor[0] as int) < (cursor[0] as int) ? anchor[0] : cursor[0])
                as int;
        final last =
            ((anchor[0] as int) > (cursor[0] as int) ? anchor[0] : cursor[0])
                as int;
        selection = TextSelection(
          baseOffset: _offset(first, 0),
          extentOffset: last < _lines.length
              ? _offset(last + 1, 0)
              : text.length,
        );
      } else {
        selection = TextSelection.collapsed(offset: head);
        for (final raw in (state['blocks'] as List? ?? const [])) {
          final block = raw as List;
          final start = _offset(block[0] as int, block[1] as int);
          final end = _nextRune(_offset(block[0] as int, block[2] as int));
          visualBlocks.add(TextRange(start: start, end: end));
        }
      }
      commandLine = state['command'] as String? ?? '';
      commandPosition = state['commandPosition'] as int? ?? 0;
      message = state['message'] as String? ?? '';
      _notify();
    } else if (method == 'paseo.close') {
      unawaited(onClose().catchError(_fail));
    } else if (method == 'paseo.failure') {
      _fail(StateError(arguments.first.toString()));
    }
  }

  int _nextRune(int offset) {
    if (offset >= text.length) return text.length;
    final code = text.codeUnitAt(offset);
    return (offset + (code >= 0xd800 && code <= 0xdbff ? 2 : 1)).clamp(
      0,
      text.length,
    );
  }

  void _replaceLines(List<String> lines) {
    _lines.clear();
    _lines.addAll(lines);
    _reindex();
  }

  void _reindex() {
    _offsets.clear();
    var offset = 0;
    for (final line in _lines) {
      _offsets.add(offset);
      offset += line.length + 1;
    }
  }

  int _offset(int row, int byteColumn) {
    final index = (row - 1).clamp(0, _lines.length - 1);
    var bytes = 0, utf16 = 0;
    for (final rune in _lines[index].runes) {
      final width = rune <= 0x7f
          ? 1
          : rune <= 0x7ff
          ? 2
          : rune <= 0xffff
          ? 3
          : 4;
      if (bytes + width > byteColumn) break;
      bytes += width;
      utf16 += rune > 0xffff ? 2 : 1;
    }
    return (_offsets[index] + utf16).clamp(0, text.length);
  }

  List<int> _position(int offset, [String? source]) {
    final value = source ?? text;
    final bounded = offset.clamp(0, value.length);
    var row = 1, start = 0;
    for (var index = 0; index < bounded; index++) {
      if (value.codeUnitAt(index) == 10) {
        row++;
        start = index + 1;
      }
    }
    return [row, utf8.encode(value.substring(start, bounded)).length];
  }

  Future<void> _enqueue(Future<void> Function() action) {
    final operation = _tail.then((_) async {
      if (!_disposed && ready) await action();
    });
    _tail = operation.catchError((Object failure) {
      error = failure;
      _notify();
    });
    return _tail;
  }

  Future<void> flush() => _tail;

  Future<void> _sendKeys(String keys) async {
    var remaining = keys;
    while (remaining.isNotEmpty) {
      final accepted = await _rpc('nvim_input', [remaining]) as int;
      if (accepted == 0) {
        throw StateError('Vim input queue is full; input was not accepted');
      }
      final bytes = utf8.encode(remaining);
      remaining = accepted >= bytes.length
          ? ''
          : utf8.decode(bytes.sublist(accepted));
    }
  }

  Future<void> input(String keys) => _enqueue(() async {
    await _sendKeys(keys);
    await _rpc('nvim_exec_lua', ['PaseoEditor.state(); return true', const []]);
  });

  /// Flutter handles platform IME composition. Only committed text reaches Vim;
  /// actual nvim_input/nvim_paste preserve its undo, registers and dot-repeat.
  Future<void> applyFlutterValue(TextEditingValue value) {
    if (value.composing.isValid && !value.composing.isCollapsed) {
      return Future.value();
    }
    final before = _optimisticText;
    final after = value.text;
    if (before == after) return setSelection(value.selection);
    var start = 0;
    while (start < before.length &&
        start < after.length &&
        before.codeUnitAt(start) == after.codeUnitAt(start)) {
      start++;
    }
    // Do not split a surrogate pair at the changed boundary.
    if (start > 0 &&
        start < before.length &&
        before.codeUnitAt(start) >= 0xdc00 &&
        before.codeUnitAt(start) <= 0xdfff) {
      start--;
    }
    var oldEnd = before.length, newEnd = after.length;
    while (oldEnd > start &&
        newEnd > start &&
        before.codeUnitAt(oldEnd - 1) == after.codeUnitAt(newEnd - 1)) {
      oldEnd--;
      newEnd--;
    }
    if (oldEnd < before.length &&
        oldEnd > 0 &&
        before.codeUnitAt(oldEnd) >= 0xdc00 &&
        before.codeUnitAt(oldEnd) <= 0xdfff) {
      oldEnd++;
      newEnd++;
    }
    final deleted = oldEnd - start;
    final inserted = after.substring(start, newEnd);
    final position = _position(start, before);
    var lastDeleted = oldEnd - 1;
    if (lastDeleted > 0 &&
        before.codeUnitAt(lastDeleted) >= 0xdc00 &&
        before.codeUnitAt(lastDeleted) <= 0xdfff) {
      lastDeleted--;
    }
    final endPosition = deleted > 0 ? _position(lastDeleted, before) : position;
    _optimisticText = after;
    _queuedEdits++;
    return _enqueue(() async {
      try {
        if (deleted > 0) {
          await _rpc('nvim_exec_lua', [
            _changeRange,
            [position, endPosition],
          ]);
        } else {
          await _rpc('nvim_exec_lua', [
            "local position = ...; vim.api.nvim_win_set_cursor(0, position); local mode = vim.fn.mode(1):sub(1, 1); if mode ~= 'i' and mode ~= 'R' then vim.api.nvim_feedkeys('i', 'nx!', false) end; return true",
            [position],
          ]);
        }
        if (inserted.isNotEmpty) {
          if (inserted.runes.length == 1 && !inserted.contains('\n')) {
            await _sendKeys(inserted.replaceAll('<', '<lt>'));
          } else {
            final pasted = await _rpc('nvim_paste', [inserted, false, -1]);
            if (pasted != true) {
              throw StateError(
                'Vim rejected the committed text; the native editor retains it',
              );
            }
          }
        }
        await _rpc('nvim_exec_lua', [
          'PaseoEditor.state(); return true',
          const [],
        ]);
      } catch (failure) {
        error = failure;
        rethrow;
      } finally {
        _queuedEdits--;
        if (_queuedEdits == 0) {
          _optimisticText = text;
          _notify();
        }
      }
    });
  }

  Future<void> setSelection(TextSelection next) {
    if (!next.isValid || next == selection || _queuedEdits > 0) {
      return Future.value();
    }
    int previousRune(int offset) {
      var previous = (offset - 1).clamp(0, text.length);
      if (previous > 0 &&
          previous < text.length &&
          text.codeUnitAt(previous) >= 0xdc00 &&
          text.codeUnitAt(previous) <= 0xdfff) {
        previous--;
      }
      return previous;
    }

    final forward = next.baseOffset <= next.extentOffset;
    final anchor = _position(
      !next.isCollapsed && !forward
          ? previousRune(next.baseOffset)
          : next.baseOffset,
    );
    final head = _position(
      !next.isCollapsed && forward
          ? previousRune(next.extentOffset)
          : next.extentOffset,
    );
    return _enqueue(() async {
      await _rpc('nvim_exec_lua', [
        _selection,
        [anchor, head, next.isCollapsed],
      ]);
    });
  }

  Future<void> markSaved(String expected) => _enqueue(() async {
    await _rpc('nvim_exec_lua', [
      "local expected = ...; local b = PaseoEditor.buffer; if table.concat(vim.api.nvim_buf_get_lines(b, 0, -1, false), '\\n') == expected then vim.bo[b].modified = false end; return true",
      [expected],
    ]);
  });

  Future<void> setCommandLine(String content, int cursor) {
    final position =
        utf8
            .encode(content.substring(0, cursor.clamp(0, content.length)))
            .length +
        1;
    _queuedCommandEdits++;
    return _enqueue(() async {
      try {
        await _rpc('nvim_exec_lua', [
          "local content, position = ...; if vim.fn.mode(1):sub(1, 1) == 'c' then vim.fn.setcmdline(content); vim.fn.setcmdpos(position); PaseoEditor.state() end; return true",
          [content, position],
        ]);
      } finally {
        _queuedCommandEdits--;
        _notify();
      }
    });
  }

  Future<void> synchronize(String next) {
    if (next == text || _queuedEdits > 0) return Future.value();
    _optimisticText = next;
    return _enqueue(() async {
      await _rpc('nvim_exec_lua', [
        _replace,
        [next],
      ]);
    });
  }

  /// Printable insert/replace input is left to Flutter's platform text input.
  /// All normal/visual/operator/command-mode keys are interpreted by Neovim.
  KeyEventResult handleKey(KeyEvent event) {
    if (event is KeyUpEvent || !ready || error != null) {
      return KeyEventResult.ignored;
    }
    final keyboard = HardwareKeyboard.instance;
    final key = event.logicalKey;
    if (key == LogicalKeyboardKey.controlLeft ||
        key == LogicalKeyboardKey.controlRight ||
        key == LogicalKeyboardKey.shiftLeft ||
        key == LogicalKeyboardKey.shiftRight ||
        key == LogicalKeyboardKey.altLeft ||
        key == LogicalKeyboardKey.altRight ||
        key == LogicalKeyboardKey.metaLeft ||
        key == LogicalKeyboardKey.metaRight) {
      return KeyEventResult.ignored;
    }
    if (key == LogicalKeyboardKey.keyZ &&
        !keyboard.isAltPressed &&
        (keyboard.isControlPressed || keyboard.isMetaPressed)) {
      final redo = keyboard.isShiftPressed;
      unawaited(
        _enqueue(() async {
          await _rpc('nvim_exec_lua', [
            "local redo = ...; local ok, message = pcall(vim.cmd, redo and 'redo' or 'undo'); if not ok then vim.v.errmsg = tostring(message) end; PaseoEditor.state(); return true",
            [redo],
          ]);
        }),
      );
      return KeyEventResult.handled;
    }
    if (acceptsText &&
        _keyNames[key] == null &&
        !keyboard.isMetaPressed &&
        (!keyboard.isControlPressed || keyboard.isAltPressed)) {
      return KeyEventResult.ignored;
    }
    final special = _keyNames[key];
    String? keys;
    if (special != null ||
        keyboard.isControlPressed ||
        keyboard.isMetaPressed ||
        keyboard.isAltPressed) {
      final name = special ?? key.keyLabel.toLowerCase();
      if (name.isEmpty) return KeyEventResult.ignored;
      final modifiers =
          '${keyboard.isControlPressed ? 'C-' : ''}${keyboard.isMetaPressed ? 'D-' : ''}${keyboard.isAltPressed ? 'M-' : ''}${keyboard.isShiftPressed && special != null ? 'S-' : ''}';
      keys = '<$modifiers$name>';
    } else if (event.character != null) {
      keys = event.character!.replaceAll('<', '<lt>');
    }
    if (keys == null || keys.isEmpty) return KeyEventResult.ignored;
    unawaited(input(keys));
    return KeyEventResult.handled;
  }

  void _fail(Object failure) {
    if (_disposed || _closing) return;
    error = failure;
    ready = false;
    for (final pending in _pending.values) {
      if (!pending.isCompleted) pending.completeError(failure);
    }
    _pending.clear();
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  Future<void> close() async {
    if (_closing) return;
    _closing = true;
    ready = false;
    for (final pending in _pending.values) {
      if (!pending.isCompleted) {
        pending.completeError(StateError('Vim editor closed'));
      }
    }
    _pending.clear();
    final process = _process;
    if (process != null) {
      try {
        _send([
          2,
          'nvim_exec_lua',
          [
            "vim.api.nvim_clear_autocmds({event='QuitPre'}); vim.cmd('qa!')",
            const [],
          ],
        ]);
        await process.stdin.flush();
      } catch (_) {
        /* Process may already have exited. */
      }
      try {
        await process.stdin.close();
      } catch (_) {
        /* Process may already have exited. */
      }
      try {
        await process.exitCode.timeout(const Duration(seconds: 2));
      } on TimeoutException {
        process.kill();
        await process.exitCode;
      }
    }
    await _stdout?.cancel();
    await _stderr?.cancel();
    _disposed = true;
    super.dispose();
  }

  static const _changeRange = r'''
local first, last = ...
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'nx', false)
vim.api.nvim_win_set_cursor(0, first)
vim.cmd('normal! v')
vim.api.nvim_win_set_cursor(0, last)
vim.api.nvim_feedkeys('c', 'nx!', false)
return true
''';

  static const _setup = r'''
local initial, channel = ...
for _, address in ipairs(vim.fn.serverlist()) do vim.fn.serverstop(address) end
vim.o.modeline = false
vim.o.exrc = false
vim.o.swapfile = false
vim.o.backup = false
vim.o.writebackup = false
vim.o.undofile = false
vim.o.shada = ''
vim.o.clipboard = ''
vim.g.loaded_python3_provider = 0
vim.g.loaded_node_provider = 0
vim.g.loaded_perl_provider = 0
vim.g.loaded_ruby_provider = 0
local clipboardCopy = function(lines, registerType)
  return vim.rpcrequest(channel, 'paseo.clipboard.set', lines, registerType)
end
local clipboardPaste = function() return vim.rpcrequest(channel, 'paseo.clipboard.get') end
vim.g.clipboard = { name = 'Paseo', copy = { ['+'] = clipboardCopy, ['*'] = clipboardCopy },
  paste = { ['+'] = clipboardPaste, ['*'] = clipboardPaste }, cache_enabled = 0 }
vim.keymap.set('n', '<D-v>', '"+p')
vim.keymap.set('x', '<D-v>', '"+p')
vim.keymap.set('i', '<D-v>', '<C-r>+')
vim.keymap.set('c', '<D-v>', '<C-r>+')
vim.keymap.set('x', '<D-c>', '"+y')
vim.o.mouse = ''
vim.o.hidden = true
vim.o.more = false
vim.o.showmode = false
vim.o.autoindent = false
vim.o.smartindent = false
vim.o.cindent = false
vim.o.formatoptions = ''
vim.o.backspace = 'indent,eol,start'
vim.o.virtualedit = 'onemore'
vim.o.expandtab = true
vim.o.tabstop = 2
vim.o.shiftwidth = 2
vim.o.fixendofline = false
vim.o.endofline = false
local buffer = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_name(buffer, 'paseo://remote-file')
vim.bo[buffer].buftype = 'acwrite'
vim.bo[buffer].bufhidden = 'hide'
vim.api.nvim_buf_set_lines(buffer, 0, -1, false, vim.split(initial, '\n', { plain = true }))
vim.bo[buffer].modified = false
PaseoEditor = { buffer = buffer, suppress = false }
function PaseoEditor.state()
  if not vim.api.nvim_buf_is_valid(buffer) then return end
  if vim.api.nvim_get_current_buf() ~= buffer then return end
  local current = vim.api.nvim_win_get_cursor(0)
  local anchor = vim.fn.getpos('v')
  local mode = vim.fn.mode(1)
  local blocks = {}
  if mode:sub(1, 1) == '\022' or mode:sub(1, 1) == '\019' then
    local first = math.min(anchor[2], current[1])
    local last = math.max(anchor[2], current[1])
    local a, b = vim.fn.virtcol('v'), vim.fn.virtcol('.')
    for row = first, last do
      local left = vim.fn.virtcol2col(0, row, math.min(a, b))
      local right = vim.fn.virtcol2col(0, row, math.max(a, b))
      table.insert(blocks, {row, math.max(0, left - 1), math.max(0, right - 1)})
    end
  end
  local command = ''
  local commandPosition = 0
  if mode:sub(1, 1) == 'c' then
    command = vim.fn.getcmdtype() .. vim.fn.getcmdline()
    commandPosition = vim.fn.getcmdpos()
  end
  vim.rpcnotify(channel, 'paseo.state', { mode = mode, cursor = current,
    anchor = { anchor[2], anchor[3] }, blocks = blocks,
    command = command, commandPosition = commandPosition, message = vim.v.errmsg })
end
vim.api.nvim_buf_attach(buffer, false, {on_lines = function(_, buf, _, first, last, newlast)
  vim.rpcnotify(channel, 'paseo.lines', first, last,
    vim.api.nvim_buf_get_lines(buf, first, newlast, false), PaseoEditor.suppress)
  vim.schedule(PaseoEditor.state)
end, on_detach = function()
  vim.rpcnotify(channel, 'paseo.failure', 'The remote Vim buffer was closed; unsaved text is retained in the source editor')
end})
vim.api.nvim_create_autocmd({'CursorMoved', 'CursorMovedI', 'ModeChanged', 'CmdlineChanged', 'CmdlineEnter', 'CmdlineLeave'}, {
  callback = function() vim.schedule(PaseoEditor.state) end,
})
vim.on_key(function() vim.schedule(PaseoEditor.state) end, vim.api.nvim_create_namespace('paseo-keys'))
vim.api.nvim_create_autocmd('BufEnter', { callback = function()
  if vim.api.nvim_get_current_buf() ~= buffer and vim.api.nvim_buf_is_valid(buffer) then
    vim.api.nvim_set_current_buf(buffer)
    error('Open each remote file in its own Paseo tab')
  end
end })
vim.api.nvim_create_autocmd('BufFilePre', { buffer = buffer, callback = function()
  error('Rename remote files through Paseo Files')
end })
vim.api.nvim_create_autocmd('FileWriteCmd', { pattern = '*', callback = function()
  error('Save the complete remote file with :write, or export a copy through Paseo')
end })
vim.api.nvim_create_autocmd('BufWriteCmd', {
  pattern = '*', callback = function(event)
    if vim.api.nvim_get_current_buf() ~= buffer then error('Open remote files in their own Paseo tab') end
    if vim.fn.fnamemodify(event.match, ':p') ~= vim.fn.fnamemodify(vim.api.nvim_buf_get_name(buffer), ':p') then
      error('Use Paseo Save Copy to write to a different destination')
    end
    local saved = vim.rpcrequest(channel, 'paseo.save')
    if saved then vim.bo[buffer].modified = false
    else error('Remote save was not acknowledged; local edits are preserved') end
  end,
})
vim.api.nvim_create_autocmd({'BufReadCmd', 'FileReadCmd'}, {
  pattern = '*', callback = function() error('This editor never reads remote paths from the local filesystem; open files through Paseo Files') end,
})
vim.api.nvim_create_autocmd('QuitPre', { callback = function()
  vim.rpcnotify(channel, 'paseo.close')
  error('Paseo is handling the tab close request')
end })
PaseoEditor.state()
return true
''';

  static const _selection = r'''
local anchor, head, collapsed = ...
local mode = vim.fn.mode(1)
if collapsed then
  if mode:sub(1, 1) == 'v' or mode:sub(1, 1) == 'V' or mode:sub(1, 1) == '\022' or
      mode:sub(1, 1) == 's' or mode:sub(1, 1) == 'S' or mode:sub(1, 1) == '\019' then
    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'nx', false)
  end
  vim.api.nvim_win_set_cursor(0, head)
else
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'nx', false)
  vim.api.nvim_win_set_cursor(0, anchor)
  vim.cmd('normal! v')
  vim.api.nvim_win_set_cursor(0, head)
end
PaseoEditor.state()
return true
''';

  static const _replace = r'''
local content = ...
PaseoEditor.suppress = true
local undo = vim.bo[PaseoEditor.buffer].undolevels
vim.bo[PaseoEditor.buffer].undolevels = -1
vim.api.nvim_buf_set_lines(PaseoEditor.buffer, 0, -1, false, vim.split(content, '\n', { plain = true }))
vim.bo[PaseoEditor.buffer].undolevels = undo
vim.bo[PaseoEditor.buffer].modified = false
PaseoEditor.suppress = false
PaseoEditor.state()
return true
''';
}
