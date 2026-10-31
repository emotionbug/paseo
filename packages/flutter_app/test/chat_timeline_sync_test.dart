import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/target.dart';
import 'package:paseo_sessions/features/chat/chat_controller.dart';

const _agentId = 'agent-1';

JsonMap _row(int seq, JsonMap item, {String? turnId}) => {
  'seq': seq,
  'seqStart': seq,
  'seqEnd': seq,
  'timestamp': '2026-05-01T09:00:00.000Z',
  'turnId': ?turnId,
  'item': item,
};

JsonMap _page({
  required String direction,
  required List<JsonMap> entries,
  String epoch = 'epoch-1',
  int? maxSeq,
  bool hasOlder = false,
  bool hasNewer = false,
  bool reset = false,
}) {
  final start = entries.isEmpty ? null : entries.first['seqStart'] as int;
  final end = entries.isEmpty ? null : entries.last['seqEnd'] as int;
  return {
    'agentId': _agentId,
    'agent': {'id': _agentId, 'status': 'idle', 'provider': 'mock'},
    'direction': direction,
    'epoch': epoch,
    'window': {'minSeq': 1, 'maxSeq': maxSeq ?? end ?? 0},
    'reset': reset,
    'staleCursor': false,
    'gap': false,
    'hasOlder': hasOlder,
    'hasNewer': hasNewer,
    'startCursor': start == null ? null : {'epoch': epoch, 'seq': start},
    'endCursor': end == null ? null : {'epoch': epoch, 'seq': end},
    'entries': entries,
  };
}

JsonMap _text(String type, String text) => {'type': type, 'text': text};

typedef _Responder = FutureOr<JsonMap> Function(JsonMap request);

class _Harness {
  _Harness(this.wire, this.client, this.controller);
  final _TestDaemon wire;
  final PaseoClient client;
  final ChatController controller;

  List<JsonMap> fetches() =>
      wire.requests.where((r) => r['type'] == 'fetch_agent_timeline_request').toList();

  void stream(int seq, JsonMap event, {String epoch = 'epoch-1'}) => wire.emit('agent_stream', {
    'agentId': _agentId,
    'seq': seq,
    'epoch': epoch,
    'timestamp': '2026-05-01T09:00:00.000Z',
    'event': event,
  });

  Future<void> settle() async {
    for (var i = 0; i < 20; i++) {
      await Future<void>.delayed(Duration.zero);
    }
  }

  Future<void> close() async {
    controller.dispose();
    await client.close();
    client.dispose();
  }
}

Future<_Harness> _open(_Responder timeline) async {
  late _TestDaemon wire;
  wire = _TestDaemon((request) {
    switch (request['type']) {
      case 'agent.timeline.set_subscription.request':
        return {'subscriptionId': 'timeline-subscription'};
      case 'agent.provider_subagents.list.request':
        return {'subagents': []};
      case 'fetch_agent_timeline_request':
        return timeline(request);
    }
    return {};
  });
  final client = PaseoClient(
    host: const HostConfig(id: 'host', label: 'host', endpoint: 'ws://127.0.0.1:1/ws'),
    clientId: 'timeline-sync-regression-client',
    autoReconnect: false,
    connector: (endpoint, {headers = const {}, protocols}) async => wire,
  );
  await client.connect();
  final controller = ChatController(
    client,
    const PanelTarget(
      kind: 'agent',
      serverId: 'host',
      workspaceId: 'main',
      cwd: '/project',
      id: _agentId,
    ),
  );
  await controller.start();
  return _Harness(wire, client, controller);
}

