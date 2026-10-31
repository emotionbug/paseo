import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/json.dart';
import '../../../ui/common.dart';
import '../../../ui/controls.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';
import '../chat_controller.dart' show maps;
import '../chat_markdown.dart'
    show ChatFindTarget, chatFindHighlightStyle, chatFindMarkdownMatchCount, chatFindPattern;
import 'chrome_controls.dart';

/// chat-find/model.ts ChatFindFailure: `connection` is the host call itself
/// failing, `historyChanged` the timeline moving under a result set, and
/// `reveal` a located match that never made it onto the screen.
enum ChatFindFailure { connection, historyChanged, reveal }

class _FindFailure implements Exception {
  const _FindFailure(this.failure);
  final ChatFindFailure failure;
}

/// model.ts tagged: a failing operation is named by its call site unless it
/// already carries a name.
Future<T> _tagged<T>(ChatFindFailure failure, Future<T> operation) => operation.catchError(
  (Object cause) => throw cause is _FindFailure ? cause : _FindFailure(failure),
);

/// model.ts AbortController: a cancel retires the running search, navigation
/// or wait, whose late results are then discarded.
class _Signal {
  bool aborted = false;
  final Set<VoidCallback> _listeners = {};

  void abort() {
    if (aborted) return;
    aborted = true;
    for (final listener in List.of(_listeners)) {
      listener();
    }
    _listeners.clear();
  }
}

/// A message the host says contains the query. [count] is the host's
/// estimate of its occurrences until a reveal replaces it with the rendered
/// count.
class _Location {
  _Location(this.seq, this.role, this.count);
  final int seq;
  final String role;
  int count;
}

/// The loaded row a location resolved to: [id] is its seqStart, [seq] its
/// seqEnd (the row's timeline cursor), [before] the newest seq loaded ahead
/// of the location.
class _Target {
  const _Target(this.id, this.seq, this.before);
  final int id, seq, before;
}

/// A message's rendered occurrence and its rendered occurrence count.
typedef ChatFindReveal = ({int occurrence, int count});

int _rowSeq(JsonMap row) => intValue(row['seqEnd'], intValue(row['seqStart']));

/// ranges.web.ts findMessageMatches over one loaded row: the occurrences of
/// [query] its rendered text holds (the user bubble's plain text, or the
/// assistant message's rendered Markdown blocks).
int chatFindRowMatchCount(JsonMap row, String query) {
  final item = jsonMap(row['item']);
  final text = stringValue(item['text']);
  return switch (item['type']) {
    'assistant_message' => chatFindMarkdownMatchCount(text, query),
    'user_message' => chatFindPattern(query)?.allMatches(text).length ?? 0,
    _ => 0,
  };
}

/// The host operations chat find runs against.
abstract interface class ChatFindOperations {
  /// agent.timeline.search.request page: {epoch, locations, nextCursor}.
  Future<JsonMap> search(String query, int? cursor);

  /// The loaded timeline's epoch, or null before the first page.
  String? get epoch;

  /// The loaded timeline rows ({seqStart, seqEnd, item}) in reading order.
  List<JsonMap> get rows;

  /// Loads the history holding [seq] of [epoch] (index.web.tsx load:
  /// fetchAgentTimeline planTimelinePromptJump); throws when the load fails.
  /// The rows arrive through [ChatFindController.historyChanged].
  Future<void> load(String epoch, int seq);

  /// Brings the row starting at [rowSeqStart] on screen with its highlighted
  /// match just below the find widget (viewport.web.ts scrollToMessage
  /// targetTop; see [chatFindScrollToMatch]).
  void scrollTo(int rowSeqStart);
}

/// agent-stream/chat-find/model.ts ChatFindModel: debounced host search
/// (120ms) over the whole chat, a position across every match, next/previous
/// stepping through each message's rendered occurrences before moving on, and
/// a failure the widget can name. The selected message is [selectedRow]; the
/// current occurrence inside it is [highlight].
class ChatFindController extends ChangeNotifier {
  ChatFindController(this.operations);
  final ChatFindOperations operations;

