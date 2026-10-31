import 'dart:async';

import 'package:flutter/foundation.dart';

import '../client.dart';
import '../host_config.dart';

/// Port of the original `HostRuntimeController` probe cycle
/// (app/src/runtime/host-runtime.ts) and `selectBestConnection`
/// (app/src/utils/connection-selection.ts).
///
/// Every saved connection of one host is probed. With nothing active, the
/// first reachable connection is adopted with its probe client. An unreachable
/// (or credential-rejected) active connection is replaced by the fastest
/// reachable one. A healthy active connection only yields to one that is at
/// least 40ms faster for three consecutive cycles.

enum ConnectionProbeStatus { pending, available, unavailable }

@immutable
class ConnectionProbe {
  const ConnectionProbe(this.status, [this.latency]);
  final ConnectionProbeStatus status;
  final Duration? latency;

  @override
  bool operator ==(Object other) =>
      other is ConnectionProbe &&
      other.status == status &&
      other.latency == latency;

  @override
  int get hashCode => Object.hash(status, latency);
}

const probeTick = Duration(seconds: 2);
const _probeSteady = Duration(seconds: 10);
const _probeMaxBackoff = Duration(seconds: 30);
const _probeInactiveWhileOnline = Duration(seconds: 120);
const _adaptiveSwitchThreshold = Duration(milliseconds: 40);
const _adaptiveSwitchConsecutiveProbes = 3;
const _probeLatencyTimeout = Duration(seconds: 5);

/// Original `probeIntervalForConnection`.
Duration probeIntervalForConnection({
  required Duration age,
  required bool isActiveOnline,
  required bool hasActiveOnlineConnection,
}) {
  if (isActiveOnline) return _probeSteady;
  if (hasActiveOnlineConnection) return _probeInactiveWhileOnline;
  if (age < const Duration(seconds: 10)) return const Duration(seconds: 2);
  if (age < const Duration(seconds: 30)) return const Duration(seconds: 5);
  if (age < const Duration(seconds: 60)) return _probeSteady;
  return _probeMaxBackoff;
}

/// Original `selectBestConnection`: the lowest available latency; ties keep
/// the earlier saved connection.
String? selectBestConnection(
  Iterable<String> candidateIds,
  Map<String, ConnectionProbe> probes,
) {
  String? best;
  Duration? bestLatency;
  for (final id in candidateIds) {
    final probe = probes[id];
    final latency = probe?.status == ConnectionProbeStatus.available
        ? probe!.latency
        : null;
    if (latency == null) continue;
    if (bestLatency == null || latency < bestLatency) {
      best = id;
      bestLatency = latency;
    }
  }
  return best;
}

/// Original `resolveTimeout` for a probe connection attempt.
Duration probeConnectTimeout(HostConfig connection) {
  final ResolvedHost target;
  try {
    target = connection.resolve();
  } on FormatException {
    return const Duration(seconds: 6);
  }
  if (target.isRelay) return const Duration(seconds: 10);
  if (target.uri.scheme == 'ssh') return const Duration(seconds: 15);
  return const Duration(seconds: 6);
}

/// Creates an unconnected client for [connection] with reconnects disabled.
typedef ProbeClientFactory = PaseoClient Function(HostConfig connection);

/// Makes [connection] the host's active connection. [existingClient] is an
/// already-admitted probe client to adopt (reconnects already re-enabled);
/// otherwise the activator creates and connects its own client.
typedef ConnectionActivator =
    Future<void> Function(HostConfig connection, PaseoClient? existingClient);

class HostConnectionSelector extends ChangeNotifier {
  HostConnectionSelector({
    required this._connections,
    required this._activeConnectionId,
    required this._activeClient,
    required this._expectedServerId,
    required this._createProbeClient,
    required this._activate,
    this._onAuthFailure,
    Stopwatch? clock,
  }) : _clock = clock ?? (Stopwatch()..start());

  final List<HostConfig> Function() _connections;
  final String? Function() _activeConnectionId;
  final PaseoClient? Function() _activeClient;
  final String? Function() _expectedServerId;
  final ProbeClientFactory _createProbeClient;
  final ConnectionActivator _activate;
  final void Function(String reason)? _onAuthFailure;
  final Stopwatch _clock;

  final _firstSeenAt = <String, Duration>{};
  final _lastProbedAt = <String, Duration>{};
  final _authRejected = <String>{};
  Map<String, ConnectionProbe> _probes = const {};
  String? _switchCandidate;
  int _switchCandidateHits = 0;
  int _probeVersion = 0;
  int _switchVersion = 0;
  Future<void>? _cycle;
  Timer? _ticker;
  bool _started = false;
  bool _disposed = false;

  /// Latest probe result per connection id (original `probeByConnectionId`).
  Map<String, ConnectionProbe> get probes => _probes;

  Future<void> start() async {
    if (_started) return;
    _started = true;
    _trackFirstSeen();
    await runProbeCycleNow();
    if (!_started) return;
    _ticker = Timer.periodic(probeTick, (_) {
      unawaited(runProbeCycleNow());
    });
  }

