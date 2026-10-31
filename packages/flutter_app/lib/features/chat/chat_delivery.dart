import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import 'chat_controller.dart';
import 'chat_drafts.dart';

/// Owns client queues independently of panel visibility and tab lifetime.
class ChatDelivery {
  ChatDelivery._(this.client, this.serverId) {
    _subscription = client.events.listen(_event);
    client.addListener(_schedule);
    drafts.addListener(_schedule);
    drafts.initialize().then((_) => _schedule());
  }
  static final Map<PaseoClient, ChatDelivery> _owners = {};
  static VoidCallback attach(PaseoClient client, String serverId) {
    final owner = _owners.putIfAbsent(
      client,
      () => ChatDelivery._(client, serverId),
    );
    return () {
      if (identical(_owners[client], owner)) {
        _owners.remove(client);
        owner._dispose();
      }
    };
  }

  final PaseoClient client;
  final String serverId;
  final drafts = ChatDrafts.instance;
  late final StreamSubscription<JsonMap> _subscription;
  final Set<String> _busy = {};
  final Map<String, OwnedObservation> _observations = {};
  bool _disposed = false, _scheduled = false;

  void _event(JsonMap event) {
    final payload = jsonMap(event['payload']);
    if (event['type'] == 'agent_stream' &&
        jsonMap(payload['event'])['type'] == 'timeline') {
      final item = jsonMap(jsonMap(payload['event'])['item']);
      final id = item['clientMessageId'];
      if (id is String) {
        for (final key in drafts.keysForServer(serverId)) {
          final queue = maps(drafts.draft(key)['queue']);
          if (queue.any((message) => message['messageId'] == id)) {
            queue.removeWhere((message) => message['messageId'] == id);
            drafts.update(key, {'queue': queue});
          }
        }
      }
    }
    if (event['type'] == 'agent_permission_resolved' ||
        (event['type'] == 'agent_stream' &&
            [
              'turn_completed',
              'turn_canceled',
              'turn_failed',
              'permission_resolved',
            ].contains(jsonMap(payload['event'])['type']))) {
      _schedule();
    }
  }

  void _schedule() {
    if (_disposed ||
        _scheduled ||
        client.status != ConnectionStatus.connected) {
      return;
    }
    _scheduled = true;
    scheduleMicrotask(() {
      _scheduled = false;
      if (_disposed) return;
      for (final key in drafts.keysForServer(serverId)) {
        final queue = maps(drafts.draft(key)['queue']);
        if (queue.isNotEmpty &&
            queue.first['state'] == 'queued' &&
            !_busy.contains(key)) {
          unawaited(_drain(key));
        } else if (queue.isEmpty) {
          _observations.remove(key)?.close();
        }
      }
    });
  }

  Future<void> _drain(String key) async {
    if (!_busy.add(key)) return;
    JsonMap? sending;
    try {
      final identity = jsonDecode(key) as List;
      final agentId = identity[2] as String;
      if (!_observations.containsKey(key)) {
        final observation = await client.observe(
          type: 'agent.timeline.set_subscription.request',
          fields: {
            'agentIds': [agentId],
          },
          responseType: 'agent.timeline.set_subscription.response',
          onEvent: (event) {
            if (event['type'] == 'subscription.restored') _schedule();
          },
        );
        if (_disposed) {
          await observation.close();
          return;
        }
        _observations[key] = observation;
      }
      final result = requireSuccess(
        await client.request('fetch_agent_request', {
          'agentId': agentId,
        }, responseType: 'fetch_agent_response'),
      );
      if (_disposed) return;
      final agent = jsonMap(result['agent']);
      if (agent.isEmpty) {
        throw StateError('The queued session no longer exists.');
      }
      if (agent['status'] != 'idle' ||
          agent['activeTurn'] != null ||
          jsonList(agent['pendingPermissions']).isNotEmpty ||
          agent['archivedAt'] != null) {
        return;
      }
      final queue = maps(drafts.draft(key)['queue']);
      if (queue.isEmpty || queue.first['state'] != 'queued') return;
      // host-runtime.ts drainQueuedAgentMessage: a drained message steers, so
      // a turn the agent starts on its own in that moment absorbs it.
      sending = queuedMessageForDelivery(queue.first, activeTurnBehavior: 'steer');
      queue[0] = {...sending, 'state': 'sending'};
      drafts.update(key, {'queue': queue});
      await drafts.flush();
      requireSuccess(
        await client.request(
          'send_agent_message_request',
          agentMessageFields(agentId, sending),
          responseType: 'send_agent_message_response',
        ),
      );
      final current = maps(drafts.draft(key)['queue'])
        ..removeWhere(
          (message) => message['messageId'] == sending!['messageId'],
        );
      drafts.update(key, {'queue': current});
    } catch (error) {
      final queue = maps(drafts.draft(key)['queue']);
      if (sending != null) {
        final index = queue.indexWhere(
          (message) => message['messageId'] == sending!['messageId'],
        );
        if (index >= 0) {
          queue[index] = {
            ...sending,
            'state': definitelyRejected(error) ? 'rejected' : 'unacknowledged',
            'error': '$error',
          };
        }
      } else if (queue.isNotEmpty) {
        // A failed readiness/observation check sent nothing; preserve queue order.
        queue[0] = {...queue.first, 'state': 'blocked', 'error': '$error'};
      }
      drafts.update(key, {'queue': queue});
    } finally {
      _busy.remove(key);
      if (sending != null) _schedule();
    }
  }

  void _dispose() {
    _disposed = true;
    drafts.removeListener(_schedule);
    client.removeListener(_schedule);
    _subscription.cancel();
    for (final observation in _observations.values) {
      observation.close();
    }
    _observations.clear();
  }
}
