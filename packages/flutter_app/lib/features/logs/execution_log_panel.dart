import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show ScrollDirection;
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:xterm/core.dart' show CellAttr, CellColor;

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'execution_log_session.dart';

/// Read-only output for an existing setup/script/attention execution. The target
/// must carry its daemon terminal id; opening this panel never starts a process.
///
/// Presented as the original `TerminalPane`: a surface0 output surface in the
/// code font with xterm's 1.0 line height and the terminal ANSI palette, the
/// original `PaneFind` widget floating at the top-right, the attach spinner
/// overlay, and the stream error row under the output. The read-only controls
/// (follow, find, copy) sit in an original `PaneContentToolbar`.
class ExecutionLogPanel extends StatefulWidget {
  const ExecutionLogPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<ExecutionLogPanel> createState() => _ExecutionLogPanelState();
}

class _ExecutionLogPanelState extends State<ExecutionLogPanel> {
  ExecutionLogSession? _session;
  Object? _error;
  final _vertical = ScrollController();
  final _horizontal = ScrollController();
  final _query = TextEditingController();
  final _findFocus = FocusNode(debugLabel: 'Find execution output');
  bool _visible = true;
  bool _follow = true;
  bool _followQueued = false;
  bool _showFind = false;
  bool _initialized = false;
  Timer? _searchDelay;
  List<_LogMatch> _matches = [];
  int _matchIndex = -1;
  int _searchedRevision = -1;
  String _fontFamily = 'monospace';
  List<String> _fontFallback = const [];
  double _fontSize = 12;
  double _rowHeight = 14;
  double _cellWidth = 8;
  TextScaler? _measuredScaler;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final visible = TickerMode.valuesOf(context).enabled;
    if (visible != _visible) {
      _visible = visible;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) _session?.setVisible(_visible);
      });
    }
    // Original terminal: the user's code font at settings.codeFontSize.
    final type = PaseoTypography.of(context);
    final font = type.monoFontFamily.isEmpty ? 'monospace' : type.monoFontFamily;
    final size = type.codeFontSize.clamp(8, 36).toDouble();
    final scaler = MediaQuery.textScalerOf(context);
    if (!_initialized ||
        font != _fontFamily ||
        size != _fontSize ||
        scaler != _measuredScaler) {
      _fontFamily = font;
      _fontFallback = type.monoFontFamilyFallback;
      _fontSize = size;
      _measuredScaler = scaler;
      final measure = TextPainter(
        text: TextSpan(text: 'M', style: _textStyle),
        textDirection: TextDirection.ltr,
        textScaler: scaler,
      )..layout();
      _cellWidth = measure.width;
      // xterm `lineHeight: 1.0`: rows are exactly one cell tall.
      _rowHeight = measure.height.ceilToDouble();
      measure.dispose();
    }
    if (!_initialized) {
      _initialized = true;
      _openSession();
    }
  }

  TextStyle get _textStyle => TextStyle(
    fontFamily: _fontFamily,
    fontFamilyFallback: _fontFallback,
    fontSize: _fontSize,
    height: 1.0,
    letterSpacing: 0,
  );

  @override
  void didUpdateWidget(ExecutionLogPanel oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.target.key != widget.target.key) _openSession();
  }

  void _openSession() {
    _session?.removeListener(_changed);
    _session?.dispose();
    _session = null;
    _matches = [];
    _matchIndex = -1;
    _searchedRevision = -1;
    _error = null;
    final id = widget.target.id;
    if (id == null || id.isEmpty) {
      _error = StateError(
        'Execution output requires an existing process identifier',
      );
      return;
    }
    final store = PaseoScope.of(context);
    final session = ExecutionLogSession(
      client: store.clientFor(widget.target.serverId),
      terminalId: id,
      scrollback: intValue(
        store.preferences['executionLogLines'],
        10000,
      ).clamp(100, 100000).toInt(),
    );
    _session = session;
    session.addListener(_changed);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted && identical(session, _session)) session.setVisible(_visible);
    });
  }

  void _changed() {
    if (!mounted) return;
    setState(() {});
    if (_follow) _queueFollow();
  }

  void _queueFollow() {
    if (_followQueued || !_visible) return;
    _followQueued = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _followQueued = false;
      if (mounted && _visible && _follow && _vertical.hasClients) {
        _vertical.jumpTo(_vertical.position.maxScrollExtent);
      }
    });
  }

  /// Original find shortcut: opens Find, or refocuses it and selects the query.
  void _openFind() {
    setState(() => _showFind = true);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _findFocus.requestFocus();
      _query.selection = TextSelection(
        baseOffset: 0,
        extentOffset: _query.text.length,
      );
    });
  }

  void _toggleFind() {
    if (_showFind) {
      _closeFind();
    } else {
      _openFind();
    }
  }

  void _closeFind() {
    _searchDelay?.cancel();
    setState(() => _showFind = false);
  }

  void _queryChanged(String value) {
    _searchDelay?.cancel();
    _searchDelay = Timer(
      const Duration(milliseconds: 180),
      () => _find(reset: true),
    );
  }

  void _find({bool previous = false, bool reset = false}) {
    _searchDelay?.cancel();
    final session = _session;
    if (session == null || !mounted) return;
    final query = _query.text;
    if (query.isEmpty) {
      setState(() {
        _matches = [];
        _matchIndex = -1;
        _searchedRevision = session.revision;
      });
      return;
    }
    // Search logical lines, not just display rows: soft-wrapped snapshots can
    // split a match at the PTY width. Offsets remain UTF-16 for Flutter spans.
    final text = StringBuffer();
    final offsets = <({int row, int offset})>[];
    for (var row = 0; row < session.lineCount; row++) {
      final line = session.terminal.buffer.lines[row];
      if (row > 0 && !line.isWrapped) {
        text.write('\n');
        offsets.add((row: row, offset: 0));
      }
      final value = session.lineAt(row);
      text.write(value);
      for (var i = 0; i < value.length; i++) {
        offsets.add((row: row, offset: i));
      }
    }
    // Original xterm search is case-insensitive. Fold only when folding keeps
    // UTF-16 offsets aligned with the presented text.
    var source = text.toString();
    var needle = query;
    final folded = source.toLowerCase();
    final foldedNeedle = query.toLowerCase();
    if (folded.length == source.length &&
        foldedNeedle.length == query.length) {
      source = folded;
      needle = foldedNeedle;
    }
    final matches = <_LogMatch>[];
    var from = 0;
    while (from < source.length) {
      final found = source.indexOf(needle, from);
      if (found < 0) break;
      final start = offsets[found];
      final end = offsets[found + needle.length - 1];
      matches.add(
        _LogMatch(
          startRow: start.row,
          startOffset: start.offset,
          endRow: end.row,
          endOffset: end.offset + 1,
        ),
      );
      from = found + needle.length;
    }
    setState(() {
      _matches = matches;
      _searchedRevision = session.revision;
      _matchIndex = matches.isEmpty
          ? -1
          : reset
          ? (previous ? matches.length - 1 : 0)
          : (_matchIndex + (previous ? -1 : 1)) % matches.length;
      if (_matchIndex >= 0) _follow = false;
    });
    if (_matchIndex >= 0 && _vertical.hasClients) {
      final match = matches[_matchIndex];
      _vertical.jumpTo(
        (match.startRow * _rowHeight)
            .clamp(
              _vertical.position.minScrollExtent,
              _vertical.position.maxScrollExtent,
            )
            .toDouble(),
      );
      if (_horizontal.hasClients) {
        _horizontal.jumpTo(
          (match.startOffset * _cellWidth)
              .clamp(
                _horizontal.position.minScrollExtent,
                _horizontal.position.maxScrollExtent,
              )
              .toDouble(),
        );
      }
    }
  }

  Future<void> _copyOutput() async {
    final session = _session;
    if (session == null) return;
    try {
      await Clipboard.setData(ClipboardData(text: session.renderedText));
      if (mounted) showCopiedToast(context);
    } catch (failure) {
      if (mounted) showError(context, failure);
    }
  }

  /// Highlight ranges of [row] in presented-text offsets: every match, and the
  /// active one separately (original xterm search: yellow matches, blue active).
  List<({int start, int end, bool active})> _rowHighlights(
    int row,
    int length,
  ) {
    if (_searchedRevision != _session?.revision || _matches.isEmpty) {
      return const [];
    }
    // Matches are ordered by start row; find the first one that may reach [row].
    var low = 0, high = _matches.length;
    while (low < high) {
      final mid = (low + high) >> 1;
      if (_matches[mid].endRow < row) {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    final ranges = <({int start, int end, bool active})>[];
    for (var i = low; i < _matches.length; i++) {
      final match = _matches[i];
      if (match.startRow > row) break;
      if (match.endRow < row) continue;
      final start = (row == match.startRow ? match.startOffset : 0)
          .clamp(0, length)
          .toInt();
      final end = (row == match.endRow ? match.endOffset : length)
          .clamp(start, length)
          .toInt();
      if (end > start) {
        ranges.add((start: start, end: end, active: i == _matchIndex));
      }
    }
    return ranges;
  }

  InlineSpan _lineSpan(int row, _TerminalPalette palette) {
    final runs = _session!.lineRuns(row);
    final length = runs.fold<int>(0, (sum, run) => sum + run.text.length);
    final ranges = _rowHighlights(row, length);
    final children = <InlineSpan>[];
    var offset = 0;
    for (final run in runs) {
      final style = palette.style(run);
      final runStart = offset, runEnd = offset + run.text.length;
      var cursor = 0;
      for (final range in ranges) {
        if (range.end <= runStart || range.start >= runEnd) continue;
        final start = math.max(range.start, runStart) - runStart;
        final end = math.min(range.end, runEnd) - runStart;
        if (start > cursor) {
          children.add(
            TextSpan(text: run.text.substring(cursor, start), style: style),
          );
        }
        children.add(
          TextSpan(
            text: run.text.substring(start, end),
            style: style.copyWith(
              backgroundColor: range.active
                  ? palette.activeMatch
                  : palette.match,
            ),
          ),
        );
        cursor = end;
      }
      if (cursor < run.text.length) {
        children.add(TextSpan(text: run.text.substring(cursor), style: style));
      }
      offset = runEnd;
    }
    return TextSpan(children: children);
  }

  String _findStatus() {
    if (_query.text.isEmpty || _searchedRevision < 0) return '';
    if (_searchedRevision != _session?.revision) {
      return 'Output changed — search again';
    }
    if (_matches.isEmpty) return tr(context, 'paneFind.noMatches');
    if (_matchIndex < 0) {
      return tr(context, 'paneFind.total', args: {'total': _matches.length});
    }
    return tr(
      context,
      'paneFind.position',
      args: {'current': _matchIndex + 1, 'total': _matches.length},
    );
  }

  Widget _toolbar(BuildContext context, ExecutionLogSession? session) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final frame = compact ? 32.0 : 20.0;
    final glyph = compact ? 18.0 : 14.0;
    final small = theme.textTheme.bodySmall!.copyWith(
      fontSize: type.uiSize(12),
    );
    final title =
        widget.target.title ??
        (session?.title.isNotEmpty == true ? session!.title : 'Execution output');
    final status = session == null
        ? null
        : session.exited
        ? 'Execution finished'
        : session.connected
        ? 'Live output · read-only'
        : 'Waiting for host · cached output';
    // Original `PaneContentToolbar`: 36px surface0 band with a bottom border; the
    // trailing glyph's hitbox overhangs the 8px rail.
    return Container(
      height: 36,
      decoration: BoxDecoration(
        color: colors.surface0,
        border: Border(bottom: BorderSide(color: theme.dividerColor)),
      ),
      padding: EdgeInsets.only(left: 12, right: 8 - (frame - glyph) / 2),
      child: Row(
        spacing: 12,
        children: [
          Expanded(
            child: Row(
              spacing: 8,
              children: [
                Flexible(
                  child: Text(
                    title,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: small.copyWith(color: colors.muted),
                  ),
                ),
                if (status != null)
                  Flexible(
                    child: Text(
                      status,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: small.copyWith(color: colors.extraMuted),
                    ),
                  ),
                if (session != null && !compact)
                  Text(
                    '${session.lineCount} lines',
                    maxLines: 1,
                    style: small.copyWith(color: colors.extraMuted),
                  ),
              ],
            ),
          ),
          Row(
            spacing: 1,
            children: [
              _ToolbarButton(
                label: _follow ? 'Pause following output' : 'Follow output',
                icon: LucideIcons.arrowDownToLine300,
                selected: _follow,
                onPressed: () {
                  final value = !_follow;
                  setState(() => _follow = value);
                  if (value) _queueFollow();
                },
              ),
              _ToolbarButton(
                label: tr(context, 'paneFind.title'),
                icon: LucideIcons.search300,
                selected: _showFind,
                onPressed: _toggleFind,
              ),
              _ToolbarButton(
                label: 'Copy all output',
                icon: LucideIcons.copy300,
                onPressed: session == null ? null : _copyOutput,
              ),
            ],
          ),
        ],
      ),
    );
  }

  Widget _output(BuildContext context, ExecutionLogSession? session) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    if (session == null) return const SizedBox.shrink();
    if (session.lineCount == 0) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Text(
            session.exited
                ? 'Execution finished without output'
                : 'No output yet',
            textAlign: TextAlign.center,
            style: theme.textTheme.bodyMedium!.copyWith(
              fontSize: type.uiSize(14),
              color: colors.muted,
            ),
          ),
        ),
      );
    }
    final palette = _TerminalPalette.of(context, _textStyle);
    return LayoutBuilder(
      builder: (context, constraints) {
        final width = math.max(
          constraints.maxWidth,
          session.terminal.viewWidth * _cellWidth,
        );
        return Scrollbar(
          controller: _horizontal,
          notificationPredicate: (notification) =>
              notification.metrics.axis == Axis.horizontal,
          child: SingleChildScrollView(
            controller: _horizontal,
            scrollDirection: Axis.horizontal,
            child: SizedBox(
              width: width,
              height: constraints.maxHeight,
              child: DefaultSelectionStyle(
                selectionColor: palette.selection,
                child: SelectionArea(
                  child: NotificationListener<UserScrollNotification>(
                    onNotification: (notification) {
                      if (notification.metrics.axis == Axis.vertical &&
                          notification.direction != ScrollDirection.idle &&
                          _follow) {
                        setState(() => _follow = false);
                      }
                      return false;
                    },
                    child: Scrollbar(
                      controller: _vertical,
                      child: ListView.builder(
                        controller: _vertical,
                        itemExtent: _rowHeight,
                        itemCount: session.lineCount,
                        itemBuilder: (context, index) => Align(
                          alignment: Alignment.centerLeft,
                          child: Text.rich(
                            _lineSpan(index, palette),
                            style: palette.base,
                            textDirection: TextDirection.ltr,
                            softWrap: false,
                            maxLines: 1,
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    if (!_visible) return const SizedBox.shrink();
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final session = _session;
    final failure = _error ?? session?.error;
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.keyF, control: true):
            _openFind,
        const SingleActivator(LogicalKeyboardKey.keyF, meta: true): _openFind,
      },
      child: Focus(
        autofocus: true,
        child: ColoredBox(
          color: colors.surface0,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _toolbar(context, session),
              Expanded(
                child: Stack(
                  children: [
                    Positioned.fill(child: _output(context, session)),
                    if (_showFind)
                      Positioned(
                        top: 0,
                        left: 0,
                        right: 0,
                        child: Align(
                          alignment: Alignment.topRight,
                          child: Padding(
                            padding: const EdgeInsets.all(8),
                            child: _PaneFind(
                              controller: _query,
                              focusNode: _findFocus,
                              status: _findStatus(),
                              canNavigate: _matches.isNotEmpty,
                              onChanged: _queryChanged,
                              onNext: () => _find(),
                              onPrevious: () => _find(previous: true),
                              onClose: _closeFind,
                            ),
                          ),
                        ),
                      ),
                    // Original attach overlay: a dimmed surface with a small
                    // muted spinner, never blocking selection underneath.
                    if (session?.attaching == true)
                      Positioned.fill(
                        child: IgnorePointer(
                          child: ColoredBox(
                            color: const Color(0x29000000),
                            child: Center(
                              child: SizedBox.square(
                                dimension: 20,
                                child: CircularProgressIndicator(
                                  strokeWidth: 2,
                                  color: colors.muted,
                                ),
                              ),
                            ),
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              // Original terminal error row: surface1 band under the output with
              // a top border and two lines of destructive small text.
              if (failure != null)
                Container(
                  decoration: BoxDecoration(
                    color: colors.surface1,
                    border: Border(top: BorderSide(color: theme.dividerColor)),
                  ),
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 4,
                  ),
                  child: Row(
                    spacing: 8,
                    children: [
                      Expanded(
                        child: Text(
                          errorMessage(failure),
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall!.copyWith(
                            fontSize: type.uiSize(12),
                            color: theme.colorScheme.error,
                          ),
                        ),
                      ),
                      if (session != null)
                        PaseoButton(
                          label: tr(context, 'common.actions.retry'),
                          variant: PaseoButtonVariant.ghost,
                          size: PaseoControlSize.xs,
                          onPressed: session.retry,
                        )
                      else
                        PaseoButton(
                          label: tr(context, 'common.actions.close'),
                          variant: PaseoButtonVariant.ghost,
                          size: PaseoControlSize.xs,
                          onPressed: () => PaseoScope.of(
                            context,
                          ).closeTab(widget.target.key),
                        ),
                    ],
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  void dispose() {
    _searchDelay?.cancel();
    _session?.removeListener(_changed);
    _session?.dispose();
    _vertical.dispose();
    _horizontal.dispose();
    _query.dispose();
    _findFocus.dispose();
    super.dispose();
  }
}

class _LogMatch {
  const _LogMatch({
    required this.startRow,
    required this.startOffset,
    required this.endRow,
    required this.endOffset,
  });
  final int startRow, startOffset, endRow, endOffset;
}

/// Original theme `terminal` colours: surface0 background, foreground text, the
/// light/dark ANSI tables (black/bright black follow the palette's sidebar and
/// surface3), and the xterm selection fill.
class _TerminalPalette {
  _TerminalPalette._(this.base, this.ansi, this.foreground, this.background,
      this.selection, this.match, this.activeMatch);

  factory _TerminalPalette.of(BuildContext context, TextStyle text) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final dark = theme.brightness == Brightness.dark;
    final foreground = theme.colorScheme.onSurface;
    final ansi = dark
        ? <Color>[
            colors.sidebar,
            const Color(0xffe07070),
            const Color(0xff5dba80),
            const Color(0xffd4a44a),
            const Color(0xff6a9de0),
            const Color(0xffb07ad0),
            const Color(0xff4aabb8),
            const Color(0xffd4d4d8),
            colors.surface3,
            const Color(0xffe89090),
            const Color(0xff7ecf9a),
            const Color(0xffe0be6e),
            const Color(0xff8ab4e8),
            const Color(0xffc49ae0),
            const Color(0xff6ec2cc),
            const Color(0xfff0f0f2),
          ]
        : const <Color>[
            Color(0xff1a1a1e),
            Color(0xffdc2626),
            Color(0xff16a34a),
            Color(0xffca8a04),
            Color(0xff2563eb),
            Color(0xff9333ea),
            Color(0xff0891b2),
            Color(0xffffffff),
            Color(0xff3f3f46),
            Color(0xffef4444),
            Color(0xff22c55e),
            Color(0xfff59e0b),
            Color(0xff3b82f6),
            Color(0xffa855f7),
            Color(0xff06b6d4),
            Color(0xfffafafa),
          ];
    return _TerminalPalette._(
      text.copyWith(color: foreground),
      ansi,
      foreground,
      colors.surface0,
      dark ? const Color(0x33ffffff) : const Color(0x26000000),
      ansi[3].withValues(alpha: .35),
      ansi[4].withValues(alpha: .45),
    );
  }

  final TextStyle base;
  final List<Color> ansi;
  final Color foreground, background, selection, match, activeMatch;

  Color? _color(int value, {bool bold = false}) {
    final index = value & CellColor.valueMask;
    switch (value & CellColor.typeMask) {
      case CellColor.named:
        // xterm.js `drawBoldTextInBrightColors`: bold normal colours brighten.
        return ansi[(bold && index < 8 ? index + 8 : index) & 15];
      case CellColor.palette:
        if (index < 16) return ansi[index];
        if (index < 232) {
          const steps = [0, 95, 135, 175, 215, 255];
          final cube = index - 16;
          return Color.fromARGB(
            255,
            steps[cube ~/ 36],
            steps[(cube ~/ 6) % 6],
            steps[cube % 6],
          );
        }
        final gray = 8 + (index - 232) * 10;
        return Color.fromARGB(255, gray, gray, gray);
      case CellColor.rgb:
        return Color(0xff000000 | index);
      default:
        return null;
    }
  }

  TextStyle style(ExecutionLogRun run) {
    final attributes = run.attributes;
    if (run.foreground == 0 && run.background == 0 && attributes == 0) {
      return base;
    }
    final bold = attributes & CellAttr.bold != 0;
    var fg = _color(run.foreground, bold: bold) ?? foreground;
    var bg = _color(run.background);
    if (attributes & CellAttr.inverse != 0) {
      final swapped = bg ?? background;
      bg = fg;
      fg = swapped;
    }
    if (attributes & CellAttr.faint != 0) fg = fg.withValues(alpha: fg.a * .5);
    if (attributes & CellAttr.invisible != 0) fg = fg.withValues(alpha: 0);
    final decorations = [
      if (attributes & CellAttr.underline != 0) TextDecoration.underline,
      if (attributes & CellAttr.strikethrough != 0) TextDecoration.lineThrough,
    ];
    return base.copyWith(
      color: fg,
      backgroundColor: bg,
      fontWeight: bold ? FontWeight.bold : null,
      fontStyle: attributes & CellAttr.italic != 0 ? FontStyle.italic : null,
      decoration: decorations.isEmpty
          ? null
          : TextDecoration.combine(decorations),
      decorationColor: fg,
    );
  }
}

/// Original `ToolbarButton` (icon-button chrome, size small): a 20px (32px
/// compact) frame with a 14px (18px compact) muted glyph, 6px radius, the
/// interaction highlight on hover/press/selected, half opacity when disabled.
class _ToolbarButton extends StatefulWidget {
  const _ToolbarButton({
    required this.label,
    required this.icon,
    required this.onPressed,
    this.selected = false,
  });
  final String label;
  final IconData icon;
  final VoidCallback? onPressed;
  final bool selected;
  @override
  State<_ToolbarButton> createState() => _ToolbarButtonState();
}

class _ToolbarButtonState extends State<_ToolbarButton> {
  bool _hovered = false, _pressed = false;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final dark = Theme.of(context).brightness == Brightness.dark;
    final enabled = widget.onPressed != null;
    final highlighted =
        enabled && (_hovered || _pressed || widget.selected);
    final button = Semantics(
      button: true,
      enabled: enabled,
      selected: widget.selected,
      label: widget.label,
      child: MouseRegion(
        cursor: enabled ? SystemMouseCursors.click : MouseCursor.defer,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: enabled ? (_) => setState(() => _pressed = true) : null,
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: widget.onPressed,
          child: Opacity(
            opacity: enabled ? 1 : .5,
            child: Container(
              width: compact ? 32 : 20,
              height: compact ? 32 : 20,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                color: highlighted
                    ? (dark
                          ? const Color(0x14ffffff)
                          : const Color(0x0f000000))
                    : null,
                borderRadius: BorderRadius.circular(6),
              ),
              child: ExcludeSemantics(
                child: Icon(
                  widget.icon,
                  size: compact ? 18 : 14,
                  color: colors.muted,
                ),
              ),
            ),
          ),
        ),
      ),
    );
    if (compact) return button;
    return Tooltip(message: widget.label, child: button);
  }
}

/// Original `PaneFind`: a 340px surface1 card (6px padding, 4px gap, border,
/// 8px radius, md shadow) holding the surface2 query field with the match count
/// inside it, then previous/next/close toolbar buttons. Enter finds the next
/// match, Shift+Enter the previous one, Escape closes.
class _PaneFind extends StatefulWidget {
  const _PaneFind({
    required this.controller,
    required this.focusNode,
    required this.status,
    required this.canNavigate,
    required this.onChanged,
    required this.onNext,
    required this.onPrevious,
    required this.onClose,
  });
  final TextEditingController controller;
  final FocusNode focusNode;
  final String status;
  final bool canNavigate;
  final ValueChanged<String> onChanged;
  final VoidCallback onNext, onPrevious, onClose;
  @override
  State<_PaneFind> createState() => _PaneFindState();
}

class _PaneFindState extends State<_PaneFind> {
  bool _hovered = false;

  @override
  void initState() {
    super.initState();
    widget.focusNode.addListener(_focusChanged);
  }

  @override
  void didUpdateWidget(_PaneFind oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.focusNode != widget.focusNode) {
      oldWidget.focusNode.removeListener(_focusChanged);
      widget.focusNode.addListener(_focusChanged);
    }
  }

  void _focusChanged() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    widget.focusNode.removeListener(_focusChanged);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final dark = theme.brightness == Brightness.dark;
    final rowHeight = compact ? 32.0 : 28.0;
    final active = widget.focusNode.hasFocus || _hovered;
    return Semantics(
      container: true,
      label: tr(context, 'paneFind.title'),
      child: Container(
        width: 340,
        padding: const EdgeInsets.all(6),
        decoration: BoxDecoration(
          color: colors.surface1,
          border: Border.all(color: theme.dividerColor),
          borderRadius: BorderRadius.circular(8),
          boxShadow: [
            dark
                ? const BoxShadow(
                    color: Color(0x33000000),
                    offset: Offset(0, 4),
                    blurRadius: 8,
                  )
                : const BoxShadow(
                    color: Color(0x0a000000),
                    offset: Offset(0, 4),
                    blurRadius: 16,
                  ),
          ],
        ),
        child: Row(
          spacing: 4,
          children: [
            Expanded(
              child: MouseRegion(
                onEnter: (_) => setState(() => _hovered = true),
                onExit: (_) => setState(() => _hovered = false),
                child: Container(
                  constraints: BoxConstraints(minHeight: rowHeight),
                  padding: const EdgeInsets.symmetric(horizontal: 8),
                  decoration: BoxDecoration(
                    color: colors.surface2,
                    borderRadius: BorderRadius.circular(6),
                    border: Border.all(
                      color: active ? colors.borderAccent : Colors.transparent,
                    ),
                  ),
                  child: Row(
                    spacing: 8,
                    children: [
                      Expanded(
                        child: CallbackShortcuts(
                          bindings: {
                            const SingleActivator(LogicalKeyboardKey.enter):
                                widget.onNext,
                            const SingleActivator(
                              LogicalKeyboardKey.enter,
                              shift: true,
                            ): widget.onPrevious,
                            const SingleActivator(LogicalKeyboardKey.escape):
                                widget.onClose,
                          },
                          child: TextField(
                            controller: widget.controller,
                            focusNode: widget.focusNode,
                            autocorrect: false,
                            enableSuggestions: false,
                            textInputAction: TextInputAction.search,
                            style: theme.textTheme.bodyMedium!.copyWith(
                              fontSize: type.uiSize(14),
                              height: 20 / 14,
                            ),
                            decoration: InputDecoration.collapsed(
                              hintText: tr(context, 'paneFind.placeholder'),
                              hintStyle: TextStyle(color: colors.muted),
                            ),
                            onChanged: widget.onChanged,
                            onSubmitted: (_) {
                              widget.onNext();
                              widget.focusNode.requestFocus();
                            },
                          ),
                        ),
                      ),
                      if (widget.status.isNotEmpty)
                        Semantics(
                          liveRegion: true,
                          label: tr(context, 'paneFind.matches'),
                          child: Text(
                            widget.status,
                            style: theme.textTheme.bodySmall!.copyWith(
                              fontSize: type.uiSize(12),
                              color: colors.muted,
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
              ),
            ),
            _ToolbarButton(
              label: tr(context, 'paneFind.previous'),
              icon: LucideIcons.arrowUp300,
              onPressed: widget.canNavigate ? widget.onPrevious : null,
            ),
            _ToolbarButton(
              label: tr(context, 'paneFind.next'),
              icon: LucideIcons.arrowDown300,
              onPressed: widget.canNavigate ? widget.onNext : null,
            ),
            _ToolbarButton(
              label: tr(context, 'paneFind.close'),
              icon: LucideIcons.x300,
              onPressed: widget.onClose,
            ),
          ],
        ),
      ),
    );
  }
}
