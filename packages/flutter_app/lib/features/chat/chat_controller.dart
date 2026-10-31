import 'dart:async';
import 'dart:convert';

import 'package:collection/collection.dart' hide mergeSort;
import 'package:flutter/foundation.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/target.dart';
import 'creation_recovery.dart';

List<JsonMap> maps(Object? value) => jsonList(value).map(jsonMap).toList();

class ChatRequestRejected implements Exception {
  const ChatRequestRejected(this.message);
  final String message;
  @override
  String toString() => message;
}

bool definitelyRejected(Object error) =>
    error is ChatRequestRejected ||
    (error is DaemonException && !error.outcomeUnknown);

JsonMap requireSuccess(JsonMap payload) {
  if (payload['error'] != null ||
      payload['accepted'] == false ||
      payload['ok'] == false ||
      payload['success'] == false) {
    throw ChatRequestRejected(
      stringValue(payload['error'], 'The daemon rejected this operation'),
    );
  }
  return payload;
}

/// timeline-fetch-policy.ts TIMELINE_FETCH_PAGE_SIZE: projected items per page.
const timelineFetchPageSize = 40;

/// viewed-timeline-sync.ts getNextRetryDelayMs: a failed catch-up or
/// subscription reconcile retries on its own, doubling from 1s to 30s.
Duration nextTimelineRetryDelay(Duration? previous) {
  if (previous == null) return const Duration(seconds: 1);
  final doubled = previous * 2;
  const ceiling = Duration(seconds: 30);
  return doubled > ceiling ? ceiling : doubled;
}

/// viewed-timeline-sync.ts ViewedTimelineStatus.
enum TimelineSyncStatus { ready, pending, error, retrying }

/// agent-snapshots.ts derivePendingPermissionKey, scoped to one agent.
String pendingPermissionKey(JsonMap request) {
  final metadata = jsonMap(request['metadata']);
  for (final candidate in [
    request['id'],
    metadata['id'],
    request['name'],
    request['title'],
  ]) {
    if (candidate is String && candidate.isNotEmpty) return candidate;
  }
  return '${request['kind']}:${jsonEncode(request['input'] ?? request['metadata'] ?? {})}';
}

/// stream.ts mergeAgentToolCallStatus: failure is sticky, a completed call is
/// never reopened, and cancellation only wins over a running call.
String mergeToolCallStatus(String existing, String incoming) {
  if (existing == 'failed' || incoming == 'failed') return 'failed';
  if (existing == 'canceled') return 'canceled';
  if (incoming == 'canceled') return existing == 'completed' ? 'completed' : 'canceled';
  if (existing == 'completed' || incoming == 'completed') return 'completed';
  return 'running';
}

bool _nonEmptyObject(Object? value) => value is Map && value.isNotEmpty;

Object? _mergeUnknownValue(Object? existing, Object? incoming) {
  if (incoming == null) return existing;
  if (!_nonEmptyObject(incoming) && _nonEmptyObject(existing)) return existing;
  return incoming;
}

/// stream.ts mergeToolCallDetail: a typed detail is never replaced by an
/// `unknown` one; same-typed details merge field by field.
JsonMap mergeToolCallDetail(JsonMap existing, JsonMap incoming) {
  if (incoming.isEmpty) return existing;
  final from = existing['type'], to = incoming['type'];
  if (from == 'unknown' && to != 'unknown') return incoming;
  if (to == 'unknown' && from != 'unknown' && existing.isNotEmpty) return existing;
  if (from == 'unknown' && to == 'unknown') {
    return {
      'type': 'unknown',
      'input': _mergeUnknownValue(existing['input'], incoming['input']),
      'output': _mergeUnknownValue(existing['output'], incoming['output']),
    };
  }
  if (from == to) return {...existing, ...incoming};
  return incoming;
}

/// stream.ts mergeAgentToolCallItem: a later update for the same call.
JsonMap mergeToolCallItem(JsonMap existing, JsonMap incoming) {
  final status = mergeToolCallStatus(
    stringValue(existing['status'], 'running'),
    stringValue(incoming['status'], 'running'),
  );
  final existingMetadata = existing['metadata'], incomingMetadata = incoming['metadata'];
  return {
    ...existing,
    ...incoming,
    'status': status,
    'error': status == 'failed'
        ? (incoming['error'] ?? existing['error'] ?? {'message': 'Tool call failed'})
        : null,
    'detail': mergeToolCallDetail(jsonMap(existing['detail']), jsonMap(incoming['detail'])),
    'metadata': incomingMetadata is! Map
        ? existingMetadata
        : existingMetadata is! Map
        ? incomingMetadata
        : {...existingMetadata, ...incomingMetadata},
  };
}

/// stream.ts agentToolCallIdentity: a call id is scoped to its turn.
String _toolIdentity(Object? callId, Object? turnId) {
  final call = stringValue(callId);
  if (turnId is! String || turnId.isEmpty) return call;
  return 'turn:${Uri.encodeComponent(turnId)}/${Uri.encodeComponent(call)}';
}

/// stream.ts produceUserMessage with the "existing" presentation: the daemon
/// echoes a prompt without its images or attachments, so the sending client
/// keeps them for the rest of its session (in memory, like the original
/// stream store), keyed by agent and clientMessageId.
class SentUserMessages {
  SentUserMessages._();
  static const _capacity = 64;
  static final _entries = <String, JsonMap>{};