  void stop() {
    _switchVersion++;
    _probeVersion++;
    _started = false;
    _ticker?.cancel();
    _ticker = null;
  }

  /// Original `updateHost`: connection edits restart first-seen tracking; a
  /// changed password forgets credential rejections and reprobes everything.
  Future<void> connectionsChanged({bool credentialsChanged = false}) async {
    if (credentialsChanged) {
      _probeVersion++;
      _authRejected.clear();
      _lastProbedAt.clear();
    }
    _trackFirstSeen();
    if (credentialsChanged) await _cycle;
    await runProbeCycleNow();
  }

  /// Original `failConnection` for the active client: a credential rejection
  /// excludes that connection and probes the remaining ones right away.
  void reportActiveFailure(String connectionId, Object error) {
    if (PaseoClient.authFailureReason(error) == null) return;
    _authRejected.add(connectionId);
    _setProbe(
      connectionId,
      const ConnectionProbe(ConnectionProbeStatus.unavailable),
    );
    for (final connection in _connections()) {
      if (!_authRejected.contains(connection.id)) {
        _lastProbedAt.remove(connection.id);
      }
    }
    if (_connections().any((c) => !_authRejected.contains(c.id))) {
      unawaited(runProbeCycleNow());
    }
  }

  Future<void> runProbeCycleNow() {
    final inFlight = _cycle;
    if (inFlight != null) return inFlight;
    late final Future<void> cycle;
    cycle = _runProbeCycle().whenComplete(() {
      if (identical(_cycle, cycle)) _cycle = null;
    });
    _cycle = cycle;
    return cycle;
  }

  void _trackFirstSeen() {
    final now = _clock.elapsed;
    final current = _connections().map((c) => c.id).toSet();
    for (final id in _firstSeenAt.keys.toList()) {
      if (!current.contains(id)) {
        _firstSeenAt.remove(id);
        _lastProbedAt.remove(id);
      }
    }
    for (final id in current) {
      _firstSeenAt.putIfAbsent(id, () => now);
    }
  }

  void _setProbe(String id, ConnectionProbe probe) {
    if (_probes[id] == probe) return;
    _probes = {..._probes, id: probe};
    if (!_disposed) notifyListeners();
  }

  void _publish(Map<String, ConnectionProbe> probes) {
    if (mapEquals(_probes, probes)) return;
    _probes = Map.unmodifiable(probes);
    if (!_disposed) notifyListeners();
  }

  Future<void> _runProbeCycle() async {
    final version = ++_probeVersion;
    final connections = _connections();
    if (connections.isEmpty) {
      _publish(const {});
      return;
    }
    final now = _clock.elapsed;
    final activeId = _activeConnectionId();
    final isOnline = _activeClient()?.status == ConnectionStatus.connected;
    final hasActiveOnline = isOnline && activeId != null;
    final toProbe = connections.where((connection) {
      if (_authRejected.contains(connection.id)) return false;
      final last = _lastProbedAt[connection.id];
      if (last == null) return true;
      final interval = probeIntervalForConnection(
        age: now - (_firstSeenAt[connection.id] ?? now),
        isActiveOnline: isOnline && connection.id == activeId,
        hasActiveOnlineConnection: hasActiveOnline,
      );
      return now - last >= interval;
    }).toList();
    if (toProbe.isEmpty) return;

    final probes = Map<String, ConnectionProbe>.of(_probes);
    for (final connection in toProbe) {
      _lastProbedAt[connection.id] = _clock.elapsed;
      final preserveActiveLatency =
          isOnline &&
          connection.id == activeId &&
          probes[connection.id]?.status == ConnectionProbeStatus.available;
      if (!preserveActiveLatency) {
        probes[connection.id] = const ConnectionProbe(
          ConnectionProbeStatus.pending,
        );
      }
    }
    _publish(probes);

    String? authFailure;
    Future<void>? activationLock;
    bool current() => version == _probeVersion;
    void publish() {
      if (current()) _publish(probes);
    }

    Future<bool> activateFirstAvailable(
      String connectionId,
      PaseoClient client,
    ) async {
      while (_activeConnectionId() == null) {
        final lock = activationLock;
        if (lock == null) {
          final activation = _switchTo(
            connectionId,
            probeVersion: version,
            existingClient: client,
          );
          activationLock = activation;
          try {
            await activation;
          } finally {
            activationLock = null;
          }
          return _activeConnectionId() == connectionId;
        }
        await lock;
      }
      return false;
    }

    Future<void> probe(HostConfig connection) async {
      PaseoClient? client;
      var closeClient = false;
      try {
        final active = _activeConnectionId() == connection.id
            ? _activeClient()
            : null;
        if (active != null) {
          client = active;
        } else {
          client = await _connectProbe(connection);
          closeClient = true;
        }
        if (!current()) return;
        final activated = await activateFirstAvailable(connection.id, client);
        closeClient = closeClient && !activated;
        if (active != null) {
          if (active.status != ConnectionStatus.connected) {
            probes[connection.id] = const ConnectionProbe(
              ConnectionProbeStatus.unavailable,
            );
            publish();
            return;
          }
          final rtt = active.lastLivenessRtt;
          if (!current()) return;
          if (rtt != null) {
            probes[connection.id] = ConnectionProbe(
              ConnectionProbeStatus.available,
              rtt,
            );
            publish();
          }
          return;
        }
        final rtt = await client.measureLatency(timeout: _probeLatencyTimeout);
        if (!current()) return;
        probes[connection.id] = ConnectionProbe(
          ConnectionProbeStatus.available,
          rtt,
        );
        publish();
      } catch (error) {
        if (current()) {
          final reason = PaseoClient.authFailureReason(error);
          if (reason != null) {
            _authRejected.add(connection.id);
            authFailure = reason;
          }
          probes[connection.id] = const ConnectionProbe(
            ConnectionProbeStatus.unavailable,
          );
          publish();
        }
      } finally {
        if (client != null && closeClient) client.dispose();
      }
    }

    await Future.wait(toProbe.map(probe));
    if (!current()) return;
    await _finalize(version, probes, authFailure);
  }

