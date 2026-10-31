import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:highlight/highlight.dart' as syntax;
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../files/source_controller.dart';
import '../files/syntax_palette.dart';
import 'chat_controller.dart' show maps;
import 'stream/tool_call_display.dart';

// Original components/tool-call-details.tsx, tool-call-sheet.tsx and
// utils/tool-call-detail-state.ts.

bool _meaningfulValue(Object? value) => switch (value) {
  null => false,
  String() => value.trim().isNotEmpty,
  num() || bool() => true,
  List() => value.any(_meaningfulValue),
  Map() => value.values.any(_meaningfulValue),
  _ => true,
};

bool _present(Object? value) => switch (value) {
  null => false,
  String() => value.isNotEmpty,
  _ => true,
};

/// tool-call-detail-state.ts hasMeaningfulToolCallDetail.
bool hasMeaningfulToolCallDetail(JsonMap detail) => switch (detail['type']) {
  'shell' => true,
  'read' || 'write' => _present(detail['filePath']) || _present(detail['content']),
  'edit' => _present(detail['filePath']) ||
      _present(detail['unifiedDiff']) ||
      _present(detail['oldString']) ||
      _present(detail['newString']),
  'search' => stringValue(detail['query']).trim().isNotEmpty ||
      _present(detail['content']) ||
      jsonList(detail['filePaths']).isNotEmpty ||
      jsonList(detail['webResults']).isNotEmpty ||
      jsonList(detail['annotations']).isNotEmpty,
  'fetch' => _present(detail['url']) || _present(detail['result']) || _present(detail['codeText']),
  'sub_agent' => _present(detail['subAgentType']) ||
      _present(detail['description']) ||
      _present(detail['log']),
  'plain_text' => _present(detail['label']) || _present(detail['text']),
  'plan' => stringValue(detail['text']).trim().isNotEmpty,
  'unknown' => _meaningfulValue(detail['input']) || _meaningfulValue(detail['output']),
  _ => false,
};

/// tool-call-detail-state.ts isPendingToolCallDetail.
bool isPendingToolCallDetail(JsonMap detail, String status, Object? error) =>
    detail['type'] != 'worktree_setup' &&
    (status == 'running' || status == 'executing') &&
    error == null &&
    !hasMeaningfulToolCallDetail(detail);

/// protocol tool-call-display.ts formatErrorText (failed calls only).
String? toolCallErrorText(String status, Object? error) {
  if (status != 'failed' || error == null) return null;
  if (error is String) return error;
  if (error is Map && error['content'] is String) return error['content'] as String;
  try {
    return const JsonEncoder.withIndent('  ').convert(error);
  } catch (_) {
    return '$error';
  }
}

/// Inline badge details are capped at [maxHeight] (ToolCall passes 400, 200
/// inside overview groups); the sheet fills its height.
class ToolCallDetailsContent extends StatelessWidget {
  const ToolCallDetailsContent({
    super.key,
    required this.detail,
    this.toolName,
    this.errorText,
    this.maxHeight = 400,
    this.fillAvailableHeight = false,
    this.fillHeight,
    this.showLoadingSkeleton = false,
  });

  final JsonMap detail;

  /// Paseo MCP tools render their unknown detail as Prompt/Details/Result.
  final String? toolName;
  final String? errorText;
  final double maxHeight;
  final bool fillAvailableHeight, showLoadingSkeleton;

  /// The sheet's content height at its largest snap point. tool-call-sheet
  /// lays its content out at that size (contentContainer flexGrow 1) and
  /// resolveShouldFill stretches shell/edit/write/read/sub_agent sections to
  /// it (styles.fillHeight), so their block reaches the sheet bottom and any
  /// Error section sits below the 60% snap's visible area.
  final double? fillHeight;

  double? get _maxHeight => fillAvailableHeight ? null : maxHeight;

  static const _fillTypes = {'shell', 'edit', 'write', 'read', 'sub_agent'};