  static void remember(String agentId, JsonMap message) {
    final id = stringValue(message['messageId']);
    final images = jsonList(message['images']);
    final attachments = jsonList(message['attachments']);
    if (agentId.isEmpty || id.isEmpty || (images.isEmpty && attachments.isEmpty)) return;
    final key = '$agentId/$id';
    _entries.remove(key);
    _entries[key] = {'images': images, 'attachments': attachments};
    if (_entries.length > _capacity) _entries.remove(_entries.keys.first);
  }

  static JsonMap? lookup(String agentId, Object? clientMessageId) {
    final id = stringValue(clientMessageId);
    return id.isEmpty ? null : _entries['$agentId/$id'];
  }
}

/// types/stream.ts reduceTimelineEvent as a resumable left fold over the
/// stored rows: live rows only ever append, so a streamed chunk folds into
/// the projection without refolding (and re-copying) the whole history, and
/// every entry it does not touch stays the identical map.
class _TimelineFold {
  _TimelineFold(this._sent);

  /// The sending client's images and attachments for a clientMessageId.
  final JsonMap? Function(Object? clientMessageId) _sent;
  final entries = <JsonMap>[];
  final _tools = <String, int>{};
  final _plugins = <String, int>{};
  final _users = <String, int>{};

  /// How many stored rows are folded in.
  var consumed = 0;

  void reset() {
    entries.clear();
    _tools.clear();
    _plugins.clear();
    _users.clear();
    consumed = 0;
  }

  /// Consecutive chunks cover consecutive sequences: their ranges coalesce,
  /// so a long streamed message keeps one range instead of one per chunk.
  static List<Object?> _joinRanges(Object? previous, Object? next) {
    final joined = [...jsonList(previous)];
    for (final range in jsonList(next)) {
      final start = intValue(jsonMap(range)['startSeq']);
      final end = intValue(jsonMap(range)['endSeq']);
      final last = joined.isEmpty ? null : jsonMap(joined.last);
      if (last != null &&
          start >= intValue(last['startSeq']) &&
          start <= intValue(last['endSeq']) + 1) {
        if (end > intValue(last['endSeq'])) {
          joined.last = {'startSeq': last['startSeq'], 'endSeq': end};
        }
      } else {
        joined.add(range);
      }
    }
    return joined;
  }

  static JsonMap _grow(JsonMap previous, JsonMap row, JsonMap item) => {
    ...previous,
    'item': item,
    'seqEnd': row['seqEnd'],
    'timestamp': row['timestamp'] ?? previous['timestamp'],
    if (row['turnId'] != null) 'turnId': row['turnId'],
    'sourceSeqRanges': _joinRanges(previous['sourceSeqRanges'], row['sourceSeqRanges']),
  };

  void add(JsonMap row) {
    final result = entries;
    final item = jsonMap(row['item']);
    final type = item['type'];
    final next = {
      ...row,
      'item': {...item},
    };
    switch (type) {
      case 'tool_call':
        final identity = _toolIdentity(item['callId'], row['turnId']);
        final index = _tools[identity];
        if (index != null) {
          final previous = result[index];
          result[index] = {
            ..._grow(previous, row, mergeToolCallItem(jsonMap(previous['item']), item)),
            'turnId': previous['turnId'],
            'timestamp': row['timestamp'] ?? previous['timestamp'],
          };
          return;
        }
        _tools[identity] = result.length;
        next['item'] = {
          ...item,
          'error': item['status'] == 'failed' ? item['error'] : null,
        };
      case 'plugin':
        final identity = '${item['pluginId']}/${item['id']}';
        final index = _plugins[identity];
        if (index != null) {
          result[index] = _grow(result[index], row, {...item});
          return;
        }
        _plugins[identity] = result.length;
      case 'user_message':
        final local = _sent(item['clientMessageId']);
        if (local != null) {
          next['item'] = {
            ...item,
            if (jsonList(item['images']).isEmpty) 'images': local['images'],
            if (jsonList(item['attachments']).isEmpty) 'attachments': local['attachments'],
          };
        }
        final id = stringValue(
          item['clientMessageId'],
          stringValue(item['messageId']),
        );
        if (id.isNotEmpty) {
          final index = _users[id];
          if (index != null) {
            // produceUserMessage "existing": the shown row keeps its text,
            // time, images and attachments; the echo adds identity and seq.
            final previous = result[index];
            final shown = jsonMap(previous['item']);
            result[index] = {
              ...next,
              'timestamp': previous['timestamp'] ?? next['timestamp'],
              'item': {
                ...jsonMap(next['item']),
                'text': shown['text'],
                if (shown['images'] != null) 'images': shown['images'],
                if (shown['attachments'] != null) 'attachments': shown['attachments'],
              },
            };
            return;
          }
          _users[id] = result.length;
        }
      case 'compaction' when item['status'] == 'completed':
        // A completion settles the pending compaction marker.
        final index = result.indexWhere((entry) {
          final existing = jsonMap(entry['item']);
          return existing['type'] == 'compaction' && existing['status'] == 'loading';
        });
        if (index >= 0) {
          final existing = jsonMap(result[index]['item']);
          result[index] = _grow(result[index], row, {
            ...existing,
            'status': 'completed',
            'trigger': item['trigger'] ?? existing['trigger'],
            'preTokens': item['preTokens'] ?? existing['preTokens'],
          });
          return;
        }
      case 'assistant_message' || 'reasoning':
        // normalizeChunk: carriage returns dropped; an empty chunk adds nothing.
        final chunk = stringValue(item['text']).replaceAll('\r', '');
        if (chunk.isEmpty) return;
        final previous = result.lastOrNull;
        final old = jsonMap(previous?['item']);
        final extend = old['type'] == type &&
            (type == 'reasoning' ||
                item['messageId'] == null ||
                item['messageId'] == old['messageId']);
        if (previous != null && extend) {
          result[result.length - 1] = _grow(previous, row, {
            ...old,
            'text': '${old['text'] ?? ''}$chunk',
          });
          return;
        }
        // A whitespace-only chunk never starts a message.
        if (chunk.trim().isEmpty) return;
        next['item'] = {...item, 'text': chunk};
    }
    result.add(next);
  }
}

