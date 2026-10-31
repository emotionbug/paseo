import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:uuid/uuid.dart';

import '../../core/json.dart';
import '../../core/legacy_state.dart' show legacyDraftUpdatedAt;
import 'forge_attachments.dart';

/// Durable client-owned drafts and queues. A queue is never a daemon API.
class ChatDrafts extends ChangeNotifier {
  ChatDrafts._();
  static final instance = ChatDrafts._();
  final Map<String, JsonMap> _drafts = {};
  final Map<String, List<JsonMap>> _inbox = {};
  final Set<String> _intents = {};
  final Set<String> _dirtyDrafts = {};
  final Set<String> _persistedDrafts = {};
  final Set<String> _legacyImports = {};
  Future<void>? _initialization;
  Future<void> _writes = Future.value();
  Object? persistenceError;

  static String key(String serverId, String workspaceId, String id) =>
      jsonEncode([serverId, workspaceId, id]);
  static String workspaceKey(String serverId, String workspaceId) =>
      jsonEncode([serverId, workspaceId]);
  Iterable<String> keysForServer(String serverId) => _drafts.keys.where((key) {
    final identity = jsonList(jsonDecode(key));
    return identity.isNotEmpty && identity.first == serverId;
  });

  Future<void> initialize() => _initialization ??= _load();
  Future<void> _load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final encoded = prefs.getString('paseo.chat.drafts.v1');
      if (encoded != null) {
        final saved = jsonMap(jsonDecode(encoded));
        for (final entry in jsonMap(saved['drafts']).entries) {
          if (_dirtyDrafts.contains(entry.key)) continue;
          final value = jsonMap(entry.value);
          value['queue'] = jsonList(value['queue']).map((item) {
            final queued = jsonMap(item);
            return queued['state'] == 'sending'
                ? {
                    ...queued,
                    'state': 'unacknowledged',
                    'error': 'The app closed before this send was acknowledged. Retry the same message to recover.',
                  }
                : queued;
          }).toList();
          _drafts[entry.key] = value;
          _persistedDrafts.add(entry.key);
        }
        for (final entry in jsonMap(saved['inbox']).entries) {
          final current = _inbox.putIfAbsent(entry.key, () => []);
          final known = current.map(jsonEncode).toSet();
          current.addAll(
            jsonList(entry.value)
                .map(jsonMap)
                .where((item) => known.add(jsonEncode(item))),
          );
        }
        _intents.addAll(jsonList(saved['intents']).whereType<String>());
        _legacyImports.addAll(jsonList(saved['legacyImports']).whereType<String>());
      }
    } catch (error) {
      persistenceError = error;
    }
    notifyListeners();
  }

  JsonMap draft(String key) => _drafts.putIfAbsent(
    key,
    () => {
      'text': '',
      'attachments': <JsonMap>[],
      'images': <JsonMap>[],
      'queue': <JsonMap>[],
      'config': <String, dynamic>{},
    },
  );

  /// Import the matching active legacy draft into an absent Flutter draft.
  /// Re-sync: a newer original record (by updatedAt) for the source this
  /// draft imported replaces it only while the draft is unchanged since that
  /// import (a newer inactive record clears it). Flutter edits always win.
  /// The original record and unresolved attachment references stay durable;
  /// source files are read-only.
  Future<void> hydrateLegacy(
    String key,
    JsonMap legacy, {
    required List<String> sourceKeys,
    bool legacyForge = false,
  }) async {
    await initialize();
    final state = legacy['state'] is Map ? jsonMap(legacy['state']) : legacy;
    final records = jsonMap(state['drafts']);
    bool absent() => !_persistedDrafts.contains(key) && !_dirtyDrafts.contains(key);
    String? sourceKey;
    JsonMap record = {};
    var resync = false;
    for (final candidate in sourceKeys) {
      final value = candidate == 'createModalDraft'
          ? state['createModalDraft'] : records[candidate];
      if (value is! Map) continue;
      final candidateRecord = jsonMap(value);
      if (_legacyImports.contains(candidate)) {
        if (!_resyncable(key, candidate, candidateRecord)) continue;
        resync = true;
      } else if (!absent() || !_activeLegacy(candidateRecord)) {
        continue;
      }
      sourceKey = candidate;
      record = candidateRecord;
      break;
    }
    if (sourceKey == null) return;
    final input = record['input'] is Map ? jsonMap(record['input']) : record;
    final attachments = <JsonMap>[], images = <JsonMap>[], unresolved = <JsonMap>[];
    var text = '';
    if (_activeLegacy(record)) {
      if (input['text'] is! String ||
          (input['attachments'] != null && input['attachments'] is! List)) {
        throw StateError('The legacy draft $sourceKey has an unsupported format. Its original data has been retained.');
      }
      text = input['text'] as String;
      final sources = [
        ...jsonList(input['attachments']),
        for (final image in jsonList(input['images']))
          {'kind': 'image', 'metadata': image},
      ];
      for (final source in sources) {
        if (source is! Map) {
          unresolved.add({'source': source, 'reason': 'Unsupported legacy attachment format'});
          continue;
        }
        final converted = await _legacyAttachment(jsonMap(source), legacyForge: legacyForge);
        if (converted.attachment != null) attachments.add(converted.attachment!);
        if (converted.image != null) images.add(converted.image!);
        if (converted.error != null) unresolved.add({'source': source, 'reason': converted.error});
      }
    }
    // Reading an image must not overwrite typing or another imported draft.
    if (resync
        ? !_resyncable(key, sourceKey, record)
        : !absent() || _legacyImports.contains(sourceKey)) {
      return;
    }
    _legacyImports.add(sourceKey);
    final fields = <String, dynamic>{
      'text': text,
      'attachments': attachments,
      'images': images,
      'unresolvedLegacyAttachments': unresolved,
    };
    update(key, {
      ...fields,
      'legacySource': {'key': sourceKey, 'record': record, 'digest': _importedDigest(fields)},
    });
    await flush();
  }

  // migration.ts: a record without lifecycle is active.
  static bool _activeLegacy(JsonMap record) =>
      record['lifecycle'] == null || record['lifecycle'] == 'active';

  /// The draft at [key] imported [sourceKey], [record] is newer than that
  /// import, and nothing changed the draft since.
  bool _resyncable(String key, String sourceKey, JsonMap record) {
    final current = _drafts[key];
    final source = jsonMap(current?['legacySource']);
    if (current == null || source['key'] != sourceKey ||
        !(legacyDraftUpdatedAt(record) > legacyDraftUpdatedAt(source['record']))) {
      return false;
    }
    if (jsonList(current['queue']).isNotEmpty || current['pending'] != null) return false;
    if (source['digest'] is String) return source['digest'] == _importedDigest(current);
    // Imported before digests were recorded: the text and every converted or
    // unresolved original attachment must still be exactly as imported.
    final imported = jsonMap(source['record']);
    final input = imported['input'] is Map ? jsonMap(imported['input']) : imported;
    return current['text'] == stringValue(input['text']) &&
        jsonList(current['attachments']).length + jsonList(current['images']).length +
                jsonList(current['unresolvedLegacyAttachments']).length ==
            jsonList(input['attachments']).length + jsonList(input['images']).length;
  }

  static String _importedDigest(JsonMap draft) => sha256.convert(utf8.encode(jsonEncode([
    draft['text'],
    jsonList(draft['attachments']),
    jsonList(draft['images']),
    jsonList(draft['unresolvedLegacyAttachments']),
  ]))).toString();

  Future<void> recoverLegacyAttachments(String key, {bool legacyForge = false}) async {
    final unresolved = List.of(jsonList(draft(key)['unresolvedLegacyAttachments']));
    for (final value in unresolved) {
      final row = jsonMap(value);
      if (row['source'] is! Map) continue;
      final converted = await _legacyAttachment(jsonMap(row['source']), legacyForge: legacyForge);
      final current = draft(key);
      final pending = List.of(jsonList(current['unresolvedLegacyAttachments']));
      final index = pending.indexWhere((item) => jsonEncode(item) == jsonEncode(row));
      if (index < 0) continue;
      if (converted.error != null) {
        pending[index] = {...row, 'reason': converted.error};
      } else {
        pending.removeAt(index);
      }
      update(key, {
        'unresolvedLegacyAttachments': pending,
        if (converted.attachment != null)
          'attachments': [...jsonList(current['attachments']), converted.attachment],
        if (converted.image != null)
          'images': [...jsonList(current['images']), converted.image],
      });
    }
    await flush();
  }

  Future<({JsonMap? attachment, JsonMap? image, String? error})> _legacyAttachment(
    JsonMap source, {
    required bool legacyForge,
  }) async {
    switch (source['kind']) {
      case 'file':
        final attachment = jsonMap(source['attachment']);
        if (attachment['type'] == 'uploaded_file') {
          return (attachment: attachment, image: null, error: null);
        }
      case 'workspace_file':
        return (attachment: source, image: null, error: null);
      case 'forge_issue':
      case 'forge_change_request':
      case 'github_issue':
      case 'github_pr':
        return (
          attachment: forgeAttachment(jsonMap(source['item']), legacy: legacyForge),
          image: null,
          error: null,
        );
      case 'image':
        final metadata = jsonMap(source['metadata']);
        if (metadata['storageType'] == 'web-indexeddb') {
          return (attachment: null, image: null,
            error: 'This image remains in the original browser image store. Export and attach it to recover it.');
        }
        final location = stringValue(metadata['storageKey'], stringValue(metadata['uri']));
        try {
          final uri = Uri.tryParse(location);
          if (location.isEmpty || uri == null ||
              (uri.hasScheme && !['file', 'data'].contains(uri.scheme))) {
            throw StateError('Image storage location is not a local file');
          }
          final bytes = uri.scheme == 'data'
              ? uri.data!.contentAsBytes()
              : await (uri.scheme == 'file' ? File.fromUri(uri) : File(location)).readAsBytes();
          return (
            attachment: null,
            image: {
              'data': base64Encode(bytes),
              'mimeType': stringValue(metadata['mimeType'], uri.data?.mimeType ?? 'image/png'),
              if (metadata['fileName'] != null) 'fileName': metadata['fileName'],
            },
            error: null,
          );
        } catch (error) {
          return (attachment: null, image: null,
            error: 'The original image could not be read: $error. Its reference has been retained.');
        }
      default:
        if (source['type'] is String && source['kind'] == null) {
          return (attachment: source, image: null, error: null);
        }
    }
    return (attachment: null, image: null,
      error: 'This legacy ${source['kind'] ?? 'attachment'} requires its original integration. The original data has been retained.');
  }

  void update(String key, JsonMap fields) {
    _dirtyDrafts.add(key);
    draft(key).addAll(fields);
    _save();
  }

  void addAttachments(
    String serverId,
    String workspaceId,
    List<JsonMap> attachments, {
    String? intentId,
  }) {
    if (intentId != null && !_intents.add(intentId)) return;
    final inbox = _inbox.putIfAbsent(
      workspaceKey(serverId, workspaceId),
      () => [],
    );
    for (final attachment in attachments) {
      if (!inbox.any((item) => jsonEncode(item) == jsonEncode(attachment))) {
        inbox.add(attachment);
      }
    }
    _save();
  }

  void addDraftAttachments(
    String key,
    List<JsonMap> attachments, {
    String? intentId,
  }) {
    if (intentId != null && !_intents.add(intentId)) return;
    final current = jsonList(draft(key)['attachments']).map(jsonMap).toList();
    final known = current.map(jsonEncode).toSet();
    current.addAll(
      attachments.where((attachment) => known.add(jsonEncode(attachment))),
    );
    update(key, {'attachments': current});
  }

  void takeAttachments(String serverId, String workspaceId, String draftKey) {
    final pending = _inbox.remove(workspaceKey(serverId, workspaceId));
    if (pending == null || pending.isEmpty) return;
    final current = jsonList(draft(draftKey)['attachments'])
        .map(jsonMap)
        .toList();
    current.addAll(
      pending.where((a) => !current.any((b) => jsonEncode(a) == jsonEncode(b))),
    );
    update(draftKey, {'attachments': current});
  }

  void acknowledge(String key, JsonMap submitted) {
    final current = draft(key);
    // A response must not erase edits made while the request was in flight.
    // The submitted text is the trimmed composer text (submit.ts).
    if (stringValue(current['text']).trim() == submitted['text']) current['text'] = '';
    for (final field in ['attachments', 'images']) {
      final sent = jsonList(submitted[field]).map(jsonEncode).toSet();
      current[field] = jsonList(current[field])
          .where((a) => !sent.contains(jsonEncode(a)))
          .toList();
    }
    current.remove('pending');
    _save();
  }

  /// The message a submit sends: the composer text trimmed (submit.ts /
  /// input.tsx sendMessageImpl) and the attachments.
  JsonMap snapshot(String key) {
    final d = draft(key);
    return {
      'messageId': const Uuid().v4(),
      'text': stringValue(d['text']).trim(),
      'attachments': List.of(jsonList(d['attachments'])),
      'images': List.of(jsonList(d['images'])),
    };
  }

  /// Commit only against the snapshot shown by the edit dialog. Delivery may
  /// have started (or completed) while the user was typing.
  bool editQueuedMessage(String key, JsonMap original, String text) {
    final current = draft(key);
    final queue = jsonList(current['queue']).map(jsonMap).toList();
    final index = queue.indexWhere(
      (item) => item['messageId'] == original['messageId'],
    );
    if (index >= 0 &&
        ['queued', 'blocked', 'rejected'].contains(queue[index]['state']) &&
        jsonEncode(queue[index]) == jsonEncode(original)) {
      final edited = {
        ...queue[index],
        'text': text,
        'state': 'queued',
        'error': null,
      };
      if (original['state'] == 'rejected') {
        // An explicit edit of a definitely rejected request is a new message,
        // not a retry of the immutable receipt. Unknown outcomes cannot enter
        // this branch.
        edited['messageId'] = const Uuid().v4();
        edited.remove('deliveryAttempted');
        edited.remove('activeTurnBehavior');
      }
      queue[index] = edited;
      update(key, {'queue': queue});
      return true;
    }
    // Never overwrite a sending snapshot. Preserve the user's edited content
    // in the composer for review, including attachments, without sending it.
    final fields = <String, dynamic>{
      'text': [
        stringValue(current['text']),
        text,
      ].where((part) => part.isNotEmpty).join('\n\n'),
    };
    for (final field in ['attachments', 'images']) {
      final values = List.of(jsonList(current[field]));
      final known = values.map(jsonEncode).toSet();
      values.addAll(
        jsonList(original[field])
            .where((value) => known.add(jsonEncode(value))),
      );
      fields[field] = values;
    }
    update(key, fields);
    return false;
  }

  Future<void> flush() async {
    await _writes;
    if (persistenceError != null) {
      throw StateError('Draft persistence failed: $persistenceError');
    }
  }

  void _save() {
    notifyListeners();
    _writes = _writes
        .then((_) async {
          await initialize();
          final encoded = jsonEncode({
            'drafts': _drafts,
            'inbox': _inbox,
            'intents': _intents.toList(),
            'legacyImports': _legacyImports.toList(),
          });
          final prefs = await SharedPreferences.getInstance();
          if (!await prefs.setString('paseo.chat.drafts.v1', encoded)) {
            throw StateError('Could not save conversation drafts');
          }
          if (persistenceError != null) {
            persistenceError = null;
            notifyListeners();
          }
        })
        .catchError((Object error) {
          persistenceError = error;
          notifyListeners();
        });
  }
}