  @override
  Widget build(BuildContext context) {
    final sections = _sections(context);
    final fill = fillAvailableHeight ? fillHeight : null;
    if (fill != null && sections.isNotEmpty && _fillTypes.contains(detail['type'])) {
      sections[0] = ConstrainedBox(
        constraints: BoxConstraints(minHeight: fill),
        child: sections[0],
      );
    }
    if (errorText != null && errorText!.isNotEmpty) {
      sections.add(_ErrorSection(text: errorText!));
    }
    final colors = Theme.of(context).colorScheme;
    if (sections.isEmpty) {
      if (showLoadingSkeleton) return const _LoadingSkeleton();
      return Text(
        tr(context, 'toolCallDetails.empty'),
        style: TextStyle(
          color: colors.onSurfaceVariant,
          fontSize: PaseoTypography.of(context).uiSize(14),
          fontStyle: FontStyle.italic,
        ),
      );
    }
    final type = detail['type'];
    final fullBleed = type == 'edit' || type == 'shell' || type == 'write';
    return SelectionArea(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        spacing: fullBleed ? 8 : 16,
        children: sections,
      ),
    );
  }

  List<Widget> _sections(BuildContext context) {
    final d = detail;
    switch (d['type']) {
      case 'shell':
        final command = stringValue(d['command']).replaceFirst(RegExp(r'\n+$'), '');
        final output = stringValue(d['output']).replaceFirst(RegExp(r'^\n+'), '');
        return [
          _FullBleedBlock(
            maxHeight: _maxHeight,
            child: Builder(
              builder: (context) => Text.rich(
                TextSpan(
                  children: [
                    TextSpan(
                      text: r'$ ',
                      style: TextStyle(color: Theme.of(context).colorScheme.onSurfaceVariant),
                    ),
                    TextSpan(text: output.isEmpty ? command : '$command\n\n$output'),
                  ],
                ),
                softWrap: false,
                style: _codeStyle(context),
              ),
            ),
          ),
        ];
      case 'sub_agent':
        return [
          _FullBleedBlock(
            maxHeight: _maxHeight,
            child: _SubAgentLog(detail: d),
          ),
        ];
      case 'edit':
        final lines = d['unifiedDiff'] != null
            ? parseUnifiedDiff(stringValue(d['unifiedDiff']))
            : buildLineDiff(stringValue(d['oldString']), stringValue(d['newString']));
        return [_DiffLines(lines: lines, maxHeight: _maxHeight)];
      case 'write':
        if (!_present(d['content'])) return [];
        return [
          _ScrollArea(
            maxHeight: _maxHeight,
            child: _HighlightedLines(
              content: stringValue(d['content']),
              path: stringValue(d['filePath']),
            ),
          ),
        ];
      case 'read':
        if (!_present(d['content'])) return [];
        return [
          _ScrollArea(
            maxHeight: _maxHeight,
            child: _HighlightedLines(
              content: stringValue(d['content']),
              path: stringValue(d['filePath']),
              startLine: d['offset'] is num ? (d['offset'] as num).toInt() : 1,
            ),
          ),
        ];
      case 'search':
        return [
          if (_present(d['content']))
            _ScrollArea(maxHeight: _maxHeight, child: _CodeText(stringValue(d['content']))),
          if (jsonList(d['filePaths']).isNotEmpty)
            _CodeText(jsonList(d['filePaths']).map(stringValue).join('\n')),
          if (jsonList(d['webResults']).isNotEmpty)
            _CodeText(
              maps(d['webResults'])
                  .map((r) => '${stringValue(r['title'])}\n${stringValue(r['url'])}')
                  .join('\n\n'),
            ),
          if (jsonList(d['annotations']).isNotEmpty)
            _CodeText(jsonList(d['annotations']).map(stringValue).join('\n\n')),
        ];
      case 'fetch':
        final url = stringValue(d['url']);
        final result = stringValue(d['result']);
        return [
          _ScrollArea(
            maxHeight: _maxHeight,
            child: _CodeText(result.isEmpty ? url : '$url\n\n$result'),
          ),
        ];
      case 'plain_text':
        if (!_present(d['text'])) return [];
        return [_PlainTextSection(text: stringValue(d['text']), maxHeight: _maxHeight)];
      case 'unknown':
        final input = d['input'], output = d['output'];
        final paseo = toolName == null ? null : buildPaseoToolDetailSections(toolName!, input, output);
        if (paseo != null) {
          return [for (final section in paseo) _PaseoSection(section: section)];
        }
        if (input is String && output == null) {
          return [_PlainTextSection(text: input, maxHeight: _maxHeight)];
        }
        final out = <Widget>[];
        for (final (key, value) in [('toolCallDetails.input', input), ('toolCallDetails.output', output)]) {
          if (!_meaningfulValue(value)) continue;
          final text = _serialize(value);
          if (text.isEmpty) continue;
          out
            ..add(_GroupHeader(title: tr(context, key)))
            ..add(_JsonBox(child: _CodeText(text)));
        }
        return out;
      default:
        return [];
    }
  }

  static String _serialize(Object? value) {
    if (value is String) return value;
    try {
      return const JsonEncoder.withIndent('  ').convert(value);
    } catch (_) {
      return '$value';
    }
  }
}

