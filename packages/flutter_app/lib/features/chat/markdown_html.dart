import 'dart:math' as math;

import 'package:markdown/markdown.dart' as md;

/// Original `components/markdown/html.ts` normalizeMarkdownHtml: comment HTML
/// (GitHub review bodies, agent `<details>` blocks) becomes Markdown before
/// rendering, so no content is dropped by the renderer and code keeps its
/// original spelling.
///
/// The original locates HTML through the CommonMark tree; here fenced code,
/// inline code spans and inline link destinations/titles are protected the
/// same way, and every tag outside them is converted.
String normalizeMarkdownHtml(String source) {
  if (!source.contains('<')) return source;
  final protected = _protectedRanges(source);
  final links = <String?>[];
  ({String name, int end})? previousTag;

  String convert(String raw, int offset) {
    if (raw.startsWith('<!--')) return '';
    final summary = _summary.firstMatch(raw);
    if (summary != null) {
      final title = normalizeMarkdownHtml(summary[1]!).trim();
      // A summary is one bold paragraph, even when its HTML title uses
      // headings or several block wrappers.
      final inline = title
          .split(RegExp(r'\n[ \t]*\n'))
          .map((block) => block.trim())
          .where((block) => block.isNotEmpty)
          .join(' ');
      return '**$inline**\n\n';
    }
    final code = _code.firstMatch(raw);
    if (code != null) return _inlineCode(_decodeEntities(code[1]!));
    final closing = raw.startsWith('</');
    final name = _tagName.firstMatch(raw)?[1]!.toLowerCase() ?? '';
    final previous = previousTag;
    final startsAfterBoundary = previous != null
        ? _blockTags.contains(previous.name) &&
              _blank.hasMatch(source.substring(previous.end, offset))
        : _blank.hasMatch(source.substring(0, offset));
    previousTag = (name: name, end: offset + raw.length);
    final emphasis = _emphasis[name];
    if (emphasis != null) return emphasis;
    final boundary = _layoutBoundary(
      name,
      source,
      raw,
      offset,
      closing,
      startsAfterBoundary,
    );
    if (boundary != null) return boundary;
    if (name == 'a') return _markdownAnchor(raw, closing, links);
    if (name == 'img' && !closing) {
      final attributes = _attributes(raw);
      final src = attributes['src'];
      final alt = attributes['alt'] ?? '';
      return src != null
          ? '![${alt.replaceAllMapped(RegExp(r'[\[\]\\]'), (m) => '\\${m[0]}')}](<${_markdownUrl(src)}>)'
          : alt;
    }
    return '';
  }

  final result = StringBuffer();
  var cursor = 0;
  for (final match in _html.allMatches(source)) {
    if (match.start < cursor) continue;
    if (protected.any((r) => match.start >= r.start && match.start < r.end)) {
      continue;
    }
    result
      ..write(source.substring(cursor, match.start))
      ..write(convert(match[0]!, match.start));
    cursor = match.end;
  }
  result.write(source.substring(cursor));
  return result.toString();
}

const _attribute =
    r'''\s+[A-Za-z_:][\w.:-]*(?:\s*=\s*(?:[^\s"'=<>`]+|'[^']*'|"[^"]*"))?''';
const _tag =
    '<(?:[A-Za-z][A-Za-z0-9-]*(?:$_attribute)*\\s*\\/?|\\/[A-Za-z][A-Za-z0-9-]*\\s*)>';
final _html = RegExp(
  r'(?<!\\)(?:<!--[\s\S]*?-->|<code\b[^>]*>[\s\S]*?<\/code\s*>|<summary\b[^>]*>[\s\S]*?<\/summary\s*>|'
  '$_tag)',
  caseSensitive: false,
);
final _summary = RegExp(
  r'^<summary\b[^>]*>([\s\S]*?)<\/summary\s*>$',
  caseSensitive: false,
);
final _code = RegExp(
  r'^<code\b[^>]*>([\s\S]*?)<\/code\s*>$',
  caseSensitive: false,
);
final _tagName = RegExp(r'^<\/?([\w-]+)');
final _blank = RegExp(r'^\s*$');
const _blockTags = {
  'div', 'p', 'table', 'tr', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', //
  'section', 'article', 'ul', 'ol', 'li', 'blockquote', 'pre',
};
const _emphasis = {'b': '**', 'strong': '**', 'i': '*', 'em': '*'};