/// One viewed conversation: subscription membership, authoritative catch-up
/// (viewed-timeline-sync.ts), response reconciliation (session-stream-reducers.ts
/// processTimelineResponse) and live stream sequencing (processAgentStreamEvent).
class ChatController extends ChangeNotifier {
  ChatController(this.client, this.target);
  final PaseoClient client;
  final PanelTarget target;
  final List<JsonMap> _rows = [];
  late final _fold = _TimelineFold((id) => SentUserMessages.lookup(agentId, id));
  List<JsonMap> timeline = [];
  JsonMap agent = {};

  /// Pending permission requests by [pendingPermissionKey], in arrival order.
  final Map<String, JsonMap> permissions = {};
  List<JsonMap> subagents = [];
  final Map<String, JsonMap> turns = {};
  String? epoch;
  JsonMap? startCursor, endCursor;
  bool hasOlder = false, loading = false, connected = false;

  /// Authoritative history has been applied (session-store timeline status
  /// "synced"). Before it, live rows are an overlay that never moves cursors.
  bool synced = false;

  /// The catch-up or subscription failure the sync-error callout reports.
  Object? error;
  String? notice;

  OwnedObservation? _observation;
  StreamSubscription<JsonMap>? _events;
  bool _disposed = false;
  int _generation = 0;
  Future<void> _loads = Future.value();
  final List<JsonMap> _buffer = [];

  // viewed-timeline-sync.ts membership: acknowledged once the daemon accepted
  // this agent's subscription; catch-up never runs before that.
  bool _subscribed = false, _membershipNeedsRetry = false;
  int _membershipGeneration = 0;
  Timer? _membershipRetry;
  Duration? _membershipDelay;

  // viewed-timeline-sync.ts CatchUpState for this agent.
  String? _catchUp; // running | complete | error
  int _catchUpGeneration = 0;
  Future<void>? _catchUpRun;
  bool _gapPending = false;
  Timer? _catchUpRetry;
  Duration? _catchUpDelay;

  // visibilityCatchUpPending / manualRetries.
  bool _pending = true, _manualRetry = false, _appVisible = true;

  bool get isSubagent => target.kind == 'provider_subagent';
  String get agentId =>
      isSubagent ? stringValue(target.data['parentAgentId']) : target.id!;
  bool get running =>
      agent['status'] == 'running' ||
      agent['activeTurn'] != null ||
      // use-agent-screen-state-machine.ts shouldUseOptimisticCreateFlowAgent:
      // a just-created agent reads as running until its snapshot says otherwise.
      (createHandoff && (agent.isEmpty || agent['status'] == 'idle'));

  /// agent-panel.tsx findActiveCreateHandoff: this client just created the
  /// agent and the viewed sync still owes it a catch-up.
  bool get createHandoff =>
      !isSubagent && CreateHandoffs.instance.active(target.serverId, agentId);

  /// viewed-timeline-sync.ts onCatchUpEnded → create-flow-store clearByAgent.
  void _endCreateHandoff() {
    if (!isSubagent) CreateHandoffs.instance.clearByAgent(target.serverId, agentId);
  }

  /// viewed-timeline-sync.ts getAgentTimelineStatus.
  TimelineSyncStatus get syncStatus {
    if (_manualRetry) return TimelineSyncStatus.retrying;
    if (error != null) return TimelineSyncStatus.error;
    if (_pending) return TimelineSyncStatus.pending;
    return TimelineSyncStatus.ready;
  }

  Future<void> start() async {
    connected = client.status == ConnectionStatus.connected;
    client.addListener(_connectionChanged);
    _events = client.events.listen(_event, onError: _reportError);
    CreateHandoffs.instance.addListener(_notify);
    await _reconcileMembership();
  }

  /// subagents/select.ts useSubagentsForParent + session-context.tsx
  /// resyncProviderSubagents: hosts with providerSubagents list the agent's
  /// provider subagents on open and again whenever the session resubscribes.
  Future<void> _refreshSubagents() async {
    if (isSubagent ||
        _disposed ||
        jsonMap(client.serverInfo['features'])['providerSubagents'] != true) {
      return;
    }
    try {
      await loadSubagents();
    } catch (e) {
      _reportError(e);
    }
  }

