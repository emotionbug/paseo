import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import 'chat_controller.dart';

/// client/src/creation/index.ts CreationClient for one creation intent keyed
/// by its `idempotencyKey`. Creation has no request deadline (`timeout: 0`),
/// and a lost connection never fails it: once the host is back, its receipt
/// is followed until it commits, and the intent is submitted again only when
/// the host holds no receipt for it (`reconnect`). Only closing the client
/// ends a creation without an outcome.
///
/// [recover] is the retry of an intent whose earlier outcome is unknown. The
/// original resubmits the same key there and the host answers from its
/// receipt: the committed or unknown outcome, the running creation's eventual
/// outcome, and a rerun of a definite failure. The receipt is read first so
/// a committed outcome is never replayed, with those same results.
Future<JsonMap> createWithReceipt(
  PaseoClient client,
  String kind,
  JsonMap fields, {
  required bool recover,
}) => _Creation(client, kind, fields).run(recover: recover);

class _Creation {
  _Creation(this.client, this.kind, this.fields)
    : key = stringValue(fields['idempotencyKey']);

  final PaseoClient client;
  final String kind, key;
  final JsonMap fields;
  final _outcome = Completer<JsonMap>();
  int _revision = -1;

  /// The receipt subscription while this creation is recovering; a lost
  /// connection then restores it instead of resuming again.
  OwnedObservation? _observation;
  bool _recovering = false;

  /// Wakes [_reconnected] on a connection change or a settled outcome.
  Completer<void>? _wake;

  Future<JsonMap> run({required bool recover}) async {
    if (client.status == ConnectionStatus.disconnected) throw _closed;
    client.addListener(_connectionChanged);
    final events = client.events.listen(null, onDone: () => _fail(_closed));
    try {
      if (recover) {
        unawaited(_recover());
      } else {
        unawaited(_submit());
      }
      return await _outcome.future;
    } finally {
      client.removeListener(_connectionChanged);
      await events.cancel();
      final observation = _observation;
      _observation = null;
      if (observation != null) {
        try {
          await observation.close();
        } catch (_) {
          // Releasing the receipt subscription cannot change a settled outcome.
        }
      }
    }
  }

  /// CreationClient.close: closing the client fails every unsettled creation.
  static const _closed = DaemonException(
    'Daemon client closed',
    code: 'DAEMON_CONNECTION_LOST',
    outcomeUnknown: true,
  );

  void _connectionChanged() {
    if (client.status == ConnectionStatus.disconnected) _fail(_closed);
    _signal();
  }

  void _signal() {
    final wake = _wake;
    _wake = null;
    wake?.complete();
  }

  void _complete(JsonMap result) {
    if (!_outcome.isCompleted) _outcome.complete(result);
    _signal();
  }

  void _fail(Object error) {
    if (!_outcome.isCompleted) _outcome.completeError(error);
    _signal();
  }

  /// A failure that only says the host is (or became) unreachable.
  bool _offline(Object error) =>
      client.status != ConnectionStatus.connected ||
      (error is DaemonException &&
          const {
            'DAEMON_CONNECTION_LOST',
            'DAEMON_NOT_CONNECTED',
          }.contains(error.code));

  /// CreationClient.submit: `request(kind, {...input})`, then the response.
  Future<void> _submit() async {
    JsonMap result;
    try {
      result = await client.request(
        '$kind.create.request',
        fields,
        responseType: '$kind.create.response',
        timeout: Duration.zero,
      );
    } on DaemonException catch (error) {
      if (_outcome.isCompleted) return;
      // The transport rejects error-bearing responses before returning their
      // payload. A failed creation can nevertheless own a real agent or have
      // an unknown prompt outcome, so interpret its receipt before rejecting.
      final payload = error.payload;
      if (payload == null || payload['creation'] is! Map) {
        if (_offline(error)) {
          // DAEMON_CONNECTION_LOST never fails a creation; reconnect() resumes
          // it, unless its receipt subscription is already being restored.
          if (!_recovering) await _resume();
          return;
        }
        _fail(error);
        return;
      }
      final receipt = jsonMap(payload['creation']);
      if (receipt['kind'] != kind || receipt['idempotencyKey'] != key) {
        _fail(
          DaemonException(
            'Creation failure carried a mismatched receipt.',
            payload: payload,
            outcomeUnknown: true,
          ),
        );
        return;
      }
      result = payload;
    } catch (error) {
      if (_outcome.isCompleted) return;
      if (_offline(error)) {
        if (!_recovering) await _resume();
        return;
      }
      _fail(error);
      return;
    }
    final creation = jsonMap(result['creation']);
    if (creation['outcomeUnknown'] == true) {
      _fail(
        DaemonException(
          stringValue(
            creation['error'],
            'Creation outcome is unknown. Recover this same intent.',
          ),
          payload: result,
          outcomeUnknown: true,
        ),
      );
      return;
    }
    if (creation['phase'] == 'failed' &&
        creation['failedStage'] == 'prompt' &&
        creation['agent'] is Map) {
      _complete({
        'agent': creation['agent'],
        'creation': creation,
        'promptError': creation['error'],
      });
      return;
    }
    try {
      _complete(requireSuccess(result));
    } catch (error) {
      _fail(error);
    }
  }

  /// Retry of an unknown outcome: the host's receipt decides (see
  /// [createWithReceipt]).
  Future<void> _recover() async {
    while (await _reconnected()) {
      JsonMap receipt;
      try {
        receipt = await client.recoverCreation(kind, key);
      } catch (error) {
        if (_outcome.isCompleted) return;
        if (_offline(error)) continue;
        // The earlier attempt's outcome is still unknown; keep its identity.
        _fail(_unknown(error));
        return;
      }
      final snapshot = receipt['snapshot'];
      if (snapshot is! Map || _rerun(jsonMap(snapshot))) {
        await _submit();
        return;
      }
      // Committed and unknown outcomes settle here; a running creation is
      // followed until it commits.
      _apply(jsonMap(snapshot));
      if (!_outcome.isCompleted) await _resume();
      return;
    }
  }

