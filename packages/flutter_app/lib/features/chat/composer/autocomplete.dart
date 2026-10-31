import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/json.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';

/// utils/agent-command-autocomplete.ts SlashCommandRange.
class SlashCommandRange {
  const SlashCommandRange(this.start, this.end, this.query, {required this.atStart});
  final int start, end;
  final String query;
  final bool atStart;
}

/// utils/file-mention-autocomplete.ts FileMentionRange.
class FileMentionRange {
  const FileMentionRange(this.start, this.end, this.query);
  final int start, end;
  final String query;
}

final _invalidSlashQuery = RegExp(r'''[/\s"']''');
final _invalidMentionQuery = RegExp(r'''[\s"'<>]''');

/// findActiveSlashCommand: the nearest `/` before the cursor that starts the
/// text or follows whitespace, with no slash, space or quote after it.
SlashCommandRange? findActiveSlashCommand(String text, int cursorIndex) {
  final cursor = cursorIndex.clamp(0, text.length);
  final before = text.substring(0, cursor);
  for (var slash = before.lastIndexOf('/');
      slash >= 0;
      slash = slash == 0 ? -1 : before.lastIndexOf('/', slash - 1)) {
    if (slash > 0 && !RegExp(r'\s').hasMatch(text[slash - 1])) continue;
    final query = before.substring(slash + 1);
    if (_invalidSlashQuery.hasMatch(query)) continue;
    return SlashCommandRange(slash, cursor, query, atStart: slash == 0);
  }
  return null;
}

/// findActiveFileMention: the nearest `@` before the cursor that is not part
/// of an email address, with no whitespace, quote or angle bracket after it.
FileMentionRange? findActiveFileMention(String text, int cursorIndex) {
  final cursor = cursorIndex.clamp(0, text.length);
  final before = text.substring(0, cursor);
  for (var at = before.lastIndexOf('@');
      at >= 0;
      at = at == 0 ? -1 : before.lastIndexOf('@', at - 1)) {
    if (at > 0 && RegExp(r'[\w+]').hasMatch(before[at - 1])) continue;
    final query = before.substring(at + 1);
    if (_invalidMentionQuery.hasMatch(query)) continue;
    return FileMentionRange(at, cursor, query);
  }
  return null;
}

/// applySlashCommandReplacement: a trailing space only when the command ends the text.
String applySlashCommandReplacement(String text, SlashCommandRange command, String name) {
  final replaced = '${text.substring(0, command.start)}/$name${text.substring(command.end)}';
  return command.end == text.length ? '$replaced ' : replaced;
}

/// applyFileMentionReplacement: the mention becomes the quoted relative path.
String applyFileMentionReplacement(String text, FileMentionRange mention, String path) =>
    '${text.substring(0, mention.start)}"${path.replaceAll('"', r'\"')}"'
    '${text.substring(mention.end)}';

/// client-slash-commands CLIENT_SLASH_COMMANDS.
const clientSlashCommands = <JsonMap>[
  {
    'name': 'exit',
    'aliases': ['quit', 'q'],
    'descriptionKey': 'composer.clientCommands.archiveAgent',
    'argumentHint': '',
  },
  {
    'name': 'clear',
    'aliases': ['new'],
    'descriptionKey': 'composer.clientCommands.freshDraft',
    'argumentHint': '',
  },
];

/// resolveClientSlashCommand: the canonical name of a whole-text client
/// command (or one of its aliases); none when attachments are present.
String? resolveClientSlashCommand(String text, {required bool hasAttachments}) {
  if (hasAttachments) return null;
  final trimmed = text.trim();
  if (!trimmed.startsWith('/')) return null;
  final name = trimmed.substring(1);
  if (name.isEmpty || RegExp(r'\s').hasMatch(name)) return null;
  for (final command in clientSlashCommands) {
    if (command['name'] == name || (command['aliases'] as List).contains(name)) {
      return stringValue(command['name']);
    }
  }
  return null;
}