  // ── membership ──────────────────────────────────────────────────────────

  /// viewed-timeline-sync.ts reconcileMembership.
  Future<void> _reconcileMembership() async {
    if (_disposed || !connected) return;
    if (_observation != null && !_membershipNeedsRetry) return;
    final generation = ++_membershipGeneration;
    _membershipNeedsRetry = false;
    final previous = _observation;
    _observation = null;
    if (previous != null) unawaited(previous.close().catchError((Object _) {}));
    final OwnedObservation observation;
    try {
      observation = await client.observe(
        type: 'agent.timeline.set_subscription.request',
        fields: {
          'agentIds': [agentId],
        },
        responseType: 'agent.timeline.set_subscription.response',
        // Stream delivery comes from client.events; this callback only owns membership.
        onEvent: _membershipEvent,
      );
    } catch (e) {
      if (_disposed || !connected || generation != _membershipGeneration) return;
      _membershipFailed(e);
      return;
    }
    if (_disposed || generation != _membershipGeneration) {
      unawaited(observation.close().catchError((Object _) {}));
      return;
    }
    _observation = observation;
    _membershipAcknowledged();
    await _startCatchUp();
    await _refreshSubagents();
  }

  void _membershipEvent(JsonMap event) {
    if (_disposed) return;
    if (event['type'] == 'subscription.restored') {
      // The client re-established membership after a reconnect.
      _membershipAcknowledged();
      unawaited(_startCatchUp());
      unawaited(_refreshSubagents());
    } else if (event['type'] == 'subscription.error' && connected) {
      final failed = _observation;
      _observation = null;
      if (failed != null) unawaited(failed.close().catchError((Object _) {}));
      _membershipFailed(jsonMap(event['payload'])['error'] ?? event['payload']);
    }
  }

  void _membershipAcknowledged() {
    _subscribed = true;
    _membershipRetry?.cancel();
    _membershipRetry = null;
    _membershipDelay = null;
  }

  void _membershipFailed(Object? failure) {
    _subscribed = false;
    _membershipNeedsRetry = true;
    final generation = _membershipGeneration;
    final delay = _membershipDelay = nextTimelineRetryDelay(_membershipDelay);
    _membershipRetry?.cancel();
    _membershipRetry = Timer(delay, () {
      _membershipRetry = null;
      if (_disposed || !connected || generation != _membershipGeneration) return;
      unawaited(_reconcileMembership());
    });
    _setError(failure);
  }

  /// viewed-timeline-sync.ts setConnected. Disconnect keeps the obligation:
  /// the chat returns to pending and catches up once membership is restored.
  void _connectionChanged() {
    final ready = client.status == ConnectionStatus.connected;
    if (ready == connected) return;
    connected = ready;
    _membershipGeneration++;
    _membershipRetry?.cancel();
    _membershipRetry = null;
    _membershipDelay = null;
    if (!ready) {
      _subscribed = false;
      _cancelCatchUp();
      _pending = true;
      error = null;
      _manualRetry = false;
    } else if (_observation == null) {
      // Never acknowledged (or released after a failure): subscribe again.
      _membershipNeedsRetry = false;
      unawaited(_reconcileMembership());
    }
    // Otherwise the client restores the observation and reports
    // subscription.restored, which starts the catch-up.
    _notify();
  }

  /// viewed-timeline-sync.ts setActive: returning to the foreground catches
  /// the chat up again, showing it as pending until that settles.
  void setAppVisible(bool visible) {
    if (_appVisible == visible) return;
    _appVisible = visible;
    if (!visible || _disposed) return;
    _pending = true;
    error = null;
    unawaited(_startCatchUp(supersede: true));
    _notify();
  }

  /// viewed-timeline-sync.ts retryVisibleAgentTimeline: the sync-error
  /// callout's retry owns a `retrying` state until the attempt settles.
  void retrySync() {
    if (_disposed || _manualRetry) return;
    final membershipRetryable = _membershipNeedsRetry && connected;
    if (_catchUp != 'error' && !membershipRetryable) return;
    _manualRetry = true;
    _notify();
    if (_catchUp == 'error') {
      _catchUpRetry?.cancel();
      unawaited(_startCatchUp(supersede: true));
      return;
    }
    _membershipRetry?.cancel();
    _membershipRetry = null;
    _membershipDelay = null;
    unawaited(_reconcileMembership());
  }

  // ── catch-up ────────────────────────────────────────────────────────────

  bool get _canCatchUp => !_disposed && connected && _subscribed;

  void _cancelCatchUp() {
    _catchUpGeneration++;
    _catchUpRetry?.cancel();
    _catchUpRetry = null;
    _catchUp = null;
    _catchUpRun = null;
    _gapPending = false;
  }