/// styles.scrollText: mono code size, 18px line height, foreground.
TextStyle _codeStyle(BuildContext context) {
  final typography = PaseoTypography.of(context);
  return typography.codeStyle.copyWith(
    height: 18 / typography.codeFontSize,
    color: Theme.of(context).colorScheme.onSurface,
  );
}

class _CodeText extends StatelessWidget {
  const _CodeText(this.text, {this.color});
  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) => Text(
    text,
    softWrap: false,
    style: _codeStyle(context).copyWith(color: color),
  );
}

/// Vertical scroll capped at [maxHeight] wrapping a horizontal scroll, so
/// long lines never wrap (original whiteSpace: pre).
class _BiScroll extends StatelessWidget {
  const _BiScroll({
    required this.child,
    required this.maxHeight,
    this.padding = const EdgeInsets.all(12),
    this.horizontalPadding = EdgeInsets.zero,
  });
  final Widget child;
  final double? maxHeight;
  final EdgeInsets padding, horizontalPadding;

  @override
  Widget build(BuildContext context) {
    final horizontal = LayoutBuilder(
      builder: (context, constraints) => SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        padding: horizontalPadding,
        child: ConstrainedBox(
          constraints: BoxConstraints(
            minWidth: (constraints.maxWidth - horizontalPadding.horizontal).clamp(0, double.infinity),
          ),
          child: child,
        ),
      ),
    );
    final body = Padding(padding: padding, child: horizontal);
    if (maxHeight == null) return body;
    return ConstrainedBox(
      constraints: BoxConstraints(maxHeight: maxHeight!),
      child: SingleChildScrollView(child: body),
    );
  }
}

/// styles.fullBleedBlock: surface1, no border; codeLine 12px padding plus
/// the extra right (16) and bottom (12) code insets.
class _FullBleedBlock extends StatelessWidget {
  const _FullBleedBlock({required this.child, required this.maxHeight});
  final Widget child;
  final double? maxHeight;

  @override
  Widget build(BuildContext context) => ColoredBox(
    color: PaseoColors.of(context).surface1,
    child: _BiScroll(
      maxHeight: maxHeight,
      padding: const EdgeInsets.only(bottom: 12),
      horizontalPadding: const EdgeInsets.only(right: 16),
      child: Padding(padding: const EdgeInsets.all(12), child: child),
    ),
  );
}

/// styles.scrollArea: 1px border, 4px radius, surface2, 12px content padding.
class _ScrollArea extends StatelessWidget {
  const _ScrollArea({required this.child, required this.maxHeight});
  final Widget child;
  final double? maxHeight;

  @override
  Widget build(BuildContext context) => DecoratedBox(
    decoration: BoxDecoration(
      color: PaseoColors.of(context).surface2,
      border: Border.all(color: Theme.of(context).colorScheme.outline),
      borderRadius: BorderRadius.circular(4),
    ),
    child: _BiScroll(maxHeight: maxHeight, child: child),
  );
}