// @getpaseo/protocol search/text-match tiers (no typo tolerance for pickers).
const _tierExact = 0, _tierWholeWord = 1, _tierPrefix = 2, _tierWordStart = 3;
const _tierSubstring = 4, _tierSubsequence = 5;

typedef MatchScore = ({int tier, int offset, int spread});

bool _boundary(String? ch) => ch == null || !RegExp(r'[a-z0-9]').hasMatch(ch);

MatchScore? _scoreMatch(String query, String text) {
  final q = query.toLowerCase(), t = text.toLowerCase();
  if (q.isEmpty || t == q) return (tier: _tierExact, offset: 0, spread: 0);
  MatchScore? best;
  var pos = 0;
  while (pos <= t.length - q.length) {
    final found = t.indexOf(q, pos);
    if (found < 0) break;
    final starts = found == 0 || _boundary(t[found - 1]);
    final ends = found + q.length >= t.length || _boundary(t[found + q.length]);
    final tier = starts && ends
        ? _tierWholeWord
        : found == 0
        ? _tierPrefix
        : starts
        ? _tierWordStart
        : _tierSubstring;
    if (best == null || tier < best.tier || (tier == best.tier && found < best.offset)) {
      best = (tier: tier, offset: found, spread: 0);
    }
    pos = found + 1;
  }
  if (best != null) return best;
  var qi = 0, first = -1, last = -1;
  for (var ti = 0; ti < t.length && qi < q.length; ti++) {
    if (RegExp(r'\s').hasMatch(t[ti])) {
      qi = 0;
      first = -1;
      last = -1;
      continue;
    }
    if (t[ti] != q[qi]) continue;
    if (first == -1) first = ti;
    last = ti;
    qi++;
  }
  if (qi != q.length || first == -1) return null;
  return (tier: _tierSubsequence, offset: first, spread: last - first + 1);
}

int compareMatchScores(MatchScore a, MatchScore b) {
  if (a.tier != b.tier) return a.tier - b.tier;
  if (a.offset != b.offset) return a.offset - b.offset;
  return a.spread - b.spread;
}

/// scoreTextFields: every query token must match one field; scores add up.
MatchScore? scoreTextFields(String query, List<String> fields) {
  final tokens = query.trim().toLowerCase().split(RegExp(r'\s+')).where((t) => t.isNotEmpty);
  var tier = 0, offset = 0, spread = 0;
  for (final token in tokens) {
    MatchScore? best;
    for (final field in fields) {
      final score = _scoreMatch(token, field);
      if (score != null && (best == null || compareMatchScores(score, best) < 0)) best = score;
    }
    if (best == null) return null;
    tier += best.tier;
    offset += best.offset;
    spread += best.tier == _tierSubsequence ? best.spread : token.length;
  }
  return (tier: tier, offset: offset, spread: spread);
}

/// components/ui/autocomplete.tsx AutocompleteOption plus the hook's payload.
class AutocompleteOption {
  const AutocompleteOption({
    required this.id,
    required this.label,
    required this.kind,
    this.detail,
    this.description,
    this.clientCommand = false,
    this.entryPath,
  });
  final String id, label, kind;
  final String? detail, description, entryPath;
  final bool clientCommand;
}

enum ComposerAutocompleteMode { command, file }

/// hooks/use-agent-autocomplete.ts + hooks/use-autocomplete.ts: which popover
/// is active for the text and cursor, its options (ordered above the input,
/// so reversed with the best match nearest the input), the keyboard-selected
/// row, and the loading/error/empty presentation.
class ComposerAutocomplete extends ChangeNotifier {
  ComposerAutocomplete({required this.loadFiles});

  /// directory_suggestions_request for a query; entries carry path and kind.
  final Future<List<JsonMap>> Function(String query) loadFiles;

