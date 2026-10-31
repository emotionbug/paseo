import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:highlight/highlight.dart' as syntax;
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../files/pane_chrome.dart';
import '../files/source_controller.dart';
import '../files/syntax_palette.dart';
import 'git_controller.dart';

class DiffLine {
  const DiffLine(
    this.type,
    this.content,
    this.oldNumber,
    this.newNumber,
    this.hunk,
  );
  final String type, content, hunk;
  final int? oldNumber, newNumber;
}

List<DiffLine> numberDiff(JsonMap file) {
  final result = <DiffLine>[];
  for (final hunk in file.array('hunks').map(jsonMap)) {
    var old = hunk.integer('oldStart');
    var next = hunk.integer('newStart');
    final header =
        '@@ -$old,${hunk.integer('oldCount')} +$next,${hunk.integer('newCount')} @@';
    result.add(DiffLine('header', header, null, null, header));
    for (final line in hunk.array('lines').map(jsonMap)) {
      final type = line.text('type');
      if (type == 'header') continue;
      result.add(
        DiffLine(
          type,
          line.text('content'),
          type == 'add' ? null : old++,
          type == 'remove' ? null : next++,
          header,
        ),
      );
    }
  }
  return result;
}

/// The original single ordering authority: directories before files within a
/// level, then plain code-unit order (never locale-dependent).
int compareDiffPaths(String a, String b) {
  final left = a.split('/'), right = b.split('/');
  for (var i = 0; ; i++) {
    final leftFile = i == left.length - 1, rightFile = i == right.length - 1;
    if (leftFile != rightFile) return leftFile ? 1 : -1;
    final order = left[i].compareTo(right[i]);
    if (order != 0) return order;
    if (leftFile) return 0;
  }
}

String diffPatchText(JsonMap patch) => numberDiff(patch)
    .map(
      (l) =>
          '${switch (l.type) {
            'add' => '+',
            'remove' => '-',
            'context' => ' ',
            _ => '',
          }}${l.content}',
    )
    .join('\n');

typedef DiffCommentHandler =
    void Function(String path, DiffLine line, String side, List<DiffLine> context);

typedef _Run = (String, String?);

class _DiffRow {
  const _DiffRow({
    this.left,
    this.right,
    this.line,
    this.hiddenStart,
    this.hiddenEnd,
  });
  final DiffLine? left, right, line;
  final int? hiddenStart, hiddenEnd;
}

/// Per-patch projection retained across rebuilds: numbered lines, syntax runs,
/// collapsed context, and unified/split rows.
class _FileModel {
  _FileModel(this.patch) : lines = numberDiff(patch) {
    for (final line in lines) {
      maxLine = math.max(maxLine, math.max(line.oldNumber ?? 0, line.newNumber ?? 0));
      maxChars = math.max(maxChars, line.content.length);
    }
  }
  static const _highlightLineLimit = 8000;
  static const _highlightWidthLimit = 2000;

  final JsonMap patch;
  final List<DiffLine> lines;
  int maxLine = 0, maxChars = 0;
  final Set<int> expanded = {};
  List<_DiffRow>? _rows;
  bool _rowsSplit = false;
  Map<DiffLine, List<_Run>>? _runs;

  void invalidate() => _rows = null;

  List<_DiffRow> rows(bool split) {
    if (_rows != null && _rowsSplit == split) return _rows!;
    _rowsSplit = split;
    _DiffRow row(DiffLine l) => !split || l.type == 'header'
        ? _DiffRow(line: l)
        : _DiffRow(
            left: l.type == 'add' ? null : l,
            right: l.type == 'remove' ? null : l,
          );
    final rows = <_DiffRow>[];
    for (var i = 0; i < lines.length;) {
      final line = lines[i];
      if (line.type == 'context') {
        var end = i;
        while (end < lines.length && lines[end].type == 'context') {
          end++;
        }
        if (end - i > 8 && !expanded.contains(i)) {
          for (var n = i; n < i + 3; n++) {
            rows.add(row(lines[n]));
          }
          rows.add(_DiffRow(hiddenStart: i, hiddenEnd: end));
          for (var n = end - 3; n < end; n++) {
            rows.add(row(lines[n]));
          }
        } else {
          for (var n = i; n < end; n++) {
            rows.add(row(lines[n]));
          }
        }
        i = end;
      } else if (split && line.type == 'remove') {
        var end = i;
        while (end < lines.length && lines[end].type == 'remove') {
          end++;
        }
        var addedEnd = end;
        while (addedEnd < lines.length && lines[addedEnd].type == 'add') {
          addedEnd++;
        }
        for (var n = 0; n < math.max(end - i, addedEnd - end); n++) {
          rows.add(
            _DiffRow(
              left: n < end - i ? lines[i + n] : null,
              right: n < addedEnd - end ? lines[end + n] : null,
            ),
          );
        }
        i = addedEnd;
      } else {
        rows.add(row(line));
        i++;
      }
    }
    return _rows = rows;
  }

