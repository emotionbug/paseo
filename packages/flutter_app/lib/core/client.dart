import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'host_config.dart';
import 'json.dart';
import 'transport/io_transport.dart';
import 'transport/relay_transport.dart';
import 'transport/binary_frames.dart';
import 'transport/legacy_workspaces.dart';

export 'transport/io_transport.dart'
    show DaemonTransport, connectDaemonTransport;
export 'transport/binary_frames.dart';

enum ConnectionStatus { disconnected, connecting, connected, error }

typedef DaemonConnector = Future<DaemonTransport> Function(
  Uri endpoint, {
  Map<String, String> headers,
  List<String>? protocols,
});
typedef CredentialProvider = Future<JsonMap?> Function(HostConfig host);

class DaemonException implements Exception {
  const DaemonException(
    this.message, {
    this.code,
    this.requestId,
    this.requestType,
    this.payload,
    this.outcomeUnknown = false,
  });
  final String message;
  final String? code, requestId, requestType;
  final JsonMap? payload;
  final bool outcomeUnknown;
  @override
  String toString() => message;
}

/// Original `hello.rejected`: the reason keeps the daemon's wording, and only
/// credential rejections stop automatic reconnects (`DaemonAuthenticationError`).
class HelloRejectedException extends DaemonException {
  HelloRejectedException(String reason, this.accepts)
    : super(
        switch (reason) {
          'password_required' => 'Password required',
          'incorrect_password' => 'Incorrect password',
          _ => 'Incompatible protocol version',
        },
        code: reason,
      );
  final List<String> accepts;
  bool get isAuthFailure =>
      code == 'password_required' || code == 'incorrect_password';
}

class PaseoClient extends ChangeNotifier {
  PaseoClient({
    required this.host,
    DaemonConnector? connector,
    this.credentials,
    this.clientId,
    this.sessionPermissions,
    this.capabilities = const {},
    bool autoReconnect = true,
  }) : _connector = connector ?? connectDaemonTransport,
       _reconnectEnabled = autoReconnect;
  final HostConfig host;
  final DaemonConnector _connector;
  final CredentialProvider? credentials;
  final String? clientId;
  final List<String>? sessionPermissions;
  final JsonMap capabilities;
  bool _reconnectEnabled;
  bool get autoReconnect => _reconnectEnabled;

  /// Original `setReconnectEnabled`: probe clients are created without
  /// reconnects and enable them when adopted as a host's active client.
  void setReconnectEnabled(bool enabled) {
    _reconnectEnabled = enabled;
    if (!enabled) {
      _reconnectTimer?.cancel();
      _reconnectTimer = null;
    }
  }
  ConnectionStatus _status = ConnectionStatus.disconnected;
  ConnectionStatus get status => _status;
  Object? _error;
  Object? get error => _error;
  JsonMap _serverInfo = {};
  JsonMap get serverInfo => _serverInfo;
  bool supports(String feature) =>
      (_serverInfo['features'] as Map?)?[feature] == true;
  final _events = StreamController<JsonMap>.broadcast(sync: true);
  final _binary = StreamController<Uint8List>.broadcast(sync: true);
  Stream<JsonMap> get events => _events.stream;
  Stream<Uint8List> get binaryEvents => _binary.stream;
  final Map<String, _Pending> _pending = {};
  final Set<OwnedObservation> _observations = {};
  LegacyWorkspaces? _legacyWorkspaces;
  final Map<String, OwnedObservation> _routes = {};
  DaemonTransport? _transport;
  StreamSubscription<Object>? _listener;
  Future<void>? _connecting;
  Completer<void>? _ready;
  Timer? _reconnectTimer, _liveness;
  _PingProbe? _ping;
  Duration? _lastLivenessRtt;
  int _generation = 0, _retry = 0, _livenessFailures = 0;
  DaemonTransport? _verification;

  // Original daemon-client.ts defaults.
  static const _reconnectBaseDelayMs = 1500;
  static const _reconnectMaxDelayMs = 30000;
  static const _livenessInterval = Duration(seconds: 10);
  static const _livenessTimeout = Duration(seconds: 15);
  static const _livenessFailureThreshold = 2;
  static const _sendQueueTimeout = Duration(seconds: 60);

  /// Original reconnect schedule: `min(1500ms * 2^attempt, 30s)`, no jitter.
  static Duration reconnectDelay(int attempt) => Duration(
    milliseconds: attempt >= 15
        ? _reconnectMaxDelayMs
        : min(_reconnectBaseDelayMs * (1 << attempt), _reconnectMaxDelayMs),
  );

  /// Failures reconnecting cannot fix (original `DaemonAuthenticationError`,
  /// `describeEndpointSecurityClose`, and identity/parent guards). Everything
  /// else, including an incompatible protocol and relay record failures, retries.
  static bool isTerminalFailure(Object? failure) =>
      authFailureReason(failure) != null ||
      (failure is RelaySecurityException && failure.closeCode == 4002) ||
      (failure is DaemonException &&
          const {
            'DAEMON_IDENTITY_MISMATCH',
            'RELAY_SECURITY_DOWNGRADE',
            'PARENT_CLIENT_CLOSED',
          }.contains(failure.code));

  /// Original `getDaemonAuthFailureReason`: `password_required` or
  /// `incorrect_password` from a hello rejection or the legacy 4401 close.
  static String? authFailureReason(Object? failure) {
    if (failure is HelloRejectedException) {
      return failure.isAuthFailure ? failure.code : null;
    }
    if (failure is DaemonTransportClosedException) {
      if (failure.reason == 'Password required') return 'password_required';
      if (failure.reason == 'Incorrect password' || failure.closeCode == 4401) {
        return 'incorrect_password';
      }
    }
    return null;
  }
  bool _closed = false, _disposed = false;
  String? _learnedServerId;
  static final _protectedIdentities = <String>{};
  static final _random = Random.secure();
  static String newRequestId() => base64Url
      .encode(List<int>.generate(18, (_) => _random.nextInt(256)))
      .replaceAll('=', '');
  Future<String> getClientId() async => clientId ?? await _persistentIdentity();
  static Future<String>? _identity;
  static Future<String> _persistentIdentity() => _identity ??= (() async {
    final prefs = await SharedPreferences.getInstance();
    final existing = prefs.getString('paseo.clientId');
    if (existing != null) return existing;
    final id = newRequestId();
    if (!await prefs.setString('paseo.clientId', id)) {
      throw StateError('Unable to persist client identity');
    }
    return id;
  })();