  /// viewed-timeline-sync.ts startCatchUp/decideCatchUp. A resume fetches
  /// `after` the local end (or the tail); a gap recovery pages `after` until
  /// current. Re-running a completed resume is a no-op unless superseded.
  Future<void> _startCatchUp({bool gap = false, bool supersede = false}) {
    if (!_canCatchUp) {
      if (gap) _gapPending = true;
      return Future.value();
    }
    if (gap) supersede = true;
    final current = _catchUp;
    if (current == 'running') {
      // keep-and-park: the running fetch owns the request; the gap runs next.
      if (gap) _gapPending = true;
      return _catchUpRun ?? Future.value();
    }
    if (current == 'complete' && !supersede) return Future.value();
    _catchUpRetry?.cancel();
    _catchUpRetry = null;
    if (supersede || current != 'error') _catchUpDelay = null;
    final generation = ++_catchUpGeneration;
    _catchUp = 'running';
    _gapPending = false;
    return _catchUpRun = _enqueue(() => _runCatchUp(generation, gap: gap));
  }

  bool _ownsCatchUp(int generation) => _canCatchUp && _catchUpGeneration == generation;

  /// viewed-timeline-sync.ts fetchUntilCurrent.
  Future<void> _runCatchUp(int generation, {required bool gap}) async {
    if (!_ownsCatchUp(generation)) return;
    try {
      // The first resume request is bounded: when it reports more newer
      // history, install the latest tail instead of replaying every page.
      var fallbackToLatestTail = !gap && endCursor != null;
      while (true) {
        final page = await _fetch(endCursor == null ? 'tail' : 'after');
        if (!_ownsCatchUp(generation)) return;
        final hasNewer = page['hasNewer'] == true;
        if (hasNewer && page['endCursor'] is Map) {
          if (fallbackToLatestTail) {
            await _fetch('tail', forceReplace: true);
            if (!_ownsCatchUp(generation)) return;
            break;
          }
          fallbackToLatestTail = false;
          continue;
        }
        if (hasNewer) {
          throw StateError('Timeline page for $agentId hasNewer without an end cursor');
        }
        break;
      }
      _catchUp = 'complete';
      _catchUpDelay = null;
      if (_gapPending) {
        _gapPending = false;
        _catchUp = null;
        scheduleMicrotask(() => unawaited(_startCatchUp(gap: true)));
        return;
      }
      _pending = false;
      _endCreateHandoff();
      error = null;
      _manualRetry = false;
      _notify();
    } catch (e) {
      if (_disposed || generation != _catchUpGeneration) return;
      final delay = _catchUpDelay = nextTimelineRetryDelay(_catchUpDelay);
      _catchUp = 'error';
      _catchUpRetry = Timer(delay, () {
        _catchUpRetry = null;
        if (_catchUpGeneration != generation || _catchUp != 'error') return;
        unawaited(_startCatchUp());
      });
      _reportError(e);
      _setError(e);
    }
  }

  void _setError(Object? failure) {
    _manualRetry = false;
    _pending = false;
    error = failure ?? StateError('Conversation synchronization failed');
    _notify();
  }

  // ── actions ─────────────────────────────────────────────────────────────

  Future<JsonMap> action(
    String type,
    JsonMap fields, {
    String? responseType,
  }) async {
    final result = requireSuccess(
      await client.request(type, {
        'agentId': agentId,
        ...fields,
      }, responseType: responseType),
    );
    final n = jsonMap(result['notice']);
    if (n.isNotEmpty) {
      notice = stringValue(n['message']);
      _notify();
    }
    return result;
  }

  /// Bring the conversation current after a local action. Failures surface
  /// through [syncStatus] and retry on their own, so this never throws.
  Future<void> reload() => _startCatchUp(supersede: true);

  /// timeline-replacement.ts requestTimelineReplacement: one latest tail,
  /// reconciled with the local range (an epoch change replaces history
  /// atomically; the current rows stay until it lands).
  Future<void> refreshTail() => _enqueue(() => _fetch('tail'));

  /// use-load-older-agent-history.ts: one `before` page. Failures are the
  /// caller's (a toast), never a timeline sync error.
  Future<void> older() => _enqueue(() async {
    if (startCursor == null || !hasOlder) return;
    await _fetch('before');
  });

  Future<void> _enqueue(Future<void> Function() operation) {
    final done = _loads.then((_) async {
      if (_disposed) return;
      loading = true;
      _notify();
      try {
        await operation();
      } finally {
        loading = false;
        if (!_disposed) {
          final pending = List<JsonMap>.of(_buffer);
          _buffer.clear();
          for (final event in pending) {
            _applyEvent(event);
          }
          _project();
        }
      }
    });
    _loads = done.catchError((Object _) {});
    return done;
  }

  // ── authoritative pages ─────────────────────────────────────────────────

  Future<JsonMap> _fetch(String direction, {bool forceReplace = false}) async {
    final generation = _generation;
    final cursor = direction == 'before' ? startCursor : endCursor;
    final result = requireSuccess(
      await client.request(
        isSubagent
            ? 'agent.provider_subagents.timeline.get.request'
            : 'fetch_agent_timeline_request',
        {
          if (isSubagent) ...{
            'parentAgentId': agentId,
            'subagentId': target.id,
          } else
            'agentId': agentId,
          'direction': direction,
          'limit': timelineFetchPageSize,
          if (!isSubagent) 'projection': 'projected',
          if (direction != 'tail' && cursor != null) 'cursor': cursor,
        },
        responseType: isSubagent
            ? 'agent.provider_subagents.timeline.get.response'
            : 'fetch_agent_timeline_response',
      ),
    );
    if (_disposed || generation != _generation) return result;
    _applyPage(direction, result, forceReplace: forceReplace);
    return result;
  }