  bool open = false;
  String query = '';

  /// idle | searching | loading | ready | error.
  String phase = 'idle';
  int? selectedRow;

  /// Position across the whole chat: the search scope, not the selected row.
  int occurrence = 0, count = 0;
  ChatFindFailure? failure;

  /// Bumped by every [show]: the widget takes focus and selects the query
  /// (index.web.tsx find shortcut → widget.focus()).
  int focusRequests = 0;

  /// viewport.web.ts targetTop clearance: the find widget's height plus 8px,
  /// so a revealed match lands just below the widget (0 while closed).
  double get matchClearance => open ? _widgetHeight + 8 : 0;
  double _widgetHeight = 0;

  /// The occurrence to highlight in [selectedRow], or null while none is
  /// revealed (viewport.web.ts CSS highlight; cleared by every cancel).
  ChatFindTarget? get highlight => open && _highlighted && selectedRow != null
      ? (query: query, occurrence: _inMessage.occurrence)
      : null;

  final List<_Location> _locations = [];
  final Map<_Location, _Target> _resolved = {};
  final Set<VoidCallback> _historyListeners = {};
  String? _resultEpoch;
  int _current = -1;
  ChatFindReveal _inMessage = (occurrence: 0, count: 0);
  bool _highlighted = false;
  _Signal _signal = _Signal();
  Timer? _timer;
  bool _disposed = false;

  void _publish() {
    if (!_disposed) notifyListeners();
  }

  void _cancel() {
    _signal.abort();
    _signal = _Signal();
    _timer?.cancel();
    _highlighted = false;
  }

  void show() {
    focusRequests++;
    if (open) {
      _publish();
      return;
    }
    open = true;
    _publish();
    if (query.isNotEmpty) setQuery(query);
  }

  void close() {
    _cancel();
    open = false;
    phase = 'idle';
    selectedRow = null;
    count = 0;
    failure = null;
    _publish();
  }

  void retry() => setQuery(query);

  void setQuery(String value) {
    _cancel();
    _locations.clear();
    _resultEpoch = null;
    _resolved.clear();
    _current = -1;
    _inMessage = (occurrence: 0, count: 0);
    query = value;
    phase = value.trim().isEmpty ? 'idle' : 'searching';
    selectedRow = null;
    occurrence = 0;
    count = 0;
    failure = null;
    _publish();
    if (value.trim().isEmpty) return;
    final signal = _signal;
    _timer = Timer(const Duration(milliseconds: 120), () => unawaited(_search(signal)));
  }

  /// model.ts updateHistory: the loaded timeline changed. A different epoch
  /// under an open result set fails the search; waits for a row re-check.
  void historyChanged(String? epoch) {
    if (_resultEpoch != null && epoch != null && epoch != _resultEpoch && open) {
      _fail(const _FindFailure(ChatFindFailure.historyChanged), _signal);
    }
    for (final listener in List.of(_historyListeners)) {
      listener();
    }
  }

  void _fail(Object error, _Signal signal) {
    if (signal.aborted) return;
    _cancel();
    phase = 'error';
    selectedRow = null;
    // Every operation call site is tagged, so an untagged failure is a bug;
    // `connection` is the least misleading thing to say about one.
    failure = error is _FindFailure ? error.failure : ChatFindFailure.connection;
    _publish();
  }