  String _text = '';
  int _cursor = 0;
  SlashCommandRange? slash;
  FileMentionRange? mention;
  List<JsonMap> providerCommands = const [];
  bool commandsLoading = false;
  String? commandsError;
  bool isDraft = false;

  /// Draft key for why commands cannot be listed yet (needs a model).
  String? draftEmptyKey;
  bool canLoadFiles = true;
  List<JsonMap> _fileEntries = const [];
  bool _filePending = false;
  String? _fileError;
  String? _loadedFileQuery;
  Timer? _debounce;
  int _fileGeneration = 0;
  int selectedIndex = -1;
  String _previousQuery = '';
  List<AutocompleteOption> options = const [];
  bool _disposed = false;

  ComposerAutocompleteMode? get mode => mention != null
      ? ComposerAutocompleteMode.file
      : slash != null
      ? ComposerAutocompleteMode.command
      : null;

  bool get visible => switch (mode) {
    ComposerAutocompleteMode.command => !commandsLoading || draftEmptyKey != null,
    ComposerAutocompleteMode.file => canLoadFiles,
    null => false,
  };

  bool get isLoading => switch (mode) {
    ComposerAutocompleteMode.command => commandsLoading && options.isEmpty,
    ComposerAutocompleteMode.file => _filePending && options.isEmpty,
    null => false,
  };

  String? get errorMessage => switch (mode) {
    ComposerAutocompleteMode.command => commandsError,
    ComposerAutocompleteMode.file => _fileError,
    null => null,
  };

  String loadingKey() => mode == ComposerAutocompleteMode.file
      ? 'agentAutocomplete.searchingWorkspace'
      : 'agentAutocomplete.loadingCommands';

  String emptyKey() => mode == ComposerAutocompleteMode.file
      ? 'agentAutocomplete.noFiles'
      : draftEmptyKey ?? 'agentAutocomplete.noCommands';

  String get _query => mode == ComposerAutocompleteMode.file
      ? mention!.query
      : slash?.query ?? '';

  void update(String text, int cursor) {
    _text = text;
    _cursor = cursor < 0 ? text.length : cursor;
    slash = findActiveSlashCommand(_text, _cursor);
    mention = findActiveFileMention(_text, _cursor);
    if (mode == ComposerAutocompleteMode.file) {
      final query = mention!.query;
      if (query != _loadedFileQuery) {
        _debounce?.cancel();
        // The original debounces the workspace search by 180ms.
        _debounce = Timer(const Duration(milliseconds: 180), () => _fetchFiles(query));
        if (_loadedFileQuery == null) _filePending = true;
      }
    } else {
      _debounce?.cancel();
    }
    _rebuild();
  }

  void setCommands(List<JsonMap> commands, {bool loading = false, String? error}) {
    providerCommands = commands;
    commandsLoading = loading;
    commandsError = error;
    _rebuild();
  }

  Future<void> _fetchFiles(String query) async {
    final own = ++_fileGeneration;
    if (!canLoadFiles) return;
    try {
      final entries = await loadFiles(query);
      if (_disposed || own != _fileGeneration) return;
      _fileEntries = entries
          .where((e) => e['kind'] == 'file' || e['kind'] == 'directory')
          .toList();
      _fileError = null;
    } catch (e) {
      if (_disposed || own != _fileGeneration) return;
      _fileError = '$e';
    }
    _loadedFileQuery = query;
    _filePending = false;
    _rebuild();
  }

  void _rebuild() {
    options = _buildOptions();
    final query = _query;
    final queryChanged = _previousQuery != query;
    _previousQuery = query;
    if (!visible || options.isEmpty) {
      selectedIndex = -1;
    } else if (queryChanged || selectedIndex < 0 || selectedIndex >= options.length) {
      selectedIndex = options.length - 1;
    }
    if (!_disposed) notifyListeners();
  }