  /// session-stream-reducers.ts processTimelineResponse.
  void _applyPage(String direction, JsonMap result, {bool forceReplace = false}) {
    final nextEpoch = stringValue(result['epoch']);
    if (result['agent'] is Map) _setAgent(jsonMap(result['agent']));
    final incoming = maps(result[isSubagent ? 'rows' : 'entries']).map((row) {
      final start = intValue(row['seqStart'], intValue(row['seq']));
      return {
        ...row,
        'seqStart': start,
        'seqEnd': intValue(row['seqEnd'], start),
      };
    }).toList();
    final pageStart = result['startCursor'] is Map
        ? intValue(jsonMap(result['startCursor'])['seq'])
        : incoming.isEmpty
        ? null
        : incoming.map((row) => row['seqStart'] as int).reduce((a, b) => a < b ? a : b);
    final pageEnd = result['endCursor'] is Map
        ? intValue(jsonMap(result['endCursor'])['seq'])
        : incoming.isEmpty
        ? null
        : incoming.map((row) => row['seqEnd'] as int).reduce((a, b) => a > b ? a : b);
    final localEnd = endCursor == null ? null : intValue(endCursor!['seq']);
    final localStart = startCursor == null ? null : intValue(startCursor!['seq']);

    var units = incoming;
    String mode;
    if (forceReplace || result['reset'] == true) {
      mode = 'replace';
    } else if (direction == 'tail') {
      // deriveBootstrapTailTimelinePolicy / deriveResumeTailPolicy.
      final windowMax = intValue(jsonMap(result['window'])['maxSeq']);
      if (!synced || localEnd == null || epoch == null || epoch != nextEpoch) {
        mode = 'replace';
      } else if (windowMax == localEnd) {
        mode = 'discard';
      } else if (windowMax < localEnd) {
        mode = 'replace';
      } else if (pageStart != null && pageStart <= localEnd + 1) {
        mode = 'append';
        units = incoming.where((row) => (row['seqEnd'] as int) > localEnd).toList();
      } else {
        mode = 'replace';
      }
    } else if (direction == 'after') {
      // acceptIncrementalTimelineUnits.
      if (pageStart == null || pageEnd == null) {
        mode = 'discard';
      } else if (localEnd == null) {
        mode = synced ? 'append' : 'replace';
      } else if (epoch != nextEpoch || pageEnd <= localEnd) {
        mode = 'discard';
      } else if (pageStart > localEnd + 1) {
        mode = 'discard';
        _gapPending = true;
      } else {
        mode = 'append';
      }
    } else {
      // acceptOlderTimelineUnits: only a page adjacent to the history start.
      mode = localStart == null ||
              epoch != nextEpoch ||
              pageEnd == null ||
              pageEnd != localStart - 1
          ? 'discard'
          : 'prepend';
    }

    switch (mode) {
      case 'replace':
        _rows
          ..clear()
          ..addAll(incoming);
        _fold.reset();
        if (epoch != nextEpoch) turns.clear();
        epoch = nextEpoch;
        startCursor = pageStart == null ? null : {'epoch': nextEpoch, 'seq': pageStart};
        endCursor = pageEnd == null ? null : {'epoch': nextEpoch, 'seq': pageEnd};
        hasOlder = result['hasOlder'] == true;
      case 'append':
        _merge(units);
        epoch = nextEpoch;
        if (pageEnd != null && (localEnd == null || pageEnd > localEnd)) {
          endCursor = {'epoch': nextEpoch, 'seq': pageEnd};
        }
        if (startCursor == null && pageStart != null) {
          startCursor = {'epoch': nextEpoch, 'seq': pageStart};
          hasOlder = result['hasOlder'] == true;
        }
      case 'prepend':
        _merge(units);
        startCursor = {'epoch': nextEpoch, 'seq': pageStart ?? localStart};
        hasOlder = result['hasOlder'] == true;
      case 'discard':
        break;
    }
    if (direction != 'before') synced = true;
  }

  /// A projected row can grow across a cursor. Replace its covered live
  /// chunks, not merely the row with the same start; never treat a sparse
  /// tool span as ownership of every sequence between its first and last event.
  void _merge(List<JsonMap> incoming) {
    final coverage = incoming.expand(_ranges).toList()
      ..sort((a, b) => a.$1.compareTo(b.$1));
    final merged = <(int, int)>[];
    for (final range in coverage) {
      if (merged.isNotEmpty && range.$1 <= merged.last.$2 + 1) {
        final last = merged.removeLast();
        merged.add((last.$1, range.$2 > last.$2 ? range.$2 : last.$2));
      } else {
        merged.add(range);
      }
    }
    _rows.removeWhere(
      (row) => _ranges(row).any((range) {
        var lo = 0, hi = merged.length;
        while (lo < hi) {
          final mid = (lo + hi) ~/ 2;
          if (merged[mid].$2 < range.$1) {
            lo = mid + 1;
          } else {
            hi = mid;
          }
        }
        return lo < merged.length && merged[lo].$1 <= range.$2;
      }),
    );
    _rows.addAll(incoming);
    mergeSort(
      _rows,
      compare: (a, b) => intValue(a['seqStart']).compareTo(intValue(b['seqStart'])),
    );
    _fold.reset();
  }

