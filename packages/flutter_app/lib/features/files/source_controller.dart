import 'package:flutter/material.dart';
import 'package:highlight/highlight.dart' as syntax;

import 'syntax_palette.dart';

class SourceController extends TextEditingController {
  SourceController({required String path}) : language = languageFor(path);
  final String language;
  String? _cachedText;
  List<syntax.Node>? _nodes;
  bool highlighting = true;
  String syntaxTheme = 'one';
  List<TextRange> visualBlocks = const [];
  String? _indexedText;
  final List<int> _lineOffsets = [0];
  String? _measuredText;
  TextStyle? _measuredStyle;
  TextScaler? _measuredScaler;
  double _measuredWidth = 0;

  void _indexLines() {
    if (_indexedText == text) return;
    _indexedText = text;
    _lineOffsets.clear();
    _lineOffsets.add(0);
    for (var index = 0; index < text.length; index++) {
      if (text.codeUnitAt(index) == 10) {
        _lineOffsets.add(index + 1);
      }
    }
  }

  double intrinsicWidth(TextStyle style, TextScaler scaler) {
    if (_measuredText == text &&
        _measuredStyle == style &&
        _measuredScaler == scaler) {
      return _measuredWidth;
    }
    final painter = TextPainter(
      text: TextSpan(text: text, style: style),
      textDirection: TextDirection.ltr,
      textScaler: scaler,
    )..layout();
    _measuredWidth = painter.width;
    painter.dispose();
    _measuredText = text;
    _measuredStyle = style;
    _measuredScaler = scaler;
    return _measuredWidth;
  }

  static String languageFor(String path) {
    final extension = path.toLowerCase().split('.').last;
    return const {
          'js': 'javascript',
          'jsx': 'javascript',
          'ts': 'typescript',
          'tsx': 'typescript',
          'py': 'python',
          'rs': 'rust',
          'sh': 'bash',
          'zsh': 'bash',
          'yml': 'yaml',
          'md': 'markdown',
          'html': 'xml',
          'htm': 'xml',
          'svg': 'xml',
          'h': 'cpp',
          'c': 'cpp',
          'cc': 'cpp',
          'kt': 'kotlin',
          'rb': 'ruby',
          'cs': 'cs',
          'm': 'objectivec',
        }[extension] ??
        extension;
  }

  @override
  TextSpan buildTextSpan({
    required BuildContext context,
    TextStyle? style,
    required bool withComposing,
  }) {
    // Respect the platform IME underline; syntax can resume when composition ends.
    if ((!highlighting && visualBlocks.isEmpty) ||
        (withComposing &&
            value.composing.isValid &&
            !value.composing.isCollapsed)) {
      return super.buildTextSpan(
        context: context,
        style: style,
        withComposing: withComposing,
      );
    }
    if (highlighting && _cachedText != text) {
      _cachedText = text;
      _nodes = syntax.highlight.parse(text, language: language).nodes;
    }
    final dark = Theme.of(context).brightness == Brightness.dark;
    var offset = 0, blockIndex = 0;
    final selectedColor =
        Theme.of(context).textSelectionTheme.selectionColor ??
        Theme.of(context).colorScheme.primary.withValues(alpha: 0.3);
    List<InlineSpan> paint(String value) {
      final start = offset, end = offset + value.length;
      offset = end;
      if (visualBlocks.isEmpty) return [TextSpan(text: value)];
      final spans = <InlineSpan>[];
      var cursor = start;
      while (blockIndex < visualBlocks.length &&
          visualBlocks[blockIndex].end <= cursor) {
        blockIndex++;
      }
      var index = blockIndex;
      while (index < visualBlocks.length && visualBlocks[index].start < end) {
        final range = visualBlocks[index];
        final left = range.start.clamp(cursor, end),
            right = range.end.clamp(left, end);
        if (left > cursor) {
          spans.add(
            TextSpan(text: value.substring(cursor - start, left - start)),
          );
        }
        if (right > left) {
          spans.add(
            TextSpan(
              text: value.substring(left - start, right - start),
              style: TextStyle(backgroundColor: selectedColor),
            ),
          );
        }
        cursor = right;
        index++;
      }
      if (cursor < end) {
        spans.add(TextSpan(text: value.substring(cursor - start)));
      }
      return spans;
    }

    TextSpan span(syntax.Node node) => TextSpan(
      style: TextStyle(color: syntaxColor(syntaxTheme, dark, node.className)),
      children: [
        if (node.value != null) ...paint(node.value!),
        ...?node.children?.map(span),
      ],
    );
    return TextSpan(
      style: style,
      children: highlighting && _nodes != null
          ? _nodes!.map(span).toList()
          : paint(text),
    );
  }

  int offsetAt(int line, int column) {
    _indexLines();
    final index = (line - 1).clamp(0, _lineOffsets.length - 1);
    final start = _lineOffsets[index];
    final end = index + 1 < _lineOffsets.length
        ? _lineOffsets[index + 1] - 1
        : text.length;
    return (start + column - 1).clamp(start, end);
  }

  (int line, int column) locationAt(int position) {
    _indexLines();
    final offset = position.clamp(0, text.length);
    var low = 0, high = _lineOffsets.length;
    while (low + 1 < high) {
      final middle = (low + high) ~/ 2;
      if (_lineOffsets[middle] <= offset) {
        low = middle;
      } else {
        high = middle;
      }
    }
    return (low + 1, offset - _lineOffsets[low] + 1);
  }
}
