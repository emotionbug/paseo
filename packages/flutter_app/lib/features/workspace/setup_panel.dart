import '../../ui/localization.dart';

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/theme.dart';
import '../settings/settings_forms.dart';
import 'project_settings_panel.dart' show WorkspaceAlert;
import 'workspace_rpc.dart';

class SetupPanel extends StatefulWidget {
  const SetupPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<SetupPanel> createState() => _SetupPanelState();
}

class _SetupPanelState extends State<SetupPanel> {
  PaseoClient? _client;
  OwnedObservation? _observation;
  StreamSubscription<JsonMap>? _events;
  JsonMap? _snapshot;
  List<JsonMap> _scripts = [];
  Object? _error;
  bool _loading = true;
  bool _busy = false;
  int _generation = 0;

  /// Original `useExpandedSetupCommands`: opened rows, and auto-expanded rows
  /// the user collapsed.
  final Set<int> _expanded = {}, _collapsed = {};

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final client = PaseoScope.of(context).clientFor(widget.target.serverId);
    if (identical(client, _client)) return;
    _observation?.close();
    _events?.cancel();
    _client = client;
    _events = client.events.listen((message) {
      final payload = workspacePayload(message);
      if (payload.text('workspaceId') != widget.target.workspaceId) return;
      if (message.text('type') == 'script_status_update' && mounted) {
        setState(
          () => _scripts = payload.array('scripts').map(jsonMap).toList(),
        );
      }
    });
    _load();
  }

  void _receive(JsonMap message) {
    if (!mounted) return;
    final payload = workspacePayload(message);
    if (message.text('type') == 'subscription.error') {
      setState(
        () => _error = payload.text('error', 'Setup observation failed.'),
      );
      return;
    }
    if (payload['workspaceId'] != null &&
        payload.text('workspaceId') != widget.target.workspaceId) {
      return;
    }
    setState(() {
      if (payload.containsKey('snapshot')) {
        _snapshot = payload['snapshot'] == null
            ? null
            : payload.object('snapshot');
      } else if (payload.containsKey('status')) {
        _snapshot = payload;
      }
    });
    if (message.text('type') == 'subscription.restored') {
      _loadScripts().catchError((Object error) {
        if (mounted) setState(() => _error = error);
      });
    }
  }

  Future<void> _load() async {
    final generation = ++_generation;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      await _observation?.close();
      _observation = null;
      final observation = await _client!.observe(
        type: 'workspace.setup.subscribe.request',
        fields: {'workspaceId': widget.target.workspaceId},
        responseType: 'workspace.setup.subscribe.response',
        onEvent: _receive,
      );
      if (!mounted || generation != _generation) {
        await observation.close();
        return;
      }
      _observation = observation;
      _receive(observation.snapshot);
      await _loadScripts();
    } catch (error) {
      if (mounted && generation == _generation) setState(() => _error = error);
    } finally {
      if (mounted && generation == _generation) {
        setState(() => _loading = false);
      }
    }
  }

  Future<void> _loadScripts() async {
    final response = await _client!.request('workspace.script.list.request', {
      'workspaceId': widget.target.workspaceId,
    }, responseType: 'workspace.script.list.response');
    requireWorkspaceSuccess(response);
    if (mounted) {
      setState(
        () => _scripts = response.array('scripts').map(jsonMap).toList(),
      );
    }
  }

  Future<void> _action(Future<void> Function() action) async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await action();
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Original `BlockedSetupNotice.runSetup` / retry: runs immediately; a
  /// failure shows under the notice (fallback `blocked.runFailed`).
  Future<void> _run() async {
    await _action(() async {
      final response = await _client!.request('workspace.setup.run.request', {
        'workspaceId': widget.target.workspaceId,
      }, responseType: 'workspace.setup.run.response');
      requireWorkspaceSuccess(response);
      if (response['started'] != true) {
        throw StateError(
          'Setup did not start. Refresh its status before retrying.',
        );
      }
    });
  }

  String _runError(Object error) {
    final message = errorMessage(error);
    return message.trim().isEmpty
        ? tr(context, 'workspace.setup.blocked.runFailed')
        : message;
  }

  void _openExecutionLog(String id, String title) {
    PaseoScope.of(context).open(
      PanelTarget(
        kind: 'execution_log',
        serverId: widget.target.serverId,
        workspaceId: widget.target.workspaceId,
        cwd: widget.target.cwd,
        id: id,
        title: title,
      ),
    );
  }

  @override
  void dispose() {
    _generation++;
    _observation?.close();
    _events?.cancel();
    super.dispose();
  }

  /// Original `resolveAutoExpandIndex`: the running command, else the last.
  static int? _autoExpandIndex(List<JsonMap> commands) {
    for (final command in commands) {
      if (command.text('status') == 'running') return command.integer('index');
    }
    return commands.isEmpty ? null : commands.last.integer('index');
  }

  void _toggle(int index, bool autoExpanded) => setState(() {
    if (_expanded.contains(index) || autoExpanded) {
      _expanded.remove(index);
      if (autoExpanded) _collapsed.add(index);
    } else {
      _expanded.add(index);
      _collapsed.remove(index);
    }
  });

  /// Original `formatDuration`.
  static String _duration(int ms) {
    if (ms < 1000) return '${ms}ms';
    final seconds = ms ~/ 1000;
    if (seconds < 60) return '${seconds}s';
    return '${seconds ~/ 60}m ${seconds % 60}s';
  }

  /// Original `processCarriageReturns`: progress-bar output keeps only the
  /// last CR-delimited segment of each line.
  static String _processLog(String text) => !text.contains('\r')
      ? text
      : text.split('\n').map((line) => line.split('\r').last).join('\n');

  /// Original `errorCard`: 12px, red-100 fill, red-800 base text.
  Widget _errorCard(String message) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    color: const Color(0xfffee2e2),
    child: SelectableText(
      message,
      style: TextStyle(
        color: const Color(0xff991b1b),
        fontSize: PaseoTypography.of(context).uiSize(14),
      ),
    ),
  );

  /// Original `SetupCommandRow`: an 8px-radius bordered item; the surface1
  /// header (12/8 insets, 8px gaps) holds an 18px status slot, the base
  /// one-line command, the sm muted duration and a 14px chevron turning 90°
  /// when open; the surface0 detail scrolls the mono log (≤400px, 12px).
  Widget _commandRow(JsonMap command, int? autoIndex, String? snapshotError) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final index = command.integer('index');
    final status = command.text('status');
    final log = command.text('log');
    final hasLog = log.trim().isNotEmpty;
    final hasError =
        status == 'failed' && snapshotError != null && snapshotError.isNotEmpty;
    final expandable = status != 'running' || hasLog || hasError;
    final autoExpanded = index == autoIndex && !_collapsed.contains(index);
    final open = _expanded.contains(index) || autoExpanded;
    final Widget statusIcon = switch (status) {
      'running' => SizedBox.square(
        dimension: 14,
        child: CircularProgressIndicator(
          strokeWidth: 2,
          color: theme.colorScheme.onSurface,
        ),
      ),
      'completed' => const Icon(
        LucideIcons.circleCheck300,
        size: 14,
        color: Color(0xff22c55e),
      ),
      _ => const Icon(
        LucideIcons.circleAlert300,
        size: 14,
        color: Color(0xffef4444),
      ),
    };
    return DecoratedBox(
      decoration: BoxDecoration(
        border: Border.all(color: theme.dividerColor),
        borderRadius: BorderRadius.circular(8),
      ),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(7),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Semantics(
              button: true,
              expanded: open,
              child: _PressOpacity(
                onTap: expandable ? () => _toggle(index, autoExpanded) : null,
                child: Container(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 12,
                    vertical: 8,
                  ),
                  decoration: BoxDecoration(
                    color: colors.surface1,
                    border: open
                        ? Border(bottom: BorderSide(color: theme.dividerColor))
                        : null,
                  ),
                  child: Row(
                    spacing: 8,
                    children: [
                      SizedBox.square(
                        dimension: 18,
                        child: Center(child: statusIcon),
                      ),
                      Expanded(
                        child: Text(
                          command.text('command'),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodyMedium?.copyWith(
                            fontSize: type.uiSize(14),
                            color: theme.colorScheme.onSurface,
                          ),
                        ),
                      ),
                      if (command['durationMs'] != null)
                        Text(
                          _duration(intValue(command['durationMs'])),
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontSize: type.uiSize(12),
                            color: colors.muted,
                          ),
                        ),
                      // Original `chevronExpanded`: an instant 90° turn.
                      Transform.rotate(
                        angle: open ? 1.5707963267948966 : 0,
                        child: Icon(
                          LucideIcons.chevronRight300,
                          size: 14,
                          color: colors.muted,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
            if (open)
              ColoredBox(
                color: colors.surface0,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Semantics(
                      label: tr(context, 'workspace.setup.accessibility.log'),
                      child: hasLog
                          ? ConstrainedBox(
                              constraints: const BoxConstraints(maxHeight: 400),
                              child: SingleChildScrollView(
                                padding: const EdgeInsets.all(12),
                                child: SelectableText(
                                  _processLog(log),
                                  style: type.codeStyle.copyWith(
                                    height: 20 / type.codeFontSize,
                                    color: theme.colorScheme.onSurface,
                                  ),
                                ),
                              ),
                            )
                          : Padding(
                              padding: const EdgeInsets.all(12),
                              child: Text(
                                tr(context, 'workspace.setup.log.noOutput'),
                                style: TextStyle(
                                  fontSize: type.uiSize(14),
                                  color: colors.muted,
                                  fontStyle: FontStyle.italic,
                                ),
                              ),
                            ),
                    ),
                    if (hasError) _errorCard(snapshotError),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }

  /// Original workspace script row: base name with the muted state hint and
  /// ghost sm actions.
  Widget _scriptRow(JsonMap script) {
    final name = script.text('scriptName');
    final running = script.text('lifecycle') == 'running';
    final url = script.text(
      'proxyUrl',
      script.text('publicProxyUrl', script.text('localProxyUrl')),
    );
    return SettingsRow(
      label: name,
      labelMaxLines: 1,
      hint: [
        if (script.text('type').isNotEmpty) script.text('type'),
        if (script.text('lifecycle').isNotEmpty) script.text('lifecycle'),
        if (script.text('health').isNotEmpty) script.text('health'),
        if (script['exitCode'] != null)
          tr(
            context,
            'workspace.scripts.states.exitCode',
            args: {'code': '${script['exitCode']}'},
          ),
      ].join(' · '),
      control: Wrap(
        spacing: 4,
        runSpacing: 4,
        children: [
          PaseoButton(
            label: tr(
              context,
              running
                  ? 'workspace.scripts.actions.stop'
                  : 'workspace.scripts.actions.run',
            ),
            icon: running ? LucideIcons.square300 : LucideIcons.play300,
            variant: PaseoButtonVariant.ghost,
            size: PaseoControlSize.sm,
            onPressed: _busy
                ? null
                : () => _action(() async {
                    final verb = running ? 'stop' : 'start';
                    final result = await _client!.request(
                      'workspace.script.$verb.request',
                      {
                        'workspaceId': widget.target.workspaceId,
                        'scriptName': name,
                      },
                      responseType: 'workspace.script.$verb.response',
                    );
                    requireWorkspaceSuccess(result);
                    await _loadScripts();
                  }),
          ),
          if (script.text('terminalId').isNotEmpty)
            PaseoButton(
              label: tr(context, 'workspace.scripts.actions.view'),
              icon: LucideIcons.squareTerminal300,
              variant: PaseoButtonVariant.ghost,
              size: PaseoControlSize.sm,
              onPressed: () =>
                  _openExecutionLog(script.text('terminalId'), name),
            ),
          if (url.isNotEmpty)
            PaseoButton(
              label: tr(context, 'workspace.scripts.actions.openService'),
              icon: LucideIcons.externalLink300,
              variant: PaseoButtonVariant.ghost,
              size: PaseoControlSize.sm,
              onPressed: () => _action(() async {
                final uri = Uri.parse(url);
                if (!const ['http', 'https'].contains(uri.scheme) ||
                    !await launchUrl(
                      uri,
                      mode: LaunchMode.externalApplication,
                    )) {
                  throw StateError(
                    'Could not open service URL in the system browser.',
                  );
                }
              }),
            ),
        ],
      ),
    );
  }

  /// Original `panels/setup-panel.tsx`: a surface0 scroll with 16px insets;
  /// the blocked notice, the waiting spinner, the empty message, or the
  /// command list (8px gaps) with any top-level error card.
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final snapshot = _snapshot;
    final detail = snapshot?.object('detail') ?? <String, dynamic>{};
    final status = snapshot?.text('status') ?? '';
    final commands = detail.array('commands').map(jsonMap).toList();
    final snapshotError = snapshot?.text('error');
    final waiting =
        snapshot == null || (status == 'running' && commands.isEmpty);
    final muted = theme.textTheme.bodyMedium?.copyWith(
      fontSize: type.uiSize(14),
      color: colors.muted,
    );
    final Widget body;
    if (_error != null && snapshot == null) {
      body = WorkspaceAlert(
        title: errorMessage(_error!),
        large: true,
        actions: [
          PaseoButton(
            label: tr(context, 'common.actions.retry'),
            variant: PaseoButtonVariant.outline,
            size: PaseoControlSize.sm,
            onPressed: _loading ? null : _load,
          ),
        ],
      );
    } else if (status == 'blocked' && snapshot!['blockedSource'] != null) {
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          WorkspaceAlert(
            warning: true,
            title: tr(context, 'workspace.setup.blocked.title'),
            description: tr(
              context,
              'workspace.setup.blocked.description',
              args: {
                'repository': snapshot
                    .object('blockedSource')
                    .text('headRepository'),
              },
            ),
            actions: [
              PaseoButton(
                label: tr(context, 'workspace.setup.blocked.run'),
                variant: PaseoButtonVariant.primary,
                size: PaseoControlSize.sm,
                loading: _busy,
                onPressed: _busy ? null : _run,
              ),
            ],
          ),
          if (_error != null) WorkspaceAlert(description: _runError(_error!)),
        ],
      );
    } else if (waiting) {
      body = Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          spacing: 12,
          children: [
            // Original `LoadingSpinner size="large"` (36px ActivityIndicator).
            SizedBox.square(
              dimension: 36,
              child: CircularProgressIndicator(
                strokeWidth: 3,
                color: colors.muted,
              ),
            ),
            Text(tr(context, 'workspace.setup.waiting'), style: muted),
          ],
        ),
      );
    } else if (status == 'completed' && commands.isEmpty) {
      body = Center(
        child: Semantics(
          label: tr(context, 'workspace.setup.accessibility.noCommands'),
          child: Text(
            tr(context, 'workspace.setup.empty.noCommands'),
            textAlign: TextAlign.center,
            style: muted,
          ),
        ),
      );
    } else {
      final autoIndex = _autoExpandIndex(commands);
      body = Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 8,
        children: [
          for (final command in commands)
            KeyedSubtree(
              key: ValueKey('${command['index']}:${command['command']}'),
              child: _commandRow(command, autoIndex, snapshotError),
            ),
          if (commands.isEmpty && detail.text('log').isNotEmpty)
            Container(
              padding: const EdgeInsets.all(12),
              decoration: BoxDecoration(
                color: colors.surface1,
                border: Border.all(color: theme.dividerColor),
                borderRadius: BorderRadius.circular(8),
              ),
              child: SelectableText(
                _processLog(detail.text('log')),
                style: type.codeStyle.copyWith(
                  height: 20 / type.codeFontSize,
                  color: theme.colorScheme.onSurface,
                ),
              ),
            ),
          if (snapshotError != null &&
              snapshotError.isNotEmpty &&
              !commands.any((c) => c.text('status') == 'failed'))
            _errorCard(snapshotError),
          if (detail.flag('truncated'))
            Text(
              'Earlier setup output was truncated by the host.',
              style: theme.textTheme.bodySmall?.copyWith(
                fontSize: type.uiSize(12),
                color: colors.muted,
              ),
            ),
          if (status == 'failed')
            Align(
              alignment: Alignment.centerLeft,
              child: PaseoButton(
                label: tr(context, 'common.actions.retry'),
                icon: LucideIcons.rotateCw300,
                variant: PaseoButtonVariant.outline,
                size: PaseoControlSize.sm,
                loading: _busy,
                onPressed: _busy ? null : _run,
              ),
            ),
          if (_error != null) WorkspaceAlert(description: _runError(_error!)),
        ],
      );
    }
    final scripts = _scripts.isEmpty
        ? null
        : Padding(
            padding: const EdgeInsets.only(top: 24),
            child: SettingsSection(
              title: tr(context, 'workspace.scripts.title'),
              flush: true,
              children: [for (final script in _scripts) _scriptRow(script)],
            ),
          );
    final centered = waiting || (status == 'completed' && commands.isEmpty);
    return ColoredBox(
      color: colors.surface0,
      child: LayoutBuilder(
        builder: (context, constraints) {
          final available = (constraints.maxHeight - 32).clamp(
            0.0,
            double.infinity,
          );
          return SingleChildScrollView(
            padding: EdgeInsets.fromLTRB(
              16,
              16,
              16,
              16 + MediaQuery.paddingOf(context).bottom,
            ),
            child: Semantics(
              label: switch (status) {
                'running' => tr(context, 'workspace.setup.status.running'),
                'completed' => tr(context, 'workspace.setup.status.completed'),
                'failed' => tr(context, 'workspace.setup.status.failed'),
                'blocked' => tr(context, 'workspace.setup.status.blocked'),
                _ => tr(context, 'workspace.setup.status.waiting'),
              },
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  // The waiting and empty states centre in the free space
                  // (original `flex: 1` containers).
                  if (centered)
                    SizedBox(
                      height: scripts == null ? available : available / 2,
                      child: body,
                    )
                  else
                    body,
                  ?scripts,
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}

/// Original pressable command header: 0.8 opacity while pressed.
class _PressOpacity extends StatefulWidget {
  const _PressOpacity({required this.child, this.onTap});
  final Widget child;
  final VoidCallback? onTap;
  @override
  State<_PressOpacity> createState() => _PressOpacityState();
}

class _PressOpacityState extends State<_PressOpacity> {
  bool _pressed = false;
  @override
  Widget build(BuildContext context) => MouseRegion(
    cursor: widget.onTap == null
        ? SystemMouseCursors.basic
        : SystemMouseCursors.click,
    child: GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: widget.onTap,
      onTapDown: widget.onTap == null
          ? null
          : (_) => setState(() => _pressed = true),
      onTapUp: (_) => setState(() => _pressed = false),
      onTapCancel: () => setState(() => _pressed = false),
      child: Opacity(opacity: _pressed ? .8 : 1, child: widget.child),
    ),
  );
}