class _JsonBox extends StatelessWidget {
  const _JsonBox({required this.child, this.error = false});
  final Widget child;
  final bool error;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return DecoratedBox(
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface2,
        border: Border.all(color: error ? colors.error : colors.outline),
        borderRadius: BorderRadius.circular(4),
      ),
      child: _BiScroll(maxHeight: null, child: child),
    );
  }
}

class _PlainTextSection extends StatelessWidget {
  const _PlainTextSection({required this.text, required this.maxHeight});
  final String text;
  final double? maxHeight;

  @override
  Widget build(BuildContext context) {
    final size = PaseoTypography.of(context).uiSize(14);
    final body = Padding(
      padding: const EdgeInsets.all(12),
      child: Text(
        text,
        style: TextStyle(
          fontSize: size,
          height: 22 / size,
          color: Theme.of(context).colorScheme.onSurface,
        ),
      ),
    );
    return DecoratedBox(
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface2,
        border: Border.all(color: Theme.of(context).colorScheme.outline),
        borderRadius: BorderRadius.circular(4),
      ),
      child: maxHeight == null
          ? body
          : ConstrainedBox(
              constraints: BoxConstraints(maxHeight: maxHeight!),
              child: SingleChildScrollView(child: body),
            ),
    );
  }
}

class _GroupHeader extends StatelessWidget {
  const _GroupHeader({required this.title});
  final String title;

  @override
  Widget build(BuildContext context) => DecoratedBox(
    decoration: BoxDecoration(
      border: Border(bottom: BorderSide(color: Theme.of(context).colorScheme.outline)),
    ),
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      child: Text(
        title,
        style: TextStyle(
          color: Theme.of(context).colorScheme.onSurfaceVariant,
          fontSize: PaseoTypography.of(context).uiSize(14),
        ),
      ),
    ),
  );
}

class _ErrorSection extends StatelessWidget {
  const _ErrorSection({required this.text});
  final String text;

  @override
  Widget build(BuildContext context) {
    final error = Theme.of(context).colorScheme.error;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 8,
      children: [
        Text(
          tr(context, 'toolCallDetails.error').toUpperCase(),
          style: TextStyle(
            color: error,
            fontSize: PaseoTypography.of(context).uiSize(12),
            fontWeight: FontWeight.w600,
            letterSpacing: .5,
          ),
        ),
        _JsonBox(error: true, child: _CodeText(text, color: error)),
      ],
    );
  }
}

class _LoadingSkeleton extends StatelessWidget {
  const _LoadingSkeleton();

  @override
  Widget build(BuildContext context) {
    final color = PaseoColors.of(context).surface3;
    Widget line(double factor) => FractionallySizedBox(
      alignment: Alignment.centerLeft,
      widthFactor: factor,
      child: Container(
        height: 12,
        decoration: BoxDecoration(color: color, borderRadius: BorderRadius.circular(999)),
      ),
    );
    return Padding(
      padding: const EdgeInsets.all(12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        spacing: 8,
        children: [line(1), line(.72), line(.48)],
      ),
    );
  }
}

/// Read/write content: syntax-highlighted by extension, with a muted
/// line-number gutter when a start line is known (Read).
class _HighlightedLines extends StatelessWidget {
  const _HighlightedLines({required this.content, required this.path, this.startLine});
  final String content, path;
  final int? startLine;

  static const _highlightLimit = 100000;

  /// Recent parses by language and content: a rebuild (a store update, the
  /// sheet relayout, scrolling back to the row) never re-runs the
  /// highlighter over the same file content.
  static final _parses = <(String, String), List<syntax.Node>?>{};