  Iterable<(int, int)> _ranges(JsonMap row) {
    final ranges = maps(row['sourceSeqRanges']);
    if (ranges.isEmpty) {
      return [(intValue(row['seqStart']), intValue(row['seqEnd']))];
    }
    return ranges.map(
      (range) => (intValue(range['startSeq']), intValue(range['endSeq'])),
    );
  }

  // ── live events ─────────────────────────────────────────────────────────

  void _event(JsonMap message) {
    if (_disposed) return;
    final payload = jsonMap(message['payload']);
    if (payload['agentId'] != agentId &&
        jsonMap(payload['agent'])['id'] != agentId &&
        payload['parentAgentId'] != agentId &&
        jsonMap(payload['subagent'])['parentAgentId'] != agentId) {
      return;
    }
    if (loading) {
      _buffer.add(message);
      return;
    }
    _applyEvent(message);
    _project();
  }

  void _applyEvent(JsonMap message) {
    final p = jsonMap(message['payload']);
    final type = stringValue(message['type']);
    if (type == 'agent.provider_subagents.update') {
      if (p['kind'] == 'upsert') {
        final child = jsonMap(p['subagent']);
        if (child['parentAgentId'] == agentId) {
          subagents.removeWhere((s) => s['id'] == child['id']);
          subagents.add(child);
        }
      } else if (p['parentAgentId'] == agentId && p['kind'] == 'remove') {
        subagents.removeWhere((s) => s['id'] == p['subagentId']);
      }
      if (isSubagent &&
          p['parentAgentId'] == agentId &&
          p['subagentId'] == target.id &&
          p['kind'] == 'timeline') {
        _live(p, {
          'type': 'timeline',
          'item': p['item'],
          'provider': p['provider'],
        });
      }
      return;
    }
    if (p['agentId'] != agentId && jsonMap(p['agent'])['id'] != agentId) return;
    if (type == 'agent.timeline.replacement') {
      if (!isSubagent) unawaited(refreshTail().catchError(_reportError));
      return;
    }
    if (type == 'agent_stream' && !isSubagent) _live(p, jsonMap(p['event']));
    if (type == 'agent_permission_request') {
      final r = jsonMap(p['request']);
      permissions[pendingPermissionKey(r)] = r;
    }
    if (type == 'agent_permission_resolved') {
      // session-context.tsx: the resolution removes the pending request.
      final requestId = p['requestId'];
      if (permissions.remove(stringValue(requestId)) == null) {
        final key = permissions.entries
            .where((entry) => entry.value['id'] == requestId)
            .firstOrNull
            ?.key;
        if (key != null) permissions.remove(key);
      }
    }
    if (p['agent'] is Map) _setAgent(jsonMap(p['agent']));
    if (type == 'agent_status' && p['info'] is Map) {
      _setAgent(jsonMap(p['info']));
    }
    // [_project] notifies once for the whole event.
  }

  /// An agent snapshot is authoritative for its activity and pending
  /// permissions (agent-store.ts replacePendingPermissions).
  void _setAgent(JsonMap value) {
    agent = value;
    final active = jsonMap(value['activeTurn']);
    if (active['turnId'] is String) {
      final id = stringValue(active['turnId']);
      final known = turns[id]?['startedAt'];
      turns[id] = {
        'type': 'turn_started',
        ...active,
        // turn-liveness.ts openTurn: the same turn keeps its first start.
        'startedAt': ?known,
      };
    }
    permissions
      ..clear()
      ..addEntries(
        maps(value['pendingPermissions'])
            .map((p) => MapEntry(pendingPermissionKey(p), p)),
      );
  }