  Future<void> _search(_Signal signal) async {
    try {
      int? cursor;
      do {
        final result = await _tagged(ChatFindFailure.connection, operations.search(query, cursor));
        if (signal.aborted) return;
        final epoch = stringValue(result['epoch']);
        final loaded = operations.epoch;
        if ((loaded != null && epoch != loaded) ||
            (_resultEpoch != null && epoch != _resultEpoch)) {
          throw const _FindFailure(ChatFindFailure.historyChanged);
        }
        _resultEpoch = epoch;
        for (final location in maps(result['locations'])) {
          _locations.add(_Location(
            intValue(location['seq']),
            stringValue(location['role'], 'assistant'),
            // COMPAT(timelineSearchCount): hosts before v0.9.0 send no count.
            location['count'] is int ? location['count'] as int : 1,
          ));
        }
        cursor = result['nextCursor'] is int ? result['nextCursor'] as int : null;
      } while (cursor != null);
      await _navigate(0, 1, signal);
    } catch (e) {
      _fail(e, signal);
    }
  }

  /// model.ts target: the loaded row of [location]'s role holding it
  /// ([exact]) or the first one after it; a prompt is a hard boundary, so an
  /// assistant hit never resolves into a later response.
  _Target? _target(_Location location, bool exact) {
    if (operations.epoch != _resultEpoch) return null;
    var before = 0;
    for (final row in operations.rows) {
      final start = intValue(row['seqStart']);
      final seq = _rowSeq(row);
      if (seq < location.seq) {
        before = math.max(before, seq);
        continue;
      }
      final type = jsonMap(row['item'])['type'];
      if (type == '${location.role}_message' && (!exact || start <= location.seq)) {
        return _Target(start, seq, before);
      }
      if (type == 'user_message') return null;
    }
    return null;
  }

  /// Resolves once [probe] finds a value in the loaded history; fails with
  /// `reveal` after 5s or when [signal] is cancelled (model.ts waitForTarget,
  /// viewport.web.ts reveal deadline).
  Future<T> _waitFor<T extends Object>(T? Function() probe, _Signal signal) {
    final completer = Completer<T>();
    late final Timer timeout;
    late final VoidCallback check, cancelled;
    void finish([T? value]) {
      if (completer.isCompleted) return;
      timeout.cancel();
      _historyListeners.remove(check);
      signal._listeners.remove(cancelled);
      if (value != null) {
        completer.complete(value);
      } else {
        completer.completeError(const _FindFailure(ChatFindFailure.reveal));
      }
    }

    check = () {
      final value = probe();
      if (value != null) finish(value);
    };
    cancelled = finish;
    timeout = Timer(const Duration(seconds: 5), finish);
    _historyListeners.add(check);
    if (signal.aborted) {
      cancelled();
    } else {
      signal._listeners.add(cancelled);
      check();
    }
    return completer.future;
  }

  /// viewport.web.ts reveal: brings the loaded row on screen and resolves its
  /// rendered occurrence count; [occurrence] < 0 names the last one. A row
  /// without a rendered match resolves {0, 0}.
  Future<ChatFindReveal> _reveal(int row, int occurrence, _Signal signal) async {
    final entry = await _waitFor(
      () => operations.rows.where((candidate) => intValue(candidate['seqStart']) == row).firstOrNull,
      signal,
    );
    if (signal.aborted) throw const _FindFailure(ChatFindFailure.reveal);
    operations.scrollTo(row);
    final count = chatFindRowMatchCount(entry, query);
    if (count == 0) return (occurrence: 0, count: 0);
    _highlighted = true;
    return (occurrence: occurrence < 0 ? count - 1 : math.min(occurrence, count - 1), count: count);
  }