  List<DiffLine> contextFor(DiffLine line) {
    final index = lines.indexOf(line);
    return lines
        .sublist(math.max(0, index - 3), math.min(lines.length, index + 4))
        .where((l) => l.type != 'header' && l.hunk == line.hunk)
        .toList();
  }

  /// Highlights each hunk's old and new sides as whole sequences so multi-line
  /// comments and strings keep their state, then splits the runs back per line.
  List<_Run>? runs(DiffLine line, String path) {
    final runs = _runs ??= _highlight(path);
    return runs[line];
  }

  Map<DiffLine, List<_Run>> _highlight(String path) {
    final result = <DiffLine, List<_Run>>{};
    if (lines.length > _highlightLineLimit || maxChars > _highlightWidthLimit) return result;
    final language = SourceController.languageFor(path);
    final hunks = <String, (List<DiffLine>, List<DiffLine>)>{};
    for (final line in lines) {
      if (line.type == 'header') continue;
      final (old, next) = hunks.putIfAbsent(line.hunk, () => ([], []));
      if (line.type != 'add') old.add(line);
      if (line.type != 'remove') next.add(line);
    }
    try {
      for (final (old, next) in hunks.values) {
        for (final (side, isNew) in [(next, true), (old, false)]) {
          if (side.isEmpty) continue;
          final split = _split(side.map((line) => line.content).join('\n'), language);
          for (var i = 0; i < side.length && i < split.length; i++) {
            // Context lines take their runs from the new side only.
            if (!isNew && side[i].type == 'context') continue;
            result[side[i]] = split[i];
          }
        }
      }
    } catch (_) {
      // Unknown or unsupported language: lines render in the plain foreground.
      result.clear();
    }
    return result;
  }

  static List<List<_Run>> _split(String text, String language) {
    final lines = <List<_Run>>[[]];
    void visit(syntax.Node node, String? inherited) {
      final token = node.className ?? inherited;
      final value = node.value;
      if (value != null) {
        final parts = value.split('\n');
        for (var i = 0; i < parts.length; i++) {
          if (i > 0) lines.add([]);
          if (parts[i].isNotEmpty) lines.last.add((parts[i], token));
        }
      }
      for (final child in node.children ?? const <syntax.Node>[]) {
        visit(child, token);
      }
    }

    for (final node in syntax.highlight.parse(text, language: language).nodes ?? const <syntax.Node>[]) {
      visit(node, null);
    }
    return lines;
  }
}

/// The original Changes document: every changed file in one scroll view with a
/// sticky per-file header. Headers come from the summary; hunks load only when a
/// file's body scrolls near the viewport, and collapsed files never load.
class DiffDocument extends StatefulWidget {
  const DiffDocument({
    super.key,
    required this.git,
    required this.collapsed,
    required this.onToggleCollapsed,
    required this.onComment,
    required this.onOpen,
    this.onEditComment,
    this.onDeleteComment,
    this.comments = const {},
    this.split = false,
    this.wrap = false,
    this.bottomInset = 0,
    this.onFileMenu,
  });
  final GitController git;
  final Set<String> collapsed;
  final ValueChanged<String> onToggleCollapsed;
  final DiffCommentHandler onComment;
  final void Function(String path, int? line) onOpen;
  final ValueChanged<JsonMap>? onEditComment, onDeleteComment;

  /// Original FileHeader `FileActionsContextMenu`: the panel's file actions for
  /// a document header (the same set as its tree rows).
  final void Function(JsonMap file, Rect anchor)? onFileMenu;

  /// path → 'side:line' → private review draft comments.
  final Map<String, Map<String, List<JsonMap>>> comments;
  final bool split, wrap;

  /// Scroll clearance kept under the last file for floating actions.
  final double bottomInset;