  Future<JsonMap?> _authentication(ResolvedHost target) async {
    if (credentials != null) return credentials!(host);
    String? token = host.localCredential;
    if (host.localCredentialPath != null &&
        !target.isRelay &&
        target.uri.scheme != 'ssh') {
      final file = File(host.localCredentialPath!);
      try {
        final lock = jsonDecode(
          await File('${file.parent.path}${Platform.pathSeparator}paseo.pid')
              .readAsString(),
        );
        if (lock is Map && lock['listen'] is String) {
          final listening = HostConfig(
            id: '',
            label: '',
            endpoint: lock['listen'] as String,
          ).resolve();
          String identity(Uri uri) =>
              uri.scheme == 'unix' || uri.scheme == 'pipe'
              ? '${uri.scheme}:${uri.path}'
              : '${const {'localhost', '127.0.0.1', '0.0.0.0', '::1', '::'}.contains(uri.host) ? 'localhost' : uri.host}:${uri.port}';
          if (identity(listening.uri) == identity(target.uri)) {
            final found = (await file.readAsString()).trim();
            if (RegExp(r'^[A-Za-z0-9_-]{43}$').hasMatch(found)) token = found;
          }
        }
      } on FileSystemException {
        /* A daemon restart can rotate/remove this file. */
      }
    }
    if (token != null && token.isNotEmpty) {
      return {'kind': 'localCredential', 'token': token};
    }
    final password = target.password;
    return password == null || password.isEmpty
        ? null
        : {'kind': 'password', 'password': password};
  }

  /// Opens a fresh attempt; native credentials enter only the application hello.
  Future<DaemonTransport> openSiblingTransport() async {
    final target = host.resolve();
    final auth = await _authentication(target);
    final headers = <String, String>{};
    final protocols = <String>[];
    if (!target.isRelay && auth?['kind'] == 'password') {
      final password = auth!['password'] as String;
      if (RegExp(r"^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$").hasMatch(password)) {
        headers['Authorization'] = 'Bearer $password';
        protocols.add('paseo.bearer.$password');
      }
    }
    var transport = await _connector(
      target.uri,
      headers: headers,
      protocols: protocols.isEmpty ? null : protocols,
    );
    if (target.isRelay) {
      try {
        final prefs = await SharedPreferences.getInstance();
        await prefs.reload();
        final key = 'paseo.relayProtected.${target.daemonPublicKey}';
        transport = await protectRelayTransport(
          transport,
          daemonPublicKey: target.daemonPublicKey!,
          requireProtected:
              target.requireProtected ||
              prefs.getBool(key) == true ||
              _protectedIdentities.contains(target.daemonPublicKey),
          onProtected: () {
            _protectedIdentities.add(target.daemonPublicKey!);
          },
        );
      } catch (_) {
        await transport.close();
        rethrow;
      }
    }
    return _AdmittedTransport(
      transport,
      auth,
      target,
      target.expectedServerId ?? _learnedServerId,
    );
  }

  Future<void> connect() {
    if (_disposed) return Future.error(StateError('Client disposed'));
    if (_status == ConnectionStatus.connected) return Future.value();
    if (_connecting != null) return _connecting!;
    _closed = false;
    _reconnectTimer?.cancel();
    final operation = _connect();
    _connecting = operation;
    operation.then(
      (_) {
        if (identical(_connecting, operation)) _connecting = null;
      },
      onError: (Object _, StackTrace _) {
        if (identical(_connecting, operation)) _connecting = null;
      },
    );
    return operation;
  }

  Future<void> _connect() async {
    final generation = ++_generation;
    _status = ConnectionStatus.connecting;
    _error = null;
    notifyListeners();
    final ready = Completer<void>();
    _ready = ready;
    // Install a handler immediately, including when the transport fails before hello.
    final deadline = ready.future.timeout(const Duration(seconds: 15));
    unawaited(deadline.catchError((Object _) {}));
    try {
      final identity = clientId ?? await _persistentIdentity();
      final opening = openSiblingTransport();
      opening.then((value) {
        if (generation != _generation) unawaited(value.close());
      }, onError: (Object _, StackTrace _) {});
      final transport = await opening.timeout(const Duration(seconds: 15));
      if (generation != _generation || _closed) {
        await transport.close();
        throw StateError('Connection cancelled');
      }
      _transport = transport;
      _listener = transport.messages.listen(
        (message) {
          if (generation == _generation) _receive(message);
        },
        onError: (Object e, StackTrace s) {
          if (generation == _generation) _lost(e);
        },
        onDone: () {
          if (generation == _generation) {
            _lost(
              const DaemonException(
                'Daemon connection closed',
                code: 'DAEMON_CONNECTION_LOST',
                outcomeUnknown: true,
              ),
            );
          }
        },
      );
      transport.send(
        jsonEncode({
          'type': 'hello',
          'clientId': identity,
          'clientType': 'mobile',
          'protocolVersion': 1,
          'capabilities': {
            'hello_rejection': true,
            'owned_subscriptions': true,
            'selective_agent_timeline': true,
            'explicit_event_subscriptions': true,
            'terminal_reflowable_snapshot': true,
            ...capabilities,
          },
          if (sessionPermissions != null)
            'sessionPermissions': sessionPermissions,
        }),
      );
      await deadline;
    } catch (e) {
      if (generation == _generation) _lost(e);
      rethrow;
    }
  }

