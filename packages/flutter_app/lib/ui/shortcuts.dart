import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/json.dart';

class ShortcutBinding {
  ShortcutBinding(this.data, {List<JsonMap>? chord})
    : chord =
          chord ??
          data.array('parsedChord').map(jsonMap).toList(growable: false);
  final JsonMap data;
  final List<JsonMap> chord;
  String get id => data.text('id');
  String get action => data.text('action');
  String get label => data.object('help').text('label', action);
  String get combo => data.text('combo');
  JsonMap get when => data.object('when');
  JsonMap get payload => data.object('payload');
  bool get matchesPlatform =>
      (when['mac'] == null || when['mac'] == Platform.isMacOS) &&
      (when['desktop'] == null ||
          when['desktop'] == !(Platform.isAndroid || Platform.isIOS));

  static List<JsonMap> parse(String value) => value
      .split(RegExp(r'\s+'))
      .where((part) => part.isNotEmpty)
      .map((step) {
        final parts = step.split('+');
        final key = parts.removeLast();
        if (key.isEmpty) throw const FormatException('A shortcut needs a key.');
        return <String, dynamic>{
          'key': key.toLowerCase(),
          'code': key == 'Digit' ? 'Digit' : key,
          if (parts.contains('Ctrl')) 'ctrl': true,
          if (parts.contains('Cmd') || parts.contains('Meta')) 'meta': true,
          if (parts.contains('Alt')) 'alt': true,
          if (parts.contains('Shift')) 'shift': true,
          if (parts.contains('Mod')) 'mod': true,
        };
      })
      .toList(growable: false);
}

class ShortcutCatalog {
  static Future<List<ShortcutBinding>>? _loading;
  static Future<List<ShortcutBinding>> load() => _loading ??= _load();
  static Future<List<ShortcutBinding>> _load() async {
    final raw = jsonDecode(await rootBundle.loadString('assets/keyboard.json'));
    return jsonList(raw)
        .map((value) => ShortcutBinding(jsonMap(value)))
        .where((binding) => binding.matchesPlatform)
        .toList(growable: false);
  }
}

class PaseoShortcutRouter {
  PaseoShortcutRouter(this._dispatch);
  final void Function(String action, JsonMap arguments) _dispatch;
  List<ShortcutBinding> _base = [];
  List<ShortcutBinding> _bindings = [];
  List<ShortcutBinding> _candidates = [];
  int _step = 0;
  Timer? _timeout;
  JsonMap _overrides = const {};

  Future<void> initialize() async {
    _base = await ShortcutCatalog.load();
    _rebuild();
  }

  void updateOverrides(JsonMap overrides) {
    if (identical(overrides, _overrides)) return;
    _overrides = overrides;
    _rebuild();
  }

  void _rebuild() {
    reset();
    _bindings = _base
        .map((binding) {
          final raw = _overrides[binding.id];
          return raw is String
              ? ShortcutBinding(binding.data, chord: ShortcutBinding.parse(raw))
              : binding;
        })
        .toList(growable: false);
  }

  void reset() {
    _timeout?.cancel();
    _timeout = null;
    _step = 0;
    _candidates = [];
  }

  void dispose() => reset();

  bool _enabled(
    ShortcutBinding binding,
    String focusScope,
    bool commandCenter,
  ) {
    final when = binding.when;
    if (when['editable'] == false &&
        (focusScope == 'editable' || focusScope == 'message-input')) {
      return false;
    }
    if (when['terminal'] == false && focusScope == 'terminal') return false;
    if (when['commandCenter'] == false && commandCenter) return false;
    final scope = when['focusScope'];
    return scope == null ||
        scope == focusScope ||
        scope is List && scope.contains(focusScope);
  }

