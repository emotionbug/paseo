import 'dart:async';
import 'dart:convert' show LineSplitter;
import 'dart:math' as math;
import 'dart:ui' as ui show BoxHeightStyle;

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:highlight/highlight.dart' as syntax;
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:markdown/markdown.dart' as md;

import '../../core/json.dart';
import '../../core/scope.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../files/syntax_palette.dart';
import 'assistant_file_links.dart';
import 'markdown_html.dart';

/// Original styles/density.ts blockGap: 12 on compact layouts, 16 from `md`.
double markdownBlockGap(BuildContext context) => isCompactLayout(context) ? 12 : 16;

/// Whether markdown follows the original native app's
/// react-native-enriched-markdown Android/iOS views rather than its web
/// renderer (the desktop app).
bool get _nativeMarkdown =>
    defaultTargetPlatform == TargetPlatform.android || defaultTargetPlatform == TargetPlatform.iOS;

/// Port of components/markdown/styles.ts createMarkdownStyles: content-size
/// (base when [compact]) paragraphs at 1.4 line height, bold headings scaled
/// 1.6/1.4/1.2/1.1/1/1 at 1.3, muted bullets, surface1 blockquotes with a 3px
/// surface2 rule, accentBright links without underline, mono code on surface2
/// (painted by [_InlineCodeChips]) and bordered tables with a surface2 header.
MarkdownStyleSheet paseoMarkdownStyleSheet(BuildContext context, {bool compact = false}) {
  final theme = Theme.of(context);
  final colors = theme.colorScheme;
  final palette = PaseoColors.of(context);
  final typography = PaseoTypography.of(context);
  final fontSize = compact ? typography.uiSize(14) : typography.contentFontSize;
  final paragraph = TextStyle(
    fontSize: fontSize,
    height: (fontSize * 1.4).roundToDouble() / fontSize,
    color: colors.onSurface,
  );
  TextStyle heading(double scale, {Color? color}) {
    final size = (fontSize * scale).roundToDouble();
    return paragraph.copyWith(
      fontSize: size,
      height: (fontSize * scale * 1.3).roundToDouble() / size,
      fontWeight: FontWeight.bold,
      color: color,
    );
  }

  final rule = BorderSide(color: colors.outline);
  return MarkdownStyleSheet(
    p: paragraph,
    a: paragraph.copyWith(color: palette.accentBright, decoration: TextDecoration.none),
    h1: heading(1.6),
    h2: heading(1.4),
    h3: heading(1.2),
    h4: heading(1.1),
    h5: heading(1),
    h6: heading(1, color: colors.onSurfaceVariant),
    em: const TextStyle(fontStyle: FontStyle.italic),
    strong: const TextStyle(fontWeight: FontWeight.bold),
    del: const TextStyle(decoration: TextDecoration.lineThrough),
    // The surface2 chip is painted by [_InlineCodeChips], which finds inline
    // code by this font family. The widget merges this sheet over
    // flutter_markdown's theme fallback, whose code style fills the glyph
    // box with the card color; a transparent fill keeps that second
    // background from being painted over the chip.
    code: TextStyle(
      fontFamily: typography.monoFontFamily,
      fontFamilyFallback: typography.monoFontFamilyFallback,
      fontSize: typography.codeFontSize,
      color: colors.onSurface,
      backgroundColor: const Color(0x00000000),
    ),
    blockSpacing: markdownBlockGap(context),
    // The builder's marker column (13 + 7) only identifies item rows;
    // [_ListMarker] replaces it with the original marker geometry.
    listIndent: 13,
    listBullet: paragraph.copyWith(color: colors.onSurfaceVariant),
    listBulletPadding: const EdgeInsets.only(right: 7),
    blockquote: paragraph.copyWith(color: colors.onSurface.withValues(alpha: .8)),
    // BlockquoteContainerView (web: blockquoteStyle): text starts borderWidth
    // 3 + gapWidth 8 + padding 12 from the edge, `padding: 12` elsewhere.
    blockquotePadding: const EdgeInsets.fromLTRB(23, 12, 12, 12),
    blockquoteDecoration: BoxDecoration(
      color: palette.surface1,
      border: Border(left: BorderSide(color: palette.surface2, width: 3)),
    ),
    // Fenced code renders through [PaseoMarkdown]'s `pre` builder.
    codeblockPadding: EdgeInsets.zero,
    codeblockDecoration: const BoxDecoration(),
    // react-native-enriched-markdown TableContainerView: bold header on
    // surface2, 1px border with a 6px radius, 12x8 cell padding, columns sized
    // to content between 60 and 300 (plus padding). Body rows alternate
    // surface0 (even, 0-based) and surface1; the builder decorates every other
    // body row starting with the second, and [_ListSpacingMarkdownBody] paints
    // surface0 behind the table.
    tableHead: paragraph.copyWith(fontWeight: FontWeight.bold),
    tableBody: paragraph,
    tableBorder: TableBorder.all(color: colors.outline, borderRadius: BorderRadius.circular(6)),
    tableHeadCellsDecoration: BoxDecoration(color: palette.surface2),
    tableCellsDecoration: BoxDecoration(color: palette.surface1),
    tableCellsPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
    tableHeadCellsPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
    // TableContainerView: a cell without a column alignment is ALIGN_NORMAL,
    // header included (the merged fallback would centre headers).
    tableHeadAlign: TextAlign.start,
    // Not Fixed/Intrinsic, so the builder returns the bare table and
    // [_ListSpacingMarkdownBody] adds the background and horizontal scroll.
    tableColumnWidth: const MinColumnWidth(
      MaxColumnWidth(IntrinsicColumnWidth(), FixedColumnWidth(60 + 24)),
      FixedColumnWidth(300 + 24),
    ),
    horizontalRuleDecoration: BoxDecoration(border: Border(top: rule)),
    checkbox: paragraph.copyWith(color: colors.onSurfaceVariant),
  );
}

/// Chat find (agent-stream/chat-find): the query and which of its matches
/// inside one message is current (0-based, rendered-text order).
typedef ChatFindTarget = ({String query, int occurrence});

/// chat-find/ranges.web.ts findRenderedMatches pattern: the trimmed query's
/// words joined by any whitespace, case-insensitive.
RegExp? chatFindPattern(String query) {
  final words = query.trim().split(RegExp(r'\s+')).where((w) => w.isNotEmpty).toList();
  if (words.isEmpty) return null;
  return RegExp(words.map(RegExp.escape).join(r'\s+'), caseSensitive: false, unicode: true);
}

/// The element tags [PaseoMarkdown] renders as separate text blocks
/// (flutter_markdown's block tags plus table cells).
const _chatFindBlocks = {
  'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'li', 'blockquote', 'pre', 'ol', 'ul', 'hr',
  'table', 'thead', 'tbody', 'tr', 'th', 'td', 'section',
};

/// chat-find/ranges.web.ts findMessageMatches over an assistant message: the
/// occurrences of [query] in the text [PaseoMarkdown] renders for [data],
/// counted inside each text block (a match never spans two blocks), in
/// document order — the same text [_BlockShaper] highlights. Images, task
/// checkboxes and alert markers render no searchable text.
int chatFindMarkdownMatchCount(String data, String query) {
  final pattern = chatFindPattern(query);
  if (pattern == null) return 0;
  final document = md.Document(extensionSet: PaseoMarkdown._extensions, encodeHtml: false);
  final nodes = document.parseLines(
    const LineSplitter().convert(_markAlerts(normalizeMarkdownHtml(data))),
  );
  var count = 0;
  StringBuffer? group;
  void flush() {
    final text = group?.toString();
    group = null;
    if (text != null) count += pattern.allMatches(text).length;
  }

  void visit(md.Node node) {
    if (node is md.Text) {
      (group ??= StringBuffer()).write(node.text);
      return;
    }
    if (node is! md.Element) return;
    switch (node.tag) {
      case 'img' || 'input':
        return;
      case 'br':
        (group ??= StringBuffer()).write('\n');
        return;
      case 'pre':
        // MarkdownCodeBlock drops the fence's final newline.
        flush();
        final code = node.textContent;
        group = StringBuffer(code.endsWith('\n') ? code.substring(0, code.length - 1) : code);
        flush();
        return;
      case 'p' when node.textContent.startsWith(_alertSentinel):
        return;
    }
    final block = _chatFindBlocks.contains(node.tag);
    if (block) flush();
    for (final child in node.children ?? const <md.Node>[]) {
      visit(child);
    }
    if (block) flush();
  }

  for (final node in nodes) {
    visit(node);
  }
  flush();
  return count;
}