  @override
  State<DiffDocument> createState() => DiffDocumentState();
}

class DiffDocumentState extends State<DiffDocument> {
  static const _headerHeight = 30.0;
  final _vertical = ScrollController();
  final _horizontal = ScrollController();
  final Map<String, GlobalKey> _keys = {};
  final Map<String, _FileModel> _models = {};

  @override
  void dispose() {
    _vertical.dispose();
    _horizontal.dispose();
    super.dispose();
  }

  /// Scrolls so [path]'s header sits at the top of the document.
  void reveal(String path) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final object = _keys[path]?.currentContext?.findRenderObject();
      if (!mounted || object is! RenderSliver || !_vertical.hasClients) return;
      final position = _vertical.position;
      _vertical.animateTo(
        object.constraints.precedingScrollExtent.clamp(0.0, position.maxScrollExtent),
        duration: const Duration(milliseconds: 220),
        curve: Curves.easeOutCubic,
      );
    });
  }

  _FileModel _model(String path, JsonMap patch) {
    final current = _models[path];
    if (current != null && identical(current.patch, patch)) return current;
    return _models[path] = _FileModel(patch);
  }

  double _gutterWidth(int maxLine, double codeSize) =>
      math.max(2, '$maxLine'.length) * (codeSize * .62).ceilToDouble() + 12;

  /// The original FileActionsContextMenu for a document header: the panel's
  /// shared file actions when provided, else open / copy path.
  Future<void> _headerMenu(String path, BuildContext anchor) async {
    final fileMenu = widget.onFileMenu;
    if (fileMenu != null) {
      final file = widget.git.files.where((f) => f.text('path') == path).firstOrNull ?? {'path': path};
      fileMenu(file, paseoAnchorRect(anchor));
      return;
    }
    final patch = widget.git.patches[path];
    final muted = PaseoColors.of(context).muted;
    Widget icon(IconData data) => Icon(data, size: 14, color: muted);
    final action = await showPaseoSelect<String>(
      context: anchor,
      title: path.split('/').last,
      options: [
        PaseoSelectOption(
          'open',
          tr(context, 'workspace.fileActions.openFile'),
          leading: icon(LucideIcons.fileText300),
        ),
        PaseoSelectOption(
          'copy-path',
          tr(context, 'workspace.fileActions.copyPath'),
          leading: icon(LucideIcons.copy300),
          separatorBefore: true,
        ),
        if (patch != null)
          PaseoSelectOption('copy-patch', 'Copy patch', leading: icon(LucideIcons.copy300)),
        PaseoSelectOption(
          'collapse',
          widget.collapsed.contains(path) ? 'Expand file' : 'Collapse file',
          leading: icon(
            widget.collapsed.contains(path)
                ? LucideIcons.listChevronsUpDown300
                : LucideIcons.listChevronsDownUp300,
          ),
          separatorBefore: true,
        ),
      ],
    );
    switch (action) {
      case 'open':
        widget.onOpen(path, null);
      case 'copy-path':
        await Clipboard.setData(ClipboardData(text: path));
      case 'copy-patch':
        if (patch != null) await Clipboard.setData(ClipboardData(text: diffPatchText(patch)));
      case 'collapse':
        widget.onToggleCollapsed(path);
    }
  }

  /// Original FileHeader document variant: 30px (28 content + 1px borderAccent
  /// rule), 12px leading inset, 8px trailing glyph rail, name in foreground and
  /// directory in muted base text, then the stat and change icon 8px apart.
  /// surface1 only while hovered or pressed — selection never tints it.
  Widget _header(JsonMap file, double width) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final path = file.text('path');
    final slash = path.lastIndexOf('/');
    final style = theme.textTheme.bodyMedium!.copyWith(
      fontWeight: FontWeight.normal,
      color: theme.colorScheme.onSurface,
    );
    final header = Material(
      color: colors.surface0,
      child: Builder(
        builder: (anchor) => InkWell(
          onTap: () => widget.onToggleCollapsed(path),
          onLongPress: () => _headerMenu(path, anchor),
          onSecondaryTap: () => _headerMenu(path, anchor),
          hoverColor: colors.surface1,
          highlightColor: colors.surface1,
          splashFactory: NoSplash.splashFactory,
          child: Container(
            height: _headerHeight,
            padding: const EdgeInsets.only(left: 12, right: 8),
            decoration: BoxDecoration(
              border: Border(bottom: BorderSide(color: colors.borderAccent)),
            ),
            child: Row(children: [
              Expanded(
                child: Tooltip(
                  message: path,
                  waitDuration: const Duration(milliseconds: 300),
                  // Desktop only, as the original enabledOnMobile={false}.
                  triggerMode: isCompactLayout(context) ? TooltipTriggerMode.manual : null,
                  child: Text.rich(
                    TextSpan(children: [
                      TextSpan(text: path.substring(slash + 1), style: style),
                      if (slash > 0) ...[
                        const WidgetSpan(child: SizedBox(width: 4)),
                        TextSpan(
                          text: path.substring(0, slash),
                          style: style.copyWith(color: colors.muted),
                        ),
                      ],
                    ]),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              ),
              const SizedBox(width: 4),
              FileDiffStat(additions: file.integer('additions'), deletions: file.integer('deletions')),
              const SizedBox(width: 8),
              FileChangeIcon(change: fileChangeKind(file)),
            ]),
          ),
        ),
      ),
    );
    if (widget.wrap) return header;
    // Pin the header to the viewport while the code scrolls horizontally.
    return ListenableBuilder(
      listenable: _horizontal,
      builder: (context, child) => Transform.translate(
        offset: Offset(_horizontal.hasClients ? _horizontal.offset : 0, 0),
        child: child,
      ),
      child: Align(alignment: Alignment.centerLeft, child: SizedBox(width: width, child: header)),
    );
  }

  /// Original document status row (binary / too large): a lineHeight + 24 band
  /// with the label in the muted code font, 12px from the leading edge.
  Widget _message(String text, {Widget? action}) {
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final lineHeight = (typography.codeFontSize * 1.5).roundToDouble();
    return Container(
      color: colors.surface0,
      constraints: BoxConstraints(minHeight: lineHeight + 24),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      alignment: Alignment.centerLeft,
      child: Row(children: [
        Flexible(
          child: Text(
            text,
            style: typography.codeStyle.copyWith(
              color: colors.muted,
              height: lineHeight / typography.codeFontSize,
            ),
          ),
        ),
        if (action != null) ...[const SizedBox(width: 8), action],
      ]),
    );
  }

  Widget _body(JsonMap file, double lineHeight) {
    final git = widget.git;
    final path = file.text('path');
    final patch = git.patches[path];
    if (patch == null) {
      final error = git.patchErrors[path];
      if (error != null) {
        return SliverToBoxAdapter(
          child: _message(
            errorMessage(error),
            action: PaseoButton(
              label: tr(context, 'common.actions.retry'),
              variant: PaseoButtonVariant.ghost,
              size: PaseoControlSize.xs,
              onPressed: () => git.ensurePatch(path),
            ),
          ),
        );
      }
      final estimate =
          (file.integer('additions') + file.integer('deletions') + 2).clamp(2, 400) * lineHeight;
      return SliverList(
        delegate: SliverChildBuilderDelegate(
          (context, _) => _PatchLoader(
            key: ValueKey('load:$path'),
            height: estimate,
            onLoad: () => git.ensurePatch(path),
          ),
          childCount: 1,
        ),
      );
    }
    final status = patch.text('status');
    if (status == 'binary' || status == 'too_large') {
      return SliverToBoxAdapter(
        child: _message(
          tr(
            context,
            status == 'binary' ? 'workspace.git.diff.binaryFile' : 'workspace.git.diff.tooLarge',
          ),
        ),
      );
    }
    final model = _model(path, patch);
    final rows = model.rows(widget.split);
    if (rows.isEmpty) {
      return SliverToBoxAdapter(child: _message('No textual changes.'));
    }
    return SliverList(
      delegate: SliverChildBuilderDelegate(
        (context, index) => _row(path, model, rows[index], lineHeight),
        childCount: rows.length,
      ),
    );
  }

  Widget _row(String path, _FileModel model, _DiffRow row, double lineHeight) {
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    if (row.hiddenStart != null) {
      return InkWell(
        onTap: () => setState(() {
          model.expanded.add(row.hiddenStart!);
          model.invalidate();
        }),
        hoverColor: colors.surface1,
        highlightColor: colors.surface1,
        splashFactory: NoSplash.splashFactory,
        child: Container(
          height: lineHeight,
          padding: EdgeInsets.only(
            left: _gutterWidth(model.maxLine, typography.codeFontSize) + 9,
          ),
          alignment: Alignment.centerLeft,
          child: Row(children: [
            Icon(LucideIcons.chevronsUpDown300, size: 14, color: colors.muted),
            const SizedBox(width: 8),
            Text(
              'Expand ${row.hiddenEnd! - row.hiddenStart! - 6} unchanged lines',
              style: typography.codeStyle.copyWith(color: colors.muted),
            ),
          ]),
        ),
      );
    }
    if (row.line != null) {
      return _cell(
        path,
        model,
        row.line,
        row.line!.type == 'remove' ? 'old' : 'new',
        unified: true,
      );
    }
    return IntrinsicHeight(
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          // The original split row paints no centre rule: each column's own
          // gutter rule separates it, and an empty side is a bare surface0 cell.
          Expanded(child: _cell(path, model, row.left, 'old')),
          Expanded(child: _cell(path, model, row.right, 'new')),
        ],
      ),
    );
  }

  Future<void> _lineMenu(
    BuildContext anchor,
    String path,
    DiffLine line,
    void Function(String side) comment,
  ) async {
    final muted = PaseoColors.of(context).muted;
    Widget icon(IconData data) => Icon(data, size: 14, color: muted);
    final action = await showPaseoSelect<String>(
      context: anchor,
      title: '$path:${line.newNumber ?? line.oldNumber}',
      options: [
        PaseoSelectOption(
          'copy',
          tr(context, 'common.actions.copyLine'),
          leading: icon(LucideIcons.copy300),
        ),
        PaseoSelectOption(
          'open',
          'Open file at line',
          leading: icon(LucideIcons.fileText300),
        ),
        if (line.oldNumber != null)
          PaseoSelectOption(
            'old',
            'Comment on old line',
            leading: icon(LucideIcons.messageSquarePlus300),
            separatorBefore: true,
          ),
        if (line.newNumber != null)
          PaseoSelectOption(
            'new',
            'Comment on new line',
            leading: icon(LucideIcons.messageSquarePlus300),
            separatorBefore: line.oldNumber == null,
          ),
      ],
    );
    switch (action) {
      case 'copy':
        await Clipboard.setData(ClipboardData(text: line.content));
      case 'open':
        widget.onOpen(path, line.newNumber ?? line.oldNumber);
      case 'old' || 'new':
        comment(action!);
    }
  }

  /// Original diff geometry: one gutter per column sized to the widest line
  /// number, a 1px rule, then syntax-coloured code inset by 8px.
  Widget _cell(String path, _FileModel model, DiffLine? line, String side, {bool unified = false}) {
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final lineHeight = (typography.codeFontSize * 1.5).roundToDouble();
    if (line == null) {
      return Container(color: colors.surface0, height: lineHeight);
    }
    final theme = Theme.of(context);
    final dark = theme.brightness == Brightness.dark;
    final syntaxTheme = stringValue(PaseoScope.of(context).preferences['syntaxTheme'], 'one');
    final header = line.type == 'header';
    final number = header
        ? null
        : side == 'old'
        ? line.oldNumber
        : line.newNumber;
    // Original createDiffPalette: additions/deletions use the status tokens
    // (statusSuccess 15% / statusDanger 10% fills, full-strength line numbers).
    final background = switch (line.type) {
      'add' => colors.success.withValues(alpha: .15),
      'remove' => colors.danger.withValues(alpha: .1),
      _ => colors.surface0,
    };
    final numberColor = switch (line.type) {
      'add' => colors.success,
      'remove' => colors.danger,
      _ => colors.muted,
    };
    final fileComments = widget.comments[path] ?? const {};
    final bodies = <JsonMap>[
      ...?fileComments['$side:$number'],
      if (unified && line.type == 'context') ...?fileComments['old:${line.oldNumber}'],
    ];
    final code = typography.codeStyle.copyWith(height: lineHeight / typography.codeFontSize);
    final gutter = _gutterWidth(model.maxLine, typography.codeFontSize);
    final runs = header ? null : model.runs(line, path);
    void comment(String side) => widget.onComment(path, line, side, model.contextFor(line));
    return Container(
      color: background,
      constraints: BoxConstraints(minHeight: lineHeight),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          IntrinsicHeight(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Material(
                  color: bodies.isEmpty ? Colors.transparent : colors.surface2,
                  child: InkWell(
                    onTap: number == null ? null : () => comment(side),
                    // The original gutter is painted on the diff canvas: no ripple or fill.
                    splashFactory: NoSplash.splashFactory,
                    hoverColor: Colors.transparent,
                    highlightColor: Colors.transparent,
                    mouseCursor: number == null ? MouseCursor.defer : SystemMouseCursors.click,
                    child: SizedBox(
                      width: gutter,
                      child: Padding(
                        padding: const EdgeInsets.only(right: 7),
                        child: Text(
                          number == null ? '' : '$number',
                          textAlign: TextAlign.right,
                          style: code.copyWith(color: numberColor),
                        ),
                      ),
                    ),
                  ),
                ),
                Container(width: 1, color: theme.dividerColor),
                Expanded(
                  child: Padding(
                    padding: const EdgeInsets.only(left: 8, right: 4),
                    child: Text.rich(
                      TextSpan(
                        text: runs == null ? line.content : null,
                        children: runs == null
                            ? null
                            : [
                                for (final (text, token) in runs)
                                  TextSpan(
                                    text: text,
                                    style: TextStyle(color: syntaxColor(syntaxTheme, dark, token)),
                                  ),
                              ],
                      ),
                      softWrap: widget.wrap,
                      overflow: widget.wrap ? TextOverflow.visible : TextOverflow.clip,
                      style: code.copyWith(
                        color: header ? colors.muted : theme.colorScheme.onSurface,
                      ),
                    ),
                  ),
                ),
                if (!header)
                  Builder(
                    builder: (anchor) => Tooltip(
                      message: 'Line actions',
                      waitDuration: const Duration(milliseconds: 300),
                      triggerMode: TooltipTriggerMode.manual,
                      child: InkWell(
                        onTap: () => _lineMenu(anchor, path, line, comment),
                        hoverColor: colors.surface2,
                        highlightColor: colors.surface2,
                        splashFactory: NoSplash.splashFactory,
                        borderRadius: BorderRadius.circular(4),
                        child: SizedBox(
                          width: 24,
                          height: lineHeight,
                          child: Icon(LucideIcons.ellipsis300, size: 14, color: colors.extraMuted),
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          ),
          // Original inline review thread: 12px insets, 8px vertical padding,
          // 6px gap; each comment a surface2 / borderAccent / 8px-radius row
          // with the body in the content size at 1.4 line height, two lines max,
          // then 26px round pencil / trash actions 4px apart.
          if (bodies.isNotEmpty)
            Container(
              color: colors.surface0,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 6,
                children: [
                  for (final comment in bodies)
                    Container(
                      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                      decoration: BoxDecoration(
                        color: colors.surface2,
                        border: Border.all(color: colors.borderAccent),
                        borderRadius: BorderRadius.circular(8),
                      ),
                      child: Row(
                        spacing: 8,
                        children: [
                          Expanded(
                            child: Text(
                              comment.text('body'),
                              maxLines: 2,
                              overflow: TextOverflow.ellipsis,
                              style: theme.textTheme.bodyMedium!.copyWith(
                                fontSize: typography.contentFontSize,
                                height: 1.4,
                                color: theme.colorScheme.onSurface,
                              ),
                            ),
                          ),
                          Row(
                            mainAxisSize: MainAxisSize.min,
                            spacing: 4,
                            children: [
                              _CommentAction(
                                label: tr(context, 'review.comment.edit'),
                                icon: LucideIcons.pencil300,
                                color: colors.muted,
                                onPressed: widget.onEditComment == null
                                    ? null
                                    : () => widget.onEditComment!(comment),
                              ),
                              _CommentAction(
                                label: tr(context, 'review.comment.delete'),
                                icon: LucideIcons.trash2300,
                                color: theme.colorScheme.error,
                                onPressed: widget.onDeleteComment == null
                                    ? null
                                    : () => widget.onDeleteComment!(comment),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                ],
              ),
            ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final git = widget.git;
    final colors = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final lineHeight = (typography.codeFontSize * 1.5).roundToDouble();
    final files = [...git.files]
      ..sort((a, b) => compareDiffPaths(a.text('path'), b.text('path')));
    _models.removeWhere((path, model) => !identical(git.patches[path], model.patch));
    final live = {for (final file in files) file.text('path')};
    _keys.removeWhere((path, _) => !live.contains(path));
    return ColoredBox(
      color: colors.surface0,
      child: LayoutBuilder(
        builder: (context, constraints) {
          final viewport = constraints.maxWidth;
          var widest = 0;
          var maxLine = 0;
          for (final model in _models.values) {
            widest = math.max(widest, model.maxChars);
            maxLine = math.max(maxLine, model.maxLine);
          }
          final column = _gutterWidth(maxLine, typography.codeFontSize) +
              widest * typography.codeFontSize * .62 +
              48;
          final width = widget.wrap
              ? viewport
              : math.max(viewport, widget.split ? column * 2 + 1 : column);
          final document = SelectionArea(
            child: ScrollConfiguration(
              behavior: ScrollConfiguration.of(context).copyWith(scrollbars: false),
              child: CustomScrollView(
                controller: _vertical,
                slivers: [
                  for (final file in files)
                    SliverMainAxisGroup(
                      key: _keys.putIfAbsent(file.text('path'), GlobalKey.new),
                      slivers: [
                        SliverPersistentHeader(
                          pinned: true,
                          delegate: _HeaderDelegate(_headerHeight, _header(file, viewport)),
                        ),
                        if (!widget.collapsed.contains(file.text('path'))) ...[
                          _body(file, lineHeight),
                          // Original DIFF_BODY_BORDER_HEIGHT: a 1px border rule
                          // closes every expanded file body.
                          SliverToBoxAdapter(
                            child: Container(height: 1, color: Theme.of(context).dividerColor),
                          ),
                        ],
                      ],
                    ),
                  SliverToBoxAdapter(child: SizedBox(height: 24 + widget.bottomInset)),
                ],
              ),
            ),
          );
          if (widget.wrap) {
            return Scrollbar(controller: _vertical, child: document);
          }
          return Scrollbar(
            controller: _vertical,
            notificationPredicate: (notification) => notification.depth == 1,
            child: Scrollbar(
              controller: _horizontal,
              child: SingleChildScrollView(
                controller: _horizontal,
                scrollDirection: Axis.horizontal,
                child: SizedBox(width: width, height: constraints.maxHeight, child: document),
              ),
            ),
          );
        },
      ),
    );
  }
}

/// Original inline review comment action: a 26px round hitbox, surface3 while
/// hovered or pressed, around a 14px glyph.
class _CommentAction extends StatefulWidget {
  const _CommentAction({
    required this.label,
    required this.icon,
    required this.color,
    required this.onPressed,
  });
  final String label;
  final IconData icon;
  final Color color;
  final VoidCallback? onPressed;

  @override
  State<_CommentAction> createState() => _CommentActionState();
}

class _CommentActionState extends State<_CommentAction> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final enabled = widget.onPressed != null;
    return Semantics(
      button: true,
      label: widget.label,
      excludeSemantics: true,
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
          child: Container(
            width: 26,
            height: 26,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: enabled && (_hovered || _pressed) ? PaseoColors.of(context).surface3 : null,
            ),
            child: Icon(widget.icon, size: 14, color: widget.color),
          ),
        ),
      ),
    );
  }
}

class _HeaderDelegate extends SliverPersistentHeaderDelegate {
  _HeaderDelegate(this.extent, this.child);
  final double extent;
  final Widget child;

  @override
  double get minExtent => extent;
  @override
  double get maxExtent => extent;

  @override
  Widget build(BuildContext context, double shrinkOffset, bool overlapsContent) =>
      SizedBox.expand(child: child);

  @override
  bool shouldRebuild(_HeaderDelegate oldDelegate) => true;
}

/// Placeholder sized like the expected patch; requests it once it is built,
/// which the lazy sliver only does near the viewport.
class _PatchLoader extends StatefulWidget {
  const _PatchLoader({super.key, required this.height, required this.onLoad});
  final double height;
  final VoidCallback onLoad;

  @override
  State<_PatchLoader> createState() => _PatchLoaderState();
}

class _PatchLoaderState extends State<_PatchLoader> {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) widget.onLoad();
    });
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Container(
      height: widget.height,
      color: colors.surface0,
      alignment: Alignment.topLeft,
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
      child: Row(children: [
        SizedBox.square(
          dimension: 12,
          child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.muted),
        ),
        const SizedBox(width: 8),
        Text(
          'Loading patch',
          style: Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.muted),
        ),
      ]),
    );
  }
}
