import 'dart:convert';

import '../json.dart';

/// Compatibility with daemons predating the workspace registry. IDs are opaque:
/// a Windows drive root must never be converted back into a filesystem path.
class LegacyWorkspaces {
  final Map<String, JsonMap> _agents = {};
  static String _id(String path) {
    final value = path
        .trim()
        .replaceAll('\\', '/')
        .replaceFirst(RegExp(r'/+$'), '');
    return value.isEmpty ? '/' : value;
  }

  JsonMap _stamp(JsonMap agent) => {
    ...agent,
    'workspaceId': agent['workspaceId'] ?? _id(agent['cwd'] as String),
  };
  JsonMap normalize(JsonMap message) {
    final payload = message['payload'];
    if (payload is! JsonMap) return message;
    if (message['type'] == 'fetch_agents_response') {
      return {
        ...message,
        'payload': {
          ...payload,
          'entries': (payload['entries'] as List)
              .map(
                (entry) => {
                  ...entry as Map,
                  'agent': _stamp(entry['agent'] as JsonMap),
                },
              )
              .toList(),
        },
      };
    }
    if (payload['agent'] is JsonMap &&
        (const {
              'agent_update',
              'fetch_agent_response',
              'fetch_agent_timeline_response',
              'cancel_agent_response',
            }.contains(message['type']) ||
            message['type'] == 'status' &&
                const {
                  'agent_created',
                  'agent_resumed',
                }.contains(payload['status']))) {
      return {
        ...message,
        'payload': {...payload, 'agent': _stamp(payload['agent'] as JsonMap)},
      };
    }
    return message;
  }

  List<JsonMap> read(List<dynamic> entries, bool reset) {
    if (reset) _agents.clear();
    final ids = <String>{};
    for (final raw in entries) {
      final entry = raw as JsonMap;
      final agent = entry['agent'] as JsonMap;
      _agents[agent['id'] as String] = entry;
      ids.add(
        agent['workspaceId'] as String? ??
            _id((entry['project'] as Map)['checkout']['cwd'] as String),
      );
    }
    return _workspaces().values
        .where((workspace) => ids.contains(workspace['id']))
        .toList();
  }

  List<JsonMap> update(JsonMap message) {
    if (message['type'] != 'agent_update') return const [];
    final before = _workspaces();
    final payload = message['payload'] as JsonMap;
    if (payload['kind'] == 'remove') {
      _agents.remove(payload['agentId']);
    } else {
      final agent = payload['agent'] as JsonMap;
      final project = payload['project'] ?? _agents[agent['id']]?['project'];
      if (agent['archivedAt'] != null) {
        _agents.remove(agent['id']);
      } else if (project != null) {
        _agents[agent['id'] as String] = {'agent': agent, 'project': project};
      }
    }
    final after = _workspaces();
    return [
      for (final entry in after.entries)
        if (jsonEncode(before[entry.key]) != jsonEncode(entry.value))
          {
            'type': 'workspace_update',
            'payload': {'kind': 'upsert', 'workspace': entry.value},
          },
      for (final id in before.keys)
        if (!after.containsKey(id))
          {
            'type': 'workspace_update',
            'payload': {'kind': 'remove', 'id': id},
          },
    ];
  }

  Map<String, JsonMap> _workspaces() {
    const priority = {
      'needs_input': 0,
      'failed': 1,
      'running': 2,
      'attention': 3,
      'done': 4,
    };
    final result = <String, JsonMap>{};
    for (final entry in _agents.values) {
      final agent = entry['agent'] as JsonMap;
      final project = entry['project'] as JsonMap;
      final checkout = project['checkout'] as JsonMap;
      final id =
          agent['workspaceId'] as String? ?? _id(checkout['cwd'] as String);
      final status =
          (agent['pendingPermissions'] as List? ?? []).isNotEmpty ||
              agent['attentionReason'] == 'permission'
          ? 'needs_input'
          : agent['status'] == 'error' || agent['attentionReason'] == 'error'
          ? 'failed'
          : agent['status'] == 'running'
          ? 'running'
          : agent['requiresAttention'] == true
          ? 'attention'
          : 'done';
      if (result.containsKey(id) &&
          priority[result[id]!['status']]! <= priority[status]!) {
        continue;
      }
      final name = (project['workspaceName'] as String?)?.trim();
      final branch = (checkout['currentBranch'] as String?)?.trim();
      result[id] = {
        'id': id,
        'projectId': project['projectKey'],
        'projectDisplayName': project['projectName'],
        'projectCustomName': null,
        'projectRootPath':
            checkout['mainRepoRoot'] ??
            checkout['worktreeRoot'] ??
            checkout['cwd'],
        'workspaceDirectory': checkout['cwd'],
        'projectKind': checkout['isGit'] == true ? 'git' : 'non_git',
        'workspaceKind': checkout['isGit'] != true
            ? 'directory'
            : checkout['isPaseoOwnedWorktree'] == true
            ? 'worktree'
            : 'checkout',
        'name': name != null && name.isNotEmpty
            ? name
            : branch != null && branch.isNotEmpty && branch != 'HEAD'
            ? branch
            : id.split('/').last,
        'title': null,
        'status': status,
        'statusEnteredAt': agent['attentionTimestamp'] ?? agent['updatedAt'],
        'activityAt': agent['updatedAt'],
        'archivingAt': null,
        'diffStat': null,
        'scripts': [],
        'githubRuntime': null,
        'project': project,
        'gitRuntime': checkout['isGit'] != true
            ? null
            : {
                'currentBranch': checkout['currentBranch'],
                'remoteUrl': checkout['remoteUrl'],
                'isPaseoOwnedWorktree': checkout['isPaseoOwnedWorktree'],
                'isDirty': null,
                'aheadBehind': null,
                'aheadOfOrigin': null,
                'behindOfOrigin': null,
              },
      };
    }
    return result;
  }
}
