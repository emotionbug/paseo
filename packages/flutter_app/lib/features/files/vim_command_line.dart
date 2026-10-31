import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../ui/theme.dart';
import 'vim_engine.dart';

/// Native composition and selection, with Neovim owning command interpretation.
class VimCommandLine extends StatefulWidget {
  const VimCommandLine({
    super.key,
    required this.engine,
    required this.sourceFocus,
  });
  final VimEngine engine;
  final FocusNode sourceFocus;
  @override
  State<VimCommandLine> createState() => _VimCommandLineState();
}

class _VimCommandLineState extends State<VimCommandLine> {
  final _controller = TextEditingController();
  final _focus = FocusNode();
  bool _syncing = false;
  bool _wasCommand = false;

  @override
  void initState() {
    super.initState();
    _controller.addListener(_edit);
    widget.engine.addListener(_sync);
    _sync();
  }

  @override
  void didUpdateWidget(VimCommandLine oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.engine != widget.engine) {
      oldWidget.engine.removeListener(_sync);
      widget.engine.addListener(_sync);
      _sync();
    }
  }

  void _returnFocus() {
    final source = widget.sourceFocus;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (source.context?.mounted == true) source.requestFocus();
    });
  }

  void _sync() {
    if (!mounted) return;
    final engine = widget.engine;
    final isCommand = engine.commandLine.isNotEmpty;
    if (isCommand &&
        !engine.pendingCommandEdits &&
        !(_controller.value.composing.isValid &&
            !_controller.value.composing.isCollapsed)) {
      final content = engine.commandLine.substring(1);
      final bytes = utf8.encode(content);
      final byteOffset = (engine.commandPosition - 1).clamp(0, bytes.length);
      final offset = utf8
          .decode(bytes.sublist(0, byteOffset), allowMalformed: true)
          .length;
      final preserveSelection =
          content == _controller.text &&
          _controller.selection.isValid &&
          !_controller.selection.isCollapsed;
      _syncing = true;
      _controller.value = TextEditingValue(
        text: content,
        selection: preserveSelection
            ? _controller.selection
            : TextSelection.collapsed(offset: offset),
      );
      _syncing = false;
    }
    if (isCommand && !_wasCommand) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _focus.requestFocus();
      });
    } else if (!isCommand && _wasCommand) {
      _returnFocus();
    }
    _wasCommand = isCommand;
    setState(() {});
  }

  void _edit() {
    if (_syncing || widget.engine.commandLine.isEmpty) return;
    final value = _controller.value;
    if (value.composing.isValid && !value.composing.isCollapsed) return;
    unawaited(
      widget.engine.setCommandLine(value.text, value.selection.extentOffset),
    );
  }

  KeyEventResult _key(FocusNode _, KeyEvent event) {
    if (event is KeyUpEvent) return KeyEventResult.ignored;
    final key = event.logicalKey;
    final keyboard = HardwareKeyboard.instance;
    if ((keyboard.isControlPressed || keyboard.isMetaPressed) &&
        (key == LogicalKeyboardKey.keyS || key == LogicalKeyboardKey.keyF)) {
      return KeyEventResult.ignored;
    }
    if ((key == LogicalKeyboardKey.enter ||
            key == LogicalKeyboardKey.numpadEnter) &&
        _controller.value.composing.isValid &&
        !_controller.value.composing.isCollapsed) {
      return KeyEventResult.ignored;
    }
    if (keyboard.isMetaPressed &&
        !keyboard.isControlPressed &&
        [
          LogicalKeyboardKey.keyA,
          LogicalKeyboardKey.keyC,
          LogicalKeyboardKey.keyV,
          LogicalKeyboardKey.keyX,
        ].contains(key)) {
      return KeyEventResult.ignored;
    }
    if (key == LogicalKeyboardKey.enter ||
        key == LogicalKeyboardKey.numpadEnter ||
        key == LogicalKeyboardKey.escape ||
        key == LogicalKeyboardKey.tab ||
        key == LogicalKeyboardKey.arrowUp ||
        key == LogicalKeyboardKey.arrowDown ||
        (key == LogicalKeyboardKey.backspace && _controller.text.isEmpty) ||
        (keyboard.isControlPressed && !keyboard.isAltPressed) ||
        keyboard.isMetaPressed) {
      return widget.engine.handleKey(event);
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) {
    final engine = widget.engine;
    // The editor's code font, as CodeMirror's vim panel inherits it.
    final style = PaseoTypography.of(context).codeStyle.copyWith(
      height: 1.45,
      color: Theme.of(context).colorScheme.onSurface,
    );
    if (engine.commandLine.isEmpty) {
      return Align(
        alignment: Alignment.centerLeft,
        child: SelectableText(
          engine.message,
          style: style.copyWith(color: PaseoColors.of(context).muted),
        ),
      );
    }
    return Focus(
      onKeyEvent: _key,
      child: Row(
        children: [
          Text(engine.commandLine[0], style: style),
          Expanded(
            child: TextField(
              controller: _controller,
              focusNode: _focus,
              keyboardType: TextInputType.text,
              autocorrect: false,
              enableSuggestions: false,
              smartDashesType: SmartDashesType.disabled,
              smartQuotesType: SmartQuotesType.disabled,
              style: style,
              cursorColor: style.color,
              textDirection: TextDirection.ltr,
              // `.cm-vim-panel input`: no border or outline, inheriting the panel background.
              decoration: const InputDecoration.collapsed(hintText: null, filled: false),
              onSubmitted: (_) {
                unawaited(engine.input('<CR>'));
              },
            ),
          ),
        ],
      ),
    );
  }

  @override
  void dispose() {
    widget.engine.removeListener(_sync);
    if (_wasCommand) _returnFocus();
    _controller.dispose();
    _focus.dispose();
    super.dispose();
  }
}