  Future<void> _navigate(int index, int direction, _Signal signal) async {
    final epoch = _resultEpoch;
    if (epoch == null) return;
    phase = 'loading';
    failure = null;
    _publish();
    var remaining = _locations.length;
    while (remaining-- > 0 && _locations.isNotEmpty) {
      index = (index + _locations.length) % _locations.length;
      final location = _locations[index];
      final cached = _resolved[location];
      final stillLoaded = cached != null &&
          operations.rows.any(
            (row) => intValue(row['seqStart']) == cached.id && _rowSeq(row) == cached.seq,
          );
      var target = stillLoaded ? cached : _target(location, true);
      if (target == null) {
        await _tagged(ChatFindFailure.connection, operations.load(epoch, location.seq));
        if (signal.aborted) return;
        target = await _waitFor(() => _target(location, false), signal);
      }
      if (signal.aborted) return;
      _resolved[location] = target;
      // Several host hits may belong to one displayed message: they collapse
      // into the location that resolved to it.
      final resolved = target;
      _locations.removeWhere(
        (candidate) =>
            !identical(candidate, location) &&
            candidate.role == location.role &&
            candidate.seq > resolved.before &&
            candidate.seq <= resolved.seq,
      );
      index = _locations.indexOf(location);
      if (selectedRow != target.id) _highlighted = false;
      selectedRow = target.id;
      _publish();
      final result = await _tagged(
        ChatFindFailure.reveal,
        _reveal(target.id, direction == 1 ? 0 : -1, signal),
      );
      if (signal.aborted) return;
      if (result.count > 0) {
        _current = index;
        phase = 'ready';
        _position(location, result);
        _publish();
        return;
      }
      _locations.removeAt(index);
      if (direction == -1) index--;
    }
    _highlighted = false;
    phase = 'ready';
    selectedRow = null;
    count = 0;
    occurrence = 0;
    _publish();
  }

  void _position(_Location location, ChatFindReveal revealed) {
    _inMessage = revealed;
    location.count = revealed.count;
    var before = 0, total = 0;
    for (final candidate in _locations) {
      if (identical(candidate, location)) before = total;
      total += candidate.count;
    }
    occurrence = before + revealed.occurrence;
    count = total;
  }

  Future<void> _move(int direction) async {
    if (phase != 'ready' || count == 0) return;
    _cancel();
    final signal = _signal;
    try {
      final next = _inMessage.occurrence + direction;
      final row = selectedRow;
      if (next >= 0 && next < _inMessage.count && row != null) {
        phase = 'loading';
        _publish();
        final result = await _tagged(ChatFindFailure.reveal, _reveal(row, next, signal));
        if (signal.aborted) return;
        if (result.count > 0) {
          phase = 'ready';
          _position(_locations[_current], result);
          _publish();
          return;
        }
      }
      await _navigate(_current + direction, direction, signal);
    } catch (e) {
      _fail(e, signal);
    }
  }

  void next() => unawaited(_move(1));
  void previous() => unawaited(_move(-1));

  @override
  void dispose() {
    _disposed = true;
    _cancel();
    super.dispose();
  }
}

/// viewport.web.ts scrollToMessage targetTop: scrolls the transcript so the
/// highlighted chat-find match inside [row] (a mounted transcript row) sits
/// [clearance] px below the viewport's top edge — just under the find
/// widget. Returns false when the row shows no highlighted match yet.
bool chatFindScrollToMatch(
  BuildContext row, {
  required double clearance,
  Duration duration = const Duration(milliseconds: 250),
}) {
  final highlight = chatFindHighlightStyle(row).backgroundColor;
  RenderBox? box;
  Rect? rect;
  void visit(Element element) {
    if (rect != null) return;
    final render = element is RenderObjectElement ? element.renderObject : null;
    final InlineSpan? text = switch (render) {
      RenderParagraph paragraph => paragraph.text,
      RenderEditable editable => editable.text,
      _ => null,
    };
    final range = text == null ? null : _highlightedRange(text, highlight);
    if (range != null && render is RenderBox) {
      final selection = TextSelection(baseOffset: range.start, extentOffset: range.end);
      final boxes = switch (render) {
        RenderParagraph paragraph => paragraph.getBoxesForSelection(selection),
        RenderEditable editable => editable.getBoxesForSelection(selection),
        _ => const <TextBox>[],
      };
      if (boxes.isNotEmpty) {
        box = render;
        rect = boxes.first.toRect();
        return;
      }
    }
    element.visitChildren(visit);
  }

  row.visitChildElements(visit);
  final target = box, area = rect;
  final rowObject = row.findRenderObject();
  final viewport = rowObject == null ? null : RenderAbstractViewport.maybeOf(rowObject);
  final position = Scrollable.maybeOf(row)?.position;
  if (target == null || area == null || viewport == null || position == null) return false;
  // The top of the match at the top edge: the trailing edge of a reversed
  // (bottom-anchored) list, the leading edge otherwise.
  final up = position.axisDirection == AxisDirection.up;
  final top = viewport.getOffsetToReveal(target, up ? 1 : 0, rect: area).offset;
  final offset = (up ? top + clearance : top - clearance)
      .clamp(position.minScrollExtent, position.maxScrollExtent);
  unawaited(position.animateTo(offset, duration: duration, curve: Curves.ease));
  return true;
}