  KeyEventResult handle(
    KeyEvent event, {
    required String focusScope,
    bool commandCenter = false,
  }) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    if (_modifiers.contains(event.logicalKey)) return KeyEventResult.ignored;
    // Original `resolveKeyboardShortcut`: a key that does not continue a pending
    // chord only resets it (it is not re-read as a first step), and at the first
    // step a chord that starts here takes precedence over a single-key binding.
    final initial = _step == 0;
    final matches = (initial ? _bindings : _candidates)
        .where(
          (binding) =>
              binding.chord.length > _step &&
              _matches(binding.chord[_step], event) &&
              !(event is KeyRepeatEvent &&
                  binding.data['repeat'] == false &&
                  binding.chord.length == _step + 1) &&
              _enabled(binding, focusScope, commandCenter),
        )
        .toList();
    final advancing = matches.where((b) => b.chord.length > _step + 1).toList();
    final completed = matches.where((b) => b.chord.length == _step + 1).firstOrNull;
    if (completed != null && !(initial && advancing.isNotEmpty)) {
      final payload = completed.payload;
      final arguments = <String, dynamic>{};
      if (payload['type'] == 'index') arguments['index'] = _digit(event);
      if (payload['type'] == 'delta') arguments['delta'] = payload['delta'];
      var action = completed.action;
      if (payload['type'] == 'message-input') {
        action = 'message-input.${payload.text('kind')}';
      }
      reset();
      _dispatch(action, arguments);
      return completed.data['preventDefault'] == false
          ? KeyEventResult.ignored
          : KeyEventResult.handled;
    }
    if (advancing.isEmpty) {
      reset();
      return KeyEventResult.ignored;
    }
    _candidates = advancing;
    _step++;
    _timeout?.cancel();
    _timeout = Timer(const Duration(milliseconds: 1500), reset);
    return KeyEventResult.handled;
  }

  static int? _digit(KeyEvent event) {
    final hid = event.physicalKey.usbHidUsage;
    if (hid >= 0x7001e && hid <= 0x70026) return hid - 0x7001d;
    return int.tryParse(event.logicalKey.keyLabel);
  }

  static bool _matches(JsonMap expected, KeyEvent event) {
    final keyboard = HardwareKeyboard.instance;
    final mod = expected['mod'] == true;
    if (keyboard.isControlPressed !=
            (expected['ctrl'] == true || mod && !Platform.isMacOS) ||
        keyboard.isMetaPressed !=
            (expected['meta'] == true || mod && Platform.isMacOS) ||
        keyboard.isAltPressed != (expected['alt'] == true) ||
        keyboard.isShiftPressed != (expected['shift'] == true)) {
      return false;
    }
    final code = expected.text('code');
    if (code == 'Digit') {
      final digit = _digit(event);
      return digit != null && digit >= 1 && digit <= 9;
    }
    final special = _keys[code.toLowerCase()];
    if (special != null) return event.logicalKey == special;
    // Original `matchesKeyOrCode`: key first (the shifted character too), and the
    // physical key only for Alt bindings (macOS rewrites Option+letter), a non-Latin
    // letter typed on a Latin binding, or an explicit code fallback. Latin layouts
    // such as Dvorak never get a second, physical-key binding.
    final key = expected.text('key').toLowerCase();
    final typed = {
      event.logicalKey.keyLabel.toLowerCase(),
      if (event.character case final character?) character.toLowerCase(),
    };
    if (key.isEmpty) return _physical(code, event);
    if (typed.contains(key)) return true;
    final shifted = expected.text('shiftedKey').toLowerCase();
    if (expected['shift'] == true && shifted.isNotEmpty && typed.contains(shifted)) {
      return true;
    }
    final latin = RegExp(r'^[a-z]$');
    final letter = RegExp(r'^\p{L}$', unicode: true);
    if (latin.hasMatch(key) &&
        typed.any((value) => letter.hasMatch(value) && !latin.hasMatch(value))) {
      return _physical(code, event);
    }
    if (expected['alt'] == true && _physical(code, event)) return true;
    return expected['codeFallback'] == true && _physical(code, event);
  }

  /// `event.code === combo.code` for the physical key codes bindings use.
  static bool _physical(String code, KeyEvent event) {
    if (code.startsWith('Key') && code.length == 4) {
      return event.physicalKey.usbHidUsage == 0x70004 + code.codeUnitAt(3) - 65;
    }
    if (code.startsWith('Digit') && code.length == 6) {
      final hid = event.physicalKey.usbHidUsage;
      final digit = int.tryParse(code.substring(5));
      return digit != null && hid == (digit == 0 ? 0x70027 : 0x7001d + digit);
    }
    return _physicalCodes[code] == event.physicalKey;
  }

  static const _physicalCodes = <String, PhysicalKeyboardKey>{
    'BracketLeft': PhysicalKeyboardKey.bracketLeft,
    'BracketRight': PhysicalKeyboardKey.bracketRight,
    'Backslash': PhysicalKeyboardKey.backslash,
    'Backquote': PhysicalKeyboardKey.backquote,
    'Comma': PhysicalKeyboardKey.comma,
    'Period': PhysicalKeyboardKey.period,
    'Slash': PhysicalKeyboardKey.slash,
    'Semicolon': PhysicalKeyboardKey.semicolon,
    'Quote': PhysicalKeyboardKey.quote,
    'Minus': PhysicalKeyboardKey.minus,
    'Equal': PhysicalKeyboardKey.equal,
  };

  static final _modifiers = {
    LogicalKeyboardKey.controlLeft,
    LogicalKeyboardKey.controlRight,
    LogicalKeyboardKey.metaLeft,
    LogicalKeyboardKey.metaRight,
    LogicalKeyboardKey.altLeft,
    LogicalKeyboardKey.altRight,
    LogicalKeyboardKey.shiftLeft,
    LogicalKeyboardKey.shiftRight,
  };
  static const _keys = <String, LogicalKeyboardKey>{
    'enter': LogicalKeyboardKey.enter,
    'escape': LogicalKeyboardKey.escape,
    'space': LogicalKeyboardKey.space,
    'backspace': LogicalKeyboardKey.backspace,
    'tab': LogicalKeyboardKey.tab,
    'delete': LogicalKeyboardKey.delete,
    'backquote': LogicalKeyboardKey.backquote,
    'backslash': LogicalKeyboardKey.backslash,
    'bracketleft': LogicalKeyboardKey.bracketLeft,
    'bracketright': LogicalKeyboardKey.bracketRight,
    'comma': LogicalKeyboardKey.comma,
    'period': LogicalKeyboardKey.period,
    'slash': LogicalKeyboardKey.slash,
    'insert': LogicalKeyboardKey.insert,
    'home': LogicalKeyboardKey.home,
    'end': LogicalKeyboardKey.end,
    'pageup': LogicalKeyboardKey.pageUp,
    'pagedown': LogicalKeyboardKey.pageDown,
    'arrowleft': LogicalKeyboardKey.arrowLeft,
    'arrowright': LogicalKeyboardKey.arrowRight,
    'arrowup': LogicalKeyboardKey.arrowUp,
    'arrowdown': LogicalKeyboardKey.arrowDown,
    'arrow left': LogicalKeyboardKey.arrowLeft,
    'arrow right': LogicalKeyboardKey.arrowRight,
    'arrow up': LogicalKeyboardKey.arrowUp,
    'arrow down': LogicalKeyboardKey.arrowDown,
    'f1': LogicalKeyboardKey.f1,
    'f2': LogicalKeyboardKey.f2,
    'f3': LogicalKeyboardKey.f3,
    'f4': LogicalKeyboardKey.f4,
    'f5': LogicalKeyboardKey.f5,
    'f6': LogicalKeyboardKey.f6,
    'f7': LogicalKeyboardKey.f7,
    'f8': LogicalKeyboardKey.f8,
    'f9': LogicalKeyboardKey.f9,
    'f10': LogicalKeyboardKey.f10,
    'f11': LogicalKeyboardKey.f11,
    'f12': LogicalKeyboardKey.f12,
  };
}