  void _receive(Object data) {
    try {
      if (data is List<int>) {
        final frame = data is Uint8List ? data : Uint8List.fromList(data);
        // Original tryHandleBinaryFrame: a malformed host-route frame resets the
        // source (reconnect); every other frame proves liveness.
        if (frame.isNotEmpty &&
            frame[0] == hostRouteOpcode &&
            decodeHostRouteFrame(frame) == null) {
          _lost(
            const DaemonException(
              'Invalid host routing frame',
              code: 'DAEMON_CONNECTION_LOST',
              outcomeUnknown: true,
            ),
          );
          return;
        }
        _livenessFailures = 0;
        _binary.add(frame);
        return;
      }
      // Original handleJsonPayload: frames that are not JSON or fail envelope
      // validation are dropped; they never tear down the connection.
      if (data is! String) return;
      final Object? decoded;
      try {
        decoded = jsonDecode(data);
      } on FormatException {
        return;
      }
      if (decoded is! Map<String, dynamic> || decoded['type'] is! String) {
        return;
      }
      if (decoded['type'] == 'pong') {
        _livenessFailures = 0;
        _resolveLivenessProbe();
        return;
      }
      if (decoded['type'] == 'hello.rejected') {
        final reason = decoded['reason'];
        if (reason is! String ||
            !const {
              'password_required',
              'incorrect_password',
              'incompatible_protocol',
            }.contains(reason)) {
          return;
        }
        _livenessFailures = 0;
        _lost(
          HelloRejectedException(
            reason,
            (decoded['accepts'] as List? ?? []).whereType<String>().toList(),
          ),
        );
        return;
      }
      if (decoded['type'] != 'session' ||
          decoded['message'] is! Map<String, dynamic>) {
        return;
      }
      var message = decoded['message'] as JsonMap;
      if (message['type'] is! String) return;
      _livenessFailures = 0;
      message = _legacyWorkspaces?.normalize(message) ?? message;
      if (!supports('ownedSubscriptions') &&
          message['type'] == 'terminals_changed' &&
          message['payload'] is JsonMap &&
          (message['payload'] as JsonMap)['requestId'] == null) {
        final payload = message['payload'] as JsonMap;
        final candidates = _pending.values
            .where(
              (p) =>
                  p.type == 'subscribe_terminals_request' &&
                  p.fields['cwd'] == payload['cwd'],
            )
            .toList();
        for (final pending in candidates) {
          final correlated = {...payload, 'requestId': pending.id};
          pending.accept?.call(correlated);
          _finish(pending.id, value: correlated);
        }
      }
      final body = message.containsKey('payload')
          ? message['payload']
          : message;
      final id = body is Map ? body['requestId'] : message['requestId'];
      final pending = id is String ? _pending[id] : null;
      final messageType = message['type'] as String;
      if (pending != null &&
          (messageType == pending.responseType ||
              messageType == 'rpc_error' ||
              RegExp(r'(?:[._/]response)$').hasMatch(messageType))) {
        try {
          if (body is! Map<String, dynamic> ||
              (message.containsKey('payload') && body['requestId'] != id)) {
            throw const FormatException('Malformed correlated response');
          }
          if (message['type'] == 'rpc_error') {
            if (body['requestType'] != pending.type ||
                body['error'] is! String) {
              throw const FormatException('Malformed RPC error');
            }
            throw DaemonException(
              body['error'] as String,
              code: body['code'] as String?,
              payload: body,
              requestId: id,
              requestType: pending.type,
              outcomeUnknown: _serverOutcomeUnknown(body),
            );
          }
          if (message['type'] != pending.responseType) {
            throw FormatException(
              'Unexpected response ${message['type']} for ${pending.type}',
            );
          }
          _validateResponse(body, pending);
          final failure = _domainError(body, pending);
          if (failure != null) throw failure;
          pending.accept?.call(body);
          // Terminal consumers install stream-slot routing synchronously here,
          // before any following binary frame can arrive.
          _events.add(message);
          if (messageType == 'agent_permission_resolved') {
            final subscriptionId = body['subscriptionId'];
            if (subscriptionId is String) {
              _routes[subscriptionId]?._deliver(message);
            }
            if (!supports('ownedSubscriptions')) {
              for (final observation in _observations.toList()) {
                if (_legacyMatches(observation, message)) {
                  observation._deliver(message);
                }
              }
            }
          }
          _finish(id as String, value: body);
        } catch (e, s) {
          _events.add(message);
          _finish(id as String, failure: e, stack: s);
        }
        return;
      }
      if (message['type'] == 'status' &&
          body is JsonMap &&
          body['status'] == 'server_info' &&
          body['serverId'] is String &&
          (body['serverId'] as String).isNotEmpty) {
        _learnedServerId ??= body['serverId'] as String;
        _serverInfo = body;
        // Original deliverSessionMessage: only the server_info that answers
        // this connection's hello admits it. Later server_info broadcasts just
        // refresh the info and must not reset per-connection state.
        if (_status != ConnectionStatus.connected) {
          _legacyWorkspaces = supports('workspaceMultiplicity')
              ? null
              : LegacyWorkspaces();
          _status = ConnectionStatus.connected;
          _error = null;
          _retry = 0;
          _startLiveness();
          if (_ready?.isCompleted == false) _ready!.complete();
          notifyListeners();
          for (final observation in _observations.toList()) {
            unawaited(_startObservation(observation, restored: true));
          }
        } else {
          notifyListeners();
        }
      }
      _events.add(message);
      final subscriptionId = body is Map ? body['subscriptionId'] : null;
      if (subscriptionId is String) _routes[subscriptionId]?._deliver(message);
      if (!supports('ownedSubscriptions')) {
        for (final observation in _observations.toList()) {
          if (_legacyMatches(observation, message)) {
            observation._deliver(message);
          }
        }
      }
      for (final update in _legacyWorkspaces?.update(message) ?? <JsonMap>[]) {
        _events.add(update);
        for (final observation in _observations.toList()) {
          if (_legacyMatches(observation, update)) observation._deliver(update);
        }
      }
    } catch (e) {
      _lost(e);
    }
  }

  static void _validateResponse(JsonMap body, _Pending pending) {
    if (body['requestId'] is! String) {
      throw const FormatException('Response omitted requestId');
    }
    if (body.containsKey('error') &&
        body['error'] != null &&
        body['error'] is! String &&
        body['error'] is! Map) {
      throw const FormatException('Invalid response error');
    }
    for (final flag in ['accepted', 'success', 'ok']) {
      if (body.containsKey(flag) && body[flag] is! bool) {
        throw FormatException('Invalid response $flag');
      }
    }
    if (body['subscriptionId'] != null && body['subscriptionId'] is! String) {
      throw const FormatException('Invalid subscription identity');
    }
    for (final key in [
      'entries',
      'projects',
      'agents',
      'workspaces',
      'models',
      'terminals',
    ]) {
      if (body.containsKey(key) && body[key] != null && body[key] is! List) {
        throw FormatException('Invalid response $key');
      }
    }
    if (body['error'] != null) return;
    void require(String field, bool valid) {
      if (!body.containsKey(field) || !valid) {
        throw FormatException('Malformed ${pending.responseType}: $field');
      }
    }

    switch (pending.responseType) {
      case 'fetch_agents_response':
      case 'fetch_workspaces_response':
      case 'fetch_agent_history_response':
      case 'fetch_recent_provider_sessions_response':
        require('entries', body['entries'] is List);
        require('pageInfo', body['pageInfo'] is Map);
      case 'project.list.response':
        require('projects', body['projects'] is List);
      case 'fetch_agent_response':
        require('agent', body['agent'] == null || body['agent'] is Map);
      case 'fetch_agent_timeline_response':
        require('entries', body['entries'] is List);
        require('epoch', body['epoch'] is String);
        require('window', body['window'] is Map);
        for (final field in [
          'reset',
          'staleCursor',
          'gap',
          'hasOlder',
          'hasNewer',
        ]) {
          require(field, body[field] is bool);
        }
      case 'subscribe_terminal_response':
        require('terminalId', body['terminalId'] is String);
        require(
          'slot',
          body['slot'] is int &&
              (body['slot'] as int) >= 0 &&
              (body['slot'] as int) <= 255,
        );
      case 'send_agent_message_response':
        require('accepted', body['accepted'] is bool);
      case 'file.upload.response':
        require('file', body['file'] == null || body['file'] is Map);
      case 'hosts.route.open.response':
      case 'hosts.link.open.response':
        require(
          'routeId',
          body['routeId'] is String && (body['routeId'] as String).isNotEmpty,
        );
      case 'status':
        const statuses = {
          'create_agent_request': 'agent_created',
          'resume_agent_request': 'agent_resumed',
          'import_agent_request': 'agent_resumed',
          'refresh_agent_request': 'agent_refreshed',
          'restart_server_request': 'restart_requested',
          'shutdown_server_request': 'shutdown_requested',
        };
        if (statuses.containsKey(pending.type)) {
          require('status', body['status'] == statuses[pending.type]);
        }
    }
  }