/// chat-find/index.web.tsx highlight: statusWarning background, surface0 text.
TextStyle chatFindHighlightStyle(BuildContext context) {
  final palette = PaseoColors.of(context);
  return TextStyle(backgroundColor: palette.warning, color: palette.surface0);
}

/// Applies [highlight] to the characters [start, end) of [span], splitting
/// text spans at the range edges (placeholders count as one character).
InlineSpan highlightSpanRange(InlineSpan span, int start, int end, TextStyle highlight) {
  var offset = 0;
  InlineSpan visit(InlineSpan span) {
    if (span is! TextSpan) {
      offset += 1;
      return span;
    }
    final text = span.text ?? '';
    final from = offset, to = offset + text.length;
    offset = to;
    final children = span.children?.map(visit).toList();
    TextSpan copy({String? text, TextStyle? style, List<InlineSpan>? children}) => TextSpan(
      text: text,
      style: style,
      children: children,
      recognizer: span.recognizer,
      mouseCursor: span.mouseCursor,
      onEnter: span.onEnter,
      onExit: span.onExit,
      semanticsLabel: span.semanticsLabel,
      locale: span.locale,
      spellOut: span.spellOut,
    );
    if (text.isEmpty || to <= start || from >= end) {
      return copy(text: span.text, style: span.style, children: children);
    }
    final a = (start - from).clamp(0, text.length), b = (end - from).clamp(0, text.length);
    final marked = (span.style ?? const TextStyle()).merge(highlight);
    return copy(
      style: span.style,
      children: [
        if (a > 0) copy(text: text.substring(0, a)),
        copy(text: text.substring(a, b), style: marked),
        if (b < text.length) copy(text: text.substring(b)),
        ...?children,
      ],
    );
  }

  return visit(span);
}

/// The shared Markdown surface (original components/markdown/renderer.tsx):
/// selectable as one region, fenced code with syntax colors and a copy glyph.
/// On desktop (renderers.web.tsx WebLink) links underline while hovered and,
/// when [linkTooltip] returns a path for their target, show it after 400ms.
///
/// With [onTapFileLink] the text follows the original link recognition
/// (markdown/source.ts linkMetadata): http(s) URLs and file paths in prose
/// are linkified, complete inline-code spans naming a file under
/// [workspaceRoot] become links that keep their code styling, and every link
/// press reports its source kind.
class PaseoMarkdown extends StatelessWidget {
  const PaseoMarkdown({
    super.key,
    required this.data,
    this.selectable = true,
    this.compact = false,
    this.onTapLink,
    this.onTapFileLink,
    this.workspaceRoot,
    this.imageBuilder,
    this.linkTooltip,
    this.find,
  });

  final String data;
  final bool selectable, compact;
  final void Function(String href)? onTapLink;
  final void Function(AssistantFileLinkSource source)? onTapFileLink;
  final String? workspaceRoot;
  final MarkdownImageBuilder? imageBuilder;
  final String? Function(String href)? linkTooltip;

  /// Chat find's current occurrence inside this message, highlighted with
  /// statusWarning on surface0 text (chat-find/index.web.tsx ::highlight).
  final ChatFindTarget? find;

  static final _inlineLink = RegExp(
    r'\[((?:[^\[\]\n]|\[[^\]\n]*\])*)\]\(\s*<?([^)\s>]+)>?(?:\s+"[^"]*")?\s*\)',
  );

  /// The original renderer has no footnotes: `[^1]` and its definition stay
  /// literal text (F02). Everything else follows GitHub-flavored Markdown.
  static final _extensions = md.ExtensionSet(
    [
      for (final syntax in md.ExtensionSet.gitHubFlavored.blockSyntaxes)
        if (syntax is! md.FootnoteDefSyntax) syntax,
    ],
    md.ExtensionSet.gitHubFlavored.inlineSyntaxes,
  );

  /// [_extensions] without the GFM autolink extension: the original
  /// recognizes only http(s) URLs and file paths ([_LinkifySyntax]), not bare
  /// `www.` hosts or e-mail addresses.
  static final _linkifiedExtensions = md.ExtensionSet(
    _extensions.blockSyntaxes,
    [
      for (final syntax in _extensions.inlineSyntaxes)
        if (syntax is! md.AutolinkExtensionSyntax) syntax,
    ],
  );

  /// Rendered link text → href, so a hovered span can name its target (the
  /// markdown engine's recognizers do not expose it).
  static Map<String, String> _linkTargets(String data) {
    final targets = <String, String>{};
    for (final match in _inlineLink.allMatches(data)) {
      final text = match.group(1)!.replaceAll(RegExp(r'[`*_~]'), '').trim();
      targets.putIfAbsent(text, () => match.group(2)!);
    }
    return targets;
  }

  @override
  Widget build(BuildContext context) {
    final styleSheet = paseoMarkdownStyleSheet(context, compact: compact);
    final tooltip = linkTooltip;
    // Original source.ts prepareMarkdownSource: comment HTML becomes Markdown
    // before rendering, so `<details>`, `<br>`, `<b>`, links and images are
    // never silently dropped.
    final source = normalizeMarkdownHtml(data);
    final targets = tooltip == null || _nativeMarkdown ? null : _linkTargets(source);
    final fileLinks = onTapFileLink;
    final tapLink = onTapLink;
    final body = _ShapedMarkdownBody(
      data: _markAlerts(source),
      styleSheet: styleSheet,
      softLineBreak: true,
      find: find,
      extensionSet: fileLinks == null ? _extensions : _linkifiedExtensions,
      inlineSyntaxes: fileLinks == null
          ? null
          : [_FileLinkCodeSyntax(workspaceRoot), _LinkifySyntax()],
      linkTooltip: tooltip == null || _nativeMarkdown
          ? null
          : (text) => tooltip(targets![text.trim()] ?? text.trim()),
      builders: {'pre': _FenceBuilder()},
      bulletBuilder: (params) => _ListMarker(
        kind: params.style == BulletStyle.orderedList ? _Marker.ordered : _Marker.bullet,
        index: params.index,
        style: styleSheet.listBullet!,
      ),
      checkboxBuilder: (checked) => _ListMarker(
        kind: checked ? _Marker.taskChecked : _Marker.task,
        style: styleSheet.listBullet!,
      ),
      onTapLink: fileLinks != null
          ? (text, href, title) {
              if (href == null) return;
              fileLinks(
                AssistantFileLinkSource(
                  href: href,
                  text: text,
                  type: switch (title) {
                    _linkifiedTitle => FileLinkSourceType.linkify,
                    _inlineCodeTitle => FileLinkSourceType.inlineCode,
                    _ => FileLinkSourceType.markdown,
                  },
                ),
              );
            }
          : tapLink == null
          ? null
          : (text, href, title) {
              if (href != null) tapLink(href);
            },
      imageBuilder: imageBuilder,
    );
    return selectable ? SelectionArea(child: body) : body;
  }
}

final _htmlBlockLine = RegExp(r'^ {0,3}<[A-Za-z!/?]', multiLine: true);
final _referenceDefinition = RegExp(r'^ {0,3}\[[^\]\n]+\]:', multiLine: true);
final _streamFence = RegExp(r'^\s*(`{3,}|~{3,})');
final _listMarkerLine = RegExp(r'^(?:[-*+]|\d{1,9}[.)])(?:\s|$)');

/// Where the last top-level block of [source] starts, when the text before
/// it parses into exactly the blocks it forms in the whole text: a blank
/// line, then an unindented line that is no list item (a list never
/// splits), outside any code fence. Null when no such boundary exists or the
/// text has line-level HTML or reference definitions, whose meaning spans
/// blocks.
int? markdownStableSplit(String source) {
  if (_htmlBlockLine.hasMatch(source) || _referenceDefinition.hasMatch(source)) return null;
  int? split;
  String? fence;
  var previousBlank = false;
  var start = 0;
  while (start < source.length) {
    final newline = source.indexOf('\n', start);
    final end = newline < 0 ? source.length : newline;
    final line = source.substring(start, end);
    final marker = _streamFence.firstMatch(line)?.group(1);
    if (fence != null) {
      // A fence closes on a run of its character at least as long.
      if (marker != null && marker[0] == fence[0] && marker.length >= fence.length && line.trim() == marker) {
        fence = null;
      }
      previousBlank = false;
    } else {
      if (previousBlank &&
          start > 0 &&
          line.isNotEmpty &&
          line.trimLeft().length == line.length &&
          !_listMarkerLine.hasMatch(line)) {
        split = start;
      }
      if (marker != null) fence = marker;
      previousBlank = marker == null && line.trim().isEmpty;
    }
    if (newline < 0) break;
    start = newline + 1;
  }
  return split;
}

