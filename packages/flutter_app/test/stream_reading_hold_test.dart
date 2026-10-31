import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/scope.dart';
import 'package:paseo_sessions/core/target.dart';
import 'package:paseo_sessions/features/chat/agent_panel.dart';
import 'package:paseo_sessions/ui/theme.dart';

const _agentId = 'a0000000-0000-4000-8000-000000000001';
const _target = PanelTarget(
  kind: 'agent',
  serverId: 'host',
  workspaceId: 'main',
  cwd: '/project',
  id: _agentId,
);

JsonMap get _runningAgent => {
  'id': _agentId,
  'workspaceId': 'main',
  'cwd': '/project',
  'title': 'Running agent',
  'provider': 'mock',
  'status': 'running',
};

JsonMap _tool(int seq) => {
  'seq': seq,
  'seqStart': seq,
  'seqEnd': seq,
  'turnId': 'turn-1',
  'timestamp': '2026-05-01T09:00:00.000Z',
  'item': {
    'type': 'tool_call',
    'callId': 'call-$seq',
    'name': 'shell',
    'status': 'completed',
    'detail': {'type': 'shell', 'command': 'echo step $seq', 'output': 'step $seq'},
  },
};

JsonMap _text(int seq, String type, String text) => {
  'seq': seq,
  'seqStart': seq,
  'seqEnd': seq,
  'turnId': 'turn-1',
  'timestamp': '2026-05-01T09:00:00.000Z',
  'item': {'type': type, 'text': text},
};

/// A running turn: a prompt, a long answer, then a run of tool rows.
List<JsonMap> _history() => [
  _text(1, 'user_message', 'Run the checks'),
  for (var i = 2; i <= 6; i++)
    _text(i, 'assistant_message', 'Paragraph $i. ${'Long reasoning text. ' * 12}'),
  for (var i = 7; i <= 30; i++) _tool(i),
];

void main() {
  testWidgets('a detached reader is not shaken by rows arriving below it', (tester) async {
    tester.view.physicalSize = const Size(424, 924);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);

    late _TestDaemon wire;
    wire = _TestDaemon((request) {
      switch (request['type']) {
        case 'agent.timeline.set_subscription.request':
          return {'subscriptionId': 'timeline-subscription'};
        case 'agent.provider_subagents.list.request':
          return {'subagents': []};
        case 'fetch_agent_timeline_request':
          final entries = _history();
          return {
            'agentId': _agentId,
            'agent': _runningAgent,
            'direction': 'tail',
            'epoch': 'epoch-1',
            'window': {'minSeq': 1, 'maxSeq': entries.length, 'nextSeq': entries.length + 1},
            'reset': false,
            'staleCursor': false,
            'gap': false,
            'hasOlder': false,
            'hasNewer': false,
            'entries': entries,
          };
      }
      return {};
    });
    final client = PaseoClient(
      host: const HostConfig(id: 'host', label: 'host', endpoint: 'ws://127.0.0.1:1/ws'),
      clientId: 'reading-hold-regression-client',
      autoReconnect: false,
      connector: (endpoint, {headers = const {}, protocols}) async => wire,
    );
    final connecting = client.connect();
    for (var i = 0; i < 5; i++) {
      await tester.pump(const Duration(milliseconds: 10));
    }
    await connecting;
    final store = AppStore(connectHosts: false, discoverLocalHosts: false);
    store.hosts.add(HostState(config: client.host, client: client, serverId: 'host'));

    try {
      await tester.pumpWidget(
        PaseoScope(
          store: store,
          child: MaterialApp(
            theme: paseoTheme(brightness: Brightness.dark, preferences: const {}),
            home: const Scaffold(body: AgentPanel(target: _target)),
          ),
        ),
      );
      for (var i = 0; i < 30; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
      final scrollable = find.byType(Scrollable).first;
      final position = tester.state<ScrollableState>(scrollable).position;
      expect(position.pixels, 0, reason: 'opens at the newest edge');

      // Scroll up into the history (a downward drag on the inverted list),
      // stopping mid-transcript, so the stream detaches.
      await tester.drag(scrollable, const Offset(0, 260));
      for (var i = 0; i < 60; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
      final detached = position.pixels;
      expect(detached, greaterThan(100));
      // What the reader is looking at: the end of the answer above the tools.
      final reading = find.textContaining('Paragraph 6.', findRichText: true);
      final readingTop = tester.getTopLeft(reading).dy;

      // Tool rows keep arriving below the reader, as in a running turn, then
      // stop; through both the row on screen must not move a pixel.
      final tops = <double>{};
      final rest = <double>{};
      var seq = 31;
      for (var frame = 0; frame < 150; frame++) {
        if (frame < 120 && frame % 10 == 0) {
          wire.emit('agent_stream', {
            'agentId': _agentId,
            'seq': seq,
            'epoch': 'epoch-1',
            'timestamp': '2026-05-01T09:00:00.000Z',
            'event': {'type': 'timeline', 'item': jsonMap(_tool(seq)['item'])},
          });
          seq++;
        }
        await tester.pump(const Duration(milliseconds: 16));
        tops.add(tester.getTopLeft(reading).dy);
        if (frame >= 120) rest.add(position.pixels);
      }
      expect(tops, {readingTop}, reason: 'the row being read shakes or drifts');
      expect(rest, hasLength(1), reason: 'the list never comes to rest');
      expect(position.pixels, greaterThan(detached), reason: 'the new rows grew the list below');

      // The hold never fights the reader's own scrolling.
      final held = position.pixels;
      await tester.drag(scrollable, const Offset(0, 120));
      for (var i = 0; i < 60; i++) {
        await tester.pump(const Duration(milliseconds: 16));
      }
      expect(position.pixels, greaterThan(held + 80), reason: 'the hold undid the scroll');
    } finally {
      await tester.pumpWidget(const SizedBox.shrink());
      await store.disposeAsync();
      client.dispose();
      await tester.runAsync(() => wire.close());
    }
  });
}

/// The daemon endpoint replaced with controlled wire responses; production
/// request correlation and validation stay in place.
class _TestDaemon implements DaemonTransport {
  _TestDaemon(this.respond);
  final FutureOr<JsonMap> Function(JsonMap) respond;
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
        'features': {
          'ownedSubscriptions': true,
          'workspaceMultiplicity': true,
          'providerSubagents': true,
        },
      });
      return;
    }
    if (envelope['type'] == 'ping') {
      _messages.add(jsonEncode({'type': 'pong'}));
      return;
    }
    final request = jsonMap(envelope['message']);
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