  static bool _serverOutcomeUnknown(JsonMap body) {
    const codes = {
      'agent_request_outcome_unknown',
      'workspace_request_outcome_unknown',
    };
    bool reportsUnknown(Map value) =>
        value['outcomeUnknown'] == true ||
        codes.contains(value['errorCode']) ||
        codes.contains(value['code']) ||
        codes.contains(value['error']);
    if (reportsUnknown(body)) return true;
    // Creation failures carry the durable receipt inside the response. Keep
    // its uncertainty even when the outer response says accepted:false.
    for (final field in ['creation', 'snapshot', 'error', 'result']) {
      final value = body[field];
      if (value is Map && reportsUnknown(value)) return true;
    }
    return false;
  }

  static DaemonException? _domainError(JsonMap body, _Pending pending) {
    final result = body['result'];
    if (body['error'] != null ||
        body['accepted'] == false ||
        body['success'] == false ||
        body['ok'] == false ||
        (result is Map && {'conflict', 'error'}.contains(result['status']))) {
      final error = body['error'];
      return DaemonException(
        error is String
            ? error
            : (error is Map ? error['message']?.toString() : null) ??
                  (result is Map ? result['error']?.toString() : null) ??
                  'Daemon rejected ${pending.type}',
        code:
            body['errorCode']?.toString() ??
            body['code']?.toString() ??
            (result is Map ? result['status']?.toString() : null),
        payload: body,
        requestId: pending.id,
        requestType: pending.type,
        outcomeUnknown: _serverOutcomeUnknown(body),
      );
    }
    return null;
  }

  Future<JsonMap> request(
    String type,
    JsonMap fields, {
    String? responseType,
    Duration timeout = const Duration(seconds: 60),
  }) => _request(type, fields, responseType: responseType, timeout: timeout);

  Future<JsonMap> _request(
    String type,
    JsonMap fields, {
    String? responseType,
    Duration timeout = const Duration(seconds: 60),
    void Function(JsonMap)? accept,
  }) {
    // Original sendWhenConnected: an attempt in progress queues the request
    // until hello is admitted (60s cap); otherwise it fails at once.
    final ready = _ready;
    if (_status == ConnectionStatus.connecting && ready != null) {
      final waited = Stopwatch()..start();
      return ready.future
          .timeout(
            _sendQueueTimeout,
            onTimeout: () => throw DaemonException(
              'Timed out waiting for connection to send message',
              code: 'DAEMON_REQUEST_TIMEOUT',
              requestType: type,
            ),
          )
          .then(
            (_) {
              final remaining = timeout - waited.elapsed;
              if (timeout > Duration.zero && remaining <= Duration.zero) {
                throw DaemonException(
                  'Daemon request timed out',
                  code: 'DAEMON_REQUEST_TIMEOUT',
                  requestType: type,
                );
              }
              return _request(
                type,
                fields,
                responseType: responseType,
                timeout: timeout > Duration.zero ? remaining : timeout,
                accept: accept,
              );
            },
            onError: (Object error, StackTrace stack) =>
                Future<JsonMap>.error(
                  error is DaemonException
                      ? error
                      : DaemonException(
                          'Connection lost before message could be sent: $error',
                          code: 'DAEMON_CONNECTION_LOST',
                          requestType: type,
                        ),
                  stack,
                ),
          );
    }
    if (_status != ConnectionStatus.connected) {
      return Future.error(
        const DaemonException(
          'Daemon is not connected',
          code: 'DAEMON_NOT_CONNECTED',
        ),
      );
    }
    if (type == 'fetch_workspaces_request' && _legacyWorkspaces != null) {
      JsonMap convert(JsonMap body) => {
        ...body,
        'entries': _legacyWorkspaces!.read(
          body['entries'] as List,
          (fields['page'] as Map?)?['cursor'] == null,
        ),
        'emptyProjects': <Object>[],
      };
      return _request(
        'fetch_agents_request',
        {
          if (fields['requestId'] != null) 'requestId': fields['requestId'],
          'scope': 'active',
          'sort': [
            {'key': 'updated_at', 'direction': 'desc'},
          ],
          if (fields['page'] != null) 'page': fields['page'],
          if (fields['subscribe'] != null) 'subscribe': fields['subscribe'],
        },
        timeout: timeout,
        accept: accept == null ? null : (body) => accept(convert(body)),
      ).then(convert);
    }
    if (type == 'checkout.diff.get.request' &&
        !supports('ownedSubscriptions')) {
      final subscriptionId = 'legacy:${newRequestId()}';
      return _request(
        'subscribe_checkout_diff_request',
        {...fields, 'subscriptionId': subscriptionId},
        timeout: timeout,
        accept: accept,
      ).whenComplete(() {
        if (_status == ConnectionStatus.connected) {
          send('unsubscribe_checkout_diff_request', {
            'subscriptionId': subscriptionId,
          });
        }
      });
    }
    final id = fields['requestId'] ?? newRequestId();
    if (id is! String || id.isEmpty || _pending.containsKey(id)) {
      return Future.error(
        ArgumentError('Request identity must be unique and nonempty'),
      );
    }
    final expected = responseType ?? _responseType(type);
    final pending = _Pending(id, type, expected, accept, fields);
    _pending[id] = pending;
    if (timeout > Duration.zero) {
      pending.timer = Timer(
        timeout,
        () => _finish(
          id,
          failure: DaemonException(
            'Daemon request timed out',
            code: 'DAEMON_REQUEST_TIMEOUT',
            requestId: id,
            requestType: type,
            outcomeUnknown: true,
          ),
        ),
      );
    }
    try {
      send(type, {...fields, 'requestId': id});
    } catch (e, s) {
      _finish(id, failure: e, stack: s);
    }
    return pending.completer.future;
  }