  List<AutocompleteOption> _buildOptions() {
    if (!visible) return const [];
    if (mode == ComposerAutocompleteMode.file) {
      return [
        for (final entry in _fileEntries.reversed)
          AutocompleteOption(
            id: '${entry['kind']}:${entry['path']}',
            label: stringValue(entry['path']),
            kind: stringValue(entry['kind']),
            entryPath: stringValue(entry['path']),
          ),
      ];
    }
    if (mode != ComposerAutocompleteMode.command) return const [];
    final entries = <(JsonMap, bool)>[];
    if (slash!.atStart) {
      final claimed = <String>{};
      if (!isDraft) {
        for (final command in clientSlashCommands) {
          entries.add((command, true));
          claimed
            ..add(stringValue(command['name']))
            ..addAll((command['aliases'] as List).cast<String>());
        }
      } else {
        for (final command in clientSlashCommands) {
          claimed
            ..add(stringValue(command['name']))
            ..addAll((command['aliases'] as List).cast<String>());
        }
      }
      for (final command in providerCommands) {
        if (!claimed.contains(stringValue(command['name']))) entries.add((command, false));
      }
    } else {
      // Inline slashes only offer provider skills.
      for (final command in providerCommands) {
        if (command['kind'] == 'skill') entries.add((command, false));
      }
    }
    final query = slash!.query.trim().toLowerCase();
    var ranked = entries;
    if (query.isNotEmpty) {
      final scored = <((JsonMap, bool), MatchScore)>[];
      for (final entry in entries) {
        final score = scoreTextFields(query, [
          stringValue(entry.$1['name']),
          ...jsonList(entry.$1['aliases']).map((alias) => '$alias'),
        ]);
        if (score != null) scored.add((entry, score));
      }
      scored.sort((a, b) {
        final byScore = compareMatchScores(a.$2, b.$2);
        return byScore != 0
            ? byScore
            : stringValue(a.$1.$1['name']).compareTo(stringValue(b.$1.$1['name']));
      });
      ranked = [for (final row in scored) row.$1];
    }
    return [
      for (final (command, client) in ranked.reversed)
        AutocompleteOption(
          id: stringValue(command['name']),
          label: '/${command['name']}',
          kind: 'command',
          detail: stringValue(command['argumentHint']).isEmpty
              ? null
              : stringValue(command['argumentHint']),
          description: client
              ? stringValue(command['descriptionKey'])
              : stringValue(command['description']),
          clientCommand: client,
        ),
    ];
  }

  /// use-autocomplete.ts onKeyPress: Up/Down move with wrap-around,
  /// Tab/Enter select (falling back to the row nearest the input), Escape
  /// clears a leading slash command. Returns the option to apply for a select.
  ({bool handled, AutocompleteOption? select, bool escape}) handleKey(KeyEvent event) {
    const none = (handled: false, select: null, escape: false);
    if (!visible || options.isEmpty) return none;
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) return none;
    final key = event.logicalKey;
    if (key == LogicalKeyboardKey.arrowUp || key == LogicalKeyboardKey.arrowDown) {
      final count = options.length;
      final down = key == LogicalKeyboardKey.arrowDown;
      selectedIndex = selectedIndex < 0
          ? (down ? 0 : count - 1)
          : down
          ? (selectedIndex % count + 1) % count
          : (selectedIndex % count - 1 + count) % count;
      notifyListeners();
      return (handled: true, select: null, escape: false);
    }
    if (event is KeyDownEvent &&
        (key == LogicalKeyboardKey.tab ||
            key == LogicalKeyboardKey.enter ||
            key == LogicalKeyboardKey.numpadEnter)) {
      final index = selectedIndex >= 0 && selectedIndex < options.length
          ? selectedIndex
          : options.length - 1;
      return (handled: true, select: options[index], escape: false);
    }
    if (event is KeyDownEvent &&
        key == LogicalKeyboardKey.escape &&
        mode == ComposerAutocompleteMode.command &&
        slash!.atStart) {
      return (handled: true, select: null, escape: true);
    }
    return none;
  }

  /// The text after applying [option]; null when nothing applies.
  String? applied(AutocompleteOption option) {
    if (option.kind == 'command') {
      final range = slash;
      if (range == null) return '/${option.id} ';
      return applySlashCommandReplacement(_text, range, option.id);
    }
    final range = mention;
    if (range == null || option.entryPath == null) return null;
    return applyFileMentionReplacement(_text, range, option.entryPath!);
  }

  @override
  void dispose() {
    _disposed = true;
    _debounce?.cancel();
    super.dispose();
  }
}

