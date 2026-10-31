import 'dart:async';

import 'package:flutter/widgets.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../files/file_sheet.dart';

/// Port of the original `assistant-file-links` module (parse.ts, resolver.ts,
/// use-file-link.ts, path-open.ts): which assistant-message text, inline code
/// and markdown links name workspace files, and how a press opens them.

/// assistant-file-links/extensions.ts.
const assistantFileExtensions = {
  'astro', 'bash', 'c', 'cc', 'cjs', 'cpp', 'cs', 'css', 'cts', 'cxx', 'env', //
  'fish', 'go', 'gql', 'gradle', 'graphql', 'h', 'hpp', 'htm', 'html', 'ini', //
  'java', 'js', 'json', 'jsonc', 'jsx', 'kt', 'kts', 'less', 'lock', 'lua', 'md', //
  'mdx', 'mjs', 'mts', 'php', 'proto', 'py', 'rb', 'rs', 'sass', 'scss', 'sh', //
  'sql', 'svelte', 'swift', 'toml', 'ts', 'tsx', 'txt', 'vue', 'xml', 'yaml', //
  'yml', 'zsh',
};

/// parse.ts InlinePathTarget.
@immutable
class InlinePathTarget {
  const InlinePathTarget({required this.raw, required this.path, this.lineStart, this.lineEnd});
  final String raw, path;
  final int? lineStart, lineEnd;

  InlinePathTarget withPath(String path) =>
      InlinePathTarget(raw: raw, path: path, lineStart: lineStart, lineEnd: lineEnd);
}

/// Where a pressed link came from (markdown/source.ts markdownLinkSource):
/// an authored markdown link, linkified prose, or a complete inline-code span.
enum FileLinkSourceType { markdown, linkify, inlineCode }

/// resolver.ts AssistantFileLinkSource.
@immutable
class AssistantFileLinkSource {
  const AssistantFileLinkSource({required this.href, required this.type, this.text});
  final String href;
  final String? text;
  final FileLinkSourceType type;
}

typedef _Lines = ({int? start, int? end});

final _inlineLineFragment = RegExp(
  r'^L([0-9]+)(?:C[0-9]+)?(?:-L?([0-9]+)(?:C[0-9]+)?)?$',
  caseSensitive: false,
);
final _inlineColonLineSuffix = RegExp(r'^(.+?):([0-9]+)(?::[0-9]+)?(?:-([0-9]+)(?::[0-9]+)?)?$');
final _inlineParenLineSuffix = RegExp(r'^(.+?)\(([0-9]+)(?:,[0-9]+)?(?:-([0-9]+)(?:,[0-9]+)?)?\)$');
final _inlineWordLineSuffix = RegExp(r'^(.+?)\s+lines?\s+([0-9]+)(?:-([0-9]+))?$', caseSensitive: false);
final _driveAbsolute = RegExp(r'^[A-Za-z]:[\\/]');

/// utils/path.ts isAbsolutePath.
bool isAbsoluteFilePath(String value) =>
    value.startsWith('/') || value.startsWith(r'\\') || _driveAbsolute.hasMatch(value);

String _safeDecode(String value) {
  try {
    return Uri.decodeComponent(value);
  } catch (_) {
    return value;
  }
}

String? _normalizePathToken(String value) {
  final trimmed = value.trim().replaceFirst(RegExp('^[\'"`]'), '').replaceFirst(RegExp('[\'"`]\$'), '');
  if (trimmed.isEmpty) return null;
  return trimmed.replaceAll(r'\', '/');
}

_Lines? _parseLineFragment(String value) {
  final fragment = value.startsWith('#') ? value.substring(1) : value;
  if (fragment.isEmpty) return (start: null, end: null);
  final match = _inlineLineFragment.firstMatch(fragment);
  final start = match?.group(1) == null ? null : int.parse(match!.group(1)!);
  final end = match?.group(2) == null ? null : int.parse(match!.group(2)!);
  if ((start != null && start <= 0) ||
      (end != null && end <= 0) ||
      (start != null && end != null && end < start)) {
    return null;
  }
  return (start: start, end: end);
}

/// parse.ts parseInlinePathToken: strict `file:line`, `file:line:col`,
/// `file:start-end`, `file(line)` and `file line N` markers only.
InlinePathTarget? parseInlinePathToken(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) return null;
  final match = _inlineColonLineSuffix.firstMatch(trimmed) ??
      _inlineParenLineSuffix.firstMatch(trimmed) ??
      _inlineWordLineSuffix.firstMatch(trimmed);
  if (match == null) return null;
  final base = match.group(1)?.trim();
  if (base == null || base.isEmpty || base.contains('://')) return null;
  final path = _normalizePathToken(base);
  if (path == null) return null;
  final lineStart = int.parse(match.group(2)!);
  if (lineStart <= 0) return null;
  final lineEnd = match.group(3) == null ? null : int.parse(match.group(3)!);
  if (lineEnd != null && (lineEnd <= 0 || lineEnd < lineStart)) return null;
  return InlinePathTarget(raw: value, path: path, lineStart: lineStart, lineEnd: lineEnd);
}