  /// CreationClient.reconnect: follow this intent's receipt over a
  /// subscription; no receipt means the host never admitted the intent, so it
  /// is submitted again.
  Future<void> _resume() async {
    _recovering = true;
    while (await _reconnected()) {
      OwnedObservation observation;
      try {
        observation = await client.observe(
          type: 'creation.subscribe.request',
          fields: {'kind': kind, 'idempotencyKey': key, 'subscribe': true},
          responseType: 'creation.subscribe.response',
          onEvent: _event,
        );
      } catch (error) {
        if (_outcome.isCompleted) return;
        if (_offline(error)) continue;
        _fail(_unknown(error));
        return;
      }
      if (_outcome.isCompleted) {
        unawaited(observation.close().catchError((Object _) {}));
        return;
      }
      _observation = observation;
      _receipt(observation.snapshot['snapshot']);
      return;
    }
  }

  /// The host's snapshot on (re)subscribing.
  void _receipt(Object? snapshot) {
    if (_outcome.isCompleted) return;
    if (snapshot is Map) {
      _apply(jsonMap(snapshot));
    } else {
      unawaited(_submit());
    }
  }

  void _event(JsonMap event) {
    final type = event['type'];
    final payload = jsonMap(event['payload']);
    if (type == '$kind.create.update') {
      _apply(payload);
    } else if (type == 'subscription.restored') {
      _receipt(payload['snapshot']);
    } else if (type == 'subscription.error') {
      _fail(
        DaemonException(stringValue(payload['error']), outcomeUnknown: true),
      );
    }
  }

  /// A definite failure the host reruns when the same intent is resubmitted
  /// (creation/index.ts `create` resets a failed record without `inFlight`).
  bool _rerun(JsonMap snapshot) =>
      snapshot['phase'] == 'failed' &&
      snapshot['outcomeUnknown'] != true &&
      !(snapshot['failedStage'] == 'prompt' && snapshot['agent'] is Map);

  /// CreationClient.receive: an older revision never replaces a newer one; a
  /// committed or failed receipt settles the creation.
  void _apply(JsonMap receipt) {
    if (_outcome.isCompleted || receipt['idempotencyKey'] != key) return;
    final revision = intValue(receipt['revision'], -1);
    if (revision < _revision) return;
    _revision = revision;
    if (receipt['phase'] == 'failed') {
      final text = stringValue(receipt['error'], 'Creation failed');
      if (receipt['outcomeUnknown'] != true &&
          receipt['failedStage'] == 'prompt' &&
          receipt['agent'] is Map) {
        _complete({
          'agent': receipt['agent'],
          'creation': receipt,
          'promptError': text,
        });
        return;
      }
      _fail(
        receipt['outcomeUnknown'] == true
            ? DaemonException(
                text,
                payload: {'creation': receipt},
                outcomeUnknown: true,
              )
            : ChatRequestRejected(text),
      );
    } else if (receipt['phase'] == 'completed') {
      _complete({
        'agent': receipt['agent'],
        'workspace': receipt['workspace'],
        'creation': receipt,
      });
    }
  }

  /// Waits until the host is connected. False once the creation settled,
  /// including by the client closing.
  Future<bool> _reconnected() async {
    while (!_outcome.isCompleted) {
      if (client.status == ConnectionStatus.connected) return true;
      if (client.status == ConnectionStatus.disconnected) {
        _fail(_closed);
        return false;
      }
      await (_wake ??= Completer<void>()).future;
    }
    return false;
  }

  /// A recovery failure says nothing about the earlier attempt, whose outcome
  /// stays unknown: its identity must survive for the next retry.
  DaemonException _unknown(Object error) => error is DaemonException
      ? DaemonException(
          error.message,
          code: error.code,
          requestId: error.requestId,
          requestType: error.requestType,
          payload: error.payload,
          outcomeUnknown: true,
        )
      : DaemonException(error.toString(), outcomeUnknown: true);
}

/// stores/create-flow-store.ts, the part a chat reads: a draft whose create
/// request returned its agent is marked "sent" for that agent
/// (create-flow.ts updatePendingAgentId + markLifecycle "sent"), handing its
/// conversation off optimistically (agent-panel.tsx findActiveCreateHandoff
/// → the "optimistic-create" continuity). The viewed timeline releases it
/// (`clearByAgent`) once the sync no longer owes that chat a catch-up: the
/// catch-up reached current, or the chat left the viewed set because its tab
/// closed. Disconnect and backgrounding keep it.
class CreateHandoffs extends ChangeNotifier {
  CreateHandoffs._();
  static final instance = CreateHandoffs._();

  final _sent = <String>{};

  static String _key(String serverId, String agentId) =>
      jsonEncode([serverId, agentId]);

  /// The created agent's draft becomes "sent".
  void markSent(String serverId, String agentId) {
    if (agentId.isEmpty) return;
    if (_sent.add(_key(serverId, agentId))) notifyListeners();
  }

  /// agent-panel.tsx findActiveCreateHandoff.
  bool active(String serverId, String? agentId) =>
      agentId != null &&
      agentId.isNotEmpty &&
      _sent.contains(_key(serverId, agentId));

  /// create-flow-store.ts clearByAgent: releases the sent handoff of this
  /// agent on this host.
  void clearByAgent(String serverId, String agentId) {
    if (_sent.remove(_key(serverId, agentId))) notifyListeners();
  }
}