/// A message whose text is still growing: the blocks before its last one
/// ([markdownStableSplit]) are built once and kept while only the last block
/// is parsed again for each revealed slice; the gap between them is the one
/// the whole document puts between those blocks. A finished message renders
/// whole.
class StreamingMarkdown extends StatefulWidget {
  const StreamingMarkdown({
    super.key,
    required this.data,
    required this.streaming,
    required this.markdown,
  });

  final String data;
  final bool streaming;

  /// Builds the markdown for a part of [data].
  final Widget Function(String data) markdown;

  @override
  State<StreamingMarkdown> createState() => _StreamingMarkdownState();
}

class _StreamingMarkdownState extends State<StreamingMarkdown> {
  ({String text, Widget widget})? _head;

  @override
  Widget build(BuildContext context) {
    final split = widget.streaming ? markdownStableSplit(widget.data) : null;
    if (split == null) {
      _head = null;
      return widget.markdown(widget.data);
    }
    // The blank lines closing the head are the gap rendered below it.
    final text = widget.data.substring(0, split).trimRight();
    final cached = _head;
    final head = cached != null && cached.text == text ? cached.widget : widget.markdown(text);
    _head = (text: text, widget: head);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        head,
        SizedBox(height: markdownBlockGap(context)),
        widget.markdown(widget.data.substring(split)),
      ],
    );
  }
}

/// Link titles that mark recognized (not authored) links; the private-use
/// prefix keeps them apart from any authored title.
const _linkifiedTitle = '\uE001linkify';
const _inlineCodeTitle = '\uE001inline-code';

/// markdown/source.ts FILE_PATTERN linkification of prose text.
class _LinkifySyntax extends md.InlineSyntax {
  _LinkifySyntax() : super(assistantLinkifyPattern.pattern);

  /// The pattern's look-behind rejects a match after any path character, so
  /// those positions are skipped without running it.
  static final _pathCharacterBefore = RegExp(r'[a-zA-Z0-9\u0080-\uFFFF_@.%+~:/\\-]');

  @override
  bool tryMatch(md.InlineParser parser, [int? startMatchPos]) {
    final position = startMatchPos ?? parser.pos;
    if (position > 0 && _pathCharacterBefore.hasMatch(parser.source[position - 1])) return false;
    return super.tryMatch(parser, startMatchPos);
  }

  @override
  bool onMatch(md.InlineParser parser, Match match) {
    final text = match[0]!;
    parser.addNode(
      md.Element.text('a', text)
        ..attributes['href'] = text
        ..attributes['title'] = _linkifiedTitle,
    );
    return true;
  }
}

/// markdown/source.ts collectInlineCodePaths: a complete code span that
/// classifies as a file link becomes a link around the code.
class _FileLinkCodeSyntax extends md.CodeSyntax {
  _FileLinkCodeSyntax(this.workspaceRoot);
  final String? workspaceRoot;

  @override
  bool onMatch(md.InlineParser parser, Match match) {
    final markerLength = match[1]!.length;
    final contentStart = parser.pos + markerLength;
    var code = parser.source.substring(contentStart, contentStart + match[0]!.length - markerLength * 2);
    // CommonMark strips one surrounding space when both ends have one.
    if (code.trim().isNotEmpty &&
        (code.startsWith(' ') || code.startsWith('\n')) &&
        (code.endsWith(' ') || code.endsWith('\n'))) {
      code = code.substring(1, code.length - 1);
    }
    code = code.replaceAll('\n', ' ');
    final node = md.Element.text('code', code);
    if (!isInlineCodeFileLink(code, workspaceRoot: workspaceRoot)) {
      parser.addNode(node);
      return true;
    }
    parser.addNode(
      md.Element('a', [node])
        ..attributes['href'] = code
        ..attributes['title'] = _inlineCodeTitle,
    );
    return true;
  }
}

/// md4c admonitions (GitHub alerts, `> [!NOTE]` …). The bundled parser has no
/// alert syntax, so the marker line of a quote becomes a sentinel paragraph
/// that [_BlockShaper] turns into the alert header.
const _alertSentinel = '\uE000paseo-alert:';
final _alertLine = RegExp(
  r'^(\s{0,3}(?:>\s?)+)\s*\[!(note|tip|important|warning|caution)\]\s*$',
  caseSensitive: false,
);
final _fenceLine = RegExp(r'^\s{0,3}(?:>\s?)*\s{0,3}(`{3,}|~{3,})');

String _markAlerts(String source) {
  if (!source.contains('[!')) return source;
  final lines = source.split('\n');
  final out = <String>[];
  String? fence;
  for (var i = 0; i < lines.length; i++) {
    final line = lines[i];
    final fenceMatch = _fenceLine.firstMatch(line);
    if (fenceMatch != null) {
      final marker = fenceMatch.group(1)!;
      if (fence == null) {
        fence = marker;
      } else if (marker[0] == fence[0] && marker.length >= fence.length) {
        fence = null;
      }
      out.add(line);
      continue;
    }
    final alert = fence == null ? _alertLine.firstMatch(line) : null;
    // Only the first line of a quote opens an alert.
    if (alert != null && (i == 0 || !lines[i - 1].trimLeft().startsWith('>'))) {
      final prefix = alert.group(1)!.trimRight();
      out
        ..add('$prefix $_alertSentinel${alert.group(2)!.toLowerCase()}')
        ..add(prefix);
      continue;
    }
    out.add(line);
  }
  return out.join('\n');
}

/// [MarkdownBody] whose built blocks pass through [_BlockShaper]. Blocks
/// stretch to the full width (RN block views and the web renderer's block
/// elements span their container), so paragraph alignment follows the text
/// direction instead of the widest block.
class _ShapedMarkdownBody extends MarkdownBody {
  const _ShapedMarkdownBody({
    required super.data,
    super.styleSheet,
    super.softLineBreak,
    super.builders,
    super.bulletBuilder,
    super.checkboxBuilder,
    super.onTapLink,
    super.imageBuilder,
    super.extensionSet,
    super.inlineSyntaxes,
    this.linkTooltip,
    this.find,
  }) : super(fitContent: false);

  /// Desktop link hover: null on native (no hover affordances there).
  final String? Function(String linkText)? linkTooltip;
  final ChatFindTarget? find;

  @override
  Widget build(BuildContext context, List<Widget>? children) {
    final shaper = _BlockShaper(
      context,
      styleSheet!,
      hoverLinks: !_nativeMarkdown,
      linkTooltip: linkTooltip,
      find: find,
    );
    return super.build(context, children?.map((child) => shaper.shape(child)).toList());
  }
}

enum _Marker { bullet, ordered, task, taskChecked }

/// Adapts flutter_markdown's block widgets to the original renderer:
/// - lists use `itemSpacing: 4` between (and inside) items instead of the
///   block gap, [_ListMarker] geometry with 20px per nesting level, and the
///   muted checkedTextColor for checked task items;
/// - tables get their surface0 backdrop (the even body rows) and scroll
///   sideways;
/// - quotes opened by an alert sentinel become admonitions;
/// - paragraphs with inline code get [_InlineCodeChips].
class _BlockShaper {
  _BlockShaper(
    BuildContext context,
    this.sheet, {
    this.hoverLinks = false,
    this.linkTooltip,
    this.find,
  }) : palette = PaseoColors.of(context),
       colors = Theme.of(context).colorScheme,
       dark = Theme.of(context).brightness == Brightness.dark,
       mono = PaseoTypography.of(context).monoFontFamily,
       findStyle = find == null ? null : chatFindHighlightStyle(context);

  final MarkdownStyleSheet sheet;
  final PaseoColors palette;
  final ColorScheme colors;
  final bool dark;
  final String mono;
  final bool hoverLinks;
  final String? Function(String linkText)? linkTooltip;

  /// Chat find: only the [ChatFindTarget.occurrence]-th match across the
  /// message's rendered text blocks (document order) is highlighted.
  final ChatFindTarget? find;
  final TextStyle? findStyle;
  int _findSeen = 0;

  /// Returns the target occurrence's range inside [text] when it falls in this
  /// block, counting every match seen so far.
  TextRange? _nextFindMatch(ChatFindTarget target, String text) {
    final pattern = chatFindPattern(target.query);
    if (pattern == null) return null;
    TextRange? hit;
    for (final match in pattern.allMatches(text)) {
      if (_findSeen == target.occurrence) hit = TextRange(start: match.start, end: match.end);
      _findSeen++;
    }
    return hit;
  }

  static String _plainText(InlineSpan span) {
    final buffer = StringBuffer();
    void visit(InlineSpan span) {
      if (span is! TextSpan) {
        buffer.write('\uFFFC');
        return;
      }
      buffer.write(span.text ?? '');
      span.children?.forEach(visit);
    }

    visit(span);
    return buffer.toString();
  }

  static const _itemSpacing = 4.0;