/// Fenced code blocks, inline code spans and inline link destinations/titles,
/// which CommonMark never parses as HTML.
List<({int start, int end})> _protectedRanges(String source) {
  final ranges = <({int start, int end})>[];
  final fence = RegExp(r'^[ \t>]*(`{3,}|~{3,})');
  var offset = 0;
  String? open;
  var openStart = 0;
  for (final line in source.split('\n')) {
    final match = fence.firstMatch(line);
    if (open == null) {
      if (match != null &&
          !(match[1]![0] == '`' && line.substring(match.end).contains('`'))) {
        open = match[1];
        openStart = offset;
      }
    } else if (match != null &&
        match[1]![0] == open[0] &&
        match[1]!.length >= open.length &&
        line.substring(match.end).trim().isEmpty) {
      ranges.add((start: openStart, end: offset + line.length));
      open = null;
    }
    offset += line.length + 1;
  }
  if (open != null) ranges.add((start: openStart, end: source.length));

  bool fenced(int index) =>
      ranges.any((r) => index >= r.start && index < r.end);

  // Inline code: a backtick run closes at the next run of the same length.
  final runs = RegExp(r'(?<!\\)`+').allMatches(source).toList();
  for (var i = 0; i < runs.length; i++) {
    final run = runs[i];
    if (fenced(run.start)) continue;
    final length = run.end - run.start;
    for (var j = i + 1; j < runs.length; j++) {
      final candidate = runs[j];
      if (fenced(candidate.start)) break;
      if (candidate.end - candidate.start == length) {
        ranges.add((start: run.start, end: candidate.end));
        i = j;
        break;
      }
    }
  }
  final destinations = RegExp(
    r'''\]\(\s*(?:<[^>\n]*>|[^\s()<]+)?(?:\s+(?:"[^"]*"|'[^']*'|\([^)]*\)))?\s*\)''',
  );
  for (final match in destinations.allMatches(source)) {
    if (!fenced(match.start)) {
      ranges.add((start: match.start, end: match.end));
    }
  }
  return ranges;
}

String? _layoutBoundary(
  String name,
  String source,
  String raw,
  int offset,
  bool closing,
  bool startsAfterBoundary,
) {
  if (name == 'br') return closing ? '' : '  \n';
  if (name == 'summary') return closing ? '**\n\n' : '**';
  if (name == 'details') {
    return (closing ? offset + raw.length == source.length : offset == 0)
        ? ''
        : '\n\n';
  }
  if (_blockTags.contains(name)) {
    return _blockBoundary(source, raw, offset, closing, startsAfterBoundary);
  }
  if ((name == 'td' || name == 'th') && closing) {
    return RegExp(r'^<t[dh]\b', caseSensitive: false)
            .hasMatch(source.substring(offset + raw.length))
        ? ' '
        : '';
  }
  return null;
}

String _blockBoundary(
  String source,
  String raw,
  int offset,
  bool closing,
  bool startsAfterBoundary,
) {
  if (closing) {
    final following = source.substring(offset + raw.length);
    if (_blank.hasMatch(following) || following.startsWith('\n\n')) return '';
    final nextClosing = RegExp(r'^\s*<\/([\w-]+)').firstMatch(following);
    if (nextClosing != null &&
        _blockTags.contains(nextClosing[1]!.toLowerCase())) {
      return '';
    }
  } else if (startsAfterBoundary) {
    return '';
  }
  return '\n\n';
}

String _markdownAnchor(String raw, bool closing, List<String?> links) {
  if (closing) {
    if (links.isEmpty) return '';
    final href = links.removeLast();
    return href == null ? '' : '](<${_markdownUrl(href)}>)';
  }
  final href = _attributes(raw)['href'];
  links.add(href);
  return href == null ? '' : '[';
}

String _markdownUrl(String url) =>
    url.replaceAllMapped(RegExp(r'[<>\r\n]'), (m) => Uri.encodeComponent(m[0]!));

String _inlineCode(String content) {
  final longest = RegExp(r'`+')
      .allMatches(content)
      .fold(0, (longest, run) => math.max(longest, run.end - run.start));
  final marker = '`' * (longest + 1);
  final padding = content.startsWith('`') || content.endsWith('`') ? ' ' : '';
  return '$marker$padding$content$padding$marker';
}

/// htmlparser2 `onopentag` attributes: lower-cased names, first value wins,
/// entity-decoded values.
Map<String, String> _attributes(String tag) {
  final name = RegExp(r'^<[A-Za-z][A-Za-z0-9-]*').firstMatch(tag);
  if (name == null) return const {};
  final result = <String, String>{};
  final attribute = RegExp(
    r'''([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?''',
  );
  for (final match in attribute.allMatches(tag.substring(name.end))) {
    result.putIfAbsent(
      match[1]!.toLowerCase(),
      () => _decodeEntities(match[2] ?? match[3] ?? match[4] ?? ''),
    );
  }
  return result;
}

/// micromark `decodeString` for entity and numeric character references.
final _entityDecoder = md.Document(
  extensionSet: md.ExtensionSet.none,
  inlineSyntaxes: [md.DecodeHtmlSyntax()],
  withDefaultInlineSyntaxes: false,
  encodeHtml: false,
);

String _decodeEntities(String text) {
  if (!text.contains('&')) return text;
  return _entityDecoder.parseInline(text).map((node) => node.textContent).join();
}