  static String _responseType(String type) {
    const exceptions = {
      'subscribe_terminals_request': 'terminals_changed',
      'agent_permission_response': 'agent_permission_resolved',
      'delete_agent_request': 'agent_deleted',
      'archive_agent_request': 'agent_archived',
      'create_agent_request': 'status',
      'resume_agent_request': 'status',
      'import_agent_request': 'status',
      'refresh_agent_request': 'status',
      'restart_server_request': 'status',
      'shutdown_server_request': 'status',
      'set_voice_mode': 'set_voice_mode_response',
      'ping': 'pong',
    };
    final special = exceptions[type];
    if (special != null) return special;
    for (final separator in ['.', '_', '/']) {
      if (type.endsWith('${separator}request')) {
        return '${type.substring(0, type.length - 7)}response';
      }
    }
    throw ArgumentError('An explicit responseType is required for $type');
  }

  void send(String type, JsonMap fields) {
    if (_status != ConnectionStatus.connected || _transport == null) {
      throw const DaemonException(
        'Daemon is not connected',
        code: 'DAEMON_NOT_CONNECTED',
      );
    }
    _transport!.send(
      jsonEncode({
        'type': 'session',
        'message': {...fields, 'type': type},
      }),
    );
  }

  void sendBinary(Uint8List frame) {
    if (_status != ConnectionStatus.connected || _transport == null) {
      throw const DaemonException(
        'Daemon is not connected',
        code: 'DAEMON_NOT_CONNECTED',
      );
    }
    _transport!.send(frame);
  }

  void _finish(
    String id, {
    JsonMap? value,
    Object? failure,
    StackTrace? stack,
  }) {
    final pending = _pending.remove(id);
    if (pending == null) return;
    pending.timer?.cancel();
    if (failure != null) {
      pending.completer.completeError(failure, stack);
    } else {
      pending.completer.complete(value!);
    }
  }

  void _lost(Object failure, {bool permanent = false}) {
    if (_transport == null && _status != ConnectionStatus.connecting) return;
    _generation++;
    final transport = _transport;
    _transport = null;
    unawaited(_listener?.cancel());
    _listener = null;
    unawaited(transport?.close());
    _stopLiveness();
    for (final pending in _pending.values.toList()) {
      _finish(
        pending.id,
        failure: DaemonException(
          'Connection lost; request outcome is unknown',
          code: 'DAEMON_CONNECTION_LOST',
          requestId: pending.id,
          requestType: pending.type,
          outcomeUnknown: true,
        ),
      );
    }
    _routes.clear();
    for (final observation in _observations) {
      observation._subscriptionId = null;
      observation._starting = false;
      if (!observation._firstSnapshot.isCompleted) {
        observation._firstSnapshot.completeError(failure);
      }
    }
    if (_ready?.isCompleted == false) _ready!.completeError(failure);
    _status = _closed ? ConnectionStatus.disconnected : ConnectionStatus.error;
    _error = failure;
    if (!_disposed) notifyListeners();
    if (!_closed &&
        _reconnectEnabled &&
        !permanent &&
        !isTerminalFailure(failure)) {
      final delay = reconnectDelay(_retry++);
      _reconnectTimer?.cancel();
      _reconnectTimer = Timer(delay, () {
        _reconnectTimer = null;
        unawaited(connect().catchError((Object _) {}));
      });
    }
  }

  /// Original `ensureConnected`: foregrounding skips the reconnect backoff
  /// (including after a credential rejection) without closing a healthy
  /// socket; [verify] probes a connected session with a 3s ping because an OS
  /// suspension may have frozen the heartbeat.
  void ensureConnected({bool verify = false}) {
    if (_disposed || _closed) return;
    if (_status == ConnectionStatus.connected) {
      if (verify) _verifyConnection();
      return;
    }
    if (_status == ConnectionStatus.connecting) return;
    unawaited(connect().catchError((Object _) {}));
  }

  void _verifyConnection() {
    final transport = _transport;
    if (transport == null || identical(_verification, transport)) return;
    _verification = transport;
    unawaited(
      request('ping', {
        'clientSentAt': DateTime.now().millisecondsSinceEpoch,
      }, timeout: const Duration(seconds: 3)).then<void>(
        (_) {},
        onError: (Object error) {
          if (!identical(_transport, transport) ||
              _status != ConnectionStatus.connected) {
            return;
          }
          _lost(error);
          ensureConnected();
        },
      ).whenComplete(() {
        if (identical(_verification, transport)) _verification = null;
      }),
    );
  }

  void _startLiveness() {
    _stopLiveness();
    _livenessFailures = 0;
    _lastLivenessRtt = null;
    _scheduleLiveness();
  }

  void _stopLiveness([Object? failure]) {
    _liveness?.cancel();
    _liveness = null;
    final probe = _ping;
    _ping = null;
    if (probe != null) {
      probe.timer.cancel();
      probe.done.completeError(
        failure ??
            const DaemonException(
              'Transport not connected',
              code: 'DAEMON_CONNECTION_LOST',
            ),
      );
    }
  }

  /// Round trip of the last answered heartbeat (original `getLastLivenessRttMs`).
  Duration? get lastLivenessRtt => _lastLivenessRtt;

  /// Original `measureLatency`: an envelope ping round trip. Shares an
  /// in-flight heartbeat probe; a measurement timeout never tears down the socket.
  Future<Duration> measureLatency({
    Duration timeout = const Duration(seconds: 5),
  }) => _sendPing(timeout, drivesLiveness: false).catchError(
    (Object error) => throw error is TimeoutException
        ? DaemonException(
            'Latency measurement timed out (${timeout.inMilliseconds}ms)',
            code: 'DAEMON_REQUEST_TIMEOUT',
          )
        : error,
  );

