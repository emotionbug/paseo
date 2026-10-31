import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart';

import '../core/app_store.dart';
import '../core/client.dart';
import '../core/json.dart';
import '../core/target.dart';
import '../platform/desktop/desktop_host.dart';

class ClientActivity {
  ClientActivity(this.store, {required this.onError}) {
    HardwareKeyboard.instance.addHandler(_keyEvent);
    _removeOpen = store.registerClientOpenHook(_attach);
    _removeClose = store.registerClientCloseHook(_detach);
    _heartbeat = Timer.periodic(const Duration(seconds: 15), (_) => _sendAll());
    // Global idle observation is not available on Wayland. Input timestamps below
    // remain actual app activity, rather than claiming a fabricated OS idle value.
    if (DesktopHost.supported &&
        Platform.environment['XDG_SESSION_TYPE'] != 'wayland') {
      _idlePoll = Timer.periodic(
        const Duration(seconds: 5),
        (_) => _readIdle(),
      );
    }
  }
  final AppStore store;
  final void Function(Object error) onError;
  final _clients = <PaseoClient, String>{};
  final _events = <PaseoClient, StreamSubscription<JsonMap>>{};
  late final VoidCallback _removeOpen;
  late final VoidCallback _removeClose;
  Timer? _heartbeat;
  Timer? _idlePoll;
  DateTime _lastActivity = DateTime.now();
  DateTime _visibilityChanged = DateTime.now();
  DateTime? _lastImmediate;
  bool _keyEvent(KeyEvent event) {
    if (event is KeyDownEvent || event is KeyRepeatEvent) recordInput();
    return false;
  }

  PanelTarget? _target;
  bool _visible = false;
  bool _disposed = false;
  bool _readingIdle = false;
  int? _badge;

  void _attach(PaseoClient client, String serverId) {
    _clients[client] = serverId;
    _events[client] = client.events.listen((event) {
      final type = event.text('type');
      if (type == 'agent_attention_required' ||
          type == 'terminal_attention_required') {
        unawaited(
          _notify(client, serverId, event).catchError((Object error) {
            if (!_disposed) onError(error);
          }),
        );
      }
    });
    _send(client, serverId);
  }

  Future<void> _detach(PaseoClient client) async {
    _clients.remove(client);
    await _events.remove(client)?.cancel();
  }

  void updateView(PanelTarget? target, {required bool visible}) {
    if (_visible == visible && _target?.key == target?.key) return;
    if (_visible != visible) _visibilityChanged = DateTime.now();
    _visible = visible;
    _target = target;
    _lastActivity = DateTime.now();
    _lastImmediate = _lastActivity;
    _sendAll();
  }

  void recordInput() {
    _lastActivity = DateTime.now();
    final previous = _lastImmediate;
    if (previous == null ||
        _lastActivity.difference(previous) >= const Duration(seconds: 5)) {
      _lastImmediate = _lastActivity;
      _sendAll();
    }
  }

  Future<void> _readIdle() async {
    if (_readingIdle || _disposed) return;
    final host = DesktopHost.instance;
    if (host == null) return;
    _readingIdle = true;
    try {
      final milliseconds = await host.integrations.idleMilliseconds();
      if (_disposed) return;
      final systemActivity = DateTime.now().subtract(
        Duration(milliseconds: milliseconds),
      );
      if (systemActivity.isAfter(_lastActivity)) _lastActivity = systemActivity;
    } catch (error) {
      _idlePoll?.cancel();
      _idlePoll = null;
      if (!_disposed) onError(error);
    } finally {
      _readingIdle = false;
    }
  }

  void _send(PaseoClient client, String serverId) {
    if (_disposed || client.status != ConnectionStatus.connected) return;
    final target = _target;
    final focused = _visible && target?.serverId == serverId;
    client.send('client_heartbeat', {
      'deviceType': Platform.isAndroid || Platform.isIOS ? 'mobile' : 'web',
      'focusedAgentId': focused && target?.kind == 'agent' ? target?.id : null,
      'focusedTerminalId': focused && target?.kind == 'execution_log'
          ? target?.id
          : null,
      'lastActivityAt': _lastActivity.toUtc().toIso8601String(),
      'appVisible': _visible,
      'appVisibilityChangedAt': _visibilityChanged.toUtc().toIso8601String(),
    });
  }

  void _sendAll() {
    if (_disposed) return;
    for (final entry in _clients.entries) {
      _send(entry.key, entry.value);
    }
    if (DesktopHost.supported) {
      unawaited(
        _updateBadge().catchError((Object error) {
          if (!_disposed) onError(error);
        }),
      );
    }
  }

  Future<void> _updateBadge() async {
    final host = DesktopHost.instance;
    if (host == null) return;
    var count = 0;
    for (final server in store.hosts) {
      for (final row in server.agents) {
        final agent = row.object('agent').isEmpty ? row : row.object('agent');
        if (agent.flag('requiresAttention') && agent['archivedAt'] == null) {
          count++;
        }
      }
    }
    if (count == _badge) return;
    await host.integrations.setBadgeCount(count);
    if (!_disposed) _badge = count;
  }

  Future<void> _notify(
    PaseoClient client,
    String serverId,
    JsonMap event,
  ) async {
    if (_disposed || !DesktopHost.supported || !_clients.containsKey(client)) {
      return;
    }
    final host = DesktopHost.instance;
    final payload = event.object('payload');
    if (host == null || !payload.flag('shouldNotify')) return;
    late String title;
    late String body;
    late JsonMap data;
    late String eventKey;
    if (event.text('type') == 'agent_attention_required') {
      final agentId = payload.text('agentId');
      if (payload.text('reason') == 'error') return;
      if (_visible &&
          _target?.serverId == serverId &&
          _target?.kind == 'agent' &&
          _target?.id == agentId) {
        return;
      }
      final notification = payload.object('notification');
      title = notification.text(
        'title',
        payload.text('reason') == 'permission'
            ? 'Approval requested'
            : 'Session finished',
      );
      body = notification.text('body');
      data = {
        'serverId': serverId,
        'agentId': agentId,
        'reason': payload['reason'],
        ...notification.object('data'),
      };
      eventKey = jsonEncode([
        serverId,
        agentId,
        payload['reason'],
        payload['timestamp'],
      ]);
    } else {
      final terminalId = payload.text('terminalId');
      if (_visible &&
          _target?.serverId == serverId &&
          _target?.kind == 'execution_log' &&
          _target?.id == terminalId) {
        return;
      }
      title = payload.text('title');
      body = payload.text('body');
      data = {
        'serverId': payload.text('serverId', serverId),
        'terminalId': terminalId,
        'cwd': payload['cwd'],
        if (payload['workspaceId'] != null)
          'workspaceId': payload['workspaceId'],
      };
      eventKey = jsonEncode([
        serverId,
        terminalId,
        payload['reason'],
        title,
        body,
      ]);
    }
    final hash = sha256.convert(utf8.encode(eventKey)).bytes;
    final id =
        ((hash[0] << 24) | (hash[1] << 16) | (hash[2] << 8) | hash[3]) &
        0x7fffffff;
    await host.notify(
      id: id,
      title: title,
      body: body,
      data: data,
      eventKey: eventKey,
    );
  }

  void dispose() {
    _disposed = true;
    _removeOpen();
    _removeClose();
    _heartbeat?.cancel();
    _idlePoll?.cancel();
    HardwareKeyboard.instance.removeHandler(_keyEvent);
    for (final subscription in _events.values) {
      unawaited(subscription.cancel());
    }
    _events.clear();
    _clients.clear();
  }
}