/// components/ui/autocomplete-popover.tsx + autocomplete.tsx: a 220px-max
/// surface1 card (1px borderAccent, 8px radius, md shadow) 12px above the
/// composer, matching its width; rows 36px min with 12/8 insets, the active or
/// hovered row on surface2, pinned to the bottom. Command rows are inline
/// (label + muted description); file rows carry a 14px folder/file glyph. A
/// selected command with a description adds a detail card above the list.
class AutocompletePopover extends StatefulWidget {
  const AutocompletePopover({
    super.key,
    required this.autocomplete,
    required this.onSelect,
    this.maxHeight = 220,
  });
  final ComposerAutocomplete autocomplete;
  final ValueChanged<AutocompleteOption> onSelect;
  final double maxHeight;

  @override
  State<AutocompletePopover> createState() => _AutocompletePopoverState();
}

class _AutocompletePopoverState extends State<AutocompletePopover> {
  final scroll = ScrollController();
  final keys = <int, GlobalKey>{};
  List<AutocompleteOption>? _pinnedFor;
  int _revealed = -1;

  @override
  void dispose() {
    scroll.dispose();
    super.dispose();
  }

  void _afterLayout(List<AutocompleteOption> options, int selected) {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !scroll.hasClients) return;
      if (!identical(_pinnedFor, options)) {
        _pinnedFor = options;
        scroll.jumpTo(scroll.position.maxScrollExtent);
      }
      if (selected != _revealed) {
        _revealed = selected;
        final row = keys[selected]?.currentContext;
        if (row != null) {
          Scrollable.ensureVisible(row, alignmentPolicy: ScrollPositionAlignmentPolicy.keepVisibleAtEnd);
          Scrollable.ensureVisible(row, alignmentPolicy: ScrollPositionAlignmentPolicy.keepVisibleAtStart);
        }
      }
    });
  }

  BoxDecoration _card(BuildContext context) {
    final palette = PaseoColors.of(context);
    final dark = Theme.of(context).brightness == Brightness.dark;
    return BoxDecoration(
      color: palette.surface1,
      border: Border.all(color: palette.borderAccent),
      borderRadius: BorderRadius.circular(8),
      boxShadow: [
        BoxShadow(
          color: dark ? const Color(0x33000000) : const Color(0x0a000000),
          offset: const Offset(0, 4),
          blurRadius: dark ? 8 : 16,
        ),
      ],
    );
  }

  String _clean(String value) => value.replaceAll(RegExp('\u26A1|\uFE0F'), '').trim();

  @override
  Widget build(BuildContext context) {
    final state = widget.autocomplete;
    final colors = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final base = text.bodyMedium!;
    final small = text.bodySmall!.copyWith(color: colors.onSurfaceVariant);
    String describe(AutocompleteOption option) {
      final raw = option.description ?? '';
      return option.clientCommand ? tr(context, raw) : raw;
    }

    Widget message(String value) => Container(
      constraints: BoxConstraints(maxHeight: widget.maxHeight),
      decoration: _card(context),
      padding: const EdgeInsets.all(12),
      width: double.infinity,
      child: Text(value, style: base.copyWith(color: colors.onSurfaceVariant)),
    );
    if (state.isLoading) return message(tr(context, state.loadingKey()));
    if (state.errorMessage != null) return message('Error: ${state.errorMessage}');
    final options = state.options;
    if (options.isEmpty) return message(tr(context, state.emptyKey()));
    final selected = state.selectedIndex;
    _afterLayout(options, selected);
    final active = selected >= 0 && selected < options.length ? options[selected] : null;
    final activeDescription = active == null ? '' : _clean(describe(active));
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: 4,
      children: [
        if (active?.kind == 'command' && activeDescription.isNotEmpty)
          Container(
            decoration: _card(context),
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 4,
              children: [
                Text(_clean(active!.label), style: base),
                Text(activeDescription, style: small),
                if (active.detail != null) Text(_clean(active.detail!), style: small),
              ],
            ),
          ),
        Flexible(
          child: Container(
            constraints: BoxConstraints(maxHeight: widget.maxHeight),
            decoration: _card(context),
            clipBehavior: Clip.antiAlias,
            child: ListView.builder(
              controller: scroll,
              shrinkWrap: true,
              padding: const EdgeInsets.symmetric(vertical: 4),
              itemCount: options.length,
              itemBuilder: (context, index) => _AutocompleteRow(
                key: keys.putIfAbsent(index, GlobalKey.new),
                option: options[index],
                description: _clean(describe(options[index])),
                selected: index == selected,
                onTap: () => widget.onSelect(options[index]),
              ),
            ),
          ),
        ),
      ],
    );
  }
}