JsonMap wireAttachment(JsonMap attachment) {
  if (attachment['kind'] != 'workspace_file') return attachment;
  final path = stringValue(attachment['path']);
  final selection = jsonMap(attachment['selection']);
  final lines = selection['kind'] == 'line_range'
      ? '\nLines: ${selection['startLine']}-${selection['endLine']}'
      : '';
  return {
    'type': 'text',
    'mimeType': 'text/plain',
    'title': path.split('/').last,
    'text': 'Workspace file: $path$lines',
  };
}

/// The behavior is part of the daemon's durable request fingerprint. Once an
/// attempt has begun, even an omitted behavior must be replayed unchanged.
/// [activeTurnBehavior] is the behavior of a first attempt: the queue drain
/// steers (host-runtime.ts drainQueuedAgentMessage), Send now follows the
/// send preference (index.tsx handleSendQueuedNow → submitMessage).
JsonMap queuedMessageForDelivery(JsonMap message, {required String activeTurnBehavior}) {
  final attempted =
      message['deliveryAttempted'] == true ||
      ['sending', 'unacknowledged', 'rejected'].contains(message['state']);
  return {
    ...message,
    if (!attempted) 'activeTurnBehavior': activeTurnBehavior,
    'deliveryAttempted': true,
  };
}

JsonMap agentMessageFields(String agentId, JsonMap message) => {
  'agentId': agentId,
  'text': message['text'],
  'messageId': message['messageId'],
  if (message['activeTurnBehavior'] != null)
    'activeTurnBehavior': message['activeTurnBehavior'],
  'images': jsonList(message['images']),
  'attachments': jsonList(message['attachments'])
      .map(jsonMap)
      .map(wireAttachment)
      .toList(),
};