String? _normalizeFileUrlPath(String pathname) {
  if (pathname.isEmpty) return null;
  final decoded = _safeDecode(pathname).replaceAll(r'\', '/');
  if (decoded.isEmpty) return null;
  if (RegExp(r'^/[A-Za-z]:/').hasMatch(decoded)) return decoded.substring(1);
  return decoded;
}

InlinePathTarget? _parseFileProtocolUrl(String value) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) return null;
  final uri = Uri.tryParse(trimmed);
  if (uri == null || uri.scheme != 'file') return null;
  final path = _normalizeFileUrlPath(uri.path);
  if (path == null) return null;
  final lines = _parseLineFragment(uri.hasFragment ? '#${uri.fragment}' : '');
  if (lines == null) return null;
  return InlinePathTarget(raw: value, path: path, lineStart: lines.start, lineEnd: lines.end);
}

InlinePathTarget? _parseAssistantInlinePathLink(String value, {bool decodeHref = false}) {
  final target = parseInlinePathToken(value);
  if (target == null) return null;
  final path = _normalizePathToken(decodeHref ? _safeDecode(target.path) : target.path);
  if (path == null || !isAbsoluteFilePath(path)) return null;
  return target.withPath(path);
}

sealed class FileLinkClassification {
  const FileLinkClassification();
}

class ExternalFileLink extends FileLinkClassification {
  const ExternalFileLink(this.raw);
  final String raw;
}

class DirectFileLink extends FileLinkClassification {
  const DirectFileLink(this.target);
  final InlinePathTarget target;
}

class AmbiguousFileLink extends FileLinkClassification {
  const AmbiguousFileLink(this.target);
  final InlinePathTarget target;
}

/// parse.ts classifyAssistantFileLink.
FileLinkClassification? classifyAssistantFileLink(
  String value, {
  String? workspaceRoot,
  bool decodeHref = false,
  bool explicitLink = false,
}) {
  final trimmed = value.trim();
  if (trimmed.isEmpty) return null;
  if (_isExternalHref(trimmed)) return ExternalFileLink(value);
  if (RegExp(r'\s').hasMatch(trimmed)) return null;
  final target = parseAssistantFileLink(
    trimmed,
    workspaceRoot: workspaceRoot,
    decodeHref: decodeHref,
    explicitLink: explicitLink,
  );
  if (target == null) return null;
  if (!explicitLink &&
      _isAmbiguousWorkspaceCandidate(trimmed, target, workspaceRoot: workspaceRoot, decodeHref: decodeHref)) {
    return AmbiguousFileLink(target);
  }
  return DirectFileLink(target);
}

/// parse.ts parseAssistantFileLink.
InlinePathTarget? parseAssistantFileLink(
  String value, {
  String? workspaceRoot,
  bool decodeHref = false,
  bool explicitLink = false,
}) {
  final fileUrl = _parseFileProtocolUrl(value);
  if (fileUrl != null) return fileUrl;
  final trimmed = value.trim();
  if (trimmed.isEmpty || _isExternalHref(trimmed)) return null;
  final inline = _parseAssistantInlinePathLink(trimmed, decodeHref: decodeHref);
  if (inline != null) return inline;
  final windows = _parseWindowsFileLink(trimmed, value, decodeHref: decodeHref);
  if (windows != null) return windows;
  final relative = _parseWorkspaceRelativeFileLink(
    trimmed,
    workspaceRoot: workspaceRoot,
    decodeHref: decodeHref,
    explicitLink: explicitLink,
  );
  if (relative != null) return relative;
  if (!isAbsoluteFilePath(trimmed)) return null;

  // Hrefs normalize `%`-escapes and dot segments like a URL; raw paths keep
  // their spelling (non-ASCII names must not be percent-encoded).
  String pathPart, hashPart;
  if (decodeHref) {
    final Uri resolved;
    try {
      resolved = Uri.parse('http://paseo.invalid').resolve(trimmed);
    } catch (_) {
      return null;
    }
    pathPart = _safeDecode(resolved.path);
    hashPart = resolved.hasFragment ? '#${resolved.fragment}' : '';
  } else {
    final hash = trimmed.indexOf('#');
    pathPart = hash >= 0 ? trimmed.substring(0, hash) : trimmed;
    hashPart = hash >= 0 ? trimmed.substring(hash) : '';
  }
  final path = _normalizePathToken(pathPart);
  if (path == null || !isAbsoluteFilePath(path)) return null;
  final lines = _parseLineFragment(hashPart);
  if (lines == null) return null;
  return InlinePathTarget(raw: value, path: path, lineStart: lines.start, lineEnd: lines.end);
}

