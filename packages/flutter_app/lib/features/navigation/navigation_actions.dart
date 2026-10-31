import 'package:flutter/material.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/localization.dart';
import '../git/worktree_archive_warning.dart';

/// Original HostBadge label: the host registry label, which the original seeds from the
/// daemon's reported hostname when the user names nothing (`label ?? hostname ?? id`). A
/// stored label that only repeats the connection endpoint or the server id is unnamed, so
/// the daemon hostname wins over it; the endpoint stays the last resort.
String hostDisplayLabel(HostState host) {
  final label = host.config.label.trim();
  final address = host.config.endpoint
      .trim()
      .replaceFirst(RegExp(r'^[a-z][a-z0-9+.-]*://', caseSensitive: false), '')
      .split('/')
      .first;
  final unnamed = label.isEmpty ||
      label == host.serverId ||
      label == host.config.endpoint.trim() ||
      label == address ||
      (address.contains(':') && label == address.substring(0, address.lastIndexOf(':')));
  if (!unnamed) return label;
  final hostname = stringValue(host.client.serverInfo['hostname']).trim();
  if (hostname.isNotEmpty) return hostname;
  return label.isNotEmpty ? label : host.serverId;
}

Future<JsonMap> navigationRequest(
  AppStore store,
  String serverId,
  String type,
  JsonMap fields,
  String responseType, {
  Duration timeout = const Duration(seconds: 60),
}) async {
  final result = await store
      .clientFor(serverId)
      .request(type, fields, responseType: responseType, timeout: timeout);
  if (result['error'] != null ||
      result['accepted'] == false ||
      result['success'] == false ||
      result['status'] == 'agent_create_failed') {
    throw StateError(result.text('error', 'The host rejected this action.'));
  }
  return result;
}

/// The agent tab target for [agent] on [serverId], placed under [project] (or the
/// agent's own project placement).
PanelTarget navigationAgentTarget(
  String serverId,
  JsonMap agent, {
  JsonMap? project,
}) {
  final placement = project ?? agent.object('project');
  return PanelTarget(
    kind: 'agent',
    serverId: serverId,
    workspaceId: agent.text('workspaceId'),
    cwd: agent.text('cwd', placement.object('checkout').text('cwd')),
    id: agent.text('id'),
    title: agent.text('title', 'Session'),
    data: {'project': placement},
  );
}

void openNavigationAgent(
  AppStore store,
  String serverId,
  JsonMap agent, {
  JsonMap? project,
}) => store.open(navigationAgentTarget(serverId, agent, project: project));

Future<String?> navigationName(
  BuildContext context,
  String title,
  String initial, {
  int? maxLength,
}) async {
  final controller = TextEditingController(text: initial);
  final key = GlobalKey<FormState>();
  try {
    return await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: Form(
          key: key,
          child: TextFormField(
            controller: controller,
            autofocus: true,
            maxLength: maxLength,
            validator: (value) =>
                value == null || value.trim().isEmpty ? 'Enter a name' : null,
            onFieldSubmitted: (_) {
              if (key.currentState!.validate()) {
                Navigator.pop(context, controller.text.trim());
              }
            },
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: Text(tr(context, 'common.actions.cancel')),
          ),
          FilledButton(
            onPressed: () {
              if (key.currentState!.validate()) {
                Navigator.pop(context, controller.text.trim());
              }
            },
            child: Text(tr(context, 'common.actions.save')),
          ),
        ],
      ),
    );
  } finally {
    controller.dispose();
  }
}

/// Original `useWorkspaceArchive` / `selectProjectWorkspacesToArchive` +
/// `archiveWorkspacesOptimistically`: only a worktree asks first, and only when it is
/// risky (`confirmRiskyWorktreeArchive`: uncommitted changes or unpushed commits);
/// directory workspaces archive without a dialog. The confirmed workspaces archive
/// together, each failure reported on its own, and an archived workspace's tabs leave
/// the layout (`purgeWorkspace`).
Future<void> archiveNavigationWorkspaces(
  BuildContext context,
  AppStore store,
  String serverId,
  List<JsonMap> workspaces,
) async {
  final confirmed = <JsonMap>[];
  for (final workspace in workspaces) {
    if (workspace.text('workspaceKind') == 'worktree') {
      if (!context.mounted) return;
      final git = workspace.object('gitRuntime');
      final diffStat = workspace['diffStat'] is Map ? workspace.object('diffStat') : null;
      if (!await confirmRiskyWorktreeArchive(
        context,
        workspaceName: workspace.text('name', workspace.text('id')),
        isDirty: git['isDirty'] as bool?,
        aheadOfOrigin: git['aheadOfOrigin'] is num ? intValue(git['aheadOfOrigin']) : null,
        diffStat: diffStat,
      )) {
        continue;
      }
    }
    confirmed.add(workspace);
  }
  if (confirmed.isEmpty) return;
  final archived = <String>{};
  final failures = <String>[];
  await Future.wait(
    confirmed.map((workspace) async {
      try {
        await navigationRequest(store, serverId, 'archive_workspace_request', {
          'workspaceId': workspace.text('id'),
        }, 'archive_workspace_response');
        archived.add(workspace.text('id'));
      } catch (error) {
        failures.add(error.toString());
      }
    }),
  );
  for (final tab in List<PanelTarget>.of(store.tabs)) {
    if (tab.serverId == serverId && archived.contains(tab.workspaceId)) {
      await store.closeTab(tab.key);
    }
  }
  await store.refreshHost(serverId);
  if (failures.isNotEmpty) throw StateError(failures.join('\n'));
}

Future<void> recoverNavigationWorkspace(
  BuildContext context,
  AppStore store,
  String serverId,
  String workspaceId,
) async {
  final result = await navigationRequest(
    store,
    serverId,
    'workspace.recovery.inspect.request',
    {'workspaceId': workspaceId},
    'workspace.recovery.inspect.response',
  );
  final state = result.object('state');
  if (state.text('kind') != 'recoverable') {
    throw StateError(
      state.text('message', 'This workspace cannot be restored.'),
    );
  }
  if (!context.mounted) return;
  if (!await confirmAction(
    context,
    title: 'Restore workspace?',
    message:
        '${state.text('workspaceName')}\n${state.text('action')}\n${state.text('branch')}',
    confirmLabel: 'Restore',
  )) {
    return;
  }
  await navigationRequest(
    store,
    serverId,
    'workspace.recovery.restore.request',
    {'workspaceId': workspaceId},
    'workspace.recovery.restore.response',
    timeout: const Duration(seconds: 150),
  );
  await store.refreshHost(serverId);
}