  static List<syntax.Node>? _nodes(String content, String language) {
    final key = (language, content);
    if (_parses.containsKey(key)) {
      final nodes = _parses.remove(key);
      return _parses[key] = nodes;
    }
    List<syntax.Node>? nodes;
    try {
      nodes = syntax.highlight.parse(content, language: language).nodes;
    } catch (_) {
      nodes = null;
    }
    if (_parses.length >= 24) _parses.remove(_parses.keys.first);
    return _parses[key] = nodes;
  }

  @override
  Widget build(BuildContext context) {
    final style = _codeStyle(context);
    final dark = Theme.of(context).brightness == Brightness.dark;
    final theme = PaseoScope.select(
      context,
      (store) => stringValue(store.preferences['syntaxTheme'], 'one'),
    );
    final nodes = content.length <= _highlightLimit
        ? _nodes(content, SourceController.languageFor(path))
        : null;
    TextSpan span(syntax.Node node) => TextSpan(
      text: node.value,
      style: TextStyle(color: syntaxColor(theme, dark, node.className)),
      children: node.children?.map(span).toList(),
    );
    final body = Text.rich(
      TextSpan(children: nodes == null ? [TextSpan(text: content)] : nodes.map(span).toList()),
      softWrap: false,
      style: style,
    );
    if (startLine == null) return body;
    final count = '\n'.allMatches(content).length + 1;
    final last = startLine! + count - 1;
    final digits = '$last'.length < 2 ? 2 : '$last'.length;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        SelectionContainer.disabled(
          child: Text(
            [for (var i = 0; i < count; i++) '${'${startLine! + i}'.padLeft(digits)} '].join('\n'),
            softWrap: false,
            style: style.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant.withValues(alpha: .6),
            ),
          ),
        ),
        body,
      ],
    );
  }
}

class _SubAgentLog extends StatelessWidget {
  const _SubAgentLog({required this.detail});
  final JsonMap detail;

  static String _toolName(String name) {
    final trimmed = name.trim();
    if (trimmed.isEmpty) return name;
    return trimmed
        .replaceAll(RegExp(r'[._-]+'), ' ')
        .split(' ')
        .where((s) => s.isNotEmpty)
        .map((s) => '${s[0].toUpperCase()}${s.substring(1)}')
        .join(' ');
  }

  @override
  Widget build(BuildContext context) {
    final style = _codeStyle(context);
    final muted = Theme.of(context).colorScheme.onSurfaceVariant;
    final actions = <(String, String?)>[];
    final remaining = <String>[];
    for (final line in stringValue(detail['log']).replaceFirst(RegExp(r'^\n+'), '').split('\n')) {
      final normalized = line.trim();
      if (normalized.isEmpty) continue;
      final match = RegExp(r'^\[([^\]]+)\](?:\s+(.*))?$').firstMatch(normalized);
      final tool = match?.group(1)?.trim();
      if (tool != null && tool.isNotEmpty) {
        final summary = match!.group(2)?.trim();
        actions.add((tool, summary == null || summary.isEmpty ? null : summary));
      } else {
        remaining.add(line);
      }
    }
    final log = remaining.join('\n').replaceFirst(RegExp(r'^\n+'), '');
    final type = stringValue(detail['subAgentType']);
    final description = stringValue(detail['description']);
    final fallback = type.isNotEmpty && description.isNotEmpty
        ? '$type: $description'
        : type.isNotEmpty
        ? type
        : description.isNotEmpty
        ? description
        : tr(context, 'toolCallDetails.subAgentActivity');
    final session = stringValue(detail['childSessionId']);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (session.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: Text('session $session', softWrap: false, style: style.copyWith(color: muted)),
          ),
        if (actions.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 4,
              children: [
                for (final (tool, summary) in actions)
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    spacing: 8,
                    children: [
                      Text(_toolName(tool), softWrap: false, style: style.copyWith(color: muted)),
                      if (summary != null) Text(summary, softWrap: false, style: style),
                    ],
                  ),
              ],
            ),
          ),
        if (log.isNotEmpty)
          Text(log, softWrap: false, style: style)
        else if (actions.isEmpty)
          Text(fallback, softWrap: false, style: style),
      ],
    );
  }
}