/// parse.ts isFileLookingAssistantToken.
bool isFileLookingAssistantToken(String value) {
  final normalized = _normalizePathToken(value);
  if (normalized == null ||
      RegExp(r'\s').hasMatch(normalized) ||
      normalized.contains('?') ||
      normalized.contains('://')) {
    return false;
  }
  final path = _heuristicLocalPath(normalized);
  return path != null && _isPlausibleLocalPath(path);
}

InlinePathTarget? _parseWindowsFileLink(String trimmed, String raw, {bool decodeHref = false}) {
  final match = RegExp(r'^([A-Za-z]:[\\/][^?#]*)(#[^?]+)?$').firstMatch(trimmed);
  if (match == null) return null;
  final pathPart = match.group(1) ?? '';
  final path = _normalizePathToken(decodeHref ? _safeDecode(pathPart) : pathPart);
  if (path == null) return null;
  final lines = _parseLineFragment(match.group(2) ?? '');
  if (lines == null) return null;
  return InlinePathTarget(raw: raw, path: path, lineStart: lines.start, lineEnd: lines.end);
}

InlinePathTarget? _parseWorkspaceRelativeFileLink(
  String value, {
  String? workspaceRoot,
  bool decodeHref = false,
  bool explicitLink = false,
}) {
  final parsed = _parseLocalPathParts(value, decodeHref: decodeHref, explicitLink: explicitLink);
  if (parsed == null || isAbsoluteFilePath(parsed.path)) return null;
  InlinePathTarget at(String path) =>
      InlinePathTarget(raw: value, path: path, lineStart: parsed.lines.start, lineEnd: parsed.lines.end);
  if (_isHomeRelativePath(parsed.path)) return at(parsed.path);
  final root = _normalizePathInput(workspaceRoot);
  if (root == null) return null;
  final path = _resolveRelativePathUnderRoot(parsed.path, root);
  return path == null ? null : at(path);
}

({String path, _Lines lines})? _parseLocalPathParts(
  String value, {
  bool decodeHref = false,
  bool explicitLink = false,
}) {
  final normalized = _normalizePathToken(value);
  if (normalized == null || normalized.contains('?')) return null;
  final hashIndex = normalized.indexOf('#');
  final beforeHash = hashIndex >= 0 ? normalized.substring(0, hashIndex) : normalized;
  final hash = hashIndex >= 0 ? normalized.substring(hashIndex) : '';
  final fragmentLines = _parseLineFragment(hash);
  if (fragmentLines == null) return null;

  // Literal line markers split before decoding; encoded punctuation belongs
  // to the file name.
  final inline = parseInlinePathToken(beforeHash);
  if (inline != null) {
    final path = decodeHref ? _safeDecode(inline.path) : inline.path;
    if (!explicitLink && !_isPlausibleLocalPath(path)) return null;
    return (path: path, lines: (start: inline.lineStart, end: inline.lineEnd));
  }
  if (beforeHash.isEmpty || beforeHash.contains(':')) return null;
  final decoded = decodeHref ? _safeDecode(beforeHash) : beforeHash;
  final plausible = parseInlinePathToken(decoded)?.path ?? decoded;
  if (!explicitLink && !_isPlausibleLocalPath(plausible)) return null;
  return (path: decoded, lines: fragmentLines);
}