class _AutocompleteRow extends StatefulWidget {
  const _AutocompleteRow({
    super.key,
    required this.option,
    required this.description,
    required this.selected,
    required this.onTap,
  });
  final AutocompleteOption option;
  final String description;
  final bool selected;
  final VoidCallback onTap;

  @override
  State<_AutocompleteRow> createState() => _AutocompleteRowState();
}

class _AutocompleteRowState extends State<_AutocompleteRow> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final text = Theme.of(context).textTheme;
    final base = text.bodyMedium!;
    final small = text.bodySmall!.copyWith(color: colors.onSurfaceVariant);
    final option = widget.option;
    final label = option.label.replaceAll(RegExp('\u26A1|\uFE0F'), '').trim();
    final fileOrDir = option.kind == 'file' || option.kind == 'directory';
    final Widget body = fileOrDir
        ? Row(
            children: [
              Container(
                width: 18,
                margin: const EdgeInsets.only(right: 4),
                alignment: Alignment.center,
                child: Icon(
                  option.kind == 'directory' ? LucideIcons.folder300 : LucideIcons.file300,
                  size: 14,
                  color: colors.onSurfaceVariant,
                ),
              ),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      spacing: 8,
                      children: [
                        Flexible(child: Text(label, style: base)),
                        if (option.detail != null) Text(option.detail!, style: small),
                      ],
                    ),
                    if (widget.description.isNotEmpty)
                      Padding(
                        padding: const EdgeInsets.only(top: 2),
                        child: Text(widget.description, maxLines: 1,
                            overflow: TextOverflow.ellipsis, style: small),
                      ),
                  ],
                ),
              ),
            ],
          )
        : Row(
            spacing: 8,
            children: [
              Text(label, style: base),
              if (widget.description.isNotEmpty)
                Expanded(
                  child: Text(widget.description, maxLines: 1,
                      overflow: TextOverflow.ellipsis, style: small),
                ),
            ],
          );
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTapDown: (_) => setState(() => pressed = true),
        onTapCancel: () => setState(() => pressed = false),
        onTapUp: (_) => setState(() => pressed = false),
        onTap: widget.onTap,
        child: Container(
          constraints: const BoxConstraints(minHeight: 36),
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          color: hovered || pressed || widget.selected ? palette.surface2 : null,
          alignment: Alignment.centerLeft,
          child: body,
        ),
      ),
    );
  }
}
