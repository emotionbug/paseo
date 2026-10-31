import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/target.dart';
import 'package:paseo_sessions/features/chat/agent_attention.dart';

AgentAttentionState _state({
  String? agentId = 'f13',
  bool connected = true,
  bool requiresAttention = true,
  String? attentionReason = 'finished',
  bool screenFocused = false,
  bool appVisible = true,
}) => (
  agentId: agentId,
  connected: connected,
  requiresAttention: requiresAttention,
  attentionReason: attentionReason,
  screenFocused: screenFocused,
  appVisible: appVisible,
);

void main() {
  group('use-agent-attention-clear port', () {
    test('a retained panel that is not focused never clears its agent', () {
      final cleared = <String>[];
      final attention = AgentAttentionClear(_state(), cleared.add);
      attention.update(_state(connected: true));
      attention.update(_state(requiresAttention: true, attentionReason: 'error'));
      expect(cleared, isEmpty);
    });

    test('entering focus clears once; staying focused does not repeat', () {
      final cleared = <String>[];
      final attention = AgentAttentionClear(_state(), cleared.add);
      attention.update(_state(screenFocused: true));
      attention.update(_state(screenFocused: true));
      expect(cleared, ['f13']);
    });

    test('a panel mounted focused clears on its focus entry', () {
      final cleared = <String>[];
      AgentAttentionClear(_state(screenFocused: true), cleared.add);
      expect(cleared, ['f13']);
    });

    test('permission attention is never cleared by any trigger', () {
      final cleared = <String>[];
      final attention = AgentAttentionClear(
        _state(attentionReason: 'permission', screenFocused: true),
        cleared.add,
      );
      attention
        ..clearOnInputFocus()
        ..clearOnPromptSend()
        ..clearOnAgentBlur();
      expect(cleared, isEmpty);
    });

    test('disconnected or attention-free agents are left alone', () {
      final cleared = <String>[];
      AgentAttentionClear(_state(connected: false, screenFocused: true), cleared.add);
      AgentAttentionClear(_state(requiresAttention: false, screenFocused: true), cleared.add);
      AgentAttentionClear(_state(agentId: null, screenFocused: true), cleared.add);
      expect(cleared, isEmpty);
    });

    test('attention raised while viewing defers the focus entry, not input focus', () {
      final cleared = <String>[];
      final attention = AgentAttentionClear(
        _state(requiresAttention: false, screenFocused: true),
        cleared.add,
      );
      // The agent finishes while the reader watches it.
      attention.update(_state(screenFocused: true));
      expect(cleared, isEmpty);
      // Leaving and coming back is a deferred focus entry: still not cleared...
      attention.update(_state(screenFocused: false));
      cleared.clear();
      attention.update(_state(screenFocused: true));
      expect(cleared, isEmpty);
      // ...until the reader interacts.
      attention.clearOnInputFocus();
      expect(cleared, ['f13']);
    });

    test('resuming the app into a focused agent clears it', () {
      final cleared = <String>[];
      final attention = AgentAttentionClear(
        _state(requiresAttention: false, screenFocused: true, appVisible: false),
        cleared.add,
      );
      attention.update(_state(screenFocused: true, appVisible: false));
      expect(cleared, isEmpty);
      attention.update(_state(screenFocused: true, appVisible: true));
      expect(cleared, ['f13']);
    });
  });

  group('pickAttentionAgent', () {
    JsonMap agent(
      String id, {
      bool requiresAttention = true,
      String? reason,
      String? at,
      String? parentAgentId,
    }) => {
      'id': id,
      'requiresAttention': requiresAttention,
      'attentionReason': reason,
      'attentionTimestamp': at,
      'parentAgentId': parentAgentId,
    };

    test('nothing requires attention', () {
      expect(pickAttentionAgent([]), isNull);
      expect(
        pickAttentionAgent([
          agent('agent-1', requiresAttention: false, reason: 'permission'),
          agent('agent-2', requiresAttention: false),
        ]),
        isNull,
      );
    });

    test('permission outranks error outranks finished', () {
      expect(
        pickAttentionAgent([
          agent('finished', reason: 'finished', at: '2026-01-01T00:00:00.000Z'),
          agent('error', reason: 'error', at: '2026-01-02T00:00:00.000Z'),
          agent('permission', reason: 'permission', at: '2026-01-03T00:00:00.000Z'),
        ]),
        'permission',
      );
    });

    test('the oldest attention wins at the same priority', () {
      expect(
        pickAttentionAgent([
          agent('newer', reason: 'permission', at: '2026-01-03T00:00:00.000Z'),
          agent('older', reason: 'permission', at: '2026-01-01T00:00:00.000Z'),
        ]),
        'older',
      );
    });

    test('subagents and reasonless attention are never picked', () {
      expect(
        pickAttentionAgent([
          agent(
            'subagent',
            reason: 'permission',
            at: '2026-01-01T00:00:00.000Z',
            parentAgentId: 'parent',
          ),
          agent('top-level', reason: 'finished', at: '2026-01-02T00:00:00.000Z'),
        ]),
        'top-level',
      );
      expect(
        pickAttentionAgent([
          agent('subagent', reason: 'permission', parentAgentId: 'parent'),
        ]),
        isNull,
      );
      expect(
        pickAttentionAgent([
          agent('reasonless', at: '2026-01-01T00:00:00.000Z'),
          agent('error', reason: 'error', at: '2026-01-02T00:00:00.000Z'),
        ]),
        'error',
      );
    });
  });

  test('opening and selecting tabs never clears agent or workspace attention', () async {
    final wire = _TestDaemon((request) => {});
    final client = PaseoClient(
      host: const HostConfig(id: 'host', label: 'host', endpoint: 'ws://127.0.0.1:1/ws'),
      clientId: 'attention-regression-client',
      autoReconnect: false,
      connector: (endpoint, {headers = const {}, protocols}) async => wire,
    );
    await client.connect();
    final store = AppStore(connectHosts: false, discoverLocalHosts: false);
    store.hosts.add(
      HostState(
        config: client.host,
        client: client,
        serverId: 'host',
      )..agents = [
          for (final id in ['f12', 'f13', 'f16'])
            {
              'id': id,
              'workspaceId': 'main',
              'title': id,
              'status': 'idle',
              'requiresAttention': true,
              'attentionReason': id == 'f12' ? 'error' : 'finished',
            },
        ],
    );
    PanelTarget target(String id) => PanelTarget(
      kind: 'agent',
      serverId: 'host',
      workspaceId: 'main',
      cwd: '/project',
      id: id,
      title: id,
    );
    try {
      store.open(target('f16'));
      store.open(target('f12'));
      store.selectTab(target('f16').key);
      await Future<void>.delayed(const Duration(milliseconds: 50));
      final types = wire.requests.map((request) => request['type']);
      expect(types, isNot(contains('clear_agent_attention')));
      expect(types, isNot(contains('workspace.clear_attention.request')));
    } finally {
      // Disposing the store closes the host's client.
      await store.disposeAsync();
      client.dispose();
    }
  });
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
      _emit(responseType, {...payload, 'requestId': request['requestId']});
    });
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    await _messages.close();
  }
}