/// The first highlighted range of [root] (placeholders count as one
/// character, as in [highlightSpanRange]).
TextRange? _highlightedRange(InlineSpan root, Color? highlight) {
  var offset = 0;
  TextRange? found;
  bool visit(InlineSpan span) {
    if (span is! TextSpan) {
      offset += 1;
      return true;
    }
    final length = span.text?.length ?? 0;
    if (length > 0 && span.style?.backgroundColor == highlight) {
      found = TextRange(start: offset, end: offset + length);
      return false;
    }
    offset += length;
    for (final child in span.children ?? const <InlineSpan>[]) {
      if (!visit(child)) return false;
    }
    return true;
  }

  visit(root);
  return found;
}

/// chat-find/index.web.tsx overlay: the PaneFind widget pinned 8px from the
/// top and 12px from the right edge of the transcript, with the failure row
/// (surface1, 8px padding, 12px foreground text and a ghost xs Retry) under it.
class ChatFindOverlay extends StatefulWidget {
  const ChatFindOverlay({super.key, required this.controller, required this.onClosed});
  final ChatFindController controller;
  final VoidCallback onClosed;

  @override
  State<ChatFindOverlay> createState() => _ChatFindOverlayState();
}

class _ChatFindOverlayState extends State<ChatFindOverlay> {
  late final TextEditingController _input = TextEditingController(text: widget.controller.query);
  final FocusNode _focus = FocusNode(debugLabel: 'chat-find');
  final GlobalKey _widgetKey = GlobalKey();

  /// The last [ChatFindController.focusRequests] the field answered.
  int _focused = -1;

