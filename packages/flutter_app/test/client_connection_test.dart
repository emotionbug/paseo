import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';

import 'package:paseo_sessions/core/client.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/transport/connection_selection.dart';
import 'package:paseo_sessions/core/transport/io_transport.dart' show DaemonTransportClosedException;

/// Connection-level parity with the original daemon-client.ts / host-runtime.ts.
void main() {
  test('reconnect backoff is min(1500ms * 2^attempt, 30s) without jitter', () {
    expect(
      [for (var i = 0; i < 7; i++) PaseoClient.reconnectDelay(i).inMilliseconds],
      [1500, 3000, 6000, 12000, 24000, 30000, 30000],
    );
    expect(PaseoClient.reconnectDelay(60).inMilliseconds, 30000);
  });

  test('only credential, endpoint-security and identity failures stop reconnects', () {
    expect(PaseoClient.isTerminalFailure(HelloRejectedException('incorrect_password', [])), isTrue);
    expect(PaseoClient.isTerminalFailure(HelloRejectedException('password_required', [])), isTrue);
    expect(PaseoClient.isTerminalFailure(HelloRejectedException('incompatible_protocol', [])), isFalse);
    expect(PaseoClient.isTerminalFailure(const DaemonTransportClosedException(4003, 'Incompatible protocol version')), isFalse);
    expect(PaseoClient.isTerminalFailure(const DaemonTransportClosedException(4401, 'Incorrect password')), isTrue);
    expect(PaseoClient.isTerminalFailure(const FormatException('bad frame')), isFalse);
    expect(HelloRejectedException('incorrect_password', []).message, 'Incorrect password');
  });

  testWidgets('invalid inbound frames are dropped without disconnecting', (tester) async {
    final wire = _Wire();
    final client = _client(() async => wire);
    await client.connect();
    wire.push('not json');
    wire.push(jsonEncode({'type': 'session', 'message': 'nope'}));
    wire.push(jsonEncode({'kind': 'unknown'}));
    await tester.pump();
    expect(client.status, ConnectionStatus.connected);
    expect(await client.request('project.list.request', {}), containsPair('projects', []));
    await client.close();
  });

  testWidgets('recent provider sessions are one list; paged directories still need pageInfo', (tester) async {
    final wire = _Wire(reply: (request) => {'entries': <Object>[]});
    final client = _client(() async => wire);
    await client.connect();
    // fetch_recent_provider_sessions_response has no pageInfo in the protocol.
    expect(
      await client.request(
        'fetch_recent_provider_sessions_request',
        {'limit': 20},
        responseType: 'fetch_recent_provider_sessions_response',
      ),
      containsPair('entries', isEmpty),
    );
    await expectLater(
      client.request(
        'fetch_agent_history_request',
        {},
        responseType: 'fetch_agent_history_response',
      ),
      throwsA(isA<FormatException>()),
    );
    await client.close();
  });

  testWidgets('incompatible protocol retries after 1.5s; a wrong password does not', (tester) async {
    for (final (reason, retries) in [('incompatible_protocol', true), ('incorrect_password', false)]) {
      var attempts = 0;
      final client = _client(() async {
        attempts++;
        return _Wire(rejection: attempts == 1 ? reason : null);
      }, autoReconnect: true);
      await expectLater(client.connect(), throwsA(isA<HelloRejectedException>()));
      expect(client.status, ConnectionStatus.error);
      await tester.pump(const Duration(milliseconds: 1499));
      expect(attempts, 1);
      await tester.pump(const Duration(milliseconds: 2));
      await tester.pump();
      expect(attempts, retries ? 2 : 1, reason: reason);
      expect(client.status, retries ? ConnectionStatus.connected : ConnectionStatus.error);
      await client.close();
    }
  });

  testWidgets('two consecutive unanswered heartbeats reconnect; traffic in between resets', (tester) async {
    final wires = <_Wire>[];
    final client = _client(() async {
      final wire = _Wire(answerPings: wires.isNotEmpty);
      wires.add(wire);
      return wire;
    }, autoReconnect: true);
    await client.connect();
    // Ping at 10s times out at 25s; inbound traffic then clears the failure count.
    await tester.pump(const Duration(seconds: 26));
    expect(wires.first.pings, 1);
    wires.first.push(jsonEncode({'type': 'session', 'message': {'type': 'activity_log', 'payload': {}}}));
    await tester.pump(const Duration(seconds: 25));
    expect(wires.first.pings, 2);
    expect(client.status, ConnectionStatus.connected);
    // Third and fourth probes fail back to back: the connection is torn down.
    await tester.pump(const Duration(seconds: 50));
    expect(wires.first.closed, isTrue);
    await tester.pump(const Duration(seconds: 2));
    expect(wires, hasLength(2));
    expect(client.status, ConnectionStatus.connected);
    await client.close();
  });

  testWidgets('requests made while connecting wait for admission', (tester) async {
    final hello = Completer<void>();
    final wire = _Wire(admit: hello.future);
    final client = _client(() async => wire);
    final connecting = client.connect();
    await tester.pump();
    expect(client.status, ConnectionStatus.connecting);
    final response = client.request('project.list.request', {});
    await tester.pump();
    expect(wire.requests, isEmpty);
    hello.complete();
    await connecting;
    expect(await response, containsPair('projects', []));
    expect(wire.requests.single['type'], 'project.list.request');
    await client.close();
    await expectLater(
      client.request('project.list.request', {}),
      throwsA(isA<DaemonException>().having((e) => e.code, 'code', 'DAEMON_NOT_CONNECTED')),
    );
  });

  test('connection selection follows the original latency and backoff rules', () {
    const available = ConnectionProbeStatus.available;
    expect(
      selectBestConnection(['lan', 'relay', 'down'], {
        'lan': const ConnectionProbe(available, Duration(milliseconds: 30)),
        'relay': const ConnectionProbe(available, Duration(milliseconds: 30)),
        'down': const ConnectionProbe(ConnectionProbeStatus.unavailable),
      }),
      'lan',
    );
    expect(selectBestConnection(['down'], {'down': const ConnectionProbe(ConnectionProbeStatus.pending)}), isNull);
    Duration interval(int seconds, {bool active = false, bool online = false}) => probeIntervalForConnection(
      age: Duration(seconds: seconds),
      isActiveOnline: active,
      hasActiveOnlineConnection: online,
    );
    expect(interval(0, active: true, online: true), const Duration(seconds: 10));
    expect(interval(0, online: true), const Duration(seconds: 120));
    expect([interval(5), interval(20), interval(45), interval(90)], const [
      Duration(seconds: 2),
      Duration(seconds: 5),
      Duration(seconds: 10),
      Duration(seconds: 30),
    ]);
  });

  testWidgets('an unreachable active connection is replaced by a reachable one', (tester) async {
    const lan = HostConfig(id: 'lan', label: 'host', endpoint: 'ws://10.0.0.2:6767/ws');
    const relay = HostConfig(id: 'tailnet', label: 'host', endpoint: 'ws://100.64.0.2:6767/ws');
    final down = _client(() async => throw const DaemonException('refused'), host: lan);
    await expectLater(down.connect(), throwsA(isA<DaemonException>()));
    var active = lan;
    PaseoClient? adopted;
    final selector = HostConnectionSelector(
      connections: () => const [lan, relay],
      activeConnectionId: () => active.id,
      activeClient: () => active.id == lan.id ? down : adopted,
      expectedServerId: () => 'host',
      createProbeClient: (connection) => _client(
        () async => connection.id == lan.id ? throw const DaemonException('refused') : _Wire(),
        host: connection,
      ),
      activate: (connection, existing) async {
        active = connection;
        if (existing != null) {
          adopted = existing;
          return;
        }
        adopted = _client(() async => _Wire(), host: connection, autoReconnect: true);
        await adopted!.connect();
      },
    );
    await selector.runProbeCycleNow();
    expect(active.id, relay.id);
    expect(adopted?.status, ConnectionStatus.connected);
    expect(selector.probes[lan.id]?.status, ConnectionProbeStatus.unavailable);
    expect(selector.probes[relay.id]?.status, ConnectionProbeStatus.available);
    selector.dispose();
    await adopted?.close();
    await down.close();
  });
}