/// utils/tool-call-parsers.ts DiffLine.
typedef ToolDiffLine = ({String type, String content});

List<String> _splitLines(String text) {
  if (text.isEmpty) return const [];
  final lines = text.replaceAll('\r\n', '\n').split('\n');
  if (lines.isNotEmpty && lines.last.isEmpty) lines.removeLast();
  return lines;
}

/// tool-call-parsers.ts parseUnifiedDiff.
List<ToolDiffLine> parseUnifiedDiff(String text) {
  final out = <ToolDiffLine>[];
  for (final line in _splitLines(text)) {
    if (line.isEmpty) {
      out.add((type: 'context', content: line));
    } else if (line.startsWith('@@')) {
      out.add((type: 'header', content: line));
    } else if (line.startsWith('+')) {
      if (!line.startsWith('+++')) out.add((type: 'add', content: line));
    } else if (line.startsWith('-')) {
      if (!line.startsWith('---')) out.add((type: 'remove', content: line));
    } else if (line.startsWith('diff --git') || line.startsWith('index ')) {
      continue;
    } else if (line.startsWith(r'\ No newline')) {
      out.add((type: 'header', content: line));
    } else {
      out.add((type: 'context', content: line));
    }
  }
  return out;
}

/// tool-call-parsers.ts buildLineDiff (LCS line diff).
List<ToolDiffLine> buildLineDiff(String original, String updated) {
  final a = _splitLines(original), b = _splitLines(updated);
  if (a.isEmpty && b.isEmpty) return const [];
  final m = a.length, n = b.length;
  final dp = List.generate(m + 1, (_) => List.filled(n + 1, 0));
  for (var i = m - 1; i >= 0; i--) {
    for (var j = n - 1; j >= 0; j--) {
      dp[i][j] = a[i] == b[j] ? dp[i + 1][j + 1] + 1 : (dp[i + 1][j] >= dp[i][j + 1] ? dp[i + 1][j] : dp[i][j + 1]);
    }
  }
  final out = <ToolDiffLine>[];
  var i = 0, j = 0;
  while (i < m && j < n) {
    if (a[i] == b[j]) {
      out.add((type: 'context', content: ' ${a[i]}'));
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      out.add((type: 'remove', content: '-${a[i]}'));
      i++;
    } else {
      out.add((type: 'add', content: '+${b[j]}'));
      j++;
    }
  }
  while (i < m) {
    out.add((type: 'remove', content: '-${a[i++]}'));
  }
  while (j < n) {
    out.add((type: 'add', content: '+${b[j++]}'));
  }
  return out;
}

