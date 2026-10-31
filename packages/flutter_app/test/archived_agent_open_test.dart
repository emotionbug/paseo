import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/target.dart';
import 'package:paseo_sessions/features/chat/chat_controller.dart';
import 'package:paseo_sessions/features/navigation/navigation_actions.dart';

/// F11: an archived agent with two persisted rows. refresh_agent_request makes the
/// daemon rehydrate provider history over the checkpoint, so merely opening it
/// (history row → tab → panel) must never send it.
const _agentId = 'f1100000-0000-4000-8000-000000000011';

JsonMap get _archivedAgent => {
  'id': _agentId,
  'workspaceId': 'main',
  'cwd': '/project',
  'title': 'F11 Archived agent',
  'provider': 'mock',
  'status': 'idle',
  'archivedAt': '2026-05-01T10:00:00.000Z',
};

JsonMap _respond(JsonMap request) {
  switch (request['type']) {
    case 'agent.timeline.set_subscription.request':
      return {'subscriptionId': 'timeline-subscription'};
    case 'fetch_agent_timeline_request':
      return {
        'agentId': _agentId,
        'agent': _archivedAgent,
        'epoch': 'epoch-1',
        'window': {},
        'reset': false,
        'staleCursor': false,
        'gap': false,
        'hasOlder': false,
        'hasNewer': false,
        'entries': [
          {
            'seq': 1,
            'timestamp': '2026-05-01T09:00:00.000Z',
            'item': {'type': 'user_message', 'text': 'Archive me', 'messageId': 'm1'},
          },
          {
            'seq': 2,
            'timestamp': '2026-05-01T09:00:05.000Z',
            'item': {'type': 'assistant_message', 'text': 'Done.'},
          },
        ],
      };
    case 'agent.provider_subagents.list.request':
      return {'subagents': []};
  }
  return {};
}

void main() {
  test('opening an archived agent from history only opens its tab', () async {
    final wire = _TestDaemon(_respond);
    final client = await _connect(wire);
    final store = AppStore(connectHosts: false, discoverLocalHosts: false);
    store.hosts.add(HostState(config: client.host, client: client, serverId: 'host'));
    try {
      openNavigationAgent(store, 'host', _archivedAgent);
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(store.tabs.map((tab) => tab.id), contains(_agentId));
      expect(
        wire.requests.map((request) => request['type']),
        isNot(contains('refresh_agent_request')),
      );
    } finally {
      await store.disposeAsync();
      client.dispose();
    }
  });

  test('loading an archived agent panel only reads its timeline', () async {
    final wire = _TestDaemon(_respond);
    final client = await _connect(wire);
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
    try {
      await controller.start();
      expect(controller.timeline, hasLength(2));
      expect(controller.agent['archivedAt'], isNotNull);
      expect(
        wire.requests.map((request) => request['type']).toSet(),
        {
          'agent.timeline.set_subscription.request',
          'fetch_agent_timeline_request',
          'agent.provider_subagents.list.request',
        },
      );
    } finally {
      controller.dispose();
      await client.close();
      client.dispose();
    }
  });
}

Future<PaseoClient> _connect(_TestDaemon wire) async {
  final client = PaseoClient(
    host: const HostConfig(id: 'host', label: 'host', endpoint: 'ws://127.0.0.1:1/ws'),
    clientId: 'archived-open-regression-client',
    autoReconnect: false,
    connector: (endpoint, {headers = const {}, protocols}) async => wire,
  );
  await client.connect();
  return client;
}

/// The daemon endpoint replaced with controlled wire responses; production
/// request correlation and validation stay in place.
class _TestDaemon implements DaemonTransport {
  _TestDaemon(this.respond);
  final FutureOr<JsonMap> Function(JsonMap) respond;
  final requests = <JsonMap>[];
  final _messages = StreamController<Object>();
  @override
  Stream<Object> get messages => _messages.stream;

  void _emit(String type, JsonMap payload) {
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
      _emit('status', {
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
    requests.add(request);
    final type = request['type'] as String;
    final responseType = type.endsWith('request')
        ? '${type.substring(0, type.length - 'request'.length)}response'
        : '${type}_response';
    Future<JsonMap>.sync(() => respond(request)).then((payload) {
      _emit(responseType, {...payload, 'requestId': request['requestId']});
    });
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    await _messages.close();
  }
}
