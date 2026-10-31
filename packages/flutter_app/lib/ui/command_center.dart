import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../core/app_store.dart';
import '../core/json.dart';
import '../core/target.dart';
import '../features/chat/agent_attention.dart';
import '../features/chat/chrome/status_mark.dart';
import '../features/chat/provider_icon.dart';
import '../features/files/file_icons.dart';
import '../features/navigation/navigation_actions.dart';
import '../features/navigation/session_history_dialog.dart';
import '../features/schedules/schedules_screen.dart';
import '../features/workspace/project_settings_panel.dart';
import 'common.dart';
import 'localization.dart';
import 'shell_pages.dart';
import 'shortcuts.dart';
import 'theme.dart';

typedef CommandDispatch = Future<void> Function(
  String action,
  JsonMap arguments,
);

/// What a chosen row does once the palette has closed (original: `setOpen(false)`, then
/// `result.run()`), run against the surface that opened the palette.
typedef _Run = FutureOr<void> Function(BuildContext context, CommandDispatch dispatch);

/// The original Command Center (`command-center/command-center.tsx`). Phones get the
/// native bottom sheet (60% snap point growing to 90% while the keyboard is up, 0.45
/// backdrop, zinc-600 handle); everything else the top-anchored 640×560 panel (92% / 80%
/// caps, 48px from the top, 8px radius, 1px border, `shadow.lg`) over a 50% scrim.
Future<void> showPaseoCommandCenter(
  BuildContext context, {
  required AppStore store,
  required bool files,
  required CommandDispatch onCommand,
}) async {
  final colors = PaseoColors.of(context);
  final dark = Theme.of(context).brightness == Brightness.dark;
  final _Run? run;
  if (isCompactLayout(context) && (Platform.isAndroid || Platform.isIOS)) {
    run = await showModalBottomSheet<_Run>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (context) {
        final size = MediaQuery.sizeOf(context);
        final keyboard = MediaQuery.viewInsetsOf(context).bottom;
        // keyboardBehavior "extend": the sheet rises to its 90% snap point while typing.
        final height = math.min(
          size.height * (keyboard > 0 ? .9 : .6),
          size.height - keyboard,
        );
        return Padding(
          padding: EdgeInsets.only(bottom: keyboard),
          child: SizedBox(
            height: math.max(0.0, height),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                // The @gorhom default handle: 10px padding around a 7.5%-wide, 4px pill.
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 10),
                  child: Center(
                    child: Container(
                      width: size.width * .075,
                      height: 4,
                      decoration: BoxDecoration(
                        color: const Color(0xff52525b),
                        borderRadius: BorderRadius.circular(4),
                      ),
                    ),
                  ),
                ),
                Expanded(
                  child: _CommandCenter(store: store, initialFiles: files, sheet: true),
                ),
              ],
            ),
          ),
        );
      },
    );
  } else {
    run = await showDialog<_Run>(
      context: context,
      barrierColor: const Color(0x80000000),
      builder: (context) {
        final size = MediaQuery.sizeOf(context);
        return Align(
          alignment: Alignment.topCenter,
          child: Padding(
            padding: const EdgeInsets.only(top: 48),
            child: Container(
              width: math.min(640.0, size.width * .92),
              height: math.min(560.0, size.height * .8),
              clipBehavior: Clip.antiAlias,
              decoration: BoxDecoration(
                color: colors.surface0,
                borderRadius: BorderRadius.circular(8),
                border: Border.all(color: Theme.of(context).dividerColor),
                boxShadow: [
                  BoxShadow(
                    color: dark ? const Color(0x66000000) : const Color(0x14000000),
                    offset: Offset(0, dark ? 12 : 8),
                    blurRadius: 24,
                  ),
                ],
              ),
              child: Material(
                type: MaterialType.transparency,
                child: _CommandCenter(store: store, initialFiles: files, sheet: false),
              ),
            ),
          ),
        );
      },
    );
  }
  if (run != null && context.mounted) await run(context, onCommand);
}

/// Original `Shortcut` badge (`components/ui/shortcut.tsx`): every chord step formatted
/// for the OS (`formatShortcut`) as normal small muted text on a half-opacity surface3
/// chip (4/2 padding, 6px radius); a multi-step chord is a row of chips 4px apart.
class ShortcutBadge extends StatelessWidget {
  const ShortcutBadge(this.combo, {super.key});
  final String combo;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Wrap(
      spacing: 4,
      runSpacing: 4,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
        for (final step in combo.split(RegExp(r'\s+')).where((step) => step.isNotEmpty))
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 2),
            decoration: BoxDecoration(
              color: colors.surface3.withValues(alpha: .5),
              borderRadius: BorderRadius.circular(6),
            ),
            child: Text(
              formatShortcutStep(step),
              style: Theme.of(context).textTheme.bodySmall!.copyWith(
                color: colors.muted,
                fontWeight: FontWeight.normal,
              ),
            ),
          ),
      ],
    );
  }
}

const _shortcutKeyDisplay = {
  'Backspace': '⌫',
  'Enter': '⏎',
  'Esc': 'Esc',
  'Escape': 'Esc',
  'Space': '␣',
  'Left': '←',
  'Right': '→',
  'Up': '↑',
  'Down': '↓',
  'ArrowLeft': '←',
  'ArrowRight': '→',
  'ArrowUp': '↑',
  'ArrowDown': '↓',
};

/// Original `formatShortcut`: on Apple platforms the modifier glyphs in ⌃⌥⇧⌘ order run
/// straight into the key (`⇧⌘F`); elsewhere `Ctrl+Shift+F`, with `Mod` read as Ctrl and
/// `Meta` as Win. Single-character keys are upper-cased; named keys use their glyph.
String formatShortcutStep(String step) {
  final parts = step.split('+');
  var key = parts.removeLast();
  if (key.isEmpty && parts.isNotEmpty && parts.last.isEmpty) {
    parts.removeLast();
    key = '+';
  }
  key = _shortcutKeyDisplay[key] ?? (key.length == 1 ? key.toUpperCase() : key);
  final modifiers = [
    for (final part in parts)
      if (part.isNotEmpty) part.toLowerCase() == 'cmd' ? 'meta' : part.toLowerCase(),
  ];
  if (Platform.isMacOS || Platform.isIOS) {
    const order = ['ctrl', 'alt', 'shift', 'mod', 'meta'];
    const symbols = {'ctrl': '⌃', 'alt': '⌥', 'shift': '⇧', 'mod': '⌘', 'meta': '⌘'};
    return '${order.where(modifiers.contains).map((m) => symbols[m]).join()}$key';
  }
  const labels = {'mod': 'Ctrl', 'ctrl': 'Ctrl', 'alt': 'Alt', 'shift': 'Shift', 'meta': 'Win'};
  return [for (final modifier in modifiers) labels[modifier] ?? modifier, key].join('+');
}

const _shortcutSections = [
  ('general', 'general'),
  ('workspaces', 'workspaces'),
  ('tabs-panes', 'tabsPanes'),
  ('layout', 'layout'),
  ('agent-input', 'agentInput'),
];