  /// session-stream-reducers.ts processTimelineSequencingGate +
  /// turn-liveness.ts for lifecycle events.
  void _live(JsonMap payload, JsonMap event) {
    final type = event['type'];
    if (type == 'timeline') {
      final seq = payload['seq'];
      final eventEpoch = payload['epoch'];
      JsonMap row(int position) => {
        'item': event['item'],
        'provider': event['provider'],
        'turnId': event['turnId'],
        'timestamp': payload['timestamp'],
        'seqStart': position,
        'seqEnd': position,
        'sourceSeqRanges': [
          {'startSeq': position, 'endSeq': position},
        ],
      };
      if (seq is! int || eventEpoch is! String) {
        // Unsequenced (legacy) delivery applies without moving cursors.
        _rows.add({...row(intValue(endCursor?['seq'])), 'sourceSeqRanges': const []});
        return;
      }
      if (!synced) {
        // No authoritative baseline yet: a live overlay the first page replaces.
        _rows.add(row(seq));
        return;
      }
      if (endCursor == null) {
        epoch = eventEpoch;
        startCursor ??= {'epoch': eventEpoch, 'seq': seq};
      } else if (epoch != eventEpoch) {
        // drop_epoch, except a fresh epoch's first event resets the timeline.
        if (seq != 1) return;
        _generation++;
        _rows.clear();
        _fold.reset();
        turns.clear();
        epoch = eventEpoch;
        startCursor = {'epoch': eventEpoch, 'seq': seq};
        hasOlder = false;
      } else {
        final end = intValue(endCursor!['seq']);
        if (seq <= end) return;
        if (seq > end + 1) {
          // Recover missed canonical rows before accepting a later live row.
          unawaited(_startCatchUp(gap: true));
          return;
        }
      }
      _rows.add(row(seq));
      endCursor = {'epoch': eventEpoch, 'seq': seq};
    } else if (type == 'turn_started') {
      final turnId = event['turnId'] is String ? event['turnId'] as String : null;
      final active = jsonMap(agent['activeTurn']);
      final sameTurn = running &&
          ((turnId != null && active['turnId'] == turnId) ||
              (turnId == null && active['turnId'] == null));
      final startedAt = sameTurn && active['startedAt'] != null
          ? active['startedAt']
          : payload['timestamp'];
      agent = {
        ...agent,
        'status': 'running',
        'activeTurn': {'turnId': turnId, 'startedAt': startedAt},
      };
      if (turnId != null) {
        turns[turnId] = {
          ...event,
          'startedAt': turns[turnId]?['startedAt'] ?? startedAt,
        };
      }
    } else if (type == 'turn_completed' ||
        type == 'turn_canceled' ||
        type == 'turn_failed') {
      // An identified terminal never closes a different identified turn.
      final turnId = event['turnId'];
      final activeId = jsonMap(agent['activeTurn'])['turnId'];
      if (running && turnId is String && activeId is String && turnId != activeId) {
        return;
      }
      agent = {
        ...agent,
        'status': type == 'turn_failed' ? 'error' : 'idle',
        'activeTurn': null,
        if (event['usage'] != null) 'lastUsage': event['usage'],
      };
    }
  }

  // ── projection (types/stream.ts reduceTimelineEvent) ───────────────────

  /// Folds the stored rows not yet projected (all of them after any change
  /// other than an append) and notifies once. [timeline] is a new list only
  /// when an entry changed, so listeners can skip work on identity; a full
  /// refold (a reload, a catch-up, an older page) keeps every entry whose
  /// content did not change as the identical map, so its row is not rebuilt.
  void _project() {
    if (_fold.consumed > _rows.length) _fold.reset();
    if (_fold.consumed < _rows.length || (_rows.isEmpty && timeline.isNotEmpty)) {
      final refold = _fold.consumed == 0;
      for (var i = _fold.consumed; i < _rows.length; i++) {
        _fold.add(_rows[i]);
      }
      _fold.consumed = _rows.length;
      if (refold && timeline.isNotEmpty) _keepUnchanged(_fold.entries);
      timeline = List.of(_fold.entries);
    }
    _notify();
  }

  void _keepUnchanged(List<JsonMap> entries) {
    final previous = {
      for (final entry in timeline)
        if (entry['seqStart'] is int) entry['seqStart'] as int: entry,
    };
    const equality = DeepCollectionEquality();
    for (var i = 0; i < entries.length; i++) {
      final entry = entries[i];
      final old = previous[entry['seqStart']];
      if (old != null &&
          old['seqEnd'] == entry['seqEnd'] &&
          old.length == entry.length &&
          equality.equals(old, entry)) {
        entries[i] = old;
      }
    }
  }

  Future<void> loadSubagents() async {
    final result = requireSuccess(
      await client.request('agent.provider_subagents.list.request', {
        'parentAgentId': agentId,
      }, responseType: 'agent.provider_subagents.list.response'),
    );
    if (_disposed) return;
    subagents = maps(result['subagents']);
    _notify();
  }

  /// daemon-client respondToPermissionAndWait(…, 15000): the response must
  /// resolve within 15s or the card's mutation fails and re-enables. The
  /// card leaves on `agent_permission_resolved` or an agent snapshot that
  /// drops the request.
  Future<void> permission(JsonMap request, JsonMap response) async {
    await action('agent_permission_response', {
      'requestId': request['id'],
      'response': response,
    }, responseType: 'agent_permission_resolved').timeout(const Duration(seconds: 15));
  }

  /// Page older history until [seq] is loaded (chat find, outline jumps).
  /// A failed page throws to the caller, which owns the failure message.
  Future<void> reveal(int seq) async {
    while (!_disposed &&
        hasOlder &&
        (startCursor == null || intValue(startCursor!['seq']) > seq)) {
      final previous = startCursor?['seq'];
      await older();
      if (previous == startCursor?['seq']) break;
    }
  }

  void _reportError(Object? e) {
    debugPrint('[ChatController] timeline synchronization failed: $e');
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _generation++;
    _membershipGeneration++;
    _catchUpGeneration++;
    _membershipRetry?.cancel();
    _catchUpRetry?.cancel();
    client.removeListener(_connectionChanged);
    _events?.cancel();
    // Release is best-effort: a lost connection has already dropped the
    // subscription daemon-side, so its error must not surface as unhandled.
    unawaited(_observation?.close().catchError((Object _) {}));
    // Closing the chat ends the obligation too (viewed-timeline-sync.ts
    // commitDesiredMembership releases it).
    _endCreateHandoff();
    CreateHandoffs.instance.removeListener(_notify);
    super.dispose();
  }
}