  double get _markerColumn => sheet.listIndent! + sheet.listBulletPadding!.horizontal;

  /// [depth] counts the lists enclosing [widget]; [root] is the outermost
  /// list's marker kind.
  Widget shape(Widget widget, {int depth = 0, _Marker? root, Color? recolor, bool inItem = false}) {
    Widget child(Widget child) => shape(child, depth: depth, root: root, recolor: recolor);
    List<Widget> each(List<Widget> children) => [for (final c in children) child(c)];
    if (widget is Text) return _text(widget, recolor);
    if (widget is MarkdownCodeBlock && find != null) {
      final code = widget.code.endsWith('\n')
          ? widget.code.substring(0, widget.code.length - 1)
          : widget.code;
      final match = _nextFindMatch(find!, code);
      if (match == null) return widget;
      return MarkdownCodeBlock(key: widget.key, code: widget.code, language: widget.language, findMatch: match);
    }
    if (widget is ClipRRect && widget.child is Table) {
      final table = widget.child! as Table;
      return SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: DecoratedBox(
          decoration: BoxDecoration(color: palette.surface0, borderRadius: BorderRadius.circular(6)),
          child: ClipRRect(
            key: widget.key,
            borderRadius: widget.borderRadius,
            clipBehavior: widget.clipBehavior,
            child: Table(
              key: table.key,
              columnWidths: table.columnWidths,
              defaultColumnWidth: table.defaultColumnWidth,
              textDirection: table.textDirection,
              border: table.border,
              defaultVerticalAlignment: table.defaultVerticalAlignment,
              textBaseline: table.textBaseline,
              children: [
                for (final row in table.children)
                  TableRow(key: row.key, decoration: row.decoration, children: each(row.children)),
              ],
            ),
          ),
        ),
      );
    }
    if (widget is DecoratedBox && widget.decoration == sheet.blockquoteDecoration) {
      return _blockquote(widget, child);
    }
    if (widget is Column) {
      final tight = inItem || widget.children.any(_isItem);
      return Column(
        key: widget.key,
        mainAxisAlignment: widget.mainAxisAlignment,
        mainAxisSize: widget.mainAxisSize,
        crossAxisAlignment: widget.crossAxisAlignment,
        textDirection: widget.textDirection,
        verticalDirection: widget.verticalDirection,
        textBaseline: widget.textBaseline,
        spacing: widget.spacing,
        children: [
          for (final c in widget.children)
            tight && _isGap(c) ? const SizedBox(height: _itemSpacing) : child(c),
        ],
      );
    }
    if (_isItem(widget)) return _item(widget as Row, depth, root, recolor);
    if (widget is Row) {
      return Row(
        key: widget.key,
        mainAxisAlignment: widget.mainAxisAlignment,
        mainAxisSize: widget.mainAxisSize,
        crossAxisAlignment: widget.crossAxisAlignment,
        textDirection: widget.textDirection,
        verticalDirection: widget.verticalDirection,
        textBaseline: widget.textBaseline,
        spacing: widget.spacing,
        children: each(widget.children),
      );
    }
    if (widget is Wrap) {
      return Wrap(
        key: widget.key,
        direction: widget.direction,
        alignment: widget.alignment,
        spacing: widget.spacing,
        runAlignment: widget.runAlignment,
        runSpacing: widget.runSpacing,
        crossAxisAlignment: widget.crossAxisAlignment,
        textDirection: widget.textDirection,
        verticalDirection: widget.verticalDirection,
        clipBehavior: widget.clipBehavior,
        children: each(widget.children),
      );
    }
    if (widget is Padding && widget.child != null) {
      return Padding(key: widget.key, padding: widget.padding, child: child(widget.child!));
    }
    if (widget is DecoratedBox && widget.child != null) {
      return DecoratedBox(
        key: widget.key,
        decoration: widget.decoration,
        position: widget.position,
        child: child(widget.child!),
      );
    }
    if (widget is SizedBox && widget.child != null) {
      return SizedBox(key: widget.key, width: widget.width, height: widget.height, child: child(widget.child!));
    }
    if (widget is Flexible) {
      return Flexible(key: widget.key, flex: widget.flex, fit: widget.fit, child: child(widget.child));
    }
    if (widget is TableCell) {
      return TableCell(key: widget.key, verticalAlignment: widget.verticalAlignment, child: child(widget.child));
    }
    if (widget is DefaultTextStyle) {
      return DefaultTextStyle(
        key: widget.key,
        style: widget.style,
        textAlign: widget.textAlign,
        softWrap: widget.softWrap,
        overflow: widget.overflow,
        maxLines: widget.maxLines,
        textWidthBasis: widget.textWidthBasis,
        textHeightBehavior: widget.textHeightBehavior,
        child: child(widget.child),
      );
    }
    return widget;
  }

  bool _isItem(Widget widget) =>
      widget is Row &&
      widget.children.isNotEmpty &&
      widget.children.first is SizedBox &&
      (widget.children.first as SizedBox).width == _markerColumn;

  bool _isGap(Widget widget) =>
      widget is SizedBox && widget.child == null && widget.height == sheet.blockSpacing;

  static _ListMarker? _markerIn(Widget column) {
    final slot = column is SizedBox ? column.child : null;
    if (slot is _ListMarker) return slot;
    if (slot is Padding && slot.child is _ListMarker) return slot.child! as _ListMarker;
    return null;
  }

  Widget _item(Row row, int depth, _Marker? root, Color? recolor) {
    final marker = _markerIn(row.children.first);
    final rootKind = depth == 0 || root == null ? marker?.kind : root;
    // taskList.checkedTextColor: foregroundMuted for the checked item's text.
    final color = marker?.kind == _Marker.taskChecked ? colors.onSurfaceVariant : recolor;
    return Row(
      key: row.key,
      mainAxisSize: row.mainAxisSize,
      textBaseline: row.textBaseline,
      crossAxisAlignment: row.crossAxisAlignment,
      children: [
        for (var i = 0; i < row.children.length; i++)
          if (i == 0 && marker != null)
            marker.placed(depth: depth, root: rootKind!)
          else if (row.children[i] is Flexible)
            Flexible(
              key: row.children[i].key,
              flex: (row.children[i] as Flexible).flex,
              fit: (row.children[i] as Flexible).fit,
              child: shape(
                (row.children[i] as Flexible).child,
                depth: depth + 1,
                root: rootKind,
                recolor: color,
                inItem: true,
              ),
            )
          else
            shape(row.children[i], depth: depth, root: root, recolor: recolor),
      ],
    );
  }

  /// GitHub octicons (react-native-enriched-markdown admonitionIcons).
  static const _alertIcons = {
    'note':
        'M0 8a8 8 0 1 1 16 0A8 8 0 0 1 0 8Zm8-6.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13ZM6.5 7.75A.75.75 0 0 1 7.25 7h1a.75.75 0 0 1 .75.75v2.75h.25a.75.75 0 0 1 0 1.5h-2a.75.75 0 0 1 0-1.5h.25v-2h-.25a.75.75 0 0 1-.75-.75ZM8 6a1 1 0 1 1 0-2 1 1 0 0 1 0 2Z',
    'tip':
        'M8 1.5c-2.363 0-4 1.69-4 3.75 0 .984.424 1.625.984 2.304l.214.253c.223.264.47.556.673.848.284.411.537.896.621 1.49a.75.75 0 0 1-1.484.211c-.04-.282-.163-.547-.37-.847a8.456 8.456 0 0 0-.542-.68c-.084-.1-.173-.205-.268-.32C3.201 7.75 2.5 6.766 2.5 5.25 2.5 2.31 4.863 0 8 0s5.5 2.31 5.5 5.25c0 1.516-.701 2.5-1.328 3.259-.095.115-.184.22-.268.319-.207.245-.383.453-.541.681-.208.3-.33.565-.37.847a.751.751 0 0 1-1.485-.212c.084-.593.337-1.078.621-1.489.203-.292.45-.584.673-.848.075-.088.147-.173.213-.253.561-.679.985-1.32.985-2.304 0-2.06-1.637-3.75-4-3.75ZM5.75 12h4.5a.75.75 0 0 1 0 1.5h-4.5a.75.75 0 0 1 0-1.5ZM6 15.25a.75.75 0 0 1 .75-.75h2.5a.75.75 0 0 1 0 1.5h-2.5a.75.75 0 0 1-.75-.75Z',
    'important':
        'M0 1.75C0 .784.784 0 1.75 0h12.5C15.216 0 16 .784 16 1.75v9.5A1.75 1.75 0 0 1 14.25 13H8.06l-2.573 2.573A1.458 1.458 0 0 1 3 14.543V13H1.75A1.75 1.75 0 0 1 0 11.25Zm1.75-.25a.25.25 0 0 0-.25.25v9.5c0 .138.112.25.25.25h2a.75.75 0 0 1 .75.75v2.19l2.72-2.72a.749.749 0 0 1 .53-.22h6.5a.25.25 0 0 0 .25-.25v-9.5a.25.25 0 0 0-.25-.25Zm7 2.25v2.5a.75.75 0 0 1-1.5 0v-2.5a.75.75 0 0 1 1.5 0ZM9 9a1 1 0 1 1-2 0 1 1 0 0 1 2 0Z',
    'warning':
        'M6.457 1.047c.659-1.234 2.427-1.234 3.086 0l6.082 11.378A1.75 1.75 0 0 1 14.082 15H1.918a1.75 1.75 0 0 1-1.543-2.575Zm1.763.707a.25.25 0 0 0-.44 0L1.698 13.132a.25.25 0 0 0 .22.368h12.164a.25.25 0 0 0 .22-.368Zm.53 3.996v2.5a.75.75 0 0 1-1.5 0v-2.5a.75.75 0 0 1 1.5 0ZM9 11a1 1 0 1 1-2 0 1 1 0 0 1 2 0Z',
    'caution':
        'M4.47.22A.749.749 0 0 1 5 0h6c.199 0 .389.079.53.22l4.25 4.25c.141.14.22.331.22.53v6a.749.749 0 0 1-.22.53l-4.25 4.25A.749.749 0 0 1 11 16H5a.749.749 0 0 1-.53-.22L.22 11.53A.749.749 0 0 1 0 11V5c0-.199.079-.389.22-.53Zm.84 1.28L1.5 5.31v5.38l3.81 3.81h5.38l3.81-3.81V5.31L10.69 1.5ZM8 4a.75.75 0 0 1 .75.75v3.5a.75.75 0 0 1-1.5 0v-3.5A.75.75 0 0 1 8 4Zm0 8a1 1 0 1 1 0-2 1 1 0 0 1 0 2Z',
  };

  static const _alertTitles = {
    'note': 'Note',
    'tip': 'Tip',
    'important': 'Important',
    'warning': 'Warning',
    'caution': 'Caution',
  };

  /// styles.ts blockquote.admonitions colors.
  Color _alertTint(String type) => switch (type) {
    'tip' => palette.success,
    // statusMerged (styles/theme.ts dark / light status colors).
    'important' => dark ? const Color(0xffa890d5) : const Color(0xff7347af),
    'warning' => palette.warning,
    'caution' => palette.danger,
    _ => palette.accentBright,
  };

  static String? _widgetPlainText(Widget widget) => switch (widget) {
    Text(:final textSpan, :final data) => textSpan?.toPlainText() ?? data,
    Column(children: [final only]) || Wrap(children: [final only]) => _widgetPlainText(only),
    Padding(child: final Widget only) => _widgetPlainText(only),
    _ => null,
  };

  /// BlockquoteContainerView admonitions (web AdmonitionRenderer): the quote
  /// box with a surface2 fill and a 3px bar in the type's tint, headed by the
  /// tinted octicon (ceil(fontSize)) and bold title round(icon * 0.4) apart,
  /// in a band of ceil(max(ceil(fontSize), fontSize * 1.35)) (web: 1.2 line
  /// height) followed by round(fontSize * 0.4).
  Widget _blockquote(DecoratedBox box, Widget Function(Widget) child) {
    final padding = box.child is Padding ? box.child! as Padding : null;
    final body = padding?.child;
    final blocks = body is Column ? body.children : [?body];
    final marker = blocks.isEmpty ? null : _widgetPlainText(blocks.first)?.trim();
    final type = marker != null && marker.startsWith(_alertSentinel)
        ? marker.substring(_alertSentinel.length)
        : null;
    if (type == null || !_alertIcons.containsKey(type)) {
      return DecoratedBox(
        key: box.key,
        decoration: box.decoration,
        position: box.position,
        child: box.child == null ? null : child(box.child!),
      );
    }
    final rest = blocks.length > 1 && _isGap(blocks[1]) ? blocks.sublist(2) : blocks.sublist(1);
    final tint = _alertTint(type);
    final fontSize = sheet.blockquote!.fontSize!;
    final iconSize = fontSize.ceilToDouble();
    final header = SelectionContainer.disabled(
      child: SizedBox(
        height: _nativeMarkdown
            ? math.max(iconSize, fontSize * 1.35).ceilToDouble()
            : math.max(iconSize, fontSize * 1.2),
        child: Row(
          spacing: (iconSize * .4).roundToDouble(),
          children: [
            SvgPicture.string(
              '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">'
              '<path d="${_alertIcons[type]}"/></svg>',
              width: iconSize,
              height: iconSize,
              colorFilter: ColorFilter.mode(tint, BlendMode.srcIn),
            ),
            Flexible(
              child: Text(
                _alertTitles[type]!,
                maxLines: 1,
                style: TextStyle(fontSize: fontSize, fontWeight: FontWeight.bold, color: tint),
              ),
            ),
          ],
        ),
      ),
    );
    return DecoratedBox(
      key: box.key,
      decoration: BoxDecoration(
        color: palette.surface2,
        border: Border(left: BorderSide(color: tint, width: 3)),
      ),
      child: Padding(
        padding: padding?.padding ?? sheet.blockquotePadding!,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: body is Column ? body.crossAxisAlignment : CrossAxisAlignment.stretch,
          children: [
            header,
            SizedBox(height: (fontSize * .4).roundToDouble()),
            for (final block in rest) child(block),
          ],
        ),
      ),
    );
  }

  Widget _text(Text text, Color? recolor) {
    final span = text.textSpan;
    if (span == null) return text;
    var colored = recolor == null ? span : _recolor(span, recolor);
    final ranges = <TextRange>[];
    var offset = 0;
    var hasLinks = false;
    void visit(InlineSpan span) {
      if (span is! TextSpan) {
        offset += 1; // A placeholder.
        return;
      }
      if (span.recognizer != null) hasLinks = true;
      final length = span.text?.length ?? 0;
      if (length > 0 && span.style?.fontFamily == mono) {
        ranges.add(TextRange(start: offset, end: offset + length));
      }
      offset += length;
      for (final child in span.children ?? const <InlineSpan>[]) {
        visit(child);
      }
    }

    // Inline-code chip ranges come from the unsplit spans; the find highlight
    // only splits spans, so their offsets stay valid.
    visit(colored);
    late final plain = _plainText(colored);
    // Android/iOS text views resolve each paragraph's direction from its
    // first strong character (TEXT_DIRECTION_FIRST_STRONG / natural
    // alignment), so an Arabic-led paragraph lays out RTL, right-aligned.
    final rtl = _nativeMarkdown && _firstStrongIsRtl(plain);
    var found = false;
    final target = find;
    if (target != null) {
      final match = _nextFindMatch(target, plain);
      if (match != null) {
        colored = highlightSpanRange(colored, match.start, match.end, findStyle!);
        found = true;
      }
    }
    final hover = hoverLinks && hasLinks;
    if (recolor == null && ranges.isEmpty && !hover && !found && !rtl) return text;
    final Text shaped = recolor == null && !found && !rtl
        ? text
        : Text.rich(
            colored,
            key: text.key,
            style: text.style,
            strutStyle: text.strutStyle,
            textAlign: text.textAlign,
            textDirection: rtl ? TextDirection.rtl : text.textDirection,
            locale: text.locale,
            softWrap: text.softWrap,
            overflow: text.overflow,
            textScaler: text.textScaler,
            maxLines: text.maxLines,
            semanticsLabel: text.semanticsLabel,
            textWidthBasis: text.textWidthBasis,
            textHeightBehavior: text.textHeightBehavior,
            selectionColor: text.selectionColor,
          );
    final Widget linked = hover ? _HoverLinkText(text: shaped, tooltip: linkTooltip) : shaped;
    if (ranges.isEmpty) return linked;
    return _InlineCodeChips(ranges: ranges, color: palette.surface2, child: linked);
  }

  static final _strongLetter = RegExp(r'\p{L}', unicode: true);

  /// Unicode bidi "first strong" for paragraph text: the first letter decides,
  /// RTL when it belongs to a right-to-left script block.
  static bool _firstStrongIsRtl(String text) {
    final first = _strongLetter.firstMatch(text);
    if (first == null) return false;
    final rune = first.group(0)!.runes.first;
    return (rune >= 0x0590 && rune <= 0x08FF) ||
        (rune >= 0xFB1D && rune <= 0xFDFF) ||
        (rune >= 0xFE70 && rune <= 0xFEFF) ||
        (rune >= 0x10800 && rune <= 0x10FFF) ||
        (rune >= 0x1E800 && rune <= 0x1EFFF);
  }

  static InlineSpan _recolor(InlineSpan span, Color color) {
    if (span is! TextSpan) return span;
    return TextSpan(
      text: span.text,
      children: span.children?.map((child) => _recolor(child, color)).toList(),
      style: (span.style ?? const TextStyle()).copyWith(color: color),
      recognizer: span.recognizer,
      mouseCursor: span.mouseCursor,
      onEnter: span.onEnter,
      onExit: span.onExit,
      semanticsLabel: span.semanticsLabel,
      locale: span.locale,
      spellOut: span.spellOut,
    );
  }
}

