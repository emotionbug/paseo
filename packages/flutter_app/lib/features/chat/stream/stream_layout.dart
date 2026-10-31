import '../../../core/json.dart';

// Ports of agent-stream/turn-membership.ts, spacing.ts, layout.ts (footer
// hosts) and tool-calls/detail-level/{projection,grouping}.ts.

/// One rendered stream row: a timeline entry, or (overview detail level) a run
/// of consecutive tool calls hosted by its latest call.
class StreamRow {
  const StreamRow(this.entry, {this.group});

  /// The rendered entry; for a group, the run's latest call.
  final JsonMap entry;

  /// Overview group: every call of the run, oldest first.
  final List<JsonMap>? group;

  String get kind => streamKind(entry);

  /// grouping.ts createRun: a run is identified by its first call.
  String get groupId => group == null ? '' : stringValue(jsonMap(group!.first['item'])['callId']);
}

String streamKind(JsonMap? entry) => stringValue(jsonMap(entry?['item'])['type']);

/// projection.ts visibleToolCallItems: approval UI owns pending plans, so
/// ExitPlanMode and a running plan_approval call never render in the stream.
bool isVisibleStreamEntry(JsonMap entry) {
  final item = jsonMap(entry['item']);
  if (item['type'] != 'tool_call') return true;
  final name = item['name'];
  return name != 'ExitPlanMode' && !(name == 'plan_approval' && item['status'] == 'running');
}

/// grouping.ts isGroupableToolCall: plans and speak calls stay standalone.
bool isGroupableToolCall(JsonMap entry) {
  final item = jsonMap(entry['item']);
  if (item['type'] != 'tool_call') return false;
  return jsonMap(item['detail'])['type'] != 'plan' &&
      stringValue(item['name']).trim().toLowerCase() != 'speak';
}

/// stream.ts reduceTimelineToolCall: a Claude `TodoWrite` call is not a tool
/// row but a task-list update (tool-call-parsers.ts extractTaskEntriesFromToolCall:
/// text = activeForm || content, completed when status is completed). Returns
/// the original entry for other calls, a synthetic `todo` entry for a valid
/// TodoWrite, and null when its input does not parse (the original drops it).
JsonMap? _todoWriteAsTodo(JsonMap entry, String? fallbackProvider) {
  final item = jsonMap(entry['item']);
  if (item['type'] != 'tool_call') return entry;
  final provider = stringValue(entry['provider'], fallbackProvider ?? '');
  final normalized = stringValue(item['name'])
      .trim()
      .replaceAll(RegExp(r'[.\s-]+'), '_')
      .toLowerCase();
  if (provider != 'claude' || (normalized != 'todowrite' && normalized != 'todo_write')) {
    return entry;
  }
  final detail = jsonMap(item['detail']);
  final input = detail['type'] == 'unknown' ? detail['input'] : null;
  final todos = input is Map ? input['todos'] : null;
  if (todos is! List) return null;
  final tasks = <JsonMap>[];
  for (final todo in todos) {
    if (todo is! Map || todo['content'] is! String) return null;
    final status = todo['status'];
    if (status != 'pending' && status != 'in_progress' && status != 'completed') return null;
    final content = todo['content'] as String;
    final active = todo['activeForm'] is String ? (todo['activeForm'] as String).trim() : '';
    final text = active.isNotEmpty ? active : content.trim();
    tasks.add({'text': text.isEmpty ? content : text, 'completed': status == 'completed'});
  }
  return {
    ...entry,
    'item': {'type': 'todo', 'provider': provider, 'items': tasks},
  };
}

/// Detailed level: visible entries one per row. Overview level
/// (prepareGroupedHistory): every run of consecutive groupable calls, even a
/// single one, collapses into one summary row. Claude TodoWrite calls become
/// task-list entries first.
List<StreamRow> buildStreamRows(
  List<JsonMap> entries, {
  required bool overview,
  String? provider,
}) {
  final rows = <StreamRow>[];
  var pending = <JsonMap>[];
  void flush() {
    if (pending.isEmpty) return;
    rows.add(StreamRow(pending.last, group: pending));
    pending = <JsonMap>[];
  }

  for (final source in entries) {
    final entry = _todoWriteAsTodo(source, provider);
    if (entry == null || !isVisibleStreamEntry(entry)) continue;
    if (overview && isGroupableToolCall(entry)) {
      pending.add(entry);
      continue;
    }
    flush();
    rows.add(StreamRow(entry));
  }
  flush();
  return rows;
}

bool isToolSequenceEntry(JsonMap? entry) => switch (streamKind(entry)) {
  'tool_call' || 'reasoning' || 'todo' => true,
  _ => false,
};