/// parse.ts normalizeInlinePathTarget: a workspace-relative file (or the
/// directory a trailing slash names).
({String directory, String? file})? normalizeInlinePathTarget(String rawPath, String? cwd) {
  final input = _normalizePathInput(rawPath);
  if (input == null) return null;
  var normalized = _resolvePathAgainstCwd(input, cwd) ?? input;
  if (normalized.startsWith('./')) {
    normalized = normalized.substring(2);
    if (normalized.isEmpty) normalized = '.';
  }
  if (normalized.isEmpty) normalized = '.';
  if (normalized == '.') return (directory: '.', file: null);
  if (normalized.endsWith('/')) {
    final dir = normalized.replaceFirst(RegExp(r'/+$'), '');
    return (directory: dir.isEmpty ? '.' : dir, file: null);
  }
  final slash = normalized.lastIndexOf('/');
  final directory = slash >= 0 ? normalized.substring(0, slash) : '.';
  return (directory: directory.isEmpty ? '.' : directory, file: normalized);
}

bool _isAllowedAbsolutePath(String path, String? workspaceRoot) {
  final root = _normalizePathInput(workspaceRoot);
  if (root == null) return true;
  final comparePath = _normalizePathForCompare(path);
  final trimmedRoot = root.replaceFirst(RegExp(r'/+$'), '');
  final compareRoot = _normalizePathForCompare(trimmedRoot.isEmpty ? '/' : trimmedRoot);
  final prefix = compareRoot == '/' ? '/' : '$compareRoot/';
  return comparePath == compareRoot || comparePath.startsWith(prefix);
}