/// The original keyboard-shortcuts dialog: searchable sections of bordered rows.
Future<void> showShortcutHelp(BuildContext context, AppStore store) async {
  final bindings = await ShortcutCatalog.load();
  if (!context.mounted) return;
  final overrides = jsonMap(store.preferences['keyboardShortcuts']);
  var query = '';
  await showAdaptiveSheet<void>(
    context: context,
    builder: (context) => StatefulBuilder(
      builder: (context, update) {
        final colors = PaseoColors.of(context);
        final theme = Theme.of(context);
        final rule = BorderSide(color: colors.surface2);
        final sections = <Widget>[];
        for (final (id, key) in _shortcutSections) {
          final title = tr(context, 'settings.shortcuts.sections.$key');
          final seen = <String>{};
          final rows = bindings.where((binding) {
            final help = binding.data.object('help');
            if (help.text('section') != id) return false;
            if (!seen.add(help.text('id', binding.action))) return false;
            return query.isEmpty ||
                binding.label.toLowerCase().contains(query) ||
                title.toLowerCase().contains(query);
          }).toList();
          if (rows.isEmpty) continue;
          sections.add(
            Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 8,
              children: [
                Text(
                  title,
                  style: theme.textTheme.bodySmall!.copyWith(
                    color: colors.muted,
                    fontWeight: FontWeight.w500,
                  ),
                ),
                DecoratedBox(
                  decoration: BoxDecoration(
                    border: Border.fromBorderSide(rule),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Column(
                    children: [
                      for (final (index, binding) in rows.indexed)
                        Container(
                          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
                          decoration: BoxDecoration(
                            border: index == rows.length - 1 ? null : Border(bottom: rule),
                          ),
                          child: Row(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            spacing: 12,
                            children: [
                              Expanded(
                                child: Text(binding.label, style: theme.textTheme.bodyMedium),
                              ),
                              switch (stringValue(overrides[binding.id], binding.combo)) {
                                '' => Text(
                                  tr(context, 'settings.shortcuts.unassigned'),
                                  style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
                                ),
                                final combo => ShortcutBadge(combo),
                              },
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
        return SheetContents(
          title: tr(context, 'settings.shortcuts.dialogTitle'),
          header: Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 12),
            child: Row(
              spacing: 8,
              children: [
                Icon(LucideIcons.search300, size: 16, color: colors.muted),
                Expanded(
                  child: TextField(
                    autofocus: !isCompactLayout(context),
                    style: theme.textTheme.bodyMedium,
                    decoration: InputDecoration(
                      hintText: tr(context, 'settings.shortcuts.searchPlaceholder'),
                      isDense: true,
                      filled: false,
                      border: InputBorder.none,
                      enabledBorder: InputBorder.none,
                      focusedBorder: InputBorder.none,
                      contentPadding: const EdgeInsets.symmetric(vertical: 8),
                    ),
                    onChanged: (value) => update(() => query = value.trim().toLowerCase()),
                  ),
                ),
              ],
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            spacing: 16,
            children: sections.isEmpty
                ? [
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 24),
                      child: Text(
                        'No results',
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted),
                      ),
                    ),
                  ]
                : sections,
          ),
        );
      },
    ),
  );
}

// ---------------------------------------------------------------------------
// Text matching: a port of `@getpaseo/protocol/search/text-match`, shared by the
// command center's ranking and the History screen's match marks.

const _tierExact = 0;
const _tierWholeWord = 1;
const _tierPrefix = 2;
const _tierWordStart = 3;
const _tierSubstring = 4;
const _tierSubsequence = 5;
const _tierFuzzy = 6;

final _wordChar = RegExp(r'[a-z0-9]');
final _whitespace = RegExp(r'\s');

/// One match: a tier plus where it landed. Lower is better on both, then on spread.
class PaseoMatchScore {
  const PaseoMatchScore(this.tier, this.offset, [this.spread]);
  final int tier;
  final int offset;
  final int? spread;
}

int paseoCompareMatchScores(PaseoMatchScore a, PaseoMatchScore b) {
  if (a.tier != b.tier) return a.tier - b.tier;
  if (a.offset != b.offset) return a.offset - b.offset;
  return (a.spread ?? 0) - (b.spread ?? 0);
}

typedef _FuzzyPolicy = ({int maxEdits, bool transpositionsOnly});

bool _isBoundary(String? char) => char == null || !_wordChar.hasMatch(char);

PaseoMatchScore? _substringMatch(String query, String text) {
  PaseoMatchScore? best;
  var position = 0;
  while (position <= text.length - query.length) {
    final found = text.indexOf(query, position);
    if (found == -1) break;
    final before = found > 0 ? text[found - 1] : null;
    final end = found + query.length;
    final after = end < text.length ? text[end] : null;
    final startsAtBoundary = found == 0 || _isBoundary(before);
    final endsAtBoundary = after == null || _isBoundary(after);
    final tier = startsAtBoundary && endsAtBoundary
        ? _tierWholeWord
        : found == 0
        ? _tierPrefix
        : startsAtBoundary
        ? _tierWordStart
        : _tierSubstring;
    if (best == null || tier < best.tier || (tier == best.tier && found < best.offset)) {
      best = PaseoMatchScore(tier, found);
    }
    position = found + 1;
  }
  return best;
}

/// Characters in order within one whitespace-delimited word.
PaseoMatchScore? _subsequenceMatch(String query, String text) {
  var queryIndex = 0;
  var first = -1;
  var last = -1;
  for (var index = 0; index < text.length && queryIndex < query.length; index++) {
    if (_whitespace.hasMatch(text[index])) {
      queryIndex = 0;
      first = -1;
      last = -1;
      continue;
    }
    if (text[index] != query[queryIndex]) continue;
    if (first == -1) first = index;
    last = index;
    queryIndex++;
  }
  if (queryIndex != query.length || first == -1) return null;
  return PaseoMatchScore(_tierSubsequence, first, last - first + 1);
}

/// Damerau-Levenshtein distance, abandoned once a whole row is over budget.
int? _editDistance(String query, String word, int budget) {
  if ((query.length - word.length).abs() > budget) return null;
  var twoRowsBack = <int>[];
  var previous = List<int>.generate(word.length + 1, (index) => index);
  for (var q = 1; q <= query.length; q++) {
    final current = <int>[q];
    var rowBest = q;
    for (var w = 1; w <= word.length; w++) {
      final substitution = query[q - 1] == word[w - 1] ? 0 : 1;
      var cost = math.min(
        math.min(current[w - 1] + 1, previous[w] + 1),
        previous[w - 1] + substitution,
      );
      if (q > 1 && w > 1 && query[q - 1] == word[w - 2] && query[q - 2] == word[w - 1]) {
        cost = math.min(cost, twoRowsBack[w - 2] + 1);
      }
      current.add(cost);
      rowBest = math.min(rowBest, cost);
    }
    if (rowBest > budget) return null;
    twoRowsBack = previous;
    previous = current;
  }
  final distance = previous[word.length];
  return distance <= budget ? distance : null;
}

bool _isAdjacentTransposition(String query, String word) {
  if (query.length != word.length) return false;
  var index = 0;
  while (index < query.length && query[index] == word[index]) {
    index++;
  }
  if (index >= query.length - 1) return false;
  if (query[index] != word[index + 1] || query[index + 1] != word[index]) return false;
  return query.substring(index + 2) == word.substring(index + 2);
}

/// Short tokens only forgive a swap; longer ones an edit or two.
_FuzzyPolicy? _fuzzyPolicyForToken(String token) {
  if (token.length <= 3) return null;
  if (token.length == 4) return (maxEdits: 1, transpositionsOnly: true);
  if (token.length <= 7) return (maxEdits: 1, transpositionsOnly: false);
  return (maxEdits: 2, transpositionsOnly: false);
}

PaseoMatchScore? _fuzzyMatch(String query, String text, _FuzzyPolicy policy) {
  if (policy.maxEdits <= 0 || query.length <= policy.maxEdits) return null;
  PaseoMatchScore? best;
  for (final word in RegExp(r'[a-z0-9]+').allMatches(text)) {
    final value = word.group(0)!;
    final candidates = {
      value,
      value.substring(0, math.min(query.length, value.length)),
      value.substring(0, math.min(query.length + policy.maxEdits, value.length)),
    };
    for (final candidate in candidates) {
      final distance = policy.transpositionsOnly
          ? (_isAdjacentTransposition(query, candidate) ? 1 : null)
          : _editDistance(query, candidate, policy.maxEdits);
      if (distance == null) continue;
      final score = PaseoMatchScore(_tierFuzzy, word.start, distance);
      if (best == null || paseoCompareMatchScores(score, best) < 0) best = score;
    }
  }
  return best;
}

PaseoMatchScore? _scoreMatch(String query, String text, {_FuzzyPolicy? fuzzy}) {
  if (query.isEmpty) return const PaseoMatchScore(_tierExact, 0);
  final q = query.toLowerCase();
  final t = text.toLowerCase();
  if (t == q) return const PaseoMatchScore(_tierExact, 0);
  final exact = _substringMatch(q, t) ?? _subsequenceMatch(q, t);
  if (exact != null) return exact;
  return fuzzy == null ? null : _fuzzyMatch(q, t, fuzzy);
}

List<String> _tokenizeQuery(String query) => query
    .trim()
    .toLowerCase()
    .split(RegExp(r'\s+'))
    .where((token) => token.isNotEmpty)
    .toList(growable: false);

/// Original `scoreTextFields`: every whitespace token must land in some field; the
/// result sums each token's best tier, offset and spread. Null filters the row out.
PaseoMatchScore? paseoScoreTextFields(
  String query,
  List<String> fields, {
  bool typoTolerant = false,
}) {
  final tokens = _tokenizeQuery(query);
  if (tokens.isEmpty) return const PaseoMatchScore(_tierExact, 0, 0);
  var tier = 0;
  var offset = 0;
  var spread = 0;
  for (final token in tokens) {
    final fuzzy = typoTolerant ? _fuzzyPolicyForToken(token) : null;
    PaseoMatchScore? best;
    for (final field in fields) {
      final score = _scoreMatch(token, field, fuzzy: fuzzy);
      if (score != null && (best == null || paseoCompareMatchScores(score, best) < 0)) {
        best = score;
      }
    }
    if (best == null) return null;
    tier += best.tier;
    offset += best.offset;
    spread += best.spread ?? token.length;
  }
  return PaseoMatchScore(tier, offset, spread);
}

List<(int, int)> _matchRanges(String query, String text, PaseoMatchScore score) {
  if (query.isEmpty) return const [];
  final q = query.toLowerCase();
  final t = text.toLowerCase();
  if (score.tier == _tierExact) return [(0, text.length)];
  if (score.tier == _tierFuzzy) {
    var end = score.offset;
    while (end < t.length && _wordChar.hasMatch(t[end])) {
      end++;
    }
    return [(score.offset, math.max(end - score.offset, 1))];
  }
  if (score.tier == _tierSubsequence) {
    final ranges = <(int, int)>[];
    var queryIndex = 0;
    for (var index = score.offset; index < t.length && queryIndex < q.length; index++) {
      if (t[index] != q[queryIndex]) continue;
      queryIndex++;
      final last = ranges.isEmpty ? null : ranges.last;
      if (last != null && last.$1 + last.$2 == index) {
        ranges[ranges.length - 1] = (last.$1, last.$2 + 1);
      } else {
        ranges.add((index, 1));
      }
    }
    return ranges;
  }
  return [(score.offset, q.length)];
}

/// Original `findHighlightRanges`: each token's best match in this one field, as merged
/// `(start, length)` spans in order.
List<(int, int)> paseoHighlightRanges(
  String query,
  String text, {
  bool typoTolerant = false,
}) {
  final ranges = <(int, int)>[];
  for (final token in _tokenizeQuery(query)) {
    final score = _scoreMatch(
      token,
      text,
      fuzzy: typoTolerant ? _fuzzyPolicyForToken(token) : null,
    );
    if (score != null) ranges.addAll(_matchRanges(token, text, score));
  }
  ranges.sort((left, right) => left.$1 - right.$1);
  final merged = <(int, int)>[];
  for (final range in ranges) {
    final last = merged.isEmpty ? null : merged.last;
    if (last != null && range.$1 <= last.$1 + last.$2) {
      merged[merged.length - 1] = (
        last.$1,
        math.max(last.$2, range.$1 + range.$2 - last.$1),
      );
    } else {
      merged.add(range);
    }
  }
  return merged;
}

/// Original `HighlightedText` spans: marked runs on a surface3 tint in the foreground
/// colour. Ranges are clamped rather than trusted.
List<InlineSpan> paseoHighlightSpans(
  BuildContext context,
  String text,
  String query, {
  bool typoTolerant = false,
}) {
  final ranges = query.trim().isEmpty
      ? const <(int, int)>[]
      : paseoHighlightRanges(query, text, typoTolerant: typoTolerant);
  if (ranges.isEmpty) return [TextSpan(text: text)];
  final mark = TextStyle(
    backgroundColor: PaseoColors.of(context).surface3,
    color: Theme.of(context).colorScheme.onSurface,
  );
  final spans = <InlineSpan>[];
  var cursor = 0;
  for (final (rangeStart, length) in ranges) {
    final start = math.max(rangeStart, cursor);
    final end = math.min(start + length, text.length);
    if (end <= start) continue;
    if (start > cursor) spans.add(TextSpan(text: text.substring(cursor, start)));
    spans.add(TextSpan(text: text.substring(start, end), style: mark));
    cursor = end;
  }
  if (cursor < text.length) spans.add(TextSpan(text: text.substring(cursor)));
  return spans;
}

/// Text with the query's terms marked (original `HighlightedText` / `QueryHighlightedText`).
class PaseoHighlightedText extends StatelessWidget {
  const PaseoHighlightedText(
    this.text, {
    super.key,
    required this.query,
    this.style,
    this.maxLines = 1,
    this.typoTolerant = false,
  });
  final String text;
  final String query;
  final TextStyle? style;
  final int? maxLines;
  final bool typoTolerant;

  @override
  Widget build(BuildContext context) => Text.rich(
    TextSpan(children: paseoHighlightSpans(context, text, query, typoTolerant: typoTolerant)),
    style: style,
    maxLines: maxLines,
    overflow: TextOverflow.ellipsis,
  );
}

const _monthNames = [
  'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
  'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
];

/// Original `describeTimeAgo`: never sub-minute ("just now"), "5m ago" / "2h ago" /
/// "3d ago", then the date itself ("Jan 15") once it is a week old.
String paseoTimeAgo(DateTime? date) {
  if (date == null) return '';
  final local = date.toLocal();
  final elapsed = DateTime.now().difference(local);
  if (elapsed.inMinutes < 1) return 'just now';
  if (elapsed.inHours < 1) return '${elapsed.inMinutes}m ago';
  if (elapsed.inDays < 1) return '${elapsed.inHours}h ago';
  if (elapsed.inDays < 7) return '${elapsed.inDays}d ago';
  return '${_monthNames[local.month - 1]} ${local.day}';
}

/// Original `formatCompactTimeAgo`: the same thresholds without the prose, for a
/// dense column ("now", "5m", "2h", "3d", then "Jan 15").
String formatCompactTimeAgo(DateTime date, [DateTime? now]) {
  final elapsed = (now ?? DateTime.now()).difference(date);
  if (elapsed < const Duration(minutes: 1)) return 'now';
  if (elapsed < const Duration(hours: 1)) return '${elapsed.inMinutes}m';
  if (elapsed < const Duration(days: 1)) return '${elapsed.inHours}h';
  if (elapsed < const Duration(days: 7)) return '${elapsed.inDays}d';
  final local = date.toLocal();
  return '${_monthNames[local.month - 1]} ${local.day}';
}

// ---------------------------------------------------------------------------
// Results (`command-center/results.ts`, `workspace-search.ts`)

enum _ResultKind { workspace, agent, file, action }

class _Score {
  const _Score(this.fieldRank, this.match);

  /// 0 when every token matched visible text, 1 when one only matched a hidden keyword.
  final int fieldRank;
  final PaseoMatchScore match;
}

_Score? _scoreFields(String query, List<String> visible, List<String> hidden) {
  final shown = paseoScoreTextFields(query, visible);
  if (shown != null) return _Score(0, shown);
  if (hidden.isEmpty) return null;
  final match = paseoScoreTextFields(query, [...visible, ...hidden]);
  return match == null ? null : _Score(1, match);
}

int _compareScores(_Score left, _Score right) => left.fieldRank != right.fieldRank
    ? left.fieldRank - right.fieldRank
    : paseoCompareMatchScores(left.match, right.match);

/// Built-in rows rank by which fields matched and the tier only; recency breaks ties.
int _compareBuiltInScores(_Score left, _Score right) => left.fieldRank != right.fieldRank
    ? left.fieldRank - right.fieldRank
    : left.match.tier - right.match.tier;

class _Result {
  const _Result({
    required this.id,
    required this.kind,
    required this.title,
    required this.run,
    this.subtitle = '',
    this.hidden = const [],
    this.activityAt,
    this.shortcut = '',
    this.icon,
    this.score,
    this.changeRequestNumber,
  });
  final String id;
  final _ResultKind kind;
  final String title;
  final String subtitle;

  /// Matched but never rendered (an agent's cwd), so it only ever ranks as a keyword.
  final List<String> hidden;
  final DateTime? activityAt;
  final String shortcut;

  /// Builds the 16px icon for the row's current backdrop (status dots knock out onto it).
  final Widget Function(Color backdrop)? icon;
  final _Score? score;
  final int? changeRequestNumber;
  final _Run run;

  bool get twoLine =>
      kind == _ResultKind.workspace ||
      kind == _ResultKind.agent ||
      (kind == _ResultKind.action && subtitle.isNotEmpty);
}

/// Most recent first; a row without a timestamp sorts after every row with one.
int _compareByRecency(_Result left, _Result right) =>
    (right.activityAt?.millisecondsSinceEpoch ?? 0) -
    (left.activityAt?.millisecondsSinceEpoch ?? 0);

/// Original `filterAndRankBuiltInResults`: the five most recent rows with no query,
/// otherwise every matching row by score, then recency. Ties keep input order.
List<_Result> _rankBuiltIn(List<_Result> rows, String query) {
  final indexed = [for (final (index, row) in rows.indexed) (index, row)];
  if (query.trim().isEmpty) {
    indexed.sort((left, right) {
      final delta = _compareByRecency(left.$2, right.$2);
      return delta != 0 ? delta : left.$1 - right.$1;
    });
    return [for (final entry in indexed.take(5)) entry.$2];
  }
  final scored = <(int, _Result, _Score)>[];
  for (final (index, row) in indexed) {
    final score = _scoreFields(query, [row.title, row.subtitle], row.hidden);
    if (score != null) scored.add((index, row, score));
  }
  scored.sort((left, right) {
    var delta = _compareBuiltInScores(left.$3, right.$3);
    if (delta == 0) delta = _compareByRecency(left.$2, right.$2);
    return delta != 0 ? delta : left.$1 - right.$1;
  });
  return [for (final entry in scored) entry.$2];
}

/// A change-request query: an optional `pr`/`mr` word, an optional `#`/`!`, a number.
final _changeRequestQuery = RegExp(r'^(?:(?:pr|mr)\s*)?[#!]?(\d+)$', caseSensitive: false);

/// Original `filterAndRankWorkspaces`: a PR/MR number hit outranks any text match.
List<_Result> _rankWorkspaces(List<_Result> rows, String query) {
  if (query.trim().isEmpty) return _rankBuiltIn(rows, query);
  final number = int.tryParse(_changeRequestQuery.firstMatch(query.trim())?.group(1) ?? '');
  final matches = <(int, _Result, _Score?, bool)>[];
  for (final (index, row) in rows.indexed) {
    final score = _scoreFields(query, [row.title, row.subtitle], const []);
    final hit = number != null && row.changeRequestNumber == number;
    if (score != null || hit) matches.add((index, row, score, hit));
  }
  matches.sort((left, right) {
    if (left.$4 != right.$4) return left.$4 ? -1 : 1;
    var delta = 0;
    if (left.$3 != null && right.$3 != null) delta = _compareBuiltInScores(left.$3!, right.$3!);
    if (delta == 0) delta = _compareByRecency(left.$2, right.$2);
    return delta != 0 ? delta : left.$1 - right.$1;
  });
  return [for (final match in matches) match.$2];
}

/// A registered action (`command-center/contributions.ts`, presentation kind "action").
class _Contribution {
  const _Contribution({
    required this.id,
    required this.group,
    required this.groupRank,
    required this.rank,
    required this.keywords,
    required this.title,
    required this.sectionTitle,
    required this.icon,
    required this.run,
    this.queryOnly = false,
    this.shortcut = '',
  });
  final String id;
  final String group;
  final int groupRank;
  final int rank;
  final List<String> keywords;

  /// Visibility "query": listed only once something has been typed.
  final bool queryOnly;
  final String title;
  final String sectionTitle;
  final IconData icon;
  final String shortcut;
  final _Run run;
}

/// Original registry order: group rank, group, rank, id.
int _compareContributions(_Contribution left, _Contribution right) {
  if (left.groupRank != right.groupRank) return left.groupRank - right.groupRank;
  final group = left.group.compareTo(right.group);
  if (group != 0) return group;
  if (left.rank != right.rank) return left.rank - right.rank;
  return left.id.compareTo(right.id);
}

class _Section {
  _Section({
    required this.id,
    required this.band,
    required this.rank,
    required this.results,
    this.title,
  });
  final String id;

  /// 0: contribution sections that reorder by match quality; 1: pinned below them.
  final int band;
  final int rank;
  final String? title;
  final List<_Result> results;
  _Score? score;
}

int _compareSections(_Section left, _Section right) {
  if (left.band != right.band) return left.band - right.band;
  if (left.score != null && right.score != null) {
    final delta = _compareScores(left.score!, right.score!);
    if (delta != 0) return delta;
  }
  if (left.rank != right.rank) return left.rank - right.rank;
  return left.id.compareTo(right.id);
}

/// Original `buildContributionSections`: query-only rows drop out with no query; with
/// one, the score is the filter and both rows and sections sort by it.
List<_Section> _buildContributionSections(
  List<_Contribution> contributions,
  String query,
  Color iconColor,
) {
  final groups = <String, _Section>{};
  final registryIndex = <String, int>{};
  final hasQuery = query.trim().isNotEmpty;
  for (final (index, contribution) in contributions.indexed) {
    if (contribution.queryOnly && !hasQuery) continue;
    _Score? score;
    if (hasQuery) {
      score = _scoreFields(query, [contribution.title, ''], contribution.keywords);
      if (score == null) continue;
    }
    final section = groups.putIfAbsent(
      contribution.group,
      () => _Section(
        id: contribution.group,
        band: 0,
        rank: contribution.groupRank,
        title: contribution.sectionTitle,
        results: [],
      ),
    );
    registryIndex[contribution.id] = index;
    section.results.add(
      _Result(
        id: contribution.id,
        kind: _ResultKind.action,
        title: contribution.title,
        shortcut: contribution.shortcut,
        icon: (_) => Icon(contribution.icon, size: 16, color: iconColor),
        score: score,
        run: contribution.run,
      ),
    );
    if (score != null && (section.score == null || _compareScores(score, section.score!) < 0)) {
      section.score = score;
    }
  }
  final sections = groups.values.toList();
  if (!hasQuery) return sections;
  for (final section in sections) {
    section.results.sort((left, right) {
      final delta = left.score != null && right.score != null
          ? _compareScores(left.score!, right.score!)
          : 0;
      return delta != 0 ? delta : registryIndex[left.id]! - registryIndex[right.id]!;
    });
  }
  return sections..sort(_compareSections);
}

class _ListRow {
  const _ListRow.section(this.key, this.title, {required this.divider, required this.height})
    : result = null;
  _ListRow.result(_Result this.result, {required this.height})
    : key = result.id,
      title = null,
      divider = false;
  final String key;
  final String? title;
  final bool divider;
  final double height;
  final _Result? result;
}

class _Projection {
  const _Projection(this.rows, this.offsets, this.selectable, this.rowIndexById);
  final List<_ListRow> rows;
  final List<double> offsets;
  final List<_Result> selectable;
  final Map<String, int> rowIndexById;
}

/// Original `projectCommandCenterRows` over `list-metrics.ts`: rows inset 6px (desktop)
/// or 12px (compact, a 44pt single line) around 20px single-line or 18+16px two-line
/// text; a section label is 8+16+8, the rule between sections 8+1+8.
_Projection _projectRows(List<_Section> sections, {required bool compact}) {
  final inset = compact ? 12.0 : 6.0;
  final singleLineRow = inset + 20 + inset;
  final twoLineRow = inset + 18 + 16 + inset;
  const sectionTitle = 8.0 + 16 + 8;
  const sectionDivider = 8.0 + 1 + 8;
  final populated = sections.where((section) => section.results.isNotEmpty).toList()
    ..sort(_compareSections);
  final rows = <_ListRow>[];
  final offsets = <double>[];
  final selectable = <_Result>[];
  final rowIndexById = <String, int>{};
  var offset = 0.0;
  for (final (index, section) in populated.indexed) {
    final divider = index > 0;
    final height = (divider ? sectionDivider : 0.0) + (section.title != null ? sectionTitle : 0.0);
    if (height > 0) {
      offsets.add(offset);
      rows.add(
        _ListRow.section(
          'section:${section.id}',
          section.title,
          divider: divider,
          height: height,
        ),
      );
      offset += height;
    }
    for (final result in section.results) {
      final rowHeight = result.twoLine ? twoLineRow : singleLineRow;
      offsets.add(offset);
      rowIndexById[result.id] = rows.length;
      rows.add(_ListRow.result(result, height: rowHeight));
      selectable.add(result);
      offset += rowHeight;
    }
  }
  return _Projection(rows, offsets, selectable, rowIndexById);
}

String _joinSubtitle(List<String?> parts) =>
    parts.whereType<String>().where((part) => part.trim().isNotEmpty).join(' · ');

DateTime? _parseDate(String value) => value.isEmpty ? null : DateTime.tryParse(value);

/// Original `shortenPath`: the home directory as `~`.
String _shortenPath(String path) => path.replaceFirst(RegExp(r'^/(?:Users|home)/[^/]+'), '~');

/// Original `resolveAgentLabel`: blank and the placeholder "New agent" have no label.
String? _agentLabel(String title) {
  final trimmed = title.trim();
  if (trimmed.isEmpty || trimmed.toLowerCase() == 'new agent') return null;
  return trimmed;
}

JsonMap _agentOf(JsonMap entry) => entry.object('agent').isEmpty ? entry : entry.object('agent');

bool _listedAgent(JsonMap agent) =>
    agent['archivedAt'] == null &&
    agent['parentAgentId'] == null &&
    agent.text('workspaceId').isNotEmpty;

StatusBucket? _workspaceBucket(String status) => switch (status) {
  'needs_input' => StatusBucket.needsInput,
  'failed' => StatusBucket.failed,
  'running' => StatusBucket.running,
  'attention' => StatusBucket.attention,
  'done' => StatusBucket.done,
  _ => null,
};

int? _changeRequestNumber(JsonMap workspace) {
  final number = workspace.object('githubRuntime').object('pullRequest')['number'];
  return number is int ? number : null;
}

/// navigation.ts navigateToWorkspace without a target: the workspace agent that needs
/// attention most (pickAttentionAgent) opens first. Otherwise the workspace is revealed;
/// the session-centric Flutter shell reveals it through its most recently active
/// session, or a new session tab when it has none.
void _openWorkspace(AppStore store, HostState host, JsonMap workspace) {
  final id = workspace.text('id');
  final workspaceAgents = [
    for (final entry in host.agents)
      if (_agentOf(entry).text('workspaceId') == id) _agentOf(entry),
  ];
  final attentionAgentId = pickAttentionAgent(
    workspaceAgents.where(
      (agent) => agent.object('labels').text('paseo.parent-agent-id').trim().isEmpty,
    ),
  );
  if (attentionAgentId != null) {
    openNavigationAgent(
      store,
      host.serverId,
      workspaceAgents.firstWhere((agent) => agent.text('id') == attentionAgentId),
    );
    return;
  }
  JsonMap? latest;
  DateTime? latestAt;
  for (final agent in workspaceAgents) {
    if (!_listedAgent(agent)) continue;
    final at = _parseDate(agent.text('lastActivityAt', agent.text('updatedAt')));
    if (latest == null || (at != null && (latestAt == null || at.isAfter(latestAt)))) {
      latest = agent;
      latestAt = at;
    }
  }
  if (latest != null) {
    openNavigationAgent(store, host.serverId, latest);
    return;
  }
  store.open(
    PanelTarget(
      kind: 'new_tab',
      serverId: host.serverId,
      workspaceId: id,
      cwd: workspace.text('workspaceDirectory'),
      title: 'New session',
    ),
  );
}

Future<void> _copyToClipboard(BuildContext context, String text, String toast) async {
  await Clipboard.setData(ClipboardData(text: text));
  if (!context.mounted) return;
  showToast(context, toast, variant: ToastVariant.success, icon: LucideIcons.circleCheck300);
}

typedef _FileEntry = ({String path, String name, String directory});

/// Original `describeWorkspaceFilePath`.
_FileEntry _describeFilePath(String path) {
  final normalized = path.replaceAll(r'\', '/');
  final separator = normalized.lastIndexOf('/');
  return (
    path: normalized,
    name: separator >= 0 ? normalized.substring(separator + 1) : normalized,
    directory: separator >= 0 ? normalized.substring(0, separator) : '',
  );
}

// ---------------------------------------------------------------------------
// The palette

class _CommandCenter extends StatefulWidget {
  const _CommandCenter({
    required this.store,
    required this.initialFiles,
    required this.sheet,
  });
  final AppStore store;
  final bool initialFiles;
  final bool sheet;
  @override
  State<_CommandCenter> createState() => _CommandCenterState();
}

class _CommandCenterState extends State<_CommandCenter> {
  final _query = TextEditingController();
  final _focus = FocusNode();
  final _scroll = ScrollController();
  late bool _filesScope = widget.initialFiles;
  List<ShortcutBinding> _bindings = const [];
  String _lastQuery = '';
  Timer? _clock;
  Timer? _focusTimer;

  // `workspace-file-search.ts`
  Timer? _fileTimer;
  int _fileGeneration = 0;
  String? _fileSource;
  String? _fileRequest;
  List<_FileEntry> _fileEntries = const [];
  bool _fileLoading = false;
  String? _fileError;

  // `interaction.ts`: the selection persists; permission to move the viewport lasts
  // for one user command.
  String? _activeId;
  bool _revealPending = true;
  double _viewport = 0;
  _Projection _projection = const _Projection([], [], [], {});

  @override
  void initState() {
    super.initState();
    _query.addListener(_queryChanged);
    widget.store.addListener(_storeChanged);
    ShortcutCatalog.load().then((bindings) {
      if (mounted) setState(() => _bindings = bindings);
    }, onError: (Object _) {});
    // Relative times keep themselves current while the palette stays open.
    _clock = Timer.periodic(const Duration(minutes: 1), (_) {
      if (mounted) setState(() {});
    });
    if (widget.sheet) {
      _focusTimer = Timer(const Duration(milliseconds: 300), () {
        if (mounted) _focus.requestFocus();
      });
    }
    _syncFileSearch();
  }

  @override
  void dispose() {
    _fileGeneration++;
    _clock?.cancel();
    _focusTimer?.cancel();
    _fileTimer?.cancel();
    widget.store.removeListener(_storeChanged);
    _query.dispose();
    _focus.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _storeChanged() {
    if (!mounted) return;
    _syncFileSearch();
    setState(() {});
  }

  void _queryChanged() {
    if (_query.text == _lastQuery) return;
    _lastQuery = _query.text;
    setState(() {
      _activeId = null;
      _revealPending = true;
      _syncFileSearch();
    });
  }

  void _setFilesScope(bool files) {
    setState(() {
      _filesScope = files;
      _activeId = null;
      _revealPending = true;
      _syncFileSearch();
    });
  }

  /// Files are searched in the active workspace when the Files scope is on or anything
  /// has been typed; the previous entries stay up while the next query loads.
  void _syncFileSearch() {
    final target = widget.store.selected;
    final enabled = _filesScope || _query.text.trim().isNotEmpty;
    final source = target != null && target.serverId.isNotEmpty && target.cwd.isNotEmpty
        ? '${target.serverId}\u0000${target.workspaceId}\u0000${target.cwd}'
        : null;
    final request = enabled && source != null ? '$source\u0000${_query.text}' : null;
    if (request == _fileRequest) return;
    _fileTimer?.cancel();
    final generation = ++_fileGeneration;
    _fileRequest = request;
    if (request == null) {
      _fileSource = null;
      _fileEntries = const [];
      _fileLoading = false;
      _fileError = null;
      return;
    }
    if (_fileSource != source) _fileEntries = const [];
    _fileSource = source;
    _fileLoading = true;
    _fileError = null;
    final query = _query.text;
    _fileTimer = Timer(
      query.trim().isEmpty ? Duration.zero : const Duration(milliseconds: 100),
      () => _searchFiles(generation, target!, query),
    );
  }

  Future<void> _searchFiles(int generation, PanelTarget target, String query) async {
    try {
      final response = await widget.store.clientFor(target.serverId).request(
        'directory_suggestions_request',
        {
          'query': query,
          'cwd': target.cwd,
          'includeFiles': true,
          'includeDirectories': false,
          'matchMode': 'fuzzy',
          'limit': 100,
        },
        responseType: 'directory_suggestions_response',
      );
      if (!mounted || generation != _fileGeneration) return;
      final error = response.text('error');
      setState(() {
        _fileEntries = error.isNotEmpty
            ? const []
            : response
                  .array('entries')
                  .map(jsonMap)
                  .where((entry) => entry['kind'] == null || entry['kind'] == 'file')
                  .map((entry) => _describeFilePath(entry.text('path')))
                  .toList(growable: false);
        _fileLoading = false;
        _fileError = error.isEmpty ? null : error;
      });
    } catch (error) {
      if (!mounted || generation != _fileGeneration) return;
      setState(() {
        _fileEntries = const [];
        _fileLoading = false;
        _fileError = errorMessage(error);
      });
    }
  }

  /// Original `resolveShortcutKeysForAction`: the help id's binding for this platform,
  /// a valid user override in its place, nothing when the override unassigns it.
  String _shortcutFor(String helpId) {
    final binding = _bindings
        .where((binding) => binding.data.object('help').text('id') == helpId)
        .firstOrNull;
    if (binding == null) return '';
    final override = jsonMap(widget.store.preferences['keyboardShortcuts'])[binding.id];
    if (override is! String) return binding.combo;
    if (override.trim().isEmpty) return '';
    try {
      ShortcutBinding.parse(override);
      return override;
    } on FormatException {
      return binding.combo;
    }
  }

  /// Root (`root-registration.tsx`) and workspace (`workspace-registration.tsx`) actions
  /// in registry order.
  List<_Contribution> _contributions(BuildContext context, bool compact) {
    final store = widget.store;
    final shortcutsAvailable = !(Platform.isAndroid || Platform.isIOS) && !compact;
    String shortcut(String helpId) => shortcutsAvailable ? _shortcutFor(helpId) : '';
    _Run dispatch(String action, [JsonMap arguments = const {}]) =>
        (_, onCommand) => onCommand(action, arguments);
    final hostId = store.selected?.serverId ?? store.hosts.firstOrNull?.serverId;
    final section = tr(context, 'shell.commandCenter.actions');
    _Contribution root(
      String id,
      int rank, {
      required String title,
      required IconData icon,
      required List<String> keywords,
      required _Run run,
      bool queryOnly = false,
      String shortcutId = '',
    }) => _Contribution(
      id: 'root:$id',
      group: 'actions',
      groupRank: 0,
      rank: rank,
      keywords: keywords,
      queryOnly: queryOnly,
      title: title,
      sectionTitle: section,
      icon: icon,
      shortcut: shortcutId.isEmpty ? '' : shortcut(shortcutId),
      run: run,
    );
    return [
      if (hostId != null)
        root(
          'add-project',
          0,
          title: tr(context, 'shell.commandCenter.addProject'),
          icon: LucideIcons.folderPlus400,
          keywords: const ['open', 'project', 'folder', 'workspace', 'repo'],
          queryOnly: true,
          shortcutId: 'new-agent',
          run: (_, _) => store.open(
            PanelTarget(
              kind: 'new_tab',
              serverId: hostId,
              workspaceId: '',
              cwd: '',
              title: 'Add project',
              data: const {'addProject': true},
            ),
          ),
        ),
      root(
        'new-workspace',
        1,
        title: tr(context, 'sidebar.actions.newWorkspace'),
        icon: LucideIcons.plus500,
        keywords: const ['new', 'workspace', 'worktree', 'branch'],
        shortcutId: 'new-workspace',
        run: dispatch('workspace.new'),
      ),
      if (hostId != null)
        root(
          'import-session',
          2,
          title: tr(context, 'importSession.title'),
          icon: LucideIcons.import400,
          keywords: const ['import', 'session', 'terminal'],
          run: (context, _) => showImportSessions(context, store: store, serverId: hostId),
        ),
      if (hostId != null)
        root(
          'history',
          4,
          title: tr(context, 'sidebar.sections.sessions'),
          icon: LucideIcons.history400,
          keywords: const ['history', 'sessions', 'recent'],
          run: (context, _) =>
              showSessionHistory(context, store: store, serverId: hostId, allHosts: true),
        ),
      root(
        'schedules',
        5,
        title: tr(context, 'sidebar.sections.schedules'),
        icon: LucideIcons.calendarClock400,
        keywords: const ['schedules', 'scheduled', 'automation', 'recurring'],
        run: (context, _) => ShellPages.open(context, const SchedulesScreen()),
      ),
      root(
        'settings',
        6,
        title: tr(context, 'sidebar.actions.settings'),
        icon: LucideIcons.settings400,
        keywords: const ['settings', 'preferences', 'config', 'configuration'],
        shortcutId: 'toggle-settings',
        run: dispatch('settings.toggle'),
      ),
      if (shortcutsAvailable)
        root(
          'keyboard-shortcuts',
          7,
          title: tr(context, 'sidebar.help.shortcuts'),
          icon: LucideIcons.keyboard400,
          keywords: const ['keyboard', 'shortcuts', 'keys', 'hotkeys'],
          shortcutId: 'show-shortcuts',
          run: dispatch('shortcuts.dialog.toggle'),
        ),
      root(
        'toggle-left-sidebar',
        8,
        title: tr(context, 'settings.shortcuts.help.toggleLeftSidebar'),
        icon: LucideIcons.panelLeft400,
        keywords: const ['toggle', 'sidebar', 'left', 'panel', 'workspaces'],
        queryOnly: true,
        shortcutId: 'toggle-left-sidebar',
        run: dispatch('sidebar.toggle.left'),
      ),
      ..._workspaceContributions(context, compact, shortcut, dispatch),
    ]..sort(_compareContributions);
  }

  /// `workspace-contributions.ts` for the focused workspace, limited to the actions the
  /// Flutter shell can carry out.
  List<_Contribution> _workspaceContributions(
    BuildContext context,
    bool compact,
    String Function(String helpId) shortcut,
    _Run Function(String action, [JsonMap arguments]) dispatch,
  ) {
    final store = widget.store;
    final target = store.selected;
    if (target == null || target.serverId.isEmpty || target.cwd.isEmpty) return const [];
    final host = store.hostFor(target.serverId);
    final workspace = target.workspaceId.isEmpty
        ? null
        : host?.workspaces.where((item) => item.text('id') == target.workspaceId).firstOrNull;
    final section = tr(context, 'workspace.header.actions.workspaceActions');
    final canSplitPanes = !(Platform.isAndroid || Platform.isIOS) && !compact;
    final isGit = workspace != null &&
        (workspace['gitRuntime'] != null || workspace.text('projectKind') == 'git');
    final branch = workspace?.object('gitRuntime').text('currentBranch').trim() ?? '';
    final pinned = workspace?['pinnedAt'] != null;
    final canPin = workspace != null && (host?.client.supports('workspacePinning') ?? false);
    final pane = store.focusedPane;
    final tabKeys = pane.tabKeys;
    final tabIndex = tabKeys.indexOf(pane.selectedKey ?? '');
    _Contribution item(
      String id,
      int rank,
      String title,
      IconData icon,
      List<String> keywords,
      _Run run, {
      bool queryOnly = true,
      String shortcutId = '',
    }) => _Contribution(
      id: 'workspace:$id',
      group: 'workspace',
      groupRank: -1,
      rank: rank,
      keywords: keywords,
      queryOnly: queryOnly,
      title: title,
      sectionTitle: section,
      icon: icon,
      shortcut: shortcutId.isEmpty ? '' : shortcut(shortcutId),
      run: run,
    );
    String t(String key) => tr(context, key);
    String open(String nameKey) =>
        tr(context, 'shell.commandCenter.open', args: {'name': t(nameKey)});
    final path = workspace?.text('workspaceDirectory', target.cwd) ?? target.cwd;
    return [
      item(
        'tab:new-agent',
        0,
        t('workspace.tabs.actions.newAgent'),
        LucideIcons.squarePen300,
        const ['tab', 'new', 'agent', 'chat'],
        dispatch('workspace.tab.target.agent'),
        queryOnly: false,
        shortcutId: 'workspace-tab-target-agent',
      ),
      if (isGit)
        item(
          'tab:open:changes',
          4,
          open('workspace.tabs.actions.changes'),
          LucideIcons.gitCompareArrows300,
          const ['open', 'changes', 'tab', 'supporting'],
          dispatch('workspace.tab.target.changes'),
        ),
      item(
        'tab:open:files',
        5,
        open('workspace.tabs.actions.files'),
        LucideIcons.files300,
        const ['open', 'files', 'tab', 'supporting'],
        dispatch('workspace.tab.target.files'),
      ),
      if (tabKeys.isNotEmpty) ...[
        item(
          'tab:previous',
          30,
          t('settings.shortcuts.help.previousTab'),
          LucideIcons.arrowLeft300,
          const ['tab', 'previous', 'switch', 'navigate'],
          dispatch('workspace.tab.navigate.relative', const {'delta': -1}),
          shortcutId: 'workspace-tab-prev',
        ),
        item(
          'tab:next',
          31,
          t('settings.shortcuts.help.nextTab'),
          LucideIcons.arrowRight300,
          const ['tab', 'next', 'switch', 'navigate'],
          dispatch('workspace.tab.navigate.relative', const {'delta': 1}),
          shortcutId: 'workspace-tab-next',
        ),
        item(
          'tab:close-current',
          32,
          t('settings.shortcuts.help.closeCurrentTab'),
          LucideIcons.x300,
          const ['tab', 'close', 'current'],
          dispatch('workspace.tab.close.current'),
          shortcutId: 'workspace-tab-close-current',
        ),
        if ((target.kind == 'agent' || target.kind == 'terminal') &&
            (target.id ?? '').isNotEmpty)
          item(
            'tab:copy-id',
            34,
            t(
              target.kind == 'agent'
                  ? 'workspace.tabs.menu.copyAgentId'
                  : 'workspace.tabs.menu.copyTerminalId',
            ),
            LucideIcons.copy300,
            const ['tab', 'copy', 'id'],
            (context, _) async {
              await Clipboard.setData(ClipboardData(text: target.id!));
              if (context.mounted) showCopiedToast(context);
            },
          ),
        if (target.kind == 'file')
          item(
            'tab:copy-file-path',
            37,
            t('workspace.tabs.menu.copyFilePath'),
            LucideIcons.copy300,
            const ['tab', 'file', 'copy', 'path'],
            (context, _) async {
              await Clipboard.setData(
                ClipboardData(text: stringValue(target.data['path'], target.id ?? '')),
              );
              if (context.mounted) showCopiedToast(context);
            },
          ),
        if (tabIndex > 0)
          item(
            'tab:close-left',
            38,
            t('workspace.tabs.menu.closeLeft'),
            LucideIcons.x300,
            const ['tab', 'close', 'left'],
            dispatch('workspace.tab.close-left'),
          ),
        if (tabIndex >= 0 && tabIndex < tabKeys.length - 1)
          item(
            'tab:close-right',
            39,
            t('workspace.tabs.menu.closeRight'),
            LucideIcons.x300,
            const ['tab', 'close', 'right'],
            dispatch('workspace.tab.close-right'),
          ),
        if (tabKeys.length > 1)
          item(
            'tab:close-others',
            40,
            t('workspace.tabs.menu.closeOthers'),
            LucideIcons.x300,
            const ['tab', 'close', 'others'],
            dispatch('workspace.tab.close-others'),
          ),
      ],
      if (canSplitPanes) ...[
        item('pane:split-right', 50, t('workspace.tabs.actions.splitRight'),
            LucideIcons.columns2300, const ['split', 'pane', 'vertical'],
            dispatch('workspace.pane.split.right'), shortcutId: 'workspace-pane-split-right'),
        item('pane:split-down', 51, t('workspace.tabs.actions.splitDown'),
            LucideIcons.rows2300, const ['split', 'pane', 'horizontal'],
            dispatch('workspace.pane.split.down'), shortcutId: 'workspace-pane-split-down'),
        for (final (index, direction) in const ['left', 'right', 'up', 'down'].indexed)
          item(
            'pane:focus-$direction',
            52 + index,
            t('settings.shortcuts.help.focusPane${_capitalized(direction)}'),
            LucideIcons.focus300,
            ['pane', 'focus', direction],
            dispatch('workspace.pane.focus.$direction'),
          ),
        for (final (index, direction) in const ['left', 'right', 'up', 'down'].indexed)
          item(
            'pane:move-tab-$direction',
            56 + index,
            t('settings.shortcuts.help.moveTab${_capitalized(direction)}'),
            LucideIcons.move300,
            ['pane', 'tab', 'move', direction],
            dispatch('workspace.pane.move-tab.$direction'),
          ),
        item('pane:close', 60, t('settings.shortcuts.help.closePane'), LucideIcons.x300,
            const ['pane', 'close'], dispatch('workspace.pane.close'),
            shortcutId: 'workspace-pane-close'),
        item('pane:focus-mode-toggle', 62, t('settings.shortcuts.help.toggleFocusMode'),
            LucideIcons.arrowDownToLine300, const ['pane', 'focus', 'mode', 'toggle'],
            dispatch('view.toggle.focus'), shortcutId: 'toggle-focus'),
      ],
      if (canPin)
        item(
          'workspace:pin',
          20,
          t(pinned ? 'sidebar.workspace.actions.unpin' : 'sidebar.workspace.actions.pin'),
          pinned ? LucideIcons.pinOff300 : LucideIcons.pin300,
          const ['pin', 'unpin', 'favorite', 'sticky'],
          dispatch('workspace.pin'),
          queryOnly: false,
          shortcutId: 'pin-workspace',
        ),
      item(
        'workspace:copy-path',
        22,
        t('workspace.header.actions.copyPath'),
        LucideIcons.copy300,
        const ['copy', 'path', 'directory', 'folder', 'cwd'],
        (context, _) async {
          if (path.isEmpty) {
            showToast(
              context,
              tr(context, 'sidebar.workspace.toasts.workspacePathUnavailable'),
              variant: ToastVariant.error,
            );
            return;
          }
          await _copyToClipboard(context, path, tr(context, 'sidebar.workspace.toasts.pathCopied'));
        },
      ),
      if (branch.isNotEmpty)
        item(
          'workspace:copy-branch-name',
          23,
          t('workspace.header.actions.copyBranchName'),
          LucideIcons.gitBranch300,
          ['copy', 'branch', 'git', branch],
          (context, _) => _copyToClipboard(
            context,
            branch,
            tr(context, 'sidebar.workspace.toasts.branchNameCopied'),
          ),
        ),
      item(
        'workspace:toggle-explorer-sidebar',
        25,
        t('workspace.tabs.explorerSidebar.toggle'),
        LucideIcons.panelRight300,
        const ['toggle', 'sidebar', 'explorer', 'files', 'changes'],
        dispatch('sidebar.toggle.right'),
        shortcutId: 'toggle-right-sidebar',
      ),
      if (!canSplitPanes)
        item(
          'workspace:toggle-focus-mode',
          26,
          t('settings.shortcuts.help.toggleFocusMode'),
          LucideIcons.focus300,
          const ['toggle', 'focus', 'zen', 'distraction', 'fullscreen'],
          dispatch('view.toggle.focus'),
          shortcutId: 'toggle-focus',
        ),
    ];
  }

  /// Workspace (rank 2) and agent (rank 3) rows, pinned below every action section.
  List<_Section> _builtInSections(BuildContext context, String query) {
    final store = widget.store;
    final colors = PaseoColors.of(context);
    final showHost = store.hosts.length > 1;
    final workspaces = <_Result>[];
    final workspaceTitles = <String, String>{};
    for (final host in store.hosts) {
      final hostLabel = hostDisplayLabel(host);
      for (final workspace in host.workspaces) {
        if (workspace['archivingAt'] != null) continue;
        final id = workspace.text('id');
        final title = workspace.text('title').trim().isNotEmpty
            ? workspace.text('title')
            : workspace.text('name');
        workspaceTitles['${host.serverId}:$id'] = title;
        final project = workspace.object('project');
        final projectName = workspace.text('projectDisplayName', project.text('projectName'));
        final bucket = _workspaceBucket(workspace.text('status'));
        final viewKey = projectViewKey(host.serverId, {
          ...project,
          'projectId': workspace.text('projectId', project.text('projectId')),
        });
        workspaces.add(
          _Result(
            id: 'workspace:${host.serverId}:$id',
            kind: _ResultKind.workspace,
            title: title,
            subtitle: _joinSubtitle([
              showHost ? hostLabel : null,
              projectName,
              workspace.object('gitRuntime').text('currentBranch'),
            ]),
            activityAt: _parseDate(workspace.text('statusEnteredAt')),
            changeRequestNumber: _changeRequestNumber(workspace),
            icon: (backdrop) => StatusIcon(
              bucket: bucket,
              backdrop: backdrop,
              size: 16,
              child: ProjectIconSquare(name: projectName, viewKey: viewKey, size: 16),
            ),
            run: (_, _) => _openWorkspace(store, host, workspace),
          ),
        );
      }
    }
    final agents = <_Result>[];
    for (final host in store.hosts) {
      final hostLabel = hostDisplayLabel(host);
      for (final entry in host.agents) {
        final agent = _agentOf(entry);
        if (!_listedAgent(agent)) continue;
        final bucket = deriveStatusBucket(
          status: agent.text('status'),
          pendingPermissionCount: agent.array('pendingPermissions').length,
          requiresAttention: agent.flag('requiresAttention'),
          attentionReason: agent['attentionReason'] is String
              ? agent['attentionReason'] as String
              : null,
        );
        agents.add(
          _Result(
            id: 'agent:${host.serverId}:${agent.text('id')}',
            kind: _ResultKind.agent,
            title: _agentLabel(agent.text('title')) ?? tr(context, 'workspace.tabs.loading'),
            subtitle: _joinSubtitle([
              showHost ? hostLabel : null,
              workspaceTitles['${host.serverId}:${agent.text('workspaceId')}'] ??
                  _shortenPath(agent.text('cwd')),
            ]),
            hidden: [agent.text('cwd')],
            activityAt: _parseDate(agent.text('lastActivityAt', agent.text('updatedAt'))),
            icon: (backdrop) => StatusIcon(
              bucket: bucket,
              backdrop: backdrop,
              size: 16,
              child: ProviderIcon(provider: agent.text('provider'), size: 16, color: colors.muted),
            ),
            run: (_, _) => openNavigationAgent(store, host.serverId, agent),
          ),
        );
      }
    }
    return [
      _Section(
        id: 'workspaces',
        band: 1,
        rank: 2,
        title: tr(context, 'shell.commandCenter.workspaces'),
        results: _rankWorkspaces(workspaces, query),
      ),
      _Section(
        id: 'agents',
        band: 1,
        rank: 3,
        title: tr(context, 'shell.commandCenter.agents'),
        results: _rankBuiltIn(agents, query),
      ),
    ];
  }

  /// Daemon-matched file rows carry no client score, so they are pinned (rank 4).
  _Section _fileSection(BuildContext context) {
    final store = widget.store;
    final target = store.selected;
    return _Section(
      id: 'files',
      band: 1,
      rank: 4,
      title: tr(context, 'shell.commandCenter.files'),
      results: [
        if (target != null)
          for (final entry in _fileEntries)
            _Result(
              id: 'file:${entry.path}',
              kind: _ResultKind.file,
              title: entry.name,
              subtitle: entry.directory,
              icon: (_) => MaterialFileIcon(fileName: entry.name, size: 16),
              run: (_, _) => store.open(
                PanelTarget(
                  kind: 'file',
                  serverId: target.serverId,
                  workspaceId: target.workspaceId,
                  cwd: target.cwd,
                  id: entry.path,
                  title: entry.name,
                  data: {'path': entry.path},
                ),
              ),
            ),
      ],
    );
  }

  void _select(_Result result) => Navigator.pop(context, result.run);

  bool get _selectionFrozen => _filesScope && _fileLoading;

  void _move(int delta) {
    final results = _projection.selectable;
    if (_selectionFrozen || results.isEmpty) return;
    final current = results.indexWhere((result) => result.id == _activeId);
    final start = current < 0 && delta < 0 ? 0 : current;
    setState(() {
      _activeId = results[(start + delta + results.length) % results.length].id;
      _revealPending = true;
    });
  }

  void _submit() {
    final active = _projection.selectable.where((result) => result.id == _activeId).firstOrNull;
    if (active != null) _select(active);
  }

  /// Scrolls just far enough to show the active row, once per user command.
  void _reveal() {
    if (!mounted || !_revealPending || _viewport <= 0 || !_scroll.hasClients) return;
    if (_selectionFrozen) return;
    final activeId = _activeId;
    if (activeId == null) {
      if (!_fileLoading) _revealPending = false;
      return;
    }
    final index = _projection.rowIndexById[activeId];
    if (index == null) return;
    final top = _projection.offsets[index];
    final bottom = top + _projection.rows[index].height;
    _revealPending = false;
    final offset = _scroll.offset;
    var next = offset;
    if (top < offset) next = top;
    if (bottom > offset + _viewport) next = bottom - _viewport;
    next = next.clamp(0.0, _scroll.position.maxScrollExtent);
    if (next != offset) _scroll.jumpTo(next);
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final compact = isCompactLayout(context);
    final query = _query.text;
    final sections = _filesScope
        ? [_fileSection(context)]
        : [
            ..._buildContributionSections(_contributions(context, compact), query, colors.muted),
            _fileSection(context),
            ..._builtInSections(context, query),
          ];
    _projection = _projectRows(sections, compact: compact);
    final selectable = _projection.selectable;
    if (!_selectionFrozen) {
      _activeId = _activeId != null && selectable.any((result) => result.id == _activeId)
          ? _activeId
          : selectable.firstOrNull?.id;
    }
    if (_revealPending) WidgetsBinding.instance.addPostFrameCallback((_) => _reveal());
    final base = theme.textTheme.bodyMedium!;
    final inputStyle = base.copyWith(height: 20 / base.fontSize!);
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.arrowDown): () => _move(1),
        const SingleActivator(LogicalKeyboardKey.arrowUp): () => _move(-1),
        const SingleActivator(LogicalKeyboardKey.escape): () => Navigator.maybePop(context),
      },
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
            decoration: BoxDecoration(
              border: Border(bottom: BorderSide(color: theme.dividerColor)),
            ),
            child: Row(
              spacing: 8,
              children: [
                if (_filesScope)
                  _ScopeChip(
                    label: tr(context, 'shell.commandCenter.files'),
                    onRemove: () => _setFilesScope(false),
                  ),
                Expanded(
                  child: Focus(
                    // Original: Backspace in an empty scoped query drops the Files scope.
                    onKeyEvent: (_, event) {
                      if (event is KeyDownEvent &&
                          event.logicalKey == LogicalKeyboardKey.backspace &&
                          _filesScope &&
                          _query.text.trim().isEmpty) {
                        _setFilesScope(false);
                        return KeyEventResult.handled;
                      }
                      return KeyEventResult.ignored;
                    },
                    child: TextField(
                      controller: _query,
                      focusNode: _focus,
                      autofocus: !widget.sheet,
                      autocorrect: false,
                      enableSuggestions: false,
                      textCapitalization: TextCapitalization.none,
                      style: inputStyle,
                      decoration: InputDecoration(
                        hintText: tr(
                          context,
                          _filesScope
                              ? 'shell.commandCenter.filePlaceholder'
                              : 'shell.commandCenter.placeholder',
                        ),
                        hintStyle: inputStyle.copyWith(color: colors.muted),
                        hintMaxLines: 1,
                        isDense: true,
                        filled: false,
                        border: InputBorder.none,
                        enabledBorder: InputBorder.none,
                        focusedBorder: InputBorder.none,
                        contentPadding: const EdgeInsets.symmetric(vertical: 4),
                      ),
                      onEditingComplete: () {},
                      onSubmitted: (_) => _submit(),
                    ),
                  ),
                ),
                SizedBox.square(
                  dimension: 14,
                  child: _fileLoading
                      ? Semantics(
                          label: tr(context, 'shell.commandCenter.searchingFiles'),
                          child: CircularProgressIndicator(
                            strokeWidth: 1.5,
                            color: colors.muted,
                          ),
                        )
                      : null,
                ),
              ],
            ),
          ),
          if (_fileError != null)
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 12),
              decoration: BoxDecoration(
                border: Border(bottom: BorderSide(color: theme.dividerColor)),
              ),
              child: Semantics(
                liveRegion: true,
                child: Text(
                  '${tr(context, 'common.errors.error')}: $_fileError',
                  style: base.copyWith(color: colors.danger),
                ),
              ),
            ),
          Expanded(
            child: LayoutBuilder(
              builder: (context, constraints) {
                _viewport = constraints.maxHeight;
                final rows = _projection.rows;
                if (rows.isEmpty) {
                  if (_fileLoading || _fileError != null) return const SizedBox.shrink();
                  return Align(
                    alignment: Alignment.topCenter,
                    child: Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 24),
                      child: Text(
                        tr(context, 'shell.commandCenter.noMatches'),
                        textAlign: TextAlign.center,
                        style: base.copyWith(color: colors.muted),
                      ),
                    ),
                  );
                }
                return NotificationListener<UserScrollNotification>(
                  onNotification: (_) {
                    _revealPending = false;
                    return false;
                  },
                  child: ListView.builder(
                    controller: _scroll,
                    padding: EdgeInsets.zero,
                    keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.manual,
                    itemCount: rows.length,
                    itemBuilder: (context, index) {
                      final row = rows[index];
                      final result = row.result;
                      if (result == null) return _SectionRow(row: row);
                      return _ResultRow(
                        key: ValueKey(row.key),
                        result: result,
                        height: row.height,
                        query: query,
                        active: result.id == _activeId,
                        onSelect: _select,
                      );
                    },
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

String _capitalized(String value) =>
    value.isEmpty ? value : '${value[0].toUpperCase()}${value.substring(1)}';

/// The Files scope chip: as tall as the input's 28px line box so showing or dropping it
/// never resizes the header; 1px borderAccent, surface2, 6px radius, 8px padding, a muted
/// 14px folder, the medium label and a muted 12px X.
class _ScopeChip extends StatelessWidget {
  const _ScopeChip({required this.label, required this.onRemove});
  final String label;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    return Semantics(
      button: true,
      label: label,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onRemove,
          child: Container(
            height: 28,
            margin: const EdgeInsets.only(right: 4),
            padding: const EdgeInsets.symmetric(horizontal: 8),
            decoration: BoxDecoration(
              color: colors.surface2,
              borderRadius: BorderRadius.circular(6),
              border: Border.all(color: colors.borderAccent),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 6,
              children: [
                Icon(LucideIcons.folder400, size: 14, color: colors.muted),
                Text(
                  label,
                  style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500),
                ),
                Icon(LucideIcons.x400, size: 12, color: colors.muted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// A section's rule (8px either side) and/or its small muted label, bottom-aligned in
/// the height list-metrics gives it.
class _SectionRow extends StatelessWidget {
  const _SectionRow({required this.row});
  final _ListRow row;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final small = theme.textTheme.bodySmall!;
    return SizedBox(
      height: row.height,
      child: Column(
        mainAxisAlignment: MainAxisAlignment.end,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (row.divider)
            Container(
              height: 1,
              margin: const EdgeInsets.symmetric(vertical: 8),
              color: theme.dividerColor,
            ),
          if (row.title != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
              child: Text(
                row.title!,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: small.copyWith(
                  color: PaseoColors.of(context).muted,
                  fontWeight: FontWeight.normal,
                  height: 16 / small.fontSize!,
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `ResultRow`: a fixed-height row, 16px side padding, surface1 while active,
/// hovered or pressed (status marks knock out onto that same backdrop).
class _ResultRow extends StatefulWidget {
  const _ResultRow({
    super.key,
    required this.result,
    required this.height,
    required this.query,
    required this.active,
    required this.onSelect,
  });
  final _Result result;
  final double height;
  final String query;
  final bool active;
  final ValueChanged<_Result> onSelect;
  @override
  State<_ResultRow> createState() => _ResultRowState();
}

class _ResultRowState extends State<_ResultRow> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final result = widget.result;
    final query = widget.query;
    final highlighted = widget.active || hovered || pressed;
    final backdrop = highlighted ? colors.surface1 : colors.surface0;
    final base = theme.textTheme.bodyMedium!;
    final small = theme.textTheme.bodySmall!;
    final title = base.copyWith(height: 18 / base.fontSize!);
    final subtitle = small.copyWith(
      color: colors.muted,
      fontWeight: FontWeight.normal,
      height: 16 / small.fontSize!,
    );
    final icon = result.icon;
    final iconSlot = SizedBox(
      width: 16,
      height: 20,
      child: icon == null ? null : Center(child: icon(backdrop)),
    );
    final Widget content;
    switch (result.kind) {
      case _ResultKind.file:
        content = Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          spacing: 12,
          children: [
            iconSlot,
            Expanded(
              child: Text.rich(
                TextSpan(
                  children: [
                    ...paseoHighlightSpans(context, result.title, query),
                    if (result.subtitle.isNotEmpty)
                      TextSpan(
                        style: TextStyle(color: colors.muted),
                        children: [
                          const TextSpan(text: ' '),
                          ...paseoHighlightSpans(context, result.subtitle, query),
                        ],
                      ),
                  ],
                ),
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: base.copyWith(height: 20 / base.fontSize!),
              ),
            ),
          ],
        );
      case _ResultKind.workspace:
      case _ResultKind.agent:
        final time = paseoTimeAgo(result.activityAt);
        content = Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          spacing: 12,
          children: [
            iconSlot,
            Expanded(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  PaseoHighlightedText(result.title, query: query, style: title),
                  // The timestamp is its own text so a long subtitle truncates without
                  // pushing the time out of view, and it is never matched.
                  Row(
                    children: [
                      Flexible(
                        child: PaseoHighlightedText(
                          result.subtitle,
                          query: query,
                          style: subtitle,
                        ),
                      ),
                      if (time.isNotEmpty)
                        Text(
                          result.subtitle.isNotEmpty ? '\u00a0· $time' : time,
                          maxLines: 1,
                          style: subtitle,
                        ),
                    ],
                  ),
                ],
              ),
            ),
          ],
        );
      case _ResultKind.action:
        content = Row(
          spacing: 12,
          children: [
            Expanded(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                spacing: 12,
                children: [
                  if (icon != null) iconSlot,
                  Expanded(
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        PaseoHighlightedText(
                          result.title,
                          query: query,
                          style: result.subtitle.isEmpty
                              ? base.copyWith(height: 20 / base.fontSize!)
                              : title,
                        ),
                        if (result.subtitle.isNotEmpty)
                          PaseoHighlightedText(result.subtitle, query: query, style: subtitle),
                      ],
                    ),
                  ),
                ],
              ),
            ),
            if (result.shortcut.isNotEmpty) ShortcutBadge(result.shortcut),
          ],
        );
    }
    return Semantics(
      button: true,
      label: result.kind == _ResultKind.file
          ? [result.title, result.subtitle].where((part) => part.isNotEmpty).join(' ')
          : null,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: () => widget.onSelect(result),
          child: Container(
            height: widget.height,
            color: highlighted ? colors.surface1 : null,
            padding: const EdgeInsets.symmetric(horizontal: 16),
            alignment: Alignment.centerLeft,
            child: content,
          ),
        ),
      ),
    );
  }
}