/// renderers.web.tsx WebLink: a hovered link underlines (LINK_HOVER_STYLE)
/// and, after a 400ms delay, a tooltip above it names the resolved path
/// (sm foreground text, 520px max width).
class _HoverLinkText extends StatefulWidget {
  const _HoverLinkText({required this.text, this.tooltip});
  final Text text;
  final String? Function(String linkText)? tooltip;

  @override
  State<_HoverLinkText> createState() => _HoverLinkTextState();
}

class _HoverLinkTextState extends State<_HoverLinkText> {
  GestureRecognizer? hovered;
  Timer? _delay;
  OverlayEntry? _tip;

  @override
  void dispose() {
    _hideTip();
    super.dispose();
  }

  void _hideTip() {
    _delay?.cancel();
    _delay = null;
    _tip?.remove();
    _tip = null;
  }

  void _enter(GestureRecognizer recognizer, String linkText, PointerEnterEvent event) {
    if (hovered == recognizer) return;
    _hideTip();
    setState(() => hovered = recognizer);
    final message = widget.tooltip?.call(linkText);
    if (message == null || message.isEmpty) return;
    final anchor = event.position;
    _delay = Timer(const Duration(milliseconds: 400), () {
      if (!mounted || hovered != recognizer) return;
      final overlay = Overlay.maybeOf(context, rootOverlay: true);
      if (overlay == null) return;
      final palette = PaseoColors.of(context);
      final theme = Theme.of(context);
      final screen = MediaQuery.sizeOf(context);
      _tip = OverlayEntry(
        builder: (_) => Positioned(
          left: anchor.dx.clamp(8, screen.width - 8),
          bottom: screen.height - anchor.dy + 12,
          child: IgnorePointer(
            child: ConstrainedBox(
              constraints: BoxConstraints(maxWidth: math.min(520.0, screen.width - 16)),
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: palette.surface1,
                  border: Border.all(color: palette.borderAccent),
                  borderRadius: BorderRadius.circular(6),
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
                  child: Text(
                    message,
                    style: theme.textTheme.bodySmall!.copyWith(
                      color: theme.colorScheme.onSurface,
                      decoration: TextDecoration.none,
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      );
      overlay.insert(_tip!);
    });
  }

  void _exit(GestureRecognizer recognizer) {
    if (hovered != recognizer) return;
    _hideTip();
    setState(() => hovered = null);
  }

  @override
  Widget build(BuildContext context) {
    final source = widget.text.textSpan!;
    // Every span of one link shares its recognizer; join their text so the
    // tooltip can look up the link's target.
    final texts = <GestureRecognizer, StringBuffer>{};
    void collect(InlineSpan span) {
      if (span is! TextSpan) return;
      final recognizer = span.recognizer;
      if (recognizer != null) (texts[recognizer] ??= StringBuffer()).write(span.text ?? '');
      span.children?.forEach(collect);
    }

    collect(source);
    InlineSpan rewrite(InlineSpan span) {
      if (span is! TextSpan) return span;
      final recognizer = span.recognizer;
      final style = recognizer != null && recognizer == hovered
          ? (span.style ?? const TextStyle()).copyWith(
              decoration: TextDecoration.underline,
              decorationColor: span.style?.color,
            )
          : span.style;
      return TextSpan(
        text: span.text,
        children: span.children?.map(rewrite).toList(),
        style: style,
        recognizer: recognizer,
        mouseCursor: recognizer != null ? SystemMouseCursors.click : span.mouseCursor,
        onEnter: recognizer == null
            ? span.onEnter
            : (event) => _enter(recognizer, texts[recognizer].toString(), event),
        onExit: recognizer == null ? span.onExit : (_) => _exit(recognizer),
        semanticsLabel: span.semanticsLabel,
        locale: span.locale,
        spellOut: span.spellOut,
      );
    }

    final text = widget.text;
    return Text.rich(
      rewrite(source),
      key: text.key,
      style: text.style,
      strutStyle: text.strutStyle,
      textAlign: text.textAlign,
      textDirection: text.textDirection,
      locale: text.locale,
      softWrap: text.softWrap,
      overflow: text.overflow,
      textScaler: text.textScaler,
      maxLines: text.maxLines,
      semanticsLabel: text.semanticsLabel,
      textWidthBasis: text.textWidthBasis,
      textHeightBehavior: text.textHeightBehavior,
      selectionColor: text.selectionColor,
    );
  }
}

/// Inline code backgrounds. Native CodeBackgroundSpan fills each line's full
/// height (top to bottom of the line) with 6 device-pixel corners on the
/// span's outer ends; web `<code>` fills the glyph box plus its 1px/4px
/// padding and transparent 1px border with 3px corners.
class _InlineCodeChips extends SingleChildRenderObjectWidget {
  const _InlineCodeChips({required this.ranges, required this.color, required Widget super.child});

  final List<TextRange> ranges;
  final Color color;

  @override
  _RenderInlineCodeChips createRenderObject(BuildContext context) => _RenderInlineCodeChips(
    ranges,
    color,
    MediaQuery.devicePixelRatioOf(context),
  );

  @override
  void updateRenderObject(BuildContext context, _RenderInlineCodeChips renderObject) {
    renderObject
      ..ranges = ranges
      ..color = color
      ..devicePixelRatio = MediaQuery.devicePixelRatioOf(context);
  }
}

class _RenderInlineCodeChips extends RenderProxyBox {
  _RenderInlineCodeChips(this._ranges, this._color, this._devicePixelRatio);

  List<TextRange> _ranges;
  set ranges(List<TextRange> value) {
    _ranges = value;
    markNeedsPaint();
  }

  Color _color;
  set color(Color value) {
    if (value == _color) return;
    _color = value;
    markNeedsPaint();
  }

  double _devicePixelRatio;
  set devicePixelRatio(double value) {
    if (value == _devicePixelRatio) return;
    _devicePixelRatio = value;
    markNeedsPaint();
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    // Text's RichText sits under at most proxy wrappers (its selection
    // MouseRegion), which share this box's origin.
    RenderObject? node = child;
    while (node is RenderProxyBox) {
      node = node.child;
    }
    if (node is RenderParagraph) _paintChips(context.canvas, node, offset);
    super.paint(context, offset);
  }

  void _paintChips(Canvas canvas, RenderParagraph paragraph, Offset offset) {
    final native = _nativeMarkdown;
    final paint = Paint()..color = _color;
    final radius = Radius.circular(native ? 6 / _devicePixelRatio : 3);
    for (final range in _ranges) {
      final lines = <Rect>[];
      for (final box in paragraph.getBoxesForSelection(
        TextSelection(baseOffset: range.start, extentOffset: range.end),
        boxHeightStyle: native ? ui.BoxHeightStyle.max : ui.BoxHeightStyle.tight,
      )) {
        final rect = box.toRect();
        if (lines.isNotEmpty && (lines.last.center.dy - rect.center.dy).abs() < rect.height / 2) {
          lines.last = lines.last.expandToInclude(rect);
        } else {
          lines.add(rect);
        }
      }
      for (var i = 0; i < lines.length; i++) {
        final first = i == 0, last = i == lines.length - 1;
        var rect = lines[i].shift(offset);
        if (!native) {
          rect = Rect.fromLTRB(
            rect.left - (first ? 5 : 0),
            rect.top - 2,
            rect.right + (last ? 5 : 0),
            rect.bottom + 2,
          );
        }
        canvas.drawRRect(
          RRect.fromRectAndCorners(
            rect,
            topLeft: first ? radius : Radius.zero,
            bottomLeft: first ? radius : Radius.zero,
            topRight: last ? radius : Radius.zero,
            bottomRight: last ? radius : Radius.zero,
          ),
          paint,
        );
      }
    }
  }
}

/// Native list spans (BaseListSpan, UnorderedListSpan, OrderedListSpan,
/// TaskListSpan) with paseo's list style: a top-level item's text starts
/// `floor(markerWidth + gapWidth 7)` from the list edge, where markerWidth is
/// the 3px bullet radius, the width of "99." in weight 500, or the 14px
/// checkbox. Nested lists indent their text marginLeft 20 per level and place
/// the marker at `depth * 20 + markerWidth` from the outermost list edge.
/// Bullets are 6px discs (rings at depth 1, squares deeper) centred on the
/// font's x-middle; numbers are right-aligned muted weight-500 text; checkboxes
/// are 3px-radius squares centred on the cap height.
class _ListMarker extends StatelessWidget {
  const _ListMarker({
    required this.kind,
    required this.style,
    this.index = 0,
    this.depth = 0,
    this.root,
  });

  final _Marker kind;
  final TextStyle style;
  final int index, depth;
  final _Marker? root;

  static const _gap = 7.0, _nestIndent = 20.0, _bullet = 6.0, _checkbox = 14.0;

  _ListMarker placed({required int depth, required _Marker root}) =>
      _ListMarker(kind: kind, style: style, index: index, depth: depth, root: root);

  static double _markerWidth(_Marker kind, TextStyle base, TextScaler scaler) {
    switch (kind) {
      case _Marker.bullet:
        return _bullet / 2;
      case _Marker.task || _Marker.taskChecked:
        return _checkbox;
      case _Marker.ordered:
        final painter = TextPainter(
          text: TextSpan(text: '99.', style: base.copyWith(fontWeight: FontWeight.w500)),
          textDirection: TextDirection.ltr,
          textScaler: scaler,
        )..layout();
        final width = painter.width;
        painter.dispose();
        return width;
    }
  }

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final scaler = MediaQuery.textScalerOf(context);
    final base = DefaultTextStyle.of(context).style.merge(style);
    final color = base.color ?? colors.onSurfaceVariant;
    final strut = TextPainter(
      text: TextSpan(text: ' ', style: base),
      textDirection: TextDirection.ltr,
      textScaler: scaler,
    )..layout();
    final baseline = strut.computeDistanceToActualBaseline(TextBaseline.alphabetic);
    strut.dispose();
    // Font metrics without the 1.4 line height.
    final natural = TextPainter(
      text: TextSpan(
        text: ' ',
        style: TextStyle(
          fontSize: base.fontSize,
          fontFamily: base.fontFamily,
          fontFamilyFallback: base.fontFamilyFallback,
          fontWeight: base.fontWeight,
        ),
      ),
      textDirection: TextDirection.ltr,
      textScaler: scaler,
    )..layout();
    final metrics = natural.computeLineMetrics();
    natural.dispose();
    final ascent = metrics.isEmpty ? baseline : metrics.first.ascent;
    final descent = metrics.isEmpty ? 0.0 : metrics.first.descent;

    final markerWidth = _markerWidth(kind, base, scaler);
    final double column, anchor;
    if (depth == 0) {
      column = (markerWidth + _gap).floorToDouble();
      anchor = markerWidth;
    } else {
      final rootMargin = (_markerWidth(root ?? kind, base, scaler) + _gap).floorToDouble();
      column = _nestIndent;
      anchor = _nestIndent + markerWidth - rootMargin;
    }

    final Widget mark;
    switch (kind) {
      case _Marker.ordered:
        mark = Positioned(
          top: 0,
          right: column - anchor,
          child: Text(
            '${index + 1}.',
            softWrap: false,
            style: style.copyWith(fontWeight: FontWeight.w500),
          ),
        );
      case _Marker.bullet:
        final ring = depth == 1;
        final centerY = baseline - (ascent - descent) / 2;
        mark = Positioned(
          left: anchor - _bullet / 2,
          top: centerY - _bullet / 2,
          child: Container(
            width: _bullet,
            height: _bullet,
            decoration: BoxDecoration(
              shape: depth > 1 ? BoxShape.rectangle : BoxShape.circle,
              color: ring ? null : color,
              border: ring ? Border.all(color: color, width: _bullet / 2 * .3) : null,
            ),
          ),
        );
      case _Marker.task || _Marker.taskChecked:
        final centerY = baseline - ascent * .72 / 2;
        mark = Positioned(
          left: anchor - _checkbox,
          top: centerY - _checkbox / 2,
          child: CustomPaint(
            size: const Size.square(_checkbox),
            painter: _CheckboxPainter(
              checked: kind == _Marker.taskChecked,
              fill: palette.accent,
              check: colors.onTertiary,
              border: colors.onSurfaceVariant,
            ),
          ),
        );
    }
    return SelectionContainer.disabled(
      child: SizedBox(
        width: column,
        child: Stack(
          clipBehavior: Clip.none,
          children: [
            // Supplies the row baseline and the line height.
            Text(' ', softWrap: false, style: style.copyWith(color: Colors.transparent)),
            mark,
          ],
        ),
      ),
    );
  }
}

/// TaskListSpan checkbox: checked fills the accent with a rounded
/// accent-foreground check (stroke max(1.5, 12%), inset 22%); unchecked is a
/// foregroundMuted outline (stroke max(1, 9%)); 3px corners.
class _CheckboxPainter extends CustomPainter {
  const _CheckboxPainter({
    required this.checked,
    required this.fill,
    required this.check,
    required this.border,
  });

  final bool checked;
  final Color fill, check, border;

  @override
  void paint(Canvas canvas, Size size) {
    final rect = Offset.zero & size;
    const radius = Radius.circular(3);
    final extent = size.width;
    if (checked) {
      canvas.drawRRect(RRect.fromRectAndRadius(rect, radius), Paint()..color = fill);
      final inset = extent * .22, mid = extent * .05;
      canvas.drawPath(
        Path()
          ..moveTo(rect.left + inset, rect.center.dy)
          ..lineTo(rect.center.dx - mid, rect.bottom - inset)
          ..lineTo(rect.right - inset, rect.top + inset),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = math.max(1.5, extent * .12)
          ..strokeCap = StrokeCap.round
          ..strokeJoin = StrokeJoin.round
          ..color = check,
      );
    } else {
      final stroke = math.max(1.0, extent * .09);
      canvas.drawRRect(
        RRect.fromRectAndRadius(rect.deflate(stroke / 2), radius),
        Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = stroke
          ..strokeJoin = StrokeJoin.round
          ..color = border,
      );
    }
  }

  @override
  bool shouldRepaint(_CheckboxPainter old) =>
      old.checked != checked || old.fill != fill || old.check != check || old.border != border;
}

class _FenceBuilder extends MarkdownElementBuilder {
  @override
  bool isBlockElement() => true;

  @override
  Widget? visitText(text, preferredStyle) => null;

  // Parameter types are inferred from the override so the transitive
  // `markdown` package needs no direct import. A `pre` element always wraps a
  // single `code` element carrying the fence's `language-*` class.
  @override
  Widget? visitElementAfterWithContext(context, element, preferredStyle, parentStyle) {
    final dynamic code = element.children?.firstOrNull;
    final Map<String, String>? attributes = code?.attributes as Map<String, String>?;
    final language = (attributes?['class'] ?? '').replaceFirst('language-', '');
    return MarkdownCodeBlock(code: element.textContent, language: language);
  }
}

/// Native (Android/iOS): react-native-enriched-markdown's
/// CodeBlockContainerView — surface2, 1px border, 8px radius; a header bar
/// (display language name, copy glyph) over a 1px divider, then a
/// horizontally scrolling, non-wrapping code pane with 12px padding at the
/// paragraph's 1.4 line height. Web (desktop) and every mermaid fence:
/// components/highlighted-code-block.tsx — surface2, no border, 8px radius,
/// 12px padding, 1.45 code line height, wrapped (RN-web Text pre-wrap) and a
/// 14px copy glyph 8px from the top-right corner, always visible on
/// compact layouts and on hover otherwise. Mermaid fences stay on this
/// source view because diagrams need the original's WebView runtime; it is
/// the original's pre-render / failed-render state.
class MarkdownCodeBlock extends StatefulWidget {
  const MarkdownCodeBlock({super.key, required this.code, this.language = '', this.findMatch});
  final String code, language;

  /// The chat-find current occurrence inside the rendered code, if any.
  final TextRange? findMatch;

  @override
  State<MarkdownCodeBlock> createState() => _MarkdownCodeBlockState();
}

class _MarkdownCodeBlockState extends State<MarkdownCodeBlock> {
  bool hovered = false, copied = false, copyHovered = false;
  Timer? _reset;

  static const _aliases = {
    'ts': 'typescript',
    'tsx': 'typescript',
    'js': 'javascript',
    'jsx': 'javascript',
    'py': 'python',
    'rs': 'rust',
    'sh': 'bash',
    'shell': 'bash',
    'zsh': 'bash',
    'console': 'bash',
    'yml': 'yaml',
    'golang': 'go',
    'c++': 'cpp',
    'c#': 'cs',
    'csharp': 'cs',
    'objc': 'objectivec',
    'objective-c': 'objectivec',
    'md': 'markdown',
    'html': 'xml',
    'kt': 'kotlin',
    'rb': 'ruby',
  };

  @override
  void dispose() {
    _reset?.cancel();
    super.dispose();
  }

  /// The last parse and what it parsed: a rebuild (hover, copy feedback, a
  /// store or parent update) reuses it instead of re-running the highlighter.
  ({String code, String language, List<syntax.Node>? nodes})? _parsed;

  List<syntax.Node>? _highlight(String code) {
    final parsed = _parsed;
    if (parsed != null && parsed.code == code && parsed.language == widget.language) {
      return parsed.nodes;
    }
    final nodes = _parse(code);
    _parsed = (code: code, language: widget.language, nodes: nodes);
    return nodes;
  }

  List<syntax.Node>? _parse(String code) {
    final first = widget.language.trim().split(RegExp(r'\s+')).first.toLowerCase().replaceFirst(RegExp(r'^\.'), '');
    if (first.isEmpty || code.length > 100000) return null;
    try {
      return syntax.highlight.parse(code, language: _aliases[first] ?? first).nodes;
    } catch (_) {
      return null;
    }
  }

  Future<void> _copy() async {
    final text = widget.code.replaceFirst(RegExp(r'\n+$'), '');
    if (text.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: text));
    if (!mounted) return;
    setState(() => copied = true);
    _reset?.cancel();
    _reset = Timer(const Duration(milliseconds: 1500), () {
      if (mounted) setState(() => copied = false);
    });
  }

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final typography = PaseoTypography.of(context);
    final compact = isCompactLayout(context);
    final dark = Theme.of(context).brightness == Brightness.dark;
    final syntaxTheme = PaseoScope.select(
      context,
      (store) => stringValue(store.preferences['syntaxTheme'], 'one'),
    );
    final code = widget.code.endsWith('\n')
        ? widget.code.substring(0, widget.code.length - 1)
        : widget.code;
    final nodes = _highlight(code);
    TextSpan span(syntax.Node node) => TextSpan(
      text: node.value,
      style: TextStyle(color: syntaxColor(syntaxTheme, dark, node.className)),
      children: node.children?.map(span).toList(),
    );
    final native = _nativeMarkdown && widget.language.trim().toLowerCase() != 'mermaid';
    final lineHeight = native
        ? (typography.contentFontSize * 1.4).roundToDouble()
        : (typography.codeFontSize * 1.45).roundToDouble();
    final style = TextStyle(
      fontFamily: typography.monoFontFamily,
      fontFamilyFallback: typography.monoFontFamilyFallback,
      fontSize: typography.codeFontSize,
      height: lineHeight / typography.codeFontSize,
      color: colors.onSurface,
    );
    final root = TextSpan(children: nodes == null ? [TextSpan(text: code)] : nodes.map(span).toList());
    final match = widget.findMatch;
    final highlighted = match == null
        ? root
        : highlightSpanRange(root, match.start, match.end, chatFindHighlightStyle(context));
    if (native) return _nativeBlock(context, highlighted, style);
    final text = Text.rich(highlighted, softWrap: true, style: style);
    final visible = compact || hovered || _nativeMarkdown;
    return MouseRegion(
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: Container(
        width: double.infinity,
        decoration: BoxDecoration(
          color: palette.surface2,
          borderRadius: BorderRadius.circular(8),
        ),
        clipBehavior: Clip.antiAlias,
        child: Stack(
          children: [
            Padding(
              padding: const EdgeInsets.all(12),
              child: text,
            ),
            Positioned(
              top: 8,
              right: 8,
              child: IgnorePointer(
                ignoring: !visible,
                child: Opacity(
                  opacity: visible ? 1 : 0,
                  child: Tooltip(
                    message: tr(context, copied ? 'message.actions.copied' : 'message.actions.copyCode'),
                    child: MouseRegion(
                      cursor: SystemMouseCursors.click,
                      onEnter: (_) => setState(() => copyHovered = true),
                      onExit: (_) => setState(() => copyHovered = false),
                      child: GestureDetector(
                        behavior: HitTestBehavior.opaque,
                        onTap: _copy,
                        child: Padding(
                          padding: const EdgeInsets.all(4),
                          child: Icon(
                            copied ? LucideIcons.check300 : LucideIcons.copy300,
                            size: 14,
                            color: copyHovered ? colors.onSurface : colors.onSurfaceVariant,
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  /// CodeBlockContainerView geometry: contentInset = ceil(padding 12 +
  /// border 1) = 13; the header is that inset plus one label line (code size
  /// × 0.85, sans-serif-medium); the label and copy glyph center between the
  /// box top and the divider, which sits halfway into the code pane's top
  /// inset. Secondary text is the code color at 0x99 alpha, the divider at
  /// 0x33. Children lay out inside the 1px border.
  Widget _nativeBlock(BuildContext context, InlineSpan code, TextStyle style) {
    const inset = 13.0, border = 1.0;
    final colors = Theme.of(context).colorScheme;
    final typography = PaseoTypography.of(context);
    final labelSize = typography.codeFontSize * .85;
    // Roboto's ascent + descent (includeFontPadding off).
    final headerHeight = inset + (labelSize * 1.171875).ceilToDouble();
    final iconSize = (typography.codeFontSize * 1.08).ceilToDouble();
    final secondary = colors.onSurface.withValues(alpha: 0x99 / 0xff);
    final dividerTop = headerHeight + inset / 2 - .5 - border;
    return Container(
      width: double.infinity,
      decoration: BoxDecoration(
        color: PaseoColors.of(context).surface2,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: colors.outline, width: border),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          SizedBox(
            height: dividerTop,
            child: SelectionContainer.disabled(
              child: Row(
                children: [
                  const SizedBox(width: inset - border),
                  Expanded(
                    child: Text(
                      _displayLanguageName(widget.language),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontSize: labelSize,
                        fontWeight: FontWeight.w500,
                        height: 1.171875,
                        color: secondary,
                      ),
                    ),
                  ),
                  Semantics(
                    button: true,
                    label: tr(context, 'message.actions.copyCode'),
                    excludeSemantics: true,
                    child: GestureDetector(
                      behavior: HitTestBehavior.opaque,
                      onTap: _copy,
                      child: SizedBox(
                        width: headerHeight,
                        height: dividerTop,
                        child: Align(
                          alignment: Alignment.centerRight,
                          child: Icon(LucideIcons.copy300, size: iconSize, color: secondary),
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(width: inset - border),
                ],
              ),
            ),
          ),
          ColoredBox(
            color: colors.onSurface.withValues(alpha: 0x33 / 0xff),
            child: const SizedBox(height: 1),
          ),
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: EdgeInsets.fromLTRB(
              inset - border,
              headerHeight + inset - border - dividerTop - 1,
              inset - border,
              inset,
            ),
            child: Text.rich(
              code,
              softWrap: false,
              textDirection: TextDirection.ltr,
              style: style,
            ),
          ),
        ],
      ),
    );
  }

  /// cpp/highlight/CodeBlockLanguages.cpp displayNameForLanguage: known
  /// fence languages map to display names; any other is lowercased with its
  /// first letter capitalized; no language shows an empty label.
  static String _displayLanguageName(String info) {
    final language = info.trim().split(RegExp(r'\s+')).first;
    if (language.isEmpty) return '';
    final lower = language.toLowerCase();
    return _languageNames[lower] ?? lower[0].toUpperCase() + lower.substring(1);
  }

  static const _languageNames = {
    'bash': 'Bash', 'c': 'C', 'cc': 'C++', 'cpp': 'C++', 'cs': 'C#',
    'csharp': 'C#', 'css': 'CSS', 'cxx': 'C++', 'dockerfile': 'Dockerfile',
    'go': 'Go', 'golang': 'Go', 'graphql': 'GraphQL', 'html': 'HTML',
    'java': 'Java', 'javascript': 'JavaScript', 'js': 'JavaScript',
    'json': 'JSON', 'jsx': 'JSX', 'markdown': 'Markdown', 'md': 'Markdown',
    'objc': 'Objective-C', 'objectivec': 'Objective-C', 'php': 'PHP',
    'py': 'Python', 'python': 'Python', 'rb': 'Ruby', 'rs': 'Rust',
    'ruby': 'Ruby', 'rust': 'Rust', 'scss': 'SCSS', 'sh': 'Shell',
    'shell': 'Shell', 'sql': 'SQL', 'swift': 'Swift', 'toml': 'TOML',
    'ts': 'TypeScript', 'tsx': 'TSX', 'typescript': 'TypeScript',
    'xml': 'XML', 'yaml': 'YAML', 'yml': 'YAML', 'zsh': 'Zsh',
  };
}