bool _isHomeRelativePath(String path) => path == '~' || path.startsWith('~/') || path.startsWith(r'~\');

bool _isExternalHref(String value) {
  if (value.contains('://')) return !value.toLowerCase().startsWith('file://');
  if (parseInlinePathToken(value) != null) return false;
  return RegExp(r'^[A-Za-z][A-Za-z0-9+.-]*:').hasMatch(value) && !_driveAbsolute.hasMatch(value);
}

bool _isAmbiguousWorkspaceCandidate(
  String value,
  InlinePathTarget target, {
  String? workspaceRoot,
  bool decodeHref = false,
}) {
  final root = _normalizePathInput(workspaceRoot);
  if (root == null || !_isAllowedAbsolutePath(target.path, root)) return false;
  final parsed = _parseLocalPathParts(value, decodeHref: decodeHref);
  if (parsed == null || isAbsoluteFilePath(parsed.path)) return false;
  return !parsed.path.contains('/');
}

String? _heuristicLocalPath(String value) {
  final hashIndex = value.indexOf('#');
  final beforeHash = hashIndex >= 0 ? value.substring(0, hashIndex) : value;
  final hash = hashIndex >= 0 ? value.substring(hashIndex) : '';
  if (_parseLineFragment(hash) == null) return null;
  final inline = parseInlinePathToken(beforeHash);
  if (inline != null) return inline.path;
  if (beforeHash.isEmpty || beforeHash.contains(':')) return null;
  return beforeHash;
}

bool _isPlausibleLocalPath(String value) {
  final normalized = _normalizePathToken(value);
  if (normalized == null) return false;
  if (isAbsoluteFilePath(normalized)) return true;
  if (normalized.startsWith('./') || normalized.startsWith('../') || normalized.startsWith('~/')) {
    return true;
  }
  final segments = normalized.split('/').where((s) => s.isNotEmpty).toList();
  if (segments.isEmpty) return false;
  if (segments.length > 1) {
    return !RegExp(r'^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$').hasMatch(segments.first) &&
        _isPlausibleFileName(segments.last);
  }
  return _isPlausibleFileName(segments.first);
}

bool _isPlausibleFileName(String fileName) {
  if (fileName.startsWith('.') && fileName.length > 1) return true;
  final dot = fileName.lastIndexOf('.');
  if (dot < 0) return false;
  return assistantFileExtensions.contains(fileName.substring(dot + 1).toLowerCase());
}

String? _resolveRelativePathUnderRoot(String value, String workspaceRoot) {
  final path = _normalizePathToken(value);
  if (path == null || isAbsoluteFilePath(path)) return null;
  final trimmedRoot = workspaceRoot.replaceFirst(RegExp(r'/+$'), '');
  final root = trimmedRoot.isEmpty ? '/' : trimmedRoot;
  final segments = <String>[];
  for (final segment in path.split('/')) {
    if (segment.isEmpty || segment == '.') continue;
    if (segment == '..') {
      if (segments.isEmpty) return null;
      segments.removeLast();
      continue;
    }
    segments.add(segment);
  }
  if (segments.isEmpty) return root;
  return root == '/' ? '/${segments.join('/')}' : '$root/${segments.join('/')}';
}

String? _normalizePathInput(String? value) {
  if (value == null) return null;
  final trimmed = value.trim().replaceFirst(RegExp('^[\'"`]'), '').replaceFirst(RegExp('[\'"`]\$'), '');
  if (trimmed.isEmpty) return null;
  return trimmed.replaceAll(r'\', '/').replaceAll(RegExp(r'/{2,}'), '/');
}

String? _resolvePathAgainstCwd(String path, String? cwd) {
  final normalizedCwd = _normalizePathInput(cwd);
  if (normalizedCwd == null || !isAbsoluteFilePath(path) || !isAbsoluteFilePath(normalizedCwd)) {
    return null;
  }
  final trimmed = normalizedCwd.replaceFirst(RegExp(r'/+$'), '');
  final base = trimmed.isEmpty ? '/' : trimmed;
  final comparePath = _normalizePathForCompare(path);
  final prefix = base == '/' ? '/' : '$base/';
  if (comparePath == _normalizePathForCompare(base)) return '.';
  if (comparePath.startsWith(_normalizePathForCompare(prefix))) {
    final rest = path.substring(prefix.length);
    return rest.isEmpty ? '.' : rest;
  }
  return null;
}

String _normalizePathForCompare(String value) =>
    RegExp(r'^[A-Za-z]:').hasMatch(value) ? value.toLowerCase() : value;

// ---- Linkification (components/markdown/source.ts) ----

const _pathCharacter = r'[a-zA-Z0-9\u0080-\uFFFF_@.%+-]';
const _pathLetter = r'[a-zA-Z]';
const _pathSegment = '$_pathCharacter*[a-zA-Z0-9\\u0080-\\uFFFF_@%+-]';
const _lineSuffix = r'(?::\d+(?::\d+)?(?:-\d+(?::\d+)?)?)?';
final _fileExtensions = assistantFileExtensions
    .map((e) => e.replaceAllMapped(RegExp('[a-z]'), (m) => '[${m[0]}${m[0]!.toUpperCase()}]'))
    .join('|');
const _pathPrefix = r'(?:[A-Za-z]:[\\/]|\.{0,2}\/|~\/)';
const _hasPathLetter = '(?=$_pathCharacter*(?:[\\\\/]$_pathCharacter*)*$_pathLetter)';
const _separatedPath = '$_pathSegment(?:[\\\\/]$_pathSegment)+';
final _fileName = '$_pathSegment\\.(?:$_fileExtensions)';

/// Bare prose paths need a known extension, a line suffix, or at least two
/// separators.
final _prosePath = '(?:$_pathPrefix$_hasPathLetter$_separatedPath|$_hasPathLetter(?:'
    '$_pathSegment(?:[\\\\/]$_pathSegment)*[\\\\/]$_fileName|'
    '$_pathSegment(?:[\\\\/]$_pathSegment){2,}|'
    '$_separatedPath(?=:)))';

/// source.ts FILE_PATTERN: http(s) URLs and file paths (with an optional
/// `:line[:col][-line[:col]]` suffix) that stand alone in prose.
final assistantLinkifyPattern = RegExp(
  '(?:https?://[^\\s<>]+|'
  '(?<![a-zA-Z0-9\\u0080-\\uFFFF_@.%\\x2b~:/\\\\-])(?:$_prosePath|$_fileName)$_lineSuffix'
  '(?![a-zA-Z0-9\\u0080-\\uFFFF_@%+:/\\\\-]|\\.$_pathCharacter))',
);

final _inlineCodeUrl = RegExp(r'^https?://[^\s<>]+$');

/// source.ts linkMetadata `linkRecognition.inlineCode`: a complete code span
/// is a link when it is a whole http(s) URL, or when collectInlineCodePaths
/// classifies it as a (non-external) file link.
bool isInlineCodeFileLink(String code, {String? workspaceRoot}) {
  if (_inlineCodeUrl.hasMatch(code)) return true;
  final classification = classifyAssistantFileLink(code, workspaceRoot: workspaceRoot);
  return classification != null && classification is! ExternalFileLink;
}

// ---- Resolution (resolver.ts) ----

sealed class _Resolution {
  const _Resolution();
}

class _Ignored extends _Resolution {
  const _Ignored();
}

class _External extends _Resolution {
  const _External(this.url);
  final String url;
}

class _File extends _Resolution {
  const _File(this.target);
  final InlinePathTarget target;
}

class _NeedsLookup extends _Resolution {
  const _NeedsLookup({required this.query, required this.token, required this.target});
  final String query, token;
  final InlinePathTarget target;
}

_Resolution _classifyForResolution(AssistantFileLinkSource source, String? workspaceRoot) {
  // Markdown hrefs are percent-encoded by the parser; inline code and
  // linkified prose are raw paths that must not be decoded.
  var token = source.href;
  var fromHref = true;
  if (source.type == FileLinkSourceType.inlineCode) {
    final text = source.text?.trim() ?? '';
    token = text.isEmpty ? source.href : text;
    fromHref = false;
  } else if (source.type == FileLinkSourceType.linkify) {
    final text = source.text?.trim() ?? '';
    if (text.isNotEmpty && isFileLookingAssistantToken(text)) {
      token = text;
      fromHref = false;
    }
  }
  token = token.trim();
  if (token.isEmpty) return const _Ignored();
  final classification = classifyAssistantFileLink(
    token,
    workspaceRoot: workspaceRoot,
    decodeHref: fromHref,
    explicitLink: source.type == FileLinkSourceType.markdown,
  );
  final InlinePathTarget target;
  if (classification == null) return const _Ignored();
  if (classification is ExternalFileLink) return _External(classification.raw);
  if (classification is DirectFileLink) {
    target = classification.target;
    if (!_resolveThroughSuggestions(source, token, target, workspaceRoot)) return _File(target);
  } else {
    target = (classification as AmbiguousFileLink).target;
  }
  final root = workspaceRoot?.trim() ?? '';
  if (root.isEmpty) return const _Ignored();
  return _NeedsLookup(query: _ambiguousQuery(target, root), token: token, target: target);
}

/// resolver.ts shouldResolveDirectFileThroughSuggestions: workspace-relative
/// inline code is looked up, so a shorthand path still finds its file.
bool _resolveThroughSuggestions(
  AssistantFileLinkSource source,
  String token,
  InlinePathTarget target,
  String? workspaceRoot,
) {
  if (source.type != FileLinkSourceType.inlineCode) return false;
  if (token.startsWith('/') || token.toLowerCase().startsWith('file://') || _driveAbsolute.hasMatch(token)) {
    return false;
  }
  final root = workspaceRoot?.trim() ?? '';
  if (root.isEmpty) return false;
  final normalizedRoot = root.replaceAll(r'\', '/').replaceFirst(RegExp(r'/+$'), '');
  return target.path.replaceAll(r'\', '/').startsWith('$normalizedRoot/');
}

String _ambiguousQuery(InlinePathTarget target, String workspaceRoot) {
  final root = workspaceRoot.replaceAll(r'\', '/').replaceFirst(RegExp(r'/+$'), '');
  final path = target.path.replaceAll(r'\', '/');
  if (path.startsWith('$root/')) return path.substring(root.length + 1);
  final slash = path.lastIndexOf('/');
  return slash >= 0 ? path.substring(slash + 1) : path;
}

/// use-file-link.ts canResolveAssistantFileLinkToFile.
@visibleForTesting
bool canResolveAssistantFileLinkToFile(AssistantFileLinkSource source, String? workspaceRoot) =>
    switch (_classifyForResolution(source, workspaceRoot)) {
      _File() || _NeedsLookup() => true,
      _ => false,
    };

/// resolver.ts fetchDaemonResolution: the first suffix-matching workspace
/// file, or null when none matches.
Future<InlinePathTarget?> _lookup(PaseoClient client, _NeedsLookup lookup, String workspaceRoot) async {
  final root = workspaceRoot.trim();
  final JsonMap response;
  try {
    response = await client.request(
      'directory_suggestions_request',
      {
        'query': lookup.query,
        'cwd': root,
        'includeFiles': true,
        'includeDirectories': false,
        'matchMode': 'suffix',
        'limit': 1,
      },
      responseType: 'directory_suggestions_response',
    );
  } catch (_) {
    return null;
  }
  if (response.text('error').isNotEmpty) return null;
  final match = response.array('entries').map(jsonMap).where((e) => e['kind'] == 'file').firstOrNull;
  if (match == null) return null;
  final base = root.replaceAll(r'\', '/').replaceFirst(RegExp(r'/+$'), '');
  final child = match.text('path').replaceAll(r'\', '/').replaceFirst(RegExp(r'^/+'), '');
  return lookup.target.withPath(base.isEmpty ? child : '$base/$child');
}

/// path-open.ts createInlinePathOpener `generation`: only the latest open
/// may finish its folder probe; a suffix lookup still in flight is not
/// cancelled by a later press (use-file-link.ts dispatches it when it lands).
int _openGeneration = 0;

/// use-file-link.ts openAssistantFileLink + path-open.ts createInlinePathOpener,
/// for an agent stream in [origin]'s workspace: http(s) links open in the
/// browser, file links (after a suffix lookup for workspace-relative inline
/// code; "No file found for …" when it misses) open the file at its line, and
/// an existing workspace folder reveals it in the Files explorer.
Future<void> openAssistantFileLink(
  BuildContext context, {
  required PanelTarget origin,
  required AssistantFileLinkSource source,
}) async {
  final root = origin.cwd;
  final resolution = _classifyForResolution(source, root);
  final store = PaseoScope.read(context);
  final InlinePathTarget target;
  if (resolution is _Ignored) return;
  if (resolution is _External) {
    final uri = Uri.tryParse(resolution.url);
    if (uri == null || (uri.scheme != 'http' && uri.scheme != 'https')) return;
    try {
      if (!await launchUrl(uri)) throw StateError('Could not open ${resolution.url}');
    } catch (error) {
      if (context.mounted) showError(context, error);
    }
    return;
  }
  if (resolution is _File) {
    target = resolution.target;
  } else {
    final lookup = resolution as _NeedsLookup;
    final found = await _lookup(store.clientFor(origin.serverId), lookup, root);
    if (!context.mounted) return;
    if (found == null) {
      showToast(
        context,
        tr(context, 'common.errors.noFileFound', args: {'token': lookup.token}),
        variant: ToastVariant.error,
      );
      return;
    }
    target = found;
  }
  await _openInlinePath(context, origin: origin, target: target);
}

/// agent-stream/view.tsx handleInlinePathPress for a path that is already
/// resolved (a tool call's file): opens it as [openAssistantFileLink] does.
Future<void> openInlinePath(BuildContext context, {required PanelTarget origin, required String path}) =>
    _openInlinePath(context, origin: origin, target: InlinePathTarget(raw: path, path: path));

/// path-open.ts createInlinePathOpener.open.
Future<void> _openInlinePath(
  BuildContext context, {
  required PanelTarget origin,
  required InlinePathTarget target,
}) async {
  final generation = ++_openGeneration;
  final root = origin.cwd;
  final store = PaseoScope.read(context);
  final normalized = normalizeInlinePathTarget(target.path, root);
  if (normalized == null) return;
  final file = normalized.file;
  var directory = file == null ? normalized.directory : null;
  if (file != null && target.lineStart == null && !isAbsoluteFilePath(file) && root.trim().isNotEmpty) {
    // path-open.ts resolveInlinePathOpen: a workspace folder (even one with
    // a dot in its name) reveals in the explorer instead of opening.
    try {
      final listed = await store.clientFor(origin.serverId).request(
        'file_explorer_request',
        {'cwd': root, 'path': file, 'mode': 'list'},
        responseType: 'file_explorer_response',
      );
      if (listed['error'] == null && listed['directory'] != null) directory = file;
    } catch (_) {
      // Not a listable folder: open it as a file.
    }
    if (!context.mounted || generation != _openGeneration) return;
  }
  if (directory != null) {
    store.openExplorer(
      PanelTarget(
        kind: 'files',
        serverId: origin.serverId,
        workspaceId: origin.workspaceId,
        cwd: origin.cwd,
        data: {'path': directory},
      ),
    );
    return;
  }
  final lineStart = target.lineStart;
  final lineEnd = target.lineEnd;
  openWorkspaceFile(
    context,
    PanelTarget(
      kind: 'file',
      serverId: origin.serverId,
      workspaceId: origin.workspaceId,
      cwd: origin.cwd,
      id: file,
      title: file!.split('/').last,
      data: {
        'path': file,
        'line': ?lineStart,
        if (lineStart != null && lineEnd != null && lineEnd >= lineStart) 'lineEnd': lineEnd,
      },
    ),
    preference: 'chatFiles',
  );
}