  /// pane-find/index.tsx focus(): focus the field and select the query.
  void _focusQuery() {
    _focused = widget.controller.focusRequests;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _focus.requestFocus();
      _input.selection = TextSelection(baseOffset: 0, extentOffset: _input.text.length);
    });
  }

  @override
  void dispose() {
    _input.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _close() {
    widget.controller.close();
    widget.onClosed();
  }

  @override
  Widget build(BuildContext context) {
    final c = widget.controller;
    if (c.focusRequests != _focused) _focusQuery();
    String status = '';
    if (c.phase == 'searching') {
      status = tr(context, 'paneFind.searching');
    } else if (c.phase == 'loading') {
      status = tr(context, 'paneFind.loading');
    } else if (c.phase == 'error') {
      status = tr(context, 'paneFind.failed');
    } else if (c.query.trim().isNotEmpty) {
      status = c.count > 0
          ? tr(context, 'paneFind.position', args: {'current': c.occurrence + 1, 'total': c.count})
          : tr(context, 'paneFind.noMatches');
    }
    final failure = switch (c.failure) {
      ChatFindFailure.connection => 'paneFind.connectionFailure',
      ChatFindFailure.historyChanged => 'paneFind.historyChangedFailure',
      ChatFindFailure.reveal => 'paneFind.revealFailure',
      null => null,
    };
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final box = _widgetKey.currentContext?.findRenderObject();
      if (mounted && box is RenderBox && box.hasSize) c._widgetHeight = box.size.height;
    });
    return Positioned(
      top: 8,
      right: 12,
      child: ConstrainedBox(
        constraints: BoxConstraints(maxWidth: MediaQuery.sizeOf(context).width - 24),
        child: Column(
          key: _widgetKey,
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            PaneFindBar(
              controller: _input,
              focusNode: _focus,
              status: status,
              canNavigate: c.phase == 'ready' && c.count > 0,
              onChanged: c.setQuery,
              onNext: c.next,
              onPrevious: c.previous,
              onClose: _close,
            ),
            if (failure != null)
              Container(
                width: 340,
                padding: const EdgeInsets.all(8),
                color: colors.surface1,
                child: Row(
                  children: [
                    Expanded(
                      child: Text(
                        tr(context, failure),
                        style: theme.textTheme.bodySmall!.copyWith(
                          fontSize: PaseoTypography.of(context).uiSize(12),
                          color: theme.colorScheme.onSurface,
                        ),
                      ),
                    ),
                    PaseoButton(
                      label: tr(context, 'paneFind.retry'),
                      size: PaseoControlSize.xs,
                      variant: PaseoButtonVariant.ghost,
                      onPressed: c.retry,
                    ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// pane-find/index.tsx PaneFind: a 340px surface1 card (6px padding, 4px gap,
/// 1px border, 8px radius, md shadow) holding the surface2 query field (28px,
/// 32 compact; 6px radius, borderAccent while focused or hovered) with the
/// match status inside it, then previous/next/close toolbar buttons. Enter
/// finds the next match, Shift+Enter the previous one, Escape closes.
class PaneFindBar extends StatefulWidget {
  const PaneFindBar({
    super.key,
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
  State<PaneFindBar> createState() => _PaneFindBarState();
}

class _PaneFindBarState extends State<PaneFindBar> {
  bool _hovered = false;

  @override
  void initState() {
    super.initState();
    widget.focusNode.addListener(_focusChanged);
  }

  @override
  void dispose() {
    widget.focusNode.removeListener(_focusChanged);
    super.dispose();
  }

  void _focusChanged() {
    if (mounted) setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final dark = theme.brightness == Brightness.dark;
    final active = widget.focusNode.hasFocus || _hovered;
    // pane-find/index.tsx: Escape anywhere in the widget closes it.
    return CallbackShortcuts(
      bindings: {const SingleActivator(LogicalKeyboardKey.escape): widget.onClose},
      child: Semantics(
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
            BoxShadow(
              color: dark ? const Color(0x33000000) : const Color(0x0a000000),
              offset: const Offset(0, 4),
              blurRadius: dark ? 8 : 16,
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
                  constraints: BoxConstraints(minHeight: isCompactLayout(context) ? 32 : 28),
                  padding: const EdgeInsets.symmetric(horizontal: 8),
                  decoration: BoxDecoration(
                    color: colors.surface2,
                    borderRadius: BorderRadius.circular(6),
                    border: Border.all(color: active ? colors.borderAccent : Colors.transparent),
                  ),
                  child: Row(
                    spacing: 8,
                    children: [
                      Expanded(
                        child: CallbackShortcuts(
                          bindings: {
                            const SingleActivator(LogicalKeyboardKey.enter, shift: true):
                                widget.onPrevious,
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
            ChromeToolbarButton(
              label: tr(context, 'paneFind.previous'),
              icon: LucideIcons.arrowUp300,
              tooltipBelow: true,
              onPressed: widget.canNavigate ? widget.onPrevious : null,
            ),
            ChromeToolbarButton(
              label: tr(context, 'paneFind.next'),
              icon: LucideIcons.arrowDown300,
              tooltipBelow: true,
              onPressed: widget.canNavigate ? widget.onNext : null,
            ),
            ChromeToolbarButton(
              label: tr(context, 'paneFind.close'),
              icon: LucideIcons.x300,
              tooltipBelow: true,
              onPressed: widget.onClose,
            ),
          ],
        ),
      ),
    ),
    );
  }
}