  Future<Duration> _sendPing(Duration timeout, {required bool drivesLiveness}) {
    final transport = _transport;
    if (_status != ConnectionStatus.connected || transport == null) {
      return Future.error(
        const DaemonException(
          'Transport not connected',
          code: 'DAEMON_CONNECTION_LOST',
        ),
      );
    }
    final existing = _ping;
    if (existing != null) return existing.done.future;
    final generation = _generation;
    late final _PingProbe probe;
    probe = _PingProbe(
      Timer(timeout, () {
        if (!identical(_ping, probe)) return;
        _ping = null;
        probe.done.completeError(TimeoutException('Ping timed out', timeout));
        if (probe.drivesLiveness && generation == _generation) {
          _recordLivenessFailure(
            DaemonException(
              'Liveness check timed out (${timeout.inMilliseconds}ms)',
              code: 'DAEMON_CONNECTION_LOST',
              outcomeUnknown: true,
            ),
          );
        }
      }),
      drivesLiveness,
    );
    unawaited(probe.done.future.then<void>((_) {}, onError: (Object _) {}));
    _ping = probe;
    try {
      transport.send(jsonEncode({'type': 'ping'}));
    } catch (error) {
      _ping = null;
      probe.timer.cancel();
      probe.done.completeError(error);
      if (drivesLiveness) _recordLivenessFailure(error);
    }
    return probe.done.future;
  }

  /// Original liveness heartbeat: one envelope ping 10s after the previous
  /// probe settles, each waiting 15s for its pong.
  void _scheduleLiveness() {
    if (_status != ConnectionStatus.connected || _liveness != null) return;
    final generation = _generation;
    _liveness = Timer(_livenessInterval, () {
      _liveness = null;
      if (generation != _generation) return;
      _sendPing(_livenessTimeout, drivesLiveness: true)
          .then<void>(
            (rtt) => _lastLivenessRtt = rtt,
            onError: (Object _) {},
          )
          .whenComplete(() {
            if (generation == _generation) _scheduleLiveness();
          });
    });
  }

  void _resolveLivenessProbe() {
    final probe = _ping;
    if (probe == null) return;
    _ping = null;
    probe.timer.cancel();
    probe.done.complete(probe.watch.elapsed);
  }

  /// Only consecutive unanswered probes, with no other inbound traffic in
  /// between, tear the connection down.
  void _recordLivenessFailure(Object error) {
    if (++_livenessFailures < _livenessFailureThreshold) return;
    _livenessFailures = 0;
    _lost(
      error is DaemonException
          ? error
          : DaemonException(
              '$error',
              code: 'DAEMON_CONNECTION_LOST',
              outcomeUnknown: true,
            ),
    );
  }

  Future<OwnedObservation> observe({
    required String type,
    required JsonMap fields,
    required String responseType,
    required void Function(JsonMap) onEvent,
  }) async {
    final observation = OwnedObservation._(
      this,
      type,
      Map<String, dynamic>.from(fields),
      responseType,
      onEvent,
    );
    _observations.add(observation);
    try {
      if (_status != ConnectionStatus.connected) await connect();
      await _startObservation(observation);
      await observation._firstSnapshot.future;
      return observation;
    } catch (_) {
      _observations.remove(observation);
      rethrow;
    }
  }

  Future<void> _startObservation(
    OwnedObservation observation, {
    bool restored = false,
  }) async {
    if (observation._closed ||
        observation._starting ||
        observation.subscriptionId != null ||
        _status != ConnectionStatus.connected) {
      return;
    }
    observation._starting = true;
    final generation = _generation;
    final legacy = !supports('ownedSubscriptions');
    final localId = 'legacy:${newRequestId()}';
    final fields = Map<String, dynamic>.from(observation.fields);
    if (const {
      'fetch_agents_request',
      'fetch_workspaces_request',
      'workspace.label.list.request',
    }.contains(observation.type)) {
      fields.putIfAbsent('subscribe', () => <String, dynamic>{});
    }
    if (legacy) {
      switch (observation.type) {
        case 'fetch_agents_request':
        case 'fetch_workspaces_request':
        case 'workspace.label.list.request':
          fields['subscribe'] = {'subscriptionId': localId};
        case 'fs.file.subscribe.request':
        case 'subscribe_checkout_diff_request':
          fields['subscriptionId'] = localId;
        case 'agent.timeline.set_subscription.request':
          fields['agentIds'] = _legacyMembers(observation.type, 'agentIds');
        case 'session.events.set_subscription.request':
          fields['events'] = _legacyMembers(observation.type, 'events')
              .where(
                const {
                  'project.update',
                  'providers_snapshot_update',
                  'agent_attention_required',
                  'agent_permission_request',
                  'agent_permission_resolved',
                }.contains,
              )
              .toList();
      }
    }
    void accept(JsonMap body) {
      if (generation != _generation) return;
      final id = legacy ? localId : body['subscriptionId'];
      if (id is! String || id.isEmpty) {
        throw const FormatException('Host omitted subscription identity');
      }
      observation._subscriptionId = id;
      observation._snapshot = {...body, 'subscriptionId': id};
      if (!observation._closed) _routes[id] = observation;
      if (restored && observation._hasSnapshot) {
        observation._deliver({
          'type': 'subscription.restored',
          'payload': observation._snapshot,
        });
      }
      observation._hasSnapshot = true;
      if (!observation._firstSnapshot.isCompleted) {
        observation._firstSnapshot.complete();
      }
    }

    try {
      final broadcast =
          legacy &&
          ((observation.type == 'agent.timeline.set_subscription.request' &&
                  !supports('selectiveAgentTimeline')) ||
              (observation.type == 'session.events.set_subscription.request' &&
                  !supports('explicitEventSubscriptions')));
      if (broadcast) {
        accept({'requestId': localId, ...fields});
      } else {
        await _request(
          observation.type,
          fields,
          responseType: observation.responseType,
          accept: accept,
        );
      }
      if (observation._closed && generation == _generation) {
        await _releaseObservation(observation);
      }
    } catch (e) {
      if (generation == _generation) {
        if (!observation._firstSnapshot.isCompleted) {
          observation._firstSnapshot.completeError(e);
        }
        if (restored) {
          observation._deliver({
            'type': 'subscription.error',
            'payload': {'error': e.toString()},
          });
        } else {
          rethrow;
        }
      }
    } finally {
      if (generation == _generation) observation._starting = false;
    }
  }