void main() {
  test('opening fetches one bounded projected tail', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: [
      _row(1, _text('user_message', 'Hi')),
    ]));
    try {
      final fetch = h.fetches().single;
      expect(fetch['direction'], 'tail');
      expect(fetch['limit'], timelineFetchPageSize);
      expect(fetch['projection'], 'projected');
      expect(h.controller.syncStatus, TimelineSyncStatus.ready);
    } finally {
      await h.close();
    }
  });

  test('a resume that overflows installs the latest tail instead of paging', () async {
    var phase = 0;
    final h = await _open((request) {
      if (phase == 0) {
        return _page(direction: 'tail', entries: [_row(1, _text('user_message', 'Hi'))]);
      }
      if (request['direction'] == 'after') {
        return _page(
          direction: 'after',
          entries: [_row(2, _text('assistant_message', 'page'))],
          maxSeq: 200,
          hasNewer: true,
        );
      }
      return _page(
        direction: 'tail',
        entries: [_row(199, _text('user_message', 'Latest')), _row(200, _text('assistant_message', 'Tail'))],
        hasOlder: true,
      );
    });
    try {
      phase = 1;
      h.controller.setAppVisible(false);
      h.controller.setAppVisible(true);
      expect(h.controller.syncStatus, TimelineSyncStatus.pending);
      await h.settle();
      final directions = h.fetches().map((r) => r['direction']).toList();
      expect(directions, ['tail', 'after', 'tail']);
      expect(h.controller.timeline.map((e) => jsonMap(e['item'])['text']), ['Latest', 'Tail']);
      expect(h.controller.hasOlder, isTrue);
      expect(h.controller.syncStatus, TimelineSyncStatus.ready);
    } finally {
      await h.close();
    }
  });

  test('a live sequence gap pages forward until current without a tail fallback', () async {
    final h = await _open((request) {
      if (request['direction'] == 'tail') {
        return _page(direction: 'tail', entries: [_row(1, _text('user_message', 'Hi'))]);
      }
      final after = intValue(jsonMap(request['cursor'])['seq']);
      return after == 1
          ? _page(direction: 'after', entries: [_row(2, _text('assistant_message', 'A'))], maxSeq: 3, hasNewer: true)
          : _page(direction: 'after', entries: [_row(3, {'type': 'tool_call', 'callId': 'c', 'name': 'x', 'status': 'completed', 'detail': {'type': 'unknown'}})]);
    });
    try {
      h.stream(4, {'type': 'timeline', 'item': _text('assistant_message', 'late')});
      await h.settle();
      expect(h.fetches().map((r) => r['direction']).toList(), ['tail', 'after', 'after']);
      // The gapped live row was dropped; history is contiguous through seq 3.
      expect(h.controller.endCursor?['seq'], 3);
      expect(h.controller.syncStatus, TimelineSyncStatus.ready);
    } finally {
      await h.close();
    }
  });

  test('a failed catch-up reports a sync error and the user retry owns retrying', () async {
    var fail = false;
    final h = await _open((request) {
      if (fail) return {'error': 'Thread busy'};
      return _page(direction: request['direction'] as String, entries: [_row(1, _text('user_message', 'Hi'))]);
    });
    try {
      fail = true;
      await h.controller.reload();
      expect(h.controller.syncStatus, TimelineSyncStatus.error);
      expect('${h.controller.error}', contains('Thread busy'));
      fail = false;
      h.controller.retrySync();
      expect(h.controller.syncStatus, TimelineSyncStatus.retrying);
      await h.settle();
      expect(h.controller.syncStatus, TimelineSyncStatus.ready);
      expect(h.controller.error, isNull);
    } finally {
      await h.close();
    }
  });

  test('retry delays double from one second to a thirty second ceiling', () {
    Duration? delay;
    final delays = <int>[];
    for (var i = 0; i < 7; i++) {
      delay = nextTimelineRetryDelay(delay);
      delays.add(delay.inSeconds);
    }
    expect(delays, [1, 2, 4, 8, 16, 30, 30]);
  });

  test('an older page failure is not a timeline sync error', () async {
    final h = await _open((request) {
      if (request['direction'] == 'before') return {'error': 'boom'};
      return _page(direction: 'tail', entries: [_row(5, _text('user_message', 'Hi'))], hasOlder: true);
    });
    try {
      await expectLater(h.controller.older(), throwsA(anything));
      expect(h.controller.error, isNull);
      expect(h.controller.syncStatus, TimelineSyncStatus.ready);
    } finally {
      await h.close();
    }
  });

  test('a same-epoch replacement keeps loaded older history and the rows on screen', () async {
    final h = await _open((request) {
      if (request['direction'] == 'before') {
        return _page(direction: 'before', entries: [_row(9, _text('user_message', 'Older'))]);
      }
      return _page(direction: 'tail', entries: [
        _row(10, _text('user_message', 'Prompt')),
        _row(11, _text('assistant_message', 'Reply')),
      ], maxSeq: 11, hasOlder: true);
    });
    try {
      await h.controller.older();
      expect(h.controller.timeline, hasLength(3));
      h.wire.emit('agent.timeline.replacement', {'agentId': _agentId, 'epoch': 'epoch-1'});
      await h.settle();
      expect(h.fetches().map((r) => r['direction']).toList(), ['tail', 'before', 'tail']);
      expect(
        h.controller.timeline.map((e) => jsonMap(e['item'])['text']),
        ['Older', 'Prompt', 'Reply'],
      );
    } finally {
      await h.close();
    }
  });

  test('a live row from another epoch is dropped unless it starts that epoch', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: [
      _row(1, _text('user_message', 'Old epoch')),
    ]));
    try {
      h.stream(7, {'type': 'timeline', 'item': _text('assistant_message', 'stray')}, epoch: 'epoch-2');
      await h.settle();
      expect(h.controller.timeline.map((e) => jsonMap(e['item'])['text']), ['Old epoch']);
      h.stream(1, {'type': 'timeline', 'item': _text('user_message', 'Fresh')}, epoch: 'epoch-2');
      await h.settle();
      expect(h.controller.timeline.map((e) => jsonMap(e['item'])['text']), ['Fresh']);
      expect(h.controller.epoch, 'epoch-2');
      expect(h.fetches(), hasLength(1));
    } finally {
      await h.close();
    }
  });

  test('a permission resolution removes its card', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: const []));
    try {
      h.wire.emit('agent_permission_request', {
        'agentId': _agentId,
        'request': {'id': 'perm-1', 'kind': 'tool', 'name': 'Bash'},
      });
      await h.settle();
      expect(h.controller.permissions, hasLength(1));
      h.wire.emit('agent_permission_resolved', {'agentId': _agentId, 'requestId': 'perm-1'});
      await h.settle();
      expect(h.controller.permissions, isEmpty);
    } finally {
      await h.close();
    }
  });

  test('a terminal event for a different identified turn keeps the active turn', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: const []));
    try {
      h.wire.emit('agent_stream', {
        'agentId': _agentId,
        'timestamp': '2026-05-01T09:00:00.000Z',
        'event': {'type': 'turn_started', 'turnId': 'turn-2'},
      });
      h.wire.emit('agent_stream', {
        'agentId': _agentId,
        'timestamp': '2026-05-01T09:00:01.000Z',
        'event': {'type': 'turn_completed', 'turnId': 'turn-1'},
      });
      await h.settle();
      expect(h.controller.running, isTrue);
      h.wire.emit('agent_stream', {
        'agentId': _agentId,
        'timestamp': '2026-05-01T09:00:02.000Z',
        'event': {'type': 'turn_completed', 'turnId': 'turn-2'},
      });
      await h.settle();
      expect(h.controller.running, isFalse);
    } finally {
      await h.close();
    }
  });

  test('tool call updates never reopen a completed call or drop a typed detail', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: [
      _row(1, {
        'type': 'tool_call',
        'callId': 'call-1',
        'name': 'shell',
        'status': 'completed',
        'detail': {'type': 'shell', 'command': 'ls'},
      }, turnId: 't1'),
      _row(2, {
        'type': 'tool_call',
        'callId': 'call-1',
        'name': 'shell',
        'status': 'running',
        'detail': {'type': 'unknown', 'input': null, 'output': null},
      }, turnId: 't1'),
      _row(3, {
        'type': 'tool_call',
        'callId': 'call-1',
        'name': 'shell',
        'status': 'running',
        'detail': {'type': 'unknown'},
      }, turnId: 't2'),
    ]));
    try {
      final rows = h.controller.timeline;
      expect(rows, hasLength(2), reason: 'a call id is scoped to its turn');
      final merged = jsonMap(rows.first['item']);
      expect(merged['status'], 'completed');
      expect(jsonMap(merged['detail'])['type'], 'shell');
      expect(merged['error'], isNull);
    } finally {
      await h.close();
    }
  });

  test('assistant and reasoning chunks extend the latest row; blank chunks start nothing', () async {
    final h = await _open((request) => _page(direction: 'tail', entries: [
      _row(1, _text('reasoning', 'Think'), turnId: 't1'),
      _row(2, {'type': 'reasoning', 'text': 'ing', 'messageId': 'r2'}, turnId: 't1'),
      _row(3, _text('assistant_message', 'Hel\r'), turnId: 't1'),
      _row(4, _text('assistant_message', 'lo'), turnId: 't2'),
      _row(5, _text('user_message', 'Next')),
      _row(6, _text('assistant_message', '   ')),
    ]));
    try {
      final texts = h.controller.timeline.map((e) => jsonMap(e['item'])['text']).toList();
      expect(texts, ['Thinking', 'Hello', 'Next']);
      expect(h.controller.timeline[1]['turnId'], 't2');
    } finally {
      await h.close();
    }
  });

  test('a sent prompt keeps its images and attachments after the daemon echoes it', () async {
    // The daemon never echoes images: the sending client keeps them, as the
    // original merged its local row with the "existing" presentation.
    final image = {'data': 'aGVsbG8=', 'mimeType': 'image/png'};
    final file = {'type': 'workspace_file', 'path': 'lib/main.dart'};
    SentUserMessages.remember(_agentId, {
      'messageId': 'client-1',
      'images': [image],
      'attachments': [file],
    });
    final h = await _open((request) => _page(direction: 'tail', entries: [
      _row(1, {'type': 'user_message', 'text': 'Look', 'clientMessageId': 'client-1'}),
    ]));
    try {
      JsonMap user() => jsonMap(h.controller.timeline.single['item']);
      expect(jsonList(user()['images']), [image]);
      expect(jsonList(user()['attachments']), [file]);
      // A re-delivered echo of the same prompt keeps the shown row.
      h.stream(2, {
        'type': 'timeline',
        'item': {'type': 'user_message', 'text': 'Look', 'clientMessageId': 'client-1', 'messageId': 'm-1'},
      });
      await h.settle();
      expect(h.controller.timeline, hasLength(1));
      expect(user()['messageId'], 'm-1');
      expect(jsonList(user()['images']), [image]);
    } finally {
      await h.close();
    }
  });
}