  Future<void> _finalize(
    int version,
    Map<String, ConnectionProbe> probes,
    String? authFailure,
  ) async {
    final connections = _connections();
    final activeId = _activeConnectionId();
    final activeProbe = activeId == null ? null : probes[activeId];
    final candidates = connections.map((c) => c.id);
    if (activeId == null || !connections.any((c) => c.id == activeId)) {
      final next = selectBestConnection(candidates, probes);
      if (next != null) {
        await _switchTo(next, probeVersion: version);
      } else if (authFailure != null) {
        _onAuthFailure?.call(authFailure);
      }
      return;
    }
    if (activeProbe?.status == ConnectionProbeStatus.unavailable ||
        _authRejected.contains(activeId)) {
      final next = selectBestConnection(candidates, probes);
      if (next != null && next != activeId) {
        await _switchTo(next, probeVersion: version);
      }
      _switchCandidate = null;
      _switchCandidateHits = 0;
      return;
    }
    if (activeProbe?.status != ConnectionProbeStatus.available) return;
    final available =
        probes.entries
            .where((e) => e.value.status == ConnectionProbeStatus.available)
            .toList()
          ..sort((a, b) => a.value.latency!.compareTo(b.value.latency!));
    final fastest = available.firstOrNull;
    if (fastest == null || fastest.key == activeId) {
      _switchCandidate = null;
      _switchCandidateHits = 0;
      return;
    }
    final improvement = activeProbe!.latency! - fastest.value.latency!;
    if (improvement < _adaptiveSwitchThreshold) {
      _switchCandidate = null;
      _switchCandidateHits = 0;
      return;
    }
    if (_switchCandidate == fastest.key) {
      _switchCandidateHits++;
    } else {
      _switchCandidate = fastest.key;
      _switchCandidateHits = 1;
    }
    if (_switchCandidateHits >= _adaptiveSwitchConsecutiveProbes) {
      _switchCandidate = null;
      _switchCandidateHits = 0;
      await _switchTo(fastest.key, probeVersion: version);
    }
  }

  Future<PaseoClient> _connectProbe(HostConfig connection) async {
    final client = _createProbeClient(connection);
    try {
      await client.connect().timeout(
        probeConnectTimeout(connection),
        onTimeout: () => throw const DaemonException(
          'Connection timed out',
          code: 'DAEMON_REQUEST_TIMEOUT',
        ),
      );
      final serverId = client.serverInfo['serverId'];
      final expected = _expectedServerId();
      if (expected != null && serverId != expected) {
        throw DaemonException(
          'Connection resolved to $serverId, expected $expected.',
          code: 'DAEMON_IDENTITY_MISMATCH',
        );
      }
      return client;
    } catch (_) {
      client.dispose();
      rethrow;
    }
  }

  /// Original `switchToConnection`: stale probe cycles and superseded
  /// switches drop (and close) their adopted client instead of activating.
  Future<void> _switchTo(
    String connectionId, {
    int? probeVersion,
    PaseoClient? existingClient,
  }) async {
    bool valid(int request) =>
        request == _switchVersion &&
        (probeVersion == null || probeVersion == _probeVersion);
    final connection = _connections()
        .where((c) => c.id == connectionId)
        .firstOrNull;
    if (connection == null ||
        (probeVersion != null && probeVersion != _probeVersion)) {
      existingClient?.dispose();
      return;
    }
    final request = ++_switchVersion;
    existingClient?.setReconnectEnabled(true);
    if (!valid(request)) {
      existingClient?.dispose();
      return;
    }
    await _activate(connection, existingClient);
  }

  @override
  void dispose() {
    stop();
    _disposed = true;
    super.dispose();
  }
}