  List<String> _legacyMembers(String type, String key) =>
      _observations
          .where((o) => !o._closed && o.type == type)
          .expand((o) => (o.fields[key] as List? ?? []).whereType<String>())
          .toSet()
          .toList()
        ..sort();
  bool _legacyMatches(OwnedObservation o, JsonMap message) {
    if (o._closed || o.subscriptionId == null) return false;
    final payload = message['payload'] is Map
        ? message['payload'] as Map
        : message;
    final type = message['type'];
    switch (o.type) {
      case 'fetch_agents_request':
        return type == 'agent_update';
      case 'fetch_workspaces_request':
        return type == 'workspace_update';
      case 'workspace.label.list.request':
        return type == 'workspace.label.update';
      case 'subscribe_terminals_request':
        return type == 'terminals_changed' && payload['cwd'] == o.fields['cwd'];
      case 'subscribe_terminal_request':
        return type == 'terminal_stream_exit' &&
            payload['terminalId'] == o.fields['terminalId'];
      case 'agent.timeline.set_subscription.request':
        return (type == 'agent_stream' ||
                type == 'agent.timeline.replacement') &&
            (o.fields['agentIds'] as List).contains(payload['agentId']);
      case 'session.events.set_subscription.request':
        return (o.fields['events'] as List).contains(
          type == 'status' ? 'status.${payload['status']}' : type,
        );
      case 'browser.host.register.request':
        return type == 'browser.automation.execute.request';
      default:
        return false;
    }
  }

  Future<void> _releaseObservation(OwnedObservation observation) async {
    final id = observation._subscriptionId;
    observation._subscriptionId = null;
    if (id == null) return;
    _routes.remove(id);
    if (_status != ConnectionStatus.connected) return;
    if (supports('ownedSubscriptions')) {
      try {
        await request('subscription.release.request', {'subscriptionId': id});
      } catch (e) {
        _lost(e);
        rethrow;
      }
    } else {
      switch (observation.type) {
        case 'fs.file.subscribe.request':
          await request('fs.file.unsubscribe.request', {'subscriptionId': id});
        case 'subscribe_checkout_diff_request':
          send('unsubscribe_checkout_diff_request', {'subscriptionId': id});
        case 'agent.timeline.set_subscription.request':
          if (supports('selectiveAgentTimeline')) {
            await request(observation.type, {
              'agentIds': _legacyMembers(observation.type, 'agentIds'),
            });
          }
        case 'session.events.set_subscription.request':
          if (supports('explicitEventSubscriptions')) {
            await request(observation.type, {
              'events': _legacyMembers(observation.type, 'events')
                  .where(
                    const {
                      'project.update',
                      'providers_snapshot_update',
                      'agent_attention_required',
                      'agent_permission_request',
                      'agent_permission_resolved',
                    }.contains,
                  )
                  .toList(),
            });
          }
        case 'subscribe_terminal_request':
          if (!_observations.any(
            (o) =>
                o.type == observation.type &&
                o.fields['terminalId'] == observation.fields['terminalId'],
          )) {
            send('unsubscribe_terminal_request', {
              'terminalId': observation.fields['terminalId'],
            });
          }
        case 'subscribe_terminals_request':
          if (!_observations.any(
            (o) =>
                o.type == observation.type &&
                o.fields['cwd'] == observation.fields['cwd'] &&
                o.fields['workspaceId'] == observation.fields['workspaceId'],
          )) {
            send('unsubscribe_terminals_request', observation.fields);
          }
      }
    }
  }

  Future<JsonMap> recoverCreation(String kind, String idempotencyKey) =>
      request('creation.subscribe.request', {
        'kind': kind,
        'idempotencyKey': idempotencyKey,
        'subscribe': false,
      });

  Future<PaseoClient> connectHost(String serverId, {String? label}) async {
    final child = PaseoClient(
      host: HostConfig(
        id: '${host.id}/$serverId',
        label: label ?? serverId,
        endpoint: 'ws://routed.invalid/ws',
        expectedServerId: serverId,
      ),
      connector: (uri, {headers = const {}, protocols}) =>
          _HostRouteTransport.open(this, serverId),
      clientId: clientId,
      capabilities: capabilities,
      sessionPermissions: sessionPermissions,
    );
    await child.connect();
    return child;
  }

  Future<void> close() async {
    _closed = true;
    _reconnectTimer?.cancel();
    _stopLiveness();
    final observations = _observations.toList();
    Object? releaseFailure;
    for (final observation in observations) {
      try {
        await observation.close();
      } catch (e) {
        releaseFailure ??= e;
      }
    }
    _lost(
      const DaemonException(
        'Client closed',
        code: 'DAEMON_CONNECTION_LOST',
        outcomeUnknown: true,
      ),
      permanent: true,
    );
    _status = ConnectionStatus.disconnected;
    _error = null;
    if (!_disposed) notifyListeners();
    if (releaseFailure != null) throw releaseFailure;
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    unawaited(
      close().whenComplete(() async {
        await _events.close();
        await _binary.close();
      }),
    );
    super.dispose();
  }
}

class OwnedObservation {
  OwnedObservation._(
    this._client,
    this.type,
    this.fields,
    this.responseType,
    this._onEvent,
  ) {
    unawaited(_firstSnapshot.future.catchError((Object _) {}));
  }
  final PaseoClient _client;
  final String type, responseType;
  final JsonMap fields;
  final void Function(JsonMap) _onEvent;
  final _firstSnapshot = Completer<void>();
  JsonMap _snapshot = {};
  JsonMap get snapshot => _snapshot;
  String? _subscriptionId;
  String? get subscriptionId => _subscriptionId;
  bool _closed = false, _starting = false, _hasSnapshot = false;
  Future<void>? _closing;
  void _deliver(JsonMap event) {
    if (!_closed) {
      try {
        _onEvent(event);
      } catch (e, s) {
        Zone.current.handleUncaughtError(e, s);
      }
    }
  }

  Future<void> close() => _closing ??= (() async {
    _closed = true;
    if (!_firstSnapshot.isCompleted) {
      _firstSnapshot.completeError(StateError('Subscription released'));
    }
    _client._observations.remove(this);
    await _client._releaseObservation(this);
  })();
}

/// One in-flight envelope ping (original `PingProbe`). Only a heartbeat probe
/// records a liveness failure when it times out.
class _PingProbe {
  _PingProbe(this.timer, this.drivesLiveness);
  final Timer timer;
  final bool drivesLiveness;
  final watch = Stopwatch()..start();
  final done = Completer<Duration>();
}

class _Pending {
  _Pending(this.id, this.type, this.responseType, this.accept, this.fields);
  final String id, type, responseType;
  final void Function(JsonMap)? accept;
  final JsonMap fields;
  final completer = Completer<JsonMap>();
  Timer? timer;
}