/// components/diff-viewer.tsx DiffViewer inside the full-bleed surface1 block.
class _DiffLines extends StatelessWidget {
  const _DiffLines({required this.lines, required this.maxHeight});
  final List<ToolDiffLine> lines;
  final double? maxHeight;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    if (lines.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(16),
        child: Center(
          child: Text(
            tr(context, 'diffViewer.empty'),
            style: TextStyle(color: colors.onSurfaceVariant, fontSize: PaseoTypography.of(context).uiSize(14)),
          ),
        ),
      );
    }
    final style = PaseoTypography.of(context).codeStyle.copyWith(height: null, color: colors.onSurface);
    return ColoredBox(
      color: palette.surface1,
      child: _BiScroll(
        maxHeight: maxHeight,
        padding: const EdgeInsets.only(bottom: 12),
        horizontalPadding: const EdgeInsets.only(right: 16),
        child: Padding(
          padding: const EdgeInsets.all(12),
          child: IntrinsicWidth(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (final line in lines)
                  ColoredBox(
                    color: switch (line.type) {
                      'add' => const Color(0x262ea043),
                      'remove' => const Color(0x1af85149),
                      _ => palette.surface1,
                    },
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 4),
                      child: Text(
                        line.content,
                        softWrap: false,
                        style: style.copyWith(
                          color: line.type == 'header' || line.type == 'context'
                              ? colors.onSurfaceVariant
                              : colors.onSurface,
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// components/tool-call-sheet.tsx: compact layouts open tool details in a
/// surface2 bottom sheet (60% / 95% snap points, 16px radius, 50% backdrop)
/// with a 20px icon, single-line semibold title and a muted close glyph.
Future<void> showToolCallSheet(
  BuildContext context, {
  required ToolGlyph icon,
  required String displayName,
  required JsonMap detail,
  String? toolName,
  String? errorText,
  bool showLoadingSkeleton = false,
}) => _showDetailSheet(
  context,
  icon: icon,
  title: (_) => displayName,
  titleMaxLines: 1,
  titleWeight: FontWeight.w600,
  body: (context, fillHeight) => ToolCallDetailsContent(
    toolName: toolName,
    detail: detail,
    errorText: errorText,
    fillAvailableHeight: true,
    fillHeight: fillHeight,
    showLoadingSkeleton: showLoadingSkeleton,
  ),
);

/// tool-calls/detail-level/overview/sheet.tsx: the same sheet titled with the
/// group summary (two lines, medium weight) listing the run's calls inside a
/// 13px / 8px inset. With [source] the summary and rows rebuild whenever it
/// notifies, and the sheet scrolls to the newest call on open and on change.
Future<void> showToolGroupSheet(
  BuildContext context, {
  required String Function(BuildContext context) summary,
  required WidgetBuilder body,
  Listenable? source,
}) => _showDetailSheet(
  context,
  icon: const ToolGlyph.icon(LucideIcons.wrench300),
  title: summary,
  titleMaxLines: 2,
  titleWeight: FontWeight.w500,
  source: source,
  pinToEnd: true,
  body: (context, _) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 13, vertical: 8),
    child: body(context),
  ),
);

Future<void> _showDetailSheet(
  BuildContext context, {
  required ToolGlyph icon,
  required String Function(BuildContext context) title,
  required int titleMaxLines,
  required FontWeight titleWeight,
  required Widget Function(BuildContext context, double fillHeight) body,
  Listenable? source,
  bool pinToEnd = false,
}) {
  final palette = PaseoColors.of(context);
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    backgroundColor: palette.surface2,
    barrierColor: const Color(0x80000000),
    shape: const RoundedRectangleBorder(
      borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
    ),
    clipBehavior: Clip.antiAlias,
    builder: (context) {
      final colors = Theme.of(context).colorScheme;
      Widget live(WidgetBuilder builder) => source == null
          ? Builder(builder: builder)
          : ListenableBuilder(listenable: source, builder: (context, _) => builder(context));
      // Gorhom lays the sheet out once for its largest (95%) snap point and
      // slides it: the handle and header stay fixed above the scrolling body,
      // and at the 60% snap the bottom of that layout is simply off screen.
      // So the body scrolls inside the 95% layout (sheet.tsx scrollToLatest
      // only moves when the run overflows that), never taking the header
      // with it. The body's fill height is the 95% size minus the handle
      // (24) and the header row (12 + 36 + 12, plus its 1px rule).
      Widget sheet(double fullHeight) => DraggableScrollableSheet(
        expand: false,
        initialChildSize: .6,
        minChildSize: .25,
        maxChildSize: .95,
        snap: true,
        snapSizes: const [.6],
        builder: (context, controller) => ClipRect(
          child: OverflowBox(
            alignment: Alignment.topCenter,
            minHeight: fullHeight,
            maxHeight: fullHeight,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                // Gorhom handle: 32x4 zinc-600 indicator with 10px insets.
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 10),
                  child: Center(
                    child: Container(
                      width: 32,
                      height: 4,
                      decoration: BoxDecoration(
                        color: const Color(0xff52525b),
                        borderRadius: BorderRadius.circular(2),
                      ),
                    ),
                  ),
                ),
                DecoratedBox(
                  decoration: BoxDecoration(
                    border: Border(bottom: BorderSide(color: colors.outline)),
                  ),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                    child: Row(
                      spacing: 8,
                      children: [
                        icon.build(20, colors.onSurface),
                        Expanded(
                          child: live(
                            (context) => Text(
                              title(context),
                              maxLines: titleMaxLines,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: PaseoTypography.of(context).uiSize(14),
                                fontWeight: titleWeight,
                                color: colors.onSurface,
                              ),
                            ),
                          ),
                        ),
                        Semantics(
                          button: true,
                          label: tr(context, 'common.actions.close'),
                          child: GestureDetector(
                            behavior: HitTestBehavior.opaque,
                            onTap: () => Navigator.pop(context),
                            child: Padding(
                              padding: const EdgeInsets.all(8),
                              child: Icon(LucideIcons.x300, size: 20, color: colors.onSurfaceVariant),
                            ),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
                Expanded(
                  child: _SheetScroll(
                    controller: controller,
                    pinToEnd: pinToEnd,
                    source: source,
                    slivers: [
                      SliverToBoxAdapter(
                        child: live(
                          (context) => body(context, fullHeight > 85 ? fullHeight - 85 : 0.0),
                        ),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      );
      return LayoutBuilder(
        builder: (context, constraints) => sheet(constraints.maxHeight * .95),
      );
    },
  );
}

/// The sheet's scroll view. With [pinToEnd] it jumps to the newest content
/// once opened and every time [source] notifies (sheet.tsx scrollToLatest).
class _SheetScroll extends StatefulWidget {
  const _SheetScroll({
    required this.controller,
    required this.slivers,
    required this.pinToEnd,
    this.source,
  });
  final ScrollController controller;
  final List<Widget> slivers;
  final bool pinToEnd;
  final Listenable? source;

  @override
  State<_SheetScroll> createState() => _SheetScrollState();
}

class _SheetScrollState extends State<_SheetScroll> {
  void _toEnd() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final controller = widget.controller;
      if (!mounted || !controller.hasClients) return;
      controller.jumpTo(controller.position.maxScrollExtent);
    });
  }

  @override
  void initState() {
    super.initState();
    if (widget.pinToEnd) {
      _toEnd();
      widget.source?.addListener(_toEnd);
    }
  }

  @override
  void dispose() {
    if (widget.pinToEnd) widget.source?.removeListener(_toEnd);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) =>
      CustomScrollView(controller: widget.controller, slivers: widget.slivers);
}

/// tool-call-details.tsx PaseoDetailSection: 16px inset, 12 gap, a bottom
/// border; a medium base title, then content-size prose (1.5 line height) or
/// 120px muted sm labels beside base values (12 apart, 16 gap).
class _PaseoSection extends StatelessWidget {
  const _PaseoSection({required this.section});
  final PaseoDetailSection section;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final typography = PaseoTypography.of(context);
    final base = typography.uiSize(14), small = typography.uiSize(12);
    final content = typography.contentFontSize;
    final lineHeight = (base * 1.5).roundToDouble();
    return DecoratedBox(
      decoration: BoxDecoration(border: Border(bottom: BorderSide(color: colors.outline))),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 12,
          children: [
            Text(
              section.title,
              style: TextStyle(fontSize: base, fontWeight: FontWeight.w500, color: colors.onSurface),
            ),
            if (section.text != null)
              Text(
                section.text!,
                style: TextStyle(
                  fontSize: content,
                  height: (content * 1.5).roundToDouble() / content,
                  color: colors.onSurface,
                ),
              )
            else
              Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 12,
                children: [
                  for (final field in section.fields)
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      spacing: 16,
                      children: [
                        SizedBox(
                          width: 120,
                          child: Text(
                            field.label,
                            style: TextStyle(
                              fontSize: small,
                              height: lineHeight / small,
                              color: colors.onSurfaceVariant,
                            ),
                          ),
                        ),
                        Expanded(
                          child: Text(
                            field.value,
                            style: TextStyle(
                              fontSize: base,
                              height: lineHeight / base,
                              color: colors.onSurface,
                            ),
                          ),
                        ),
                      ],
                    ),
                ],
              ),
          ],
        ),
      ),
    );
  }
}