/// turn-membership.ts continuesTurn: canonical turn IDs win; otherwise a user
/// message starts a new turn.
bool continuesTurn(JsonMap? previous, JsonMap? next) {
  if (previous == null || next == null) return false;
  if (previous['turnId'] != null && next['turnId'] != null) {
    return previous['turnId'] == next['turnId'];
  }
  return streamKind(next) != 'user_message';
}

/// A visible response spans turns whose prompts are absent from the timeline.
bool continuesResponse(JsonMap? previous, JsonMap? next) {
  if (previous == null || next == null) return false;
  return continuesTurn(previous, next) || streamKind(next) != 'user_message';
}

/// spacing.ts getGapBetweenStreamItems.
double gapBetweenStreamItems(JsonMap? entry, JsonMap? below) {
  if (entry == null || below == null) return 0;
  final kind = streamKind(entry), next = streamKind(below);
  if (kind == 'user_message' && next == 'user_message') return 4;
  if (kind == 'user_message' && next == 'assistant_message') return 0;
  if (isToolSequenceEntry(entry) && isToolSequenceEntry(below)) return 0;
  if (kind == 'user_message' && isToolSequenceEntry(below)) return 16;
  if (kind == 'assistant_message' && isToolSequenceEntry(below)) return 4;
  if (isToolSequenceEntry(entry) && next == 'assistant_message') return 4;
  return 16;
}

/// layout.ts findLatestAssistantInResponse: walks up from [start] while the
/// rows continue one response and returns the latest assistant message index.
int? latestAssistantInResponse(List<StreamRow> rows, int start) {
  JsonMap? later;
  for (var index = start; index >= 0 && index < rows.length; index--) {
    final entry = rows[index].entry;
    if (later != null && !continuesResponse(entry, later)) return null;
    if (streamKind(entry) == 'assistant_message') return index;
    later = entry;
  }
  return null;
}

/// strategy.ts collectAssistantResponseContent: the response's assistant
/// messages up to [start], oldest first, joined by blank lines.
String collectAssistantResponseContent(List<StreamRow> rows, int start) {
  final messages = <String>[];
  JsonMap? later;
  for (var index = start; index >= 0 && index < rows.length; index--) {
    final entry = rows[index].entry;
    if (later != null && !continuesResponse(entry, later)) break;
    if (streamKind(entry) == 'assistant_message') {
      messages.add(stringValue(jsonMap(entry['item'])['text']));
    }
    later = entry;
  }
  return messages.reversed.join('\n\n');
}

/// layout.ts resolveCompletedFooter: a footer closes a response right before
/// the next user prompt, hosted by the response's latest assistant message.
int? completedFooterHost(List<StreamRow> rows, int index, {int? auxiliaryHost}) {
  final row = rows[index];
  final below = index + 1 < rows.length ? rows[index + 1].entry : null;
  if (row.kind == 'user_message' || below == null || continuesResponse(row.entry, below)) {
    return null;
  }
  final host = latestAssistantInResponse(rows, index);
  return host == null || host == auxiliaryHost ? null : host;
}

/// layout.ts resolveAuxiliaryTurnFooter: with no active turn, the stream ends
/// with the latest response's completed footer.
int? auxiliaryFooterHost(List<StreamRow> rows, {required bool turnActive}) {
  if (turnActive || rows.isEmpty) return null;
  return latestAssistantInResponse(rows, rows.length - 1);
}

/// timeline/turn-time.ts TurnTiming: when a turn's last item landed and when
/// its opening prompt was sent (null for a turn without a visible prompt).
typedef TurnTiming = ({DateTime? startedAt, DateTime completedAt});

/// timeline/turn-time.ts deriveStreamTurnTiming: walks [rows] by
/// chronological turn (turn-membership.ts startsNewTurn). A turn with
/// assistant rows maps each of their indexes to its timing: completed at its
/// last item's timestamp, started at its user message's. The trailing open
/// turn only closes once no turn is active.
Map<int, TurnTiming> deriveTurnTiming(List<StreamRow> rows, {required bool turnActive}) {
  final timing = <int, TurnTiming>{};
  DateTime? userAt, lastItemAt;
  var assistants = <int>[];
  void close() {
    final completedAt = lastItemAt;
    if (completedAt == null || assistants.isEmpty) return;
    final value = (startedAt: userAt, completedAt: completedAt);
    for (final index in assistants) {
      timing[index] = value;
    }
  }

  JsonMap? previous;
  for (var index = 0; index < rows.length; index++) {
    final entry = rows[index].entry;
    final at = DateTime.tryParse(stringValue(entry['timestamp']));
    if (previous == null || !continuesTurn(previous, entry)) {
      close();
      userAt = streamKind(entry) == 'user_message' ? at : null;
      lastItemAt = null;
      assistants = <int>[];
    }
    lastItemAt = at ?? lastItemAt;
    if (streamKind(entry) == 'assistant_message') assistants.add(index);
    previous = entry;
  }
  if (!turnActive) close();
  return timing;
}