/// The daemon endpoint replaced with controlled wire responses; production
/// request correlation and validation stay in place.
class _TestDaemon implements DaemonTransport {
  _TestDaemon(this.respond);
  final _Responder respond;
  final requests = <JsonMap>[];
  final _messages = StreamController<Object>();
  @override
  Stream<Object> get messages => _messages.stream;

  void emit(String type, JsonMap payload) {
    if (!_messages.isClosed) {
      _messages.add(jsonEncode({
        'type': 'session',
        'message': {'type': type, 'payload': payload},
      }));
    }
  }

  @override
  void send(Object data) {
    final envelope = jsonMap(jsonDecode(data as String));
    if (envelope['type'] == 'hello') {
      emit('status', {
        'status': 'server_info',
        'serverId': 'host',
        'features': {'ownedSubscriptions': true, 'workspaceMultiplicity': true},
      });
      return;
    }
    if (envelope['type'] == 'ping') {
      _messages.add(jsonEncode({'type': 'pong'}));
      return;
    }
    final request = jsonMap(envelope['message']);
    requests.add(request);
    final type = request['type'] as String;
    final responseType = type.endsWith('request')
        ? '${type.substring(0, type.length - 'request'.length)}response'
        : '${type}_response';
    Future<JsonMap>.sync(() => respond(request)).then((payload) {
      emit(responseType, {...payload, 'requestId': request['requestId']});
    });
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    await _messages.close();
  }
}