PaseoClient _client(
  Future<DaemonTransport> Function() open, {
  HostConfig host = const HostConfig(id: 'host', label: 'host', endpoint: 'ws://127.0.0.1:1/ws'),
  bool autoReconnect = false,
}) => PaseoClient(
  host: host,
  clientId: 'connection-regression-client',
  autoReconnect: autoReconnect,
  connector: (endpoint, {headers = const {}, protocols}) => open(),
);

/// The daemon endpoint replaced with controlled frames; production envelope
/// handling, correlation and timers stay in place.
class _Wire implements DaemonTransport {
  _Wire({this.rejection, this.answerPings = true, this._admit, this.reply});
  final String? rejection;
  final bool answerPings;
  final Future<void>? _admit;

  /// The response payload for a request; by default an empty project list.
  final JsonMap Function(JsonMap request)? reply;
  final requests = <JsonMap>[];
  final _messages = StreamController<Object>();
  int pings = 0;
  bool closed = false;

  @override
  Stream<Object> get messages => _messages.stream;

  void push(String frame) {
    if (!_messages.isClosed) _messages.add(frame);
  }

  void _emit(String type, JsonMap payload) => push(jsonEncode({
    'type': 'session',
    'message': {'type': type, 'payload': payload},
  }));

  @override
  void send(Object data) {
    final envelope = jsonMap(jsonDecode(data as String));
    switch (envelope['type']) {
      case 'hello':
        if (rejection != null) {
          push(jsonEncode({'type': 'hello.rejected', 'reason': rejection, 'accepts': ['password']}));
          return;
        }
        void admit() => _emit('status', {
          'status': 'server_info',
          'serverId': 'host',
          'features': {'ownedSubscriptions': true, 'workspaceMultiplicity': true},
        });
        final gate = _admit;
        if (gate == null) {
          admit();
        } else {
          unawaited(gate.then((_) => admit()));
        }
      case 'ping':
        pings++;
        if (answerPings) push(jsonEncode({'type': 'pong'}));
      case 'session':
        final request = jsonMap(envelope['message']);
        requests.add(request);
        final type = request['type'] as String;
        _emit('${type.substring(0, type.length - 'request'.length)}response', {
          'requestId': request['requestId'],
          ...reply?.call(request) ?? {'projects': <Object>[]},
        });
    }
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    closed = true;
    await _messages.close();
  }
}