class _AdmittedTransport implements DaemonTransport {
  _AdmittedTransport(this.inner, this.auth, this.target, this.expectedServerId);
  final DaemonTransport inner;
  final JsonMap? auth;
  final ResolvedHost target;
  final String? expectedServerId;
  // Original createEncryptedTransport `awaitingServerInfo`: only the first
  // server_info is checked; invalid frames pass through to be dropped.
  bool _awaitingServerInfo = true;
  @override
  Stream<Object> get messages => inner.messages.asyncMap((data) async {
    if (_awaitingServerInfo && data is String) {
      Object? root;
      try {
        root = jsonDecode(data);
      } on FormatException {
        return data;
      }
      if (root is Map && root['type'] == 'session') {
        final message = root['message'];
        final payload = message is Map ? message['payload'] : null;
        if (message is Map &&
            message['type'] == 'status' &&
            payload is Map &&
            payload['status'] == 'server_info') {
          _awaitingServerInfo = false;
          if (expectedServerId != null &&
              payload['serverId'] != expectedServerId) {
            await inner.close(4002, 'Daemon identity mismatch');
            throw const DaemonException(
              'Connected daemon identity does not match the selected host',
              code: 'DAEMON_IDENTITY_MISMATCH',
            );
          }
          if (inner is RelayTransport) {
            final relay = inner as RelayTransport;
            final prefs = await SharedPreferences.getInstance();
            await prefs.reload();
            if (!relay.protectedMode &&
                ((payload['features'] as Map?)?['relayProtectedSecurity'] ==
                        true ||
                    prefs.getBool(
                          'paseo.relayProtected.${target.daemonPublicKey}',
                        ) ==
                        true ||
                    PaseoClient._protectedIdentities.contains(
                      target.daemonPublicKey,
                    ))) {
              await inner.close(4002, 'Relay security downgrade');
              throw const DaemonException(
                'Relay protection downgrade detected',
                code: 'RELAY_SECURITY_DOWNGRADE',
              );
            }
            if (relay.protectedMode) {
              if (!await prefs.setBool(
                'paseo.relayProtected.${target.daemonPublicKey}',
                true,
              )) {
                await inner.close(
                  4002,
                  'Unable to persist relay security policy',
                );
                throw StateError('Unable to persist relay security policy');
              }
            }
          }
        }
      }
    }
    return data;
  });
  @override
  void send(Object data) {
    if (data is String) {
      final root = jsonDecode(data);
      if (root is Map<String, dynamic> && root['type'] == 'hello') {
        root.remove('auth');
        if (auth != null) root['auth'] = auth;
        data = jsonEncode(root);
      }
    }
    inner.send(data);
  }

  @override
  Future<void> close([int? code, String? reason]) => inner.close(code, reason);
}

class _HostRouteTransport implements DaemonTransport {
  _HostRouteTransport(this.parent, this.serverId);
  final PaseoClient parent;
  final String serverId;
  final scopeId = PaseoClient.newRequestId();
  final _messages = StreamController<Object>(sync: true);
  StreamSubscription<JsonMap>? _events;
  StreamSubscription<Uint8List>? _binary;
  String? _routeId;
  bool _closed = false;
  static Future<_HostRouteTransport> open(
    PaseoClient parent,
    String serverId,
  ) async {
    if (parent._closed || parent._disposed) {
      throw const DaemonException(
        'Parent client closed',
        code: 'PARENT_CLIENT_CLOSED',
      );
    }
    final failure = parent.error;
    if (PaseoClient.isTerminalFailure(failure)) {
      throw failure!;
    }
    if (parent.status != ConnectionStatus.connected) await parent.connect();
    final route = _HostRouteTransport(parent, serverId);
    parent.addListener(route._parentChanged);
    route._events = parent.events.listen(route._receive);
    route._binary = parent.binaryEvents.listen(route._receiveBinary);
    try {
      await parent._request(
        'hosts.route.open.request',
        {'serverId': serverId, 'scopeId': route.scopeId},
        accept: (payload) {
          final id = payload['routeId'];
          if (id is! String || id.isEmpty) {
            throw const FormatException('Host omitted route identity');
          }
          route._routeId = id;
        },
      );
      return route;
    } catch (_) {
      await route.close();
      rethrow;
    }
  }

  void _parentChanged() {
    if (parent.status != ConnectionStatus.connected && !_closed) {
      _messages.addError(
        const DaemonException(
          'Parent host disconnected',
          code: 'DAEMON_CONNECTION_LOST',
          outcomeUnknown: true,
        ),
      );
      unawaited(close());
    }
  }

  void _receive(JsonMap message) {
    final payload = message['payload'];
    if (payload is! Map || payload['subscriptionId'] != _routeId) return;
    if (message['type'] == 'hosts.route.closed') {
      _messages.addError(
        DaemonException(
          payload['message']?.toString() ?? 'Host route closed',
          code: payload['code']?.toString(),
        ),
      );
      unawaited(close());
    } else if (message['type'] == 'hosts.route.frame') {
      final data = payload['data'];
      if (data is! String || payload['binary'] is! bool) {
        _messages.addError(const FormatException('Invalid host route frame'));
        unawaited(close());
        return;
      }
      _messages.add(payload['binary'] == true ? base64.decode(data) : data);
    }
  }

  void _receiveBinary(Uint8List data) {
    if (data.isEmpty || data[0] != 0x30) return;
    try {
      final frame = decodeHostRouteFrame(data);
      if (frame != null && frame.routeId == _routeId) {
        _messages.add(frame.payload);
      }
    } catch (e) {
      _messages.addError(e);
      unawaited(close());
    }
  }

  @override
  Stream<Object> get messages => _messages.stream;
  @override
  void send(Object data) {
    if (_closed || _routeId == null) throw StateError('Host route closed');
    if (data is String) {
      parent.send('hosts.route.frame.request', {
        'routeId': _routeId,
        'data': data,
        'binary': false,
      });
    } else if (data is Uint8List) {
      parent.sendBinary(encodeHostRouteFrame(_routeId!, data));
    } else {
      throw ArgumentError('Unsupported route frame');
    }
  }

  @override
  Future<void> close([int? code, String? reason]) async {
    if (_closed) return;
    _closed = true;
    parent.removeListener(_parentChanged);
    await _events?.cancel();
    await _binary?.cancel();
    if (parent.status == ConnectionStatus.connected) {
      parent.send('hosts.route.close.request', {
        if (_routeId != null) 'routeId': _routeId else 'scopeId': scopeId,
      });
    }
    unawaited(_messages.close());
  }
}
