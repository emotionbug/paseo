import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/provider_icon.dart';
import 'settings_forms.dart';
import 'settings_host_widgets.dart';
import 'profile_settings.dart';
import 'provider_catalog.dart';
import 'usage_settings.dart';

/// Protocol `PROMPT_SENTINEL`: where a typed prompt goes in a profile.
const _promptSentinel = '{{{prompt}}}';

/// Protocol `DEFAULT_TERMINAL_PROFILES`, shown until the host persists a list.
const defaultTerminalProfiles = <JsonMap>[
  {'id': 'claude', 'name': 'Claude Code', 'command': 'claude', 'args': [_promptSentinel], 'icon': 'claude'},
  {'id': 'codex', 'name': 'Codex', 'command': 'codex', 'args': [_promptSentinel], 'icon': 'codex'},
  {
    'id': 'opencode',
    'name': 'OpenCode',
    'command': 'opencode',
    'args': ['--prompt=$_promptSentinel'],
    'icon': 'opencode',
  },
  {'id': 'pi', 'name': 'Pi', 'command': 'pi', 'args': [_promptSentinel], 'icon': 'pi'},
];

/// Protocol `getTerminalProfileIcon`: the explicit icon, else a provider glyph
/// whose name equals the command's base name (`cursor-agent` → `cursor`).
String? terminalProfileIcon(JsonMap profile) {
  final explicit = stringValue(profile['icon']);
  if (explicit.isNotEmpty) return explicit;
  final command = stringValue(profile['command']);
  final start = math.max(command.lastIndexOf('/'), command.lastIndexOf(r'\')) + 1;
  var base = command.substring(start).toLowerCase();
  final dot = base.indexOf('.');
  if (dot > 0) base = base.substring(0, dot);
  if (base == 'cursor-agent') return 'cursor';
  return ProviderIcon.svgFor(base) == null ? null : base;
}

/// The host settings pages backed by the daemon config (original
/// `screens/settings/host-page.tsx` sections).
class DaemonSettings extends StatefulWidget {
  final HostState host;
  final String section;
  const DaemonSettings({
    super.key,
    required this.host,
    this.section = 'host',
  });
  @override
  State<DaemonSettings> createState() => _DaemonSettingsState();
}

class _DaemonSettingsState extends State<DaemonSettings> {
  JsonMap config = {};
  List<JsonMap> providers = [];
  Object? error;
  bool loading = true, busy = false, disconnected = false;
  StreamSubscription<JsonMap>? subscription;
  int generation = 0;
  late PaseoClient boundClient;
  late ConnectionStatus connectionStatus;

  /// Republished on every rebuild so open provider sheets follow the page.
  final providerSnapshot = ValueNotifier<ProviderSettingsSnapshot>((
    config: const {},
    providers: const [],
  ));
  PaseoClient get client => boundClient;
  bool get manage =>
      !client.serverInfo.containsKey('permissions') ||
      jsonList(client.serverInfo['permissions']).contains('daemon.manage');
  bool feature(String key) =>
      jsonMap(client.serverInfo['features'])[key] == true;

  @override
  void setState(VoidCallback fn) {
    super.setState(fn);
    providerSnapshot.value = (config: config, providers: providers);
  }

  @override
  void initState() {
    super.initState();
    bindClient();
  }

  void bindClient() {
    boundClient = widget.host.client;
    connectionStatus = client.status;
    client.addListener(connectionChanged);
    final observedClient = client;
    subscription = client.events.listen((event) {
      if (!mounted ||
          !identical(observedClient, client) ||
          client.status != ConnectionStatus.connected) {
        return;
      }
      final payload = jsonMap(event['payload']);
      if (event['type'] == 'status' &&
          payload['status'] == 'daemon_config_changed') {
        setState(() => config = jsonMap(payload['config']));
      }
      if (event['type'] == 'providers_snapshot_update' &&
          payload['cwd'] == null) {
        if (payload.containsKey('entries') ||
            payload.containsKey('compactSnapshot')) {
          setState(() => providers = providerEntries(payload));
        } else {
          unawaited(reloadProviders().catchError((Object _) {}));
        }
      }
    });
    load();
  }

  void connectionChanged() {
    if (!mounted || connectionStatus == client.status) return;
    connectionStatus = client.status;
    load();
  }

  @override
  void didUpdateWidget(DaemonSettings old) {
    super.didUpdateWidget(old);
    if (!identical(boundClient, widget.host.client)) {
      boundClient.removeListener(connectionChanged);
      subscription?.cancel();
      ++generation;
      bindClient();
    }
  }

  @override
  void dispose() {
    ++generation;
    boundClient.removeListener(connectionChanged);
    subscription?.cancel();
    providerSnapshot.dispose();
    super.dispose();
  }

  Future<void> load() async {
    if (!mounted) return;
    final own = ++generation;
    if (client.status != ConnectionStatus.connected) {
      setState(() {
        loading = false;
        disconnected = true;
        error = null;
        config = {};
        providers = [];
      });
      return;
    }
    final observedClient = client;
    setState(() {
      loading = true;
      disconnected = false;
      error = null;
    });
    try {
      final results = await Future.wait([
        settingsRequest(
          observedClient,
          'get_daemon_config_request',
          {},
          responseType: 'get_daemon_config_response',
        ),
        settingsRequest(
          observedClient,
          'get_providers_snapshot_request',
          {},
          responseType: 'get_providers_snapshot_response',
        ),
      ]);
      if (mounted && own == generation) {
        setState(() {
          config = jsonMap(results[0]['config']);
          providers = providerEntries(results[1]);
          loading = false;
        });
      }
    } catch (e) {
      if (mounted && own == generation) {
        setState(() {
          error = e;
          loading = false;
        });
      }
    }
  }

  /// Re-reads the provider snapshot without the page-level loading state.
  Future<void> reloadProviders() async {
    final own = generation;
    final snapshot = await settingsRequest(
      client,
      'get_providers_snapshot_request',
      {},
      responseType: 'get_providers_snapshot_response',
    );
    if (mounted && own == generation) {
      setState(() => providers = providerEntries(snapshot));
    }
  }

  /// Original `useProvidersSnapshot().refresh(providers)`.
  Future<void> refreshProviders(List<String> ids) async {
    await settingsRequest(
      client,
      'refresh_providers_snapshot_request',
      {'providers': ids},
      responseType: 'refresh_providers_snapshot_response',
    );
    await reloadProviders();
  }

  Future<void> patch(JsonMap value) async {
    if (!manage) {
      throw StateError(
        'This connection does not have daemon.manage permission.',
      );
    }
    final own = generation;
    final result = await settingsRequest(client, 'set_daemon_config_request', {
      'config': value,
    }, responseType: 'set_daemon_config_response');
    if (mounted && own == generation) {
      setState(() => config = jsonMap(result['config']));
    }
  }

  Future<void> run(Future<void> Function() operation) async {
    if (busy) return;
    setState(() => busy = true);
    try {
      await operation();
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => busy = false);
    }
  }

  Widget toggle(
    String key,
    String label, {
    String? group,
    String? hint,
    bool fallback = false,
  }) {
    final values = group == null ? config : jsonMap(config[group]);
    return SettingsSwitchRow(
      label: label,
      hint: hint,
      value: values[key] is bool ? values[key] as bool : fallback,
      onChanged: busy || !manage
          ? null
          : (v) => run(
              () => patch(
                group == null
                    ? {key: v}
                    : {
                        group: {key: v},
                      },
              ),
            ),
    );
  }

  Future<void> providerForm(JsonMap p) async {
    final id = stringValue(p['provider']);
    final existing = jsonMap(jsonMap(config['providers'])[id]);
    await editSettingsForm(
      context,
      title: '$id configuration',
      fields: [
        SettingField(
          'label',
          'Display name',
          value: existing['label'] ?? p['label'],
        ),
        SettingField(
          'command',
          'Command and arguments (one per line)',
          value: jsonList(existing['command']).join('\n'),
          multiline: true,
          parse: textLines,
        ),
        SettingField(
          'env',
          'Environment variables (NAME=value, one per line)',
          value: jsonMap(existing['env']).entries
              .map((e) => '${e.key}=${e.value}')
              .join('\n'),
          multiline: true,
          parse: (v) {
            final result = <String, String>{};
            for (final line in textLines(v)) {
              final at = line.indexOf('=');
              if (at < 1) throw const FormatException('Use NAME=value');
              result[line.substring(0, at)] = line.substring(at + 1);
            }
            return result;
          },
        ),
        SettingField(
          'paseoTools',
          'Enable Paseo tools for this provider',
          value: jsonMap(existing['paseoTools'])['enabled'] != false,
        ),
        SettingField(
          'disabledTools',
          'Disabled Paseo tool IDs (one per line)',
          value: jsonList(jsonMap(existing['paseoTools'])['disabledTools'])
              .join('\n'),
          multiline: true,
          parse: textLines,
        ),
      ],
      onSave: (values) async {
        await patch({
          'providers': {
            id: {
              ...existing,
              'label': values['label'],
              if ((values['command'] as List).isNotEmpty)
                'command': values['command'],
              'env': values['env'],
              'paseoTools': {
                ...jsonMap(existing['paseoTools']),
                'enabled': values['paseoTools'],
                'disabledTools': values['disabledTools'],
              },
            },
          },
        });
      },
    );
  }

  /// Original `AppendSystemPromptCard` sheet (560px): the text-area card over
  /// ghost Reset and default Save sm buttons, end-aligned, 8px apart.
  Future<void> editSystemPrompt() => showAdaptiveSheet<void>(
    context: context,
    maxWidth: 560,
    builder: (context) => _SystemPromptSheet(
      initial: stringValue(config['appendSystemPrompt']),
      onSave: (value) => patch({'appendSystemPrompt': value}),
    ),
  );

  Future<void> editTerminalProfile(
    List<JsonMap> profiles, [
    JsonMap? profile,
  ]) => showAdaptiveSheet<void>(
    context: context,
    maxWidth: 480,
    builder: (context) => _TerminalProfileSheet(
      profile: profile,
      onSave: (draft) async {
        final args = draft.args.trim().isEmpty
            ? null
            : draft.args.trim().split(RegExp(r'\s+'));
        JsonMap apply(JsonMap base) => {
          ...base,
          'name': draft.name,
          'command': draft.command,
          'args': ?args,
        }..removeWhere((key, value) => key == 'args' && args == null);
        final next = profile == null
            ? [
                ...profiles,
                apply({
                  'id':
                      'profile_${DateTime.now().millisecondsSinceEpoch.toRadixString(36)}_${math.Random().nextInt(1 << 32).toRadixString(36)}',
                }),
              ]
            : [
                for (final p in profiles) p['id'] == profile['id'] ? apply(p) : p,
              ];
        await patch({'terminalProfiles': next});
      },
    ),
  );

  Future<void> saveTerminalProfiles(List<JsonMap> next) async {
    try {
      await patch({'terminalProfiles': next});
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  @override
  Widget build(BuildContext context) {
    if (error != null) return ErrorState(error: error!, onRetry: load);
    // The providers section keeps its original "Loading..." card.
    if (loading && widget.section != 'providers') {
      return const SizedBox(
        height: 180,
        child: Center(child: SettingsSpinner(size: 24)),
      );
    }
    return switch (widget.section) {
      'agents' => _agents(context),
      'workspaces' => _workspaces(context),
      'host' => _host(context),
      'providers' => ProvidersSection(
        client: client,
        snapshot: providerSnapshot,
        connected: !disconnected,
        loading: loading,
        supportsHoplite: feature('hopliteProvider'),
        supportsRemoval: feature('providerRemoval'),
        patch: patch,
        refresh: refreshProviders,
        onConfigure: manage ? providerForm : null,
      ),
      'metadata' when manage && !disconnected =>
        MetadataSettings(client: client, config: config, patch: patch),
      'terminals' => _terminals(context),
      'connections' when !disconnected => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (feature('connectedHosts')) ConnectedHostsSettings(client: client),
          HubAccessSettings(client: client),
        ],
      ),
      'usage' => UsageSettings(client: client, serverId: widget.host.serverId),
      _ => const SizedBox.shrink(),
    };
  }

  /// Original `HostAgentsPage`: the agents section's separate tool and
  /// system-prompt cards (or the unavailable card), skills, then profiles.
  Widget _agents(BuildContext context) {
    String t(String key) => tr(context, key);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (disconnected)
          SettingsEmptyCard(t('settings.host.agents.unavailable'))
        else
          SettingsSection(
            title: t('settings.hostSections.agents'),
            card: false,
            children: [
              SettingsCard(
                children: [
                  toggle(
                    'injectIntoAgents',
                    t('settings.host.orchestration.enableTools.title'),
                    hint: t('settings.host.orchestration.enableTools.hint'),
                    group: 'mcp',
                    fallback: true,
                  ),
                ],
              ),
              SettingsCard(
                children: [
                  SettingsActionRow(
                    label: t('settings.host.orchestration.systemPrompt.title'),
                    hint: t('settings.host.orchestration.systemPrompt.hint'),
                    actionLabel: t('settings.host.orchestration.systemPrompt.edit'),
                    onPressed: manage ? editSystemPrompt : null,
                  ),
                ],
              ),
            ],
          ),
        SkillsSettings(
          client: client,
          canManage: manage,
          connected: !disconnected,
          supported: feature('skillManagement'),
        ),
        if (!disconnected && manage && feature('agentProfiles'))
          ProfileSettings(
            client: client,
            config: config,
            patch: patch,
            providers: providers,
          ),
      ],
    );
  }

  /// Original `HostWorkspacesPage`.
  Widget _workspaces(BuildContext context) {
    if (disconnected) {
      return SettingsEmptyCard(tr(context, 'settings.host.workspaces.unavailable'));
    }
    return SettingsSection(
      title: tr(context, 'settings.hostSections.workspaces'),
      children: [
        toggle(
          'autoArchiveAfterMerge',
          'Archive merged PR workspaces',
          hint:
              'Automatically archive clean Paseo workspaces after their pull request is merged',
        ),
      ],
    );
  }

  /// Host configuration and diagnostics rows (not part of the original host
  /// page), in the original settings row/card styles.
  Widget _host(BuildContext context) {
    String t(String key) => tr(context, key);
    if (disconnected) return const SizedBox.shrink();
    Future<void> report(
      String type,
      String response,
      String title, [
      String? field,
    ]) => run(() async {
      final result = await settingsRequest(client, type, {}, responseType: response);
      if (context.mounted) {
        await showSettingsReport(context, title, field == null ? result : result[field]);
      }
    });
    return SettingsSection(
      title: 'Host configuration',
      children: [
        SettingsRow(
          label: 'Server ID',
          hint: [
            widget.host.serverId,
            if (client.serverInfo.containsKey('permissions'))
              'Granted permissions: ${jsonList(client.serverInfo['permissions']).join(', ')}',
            if (!manage) 'This connection is read-only for daemon configuration.',
          ].join('\n'),
          control: settingsIconButton(
            context,
            LucideIcons.rotateCw300,
            load,
            label: t('usage.refresh'),
          ),
        ),
        SettingsActionRow(
          label: 'Host diagnostics',
          actionLabel: t('settings.diagnostics.app.run'),
          onPressed: busy
              ? null
              : () => report(
                  'diagnostics.request',
                  'diagnostics.response',
                  'Host diagnostics',
                  'diagnostic',
                ),
        ),
        SettingsActionRow(
          label: t('desktop.daemon.fullStatus.modalTitle'),
          actionLabel: t('desktop.daemon.fullStatus.view'),
          onPressed: busy
              ? null
              : () => report(
                  'daemon.get_status.request',
                  'daemon.get_status.response',
                  t('desktop.daemon.fullStatus.modalTitle'),
                ),
        ),
        toggle('enabled', 'Relay enabled', group: 'relay'),
        if (jsonMap(config['relay']).containsKey('requireProtectedSecurity'))
          toggle(
            'requireProtectedSecurity',
            'Require protected relay security',
            group: 'relay',
          ),
        SettingsActionRow(
          label: 'Network and Git limits',
          hint: 'Allowed hostnames, trusted proxies, CORS origins and Git process limits.',
          actionLabel: t('settings.host.orchestration.systemPrompt.edit'),
          onPressed: !manage || busy ? null : editNetworkLimits,
        ),
        SettingsActionRow(
          label: 'Reload configuration from disk',
          hint: 'Shows paths applied, restart-required and controlled by overrides.',
          actionLabel: t('common.actions.reload'),
          onPressed: !manage || busy
              ? null
              : () => run(() async {
                  final result = await settingsRequest(
                    client,
                    'daemon.config.reload.request',
                    {},
                    responseType: 'daemon.config.reload.response',
                  );
                  if (context.mounted) {
                    await showSettingsReport(context, 'Configuration reload', result);
                  }
                  await load();
                }),
        ),
      ],
    );
  }

  Future<void> editNetworkLimits() => editSettingsForm(
    context,
    title: 'Network and Git limits',
    fields: [
      SettingField(
        'hostnames',
        'Allowed hostnames (one per line; * permits all)',
        value: config['hostnames'] == true
            ? '*'
            : jsonList(config['hostnames']).join('\n'),
        multiline: true,
        parse: (s) => s.trim() == '*' ? true : textLines(s),
      ),
      SettingField(
        'trustedProxies',
        'Trusted proxies (one per line; * trusts all)',
        value: config['trustedProxies'] == true
            ? '*'
            : jsonList(config['trustedProxies']).join('\n'),
        multiline: true,
        parse: (s) => s.trim() == '*' ? true : textLines(s),
      ),
      SettingField(
        'origins',
        'Allowed CORS origins',
        value: jsonList(jsonMap(config['cors'])['allowedOrigins']).join('\n'),
        multiline: true,
        parse: textLines,
      ),
      SettingField(
        'rate',
        'Git processes per second',
        value: jsonMap(config['git'])['maxProcessesPerSecond'] ?? 10,
        parse: positiveInteger,
      ),
      SettingField(
        'concurrency',
        'Concurrent Git processes',
        value: jsonMap(config['git'])['maxProcessConcurrency'] ?? 4,
        parse: positiveInteger,
      ),
      SettingField(
        'catalogRefreshTimeoutMs',
        'Catalog refresh timeout (ms)',
        value: config['catalogRefreshTimeoutMs'] ?? 60000,
        parse: positiveInteger,
      ),
    ],
    onSave: (v) => patch({
      'hostnames': v['hostnames'],
      'trustedProxies': v['trustedProxies'],
      'cors': {'allowedOrigins': v['origins']},
      'git': {
        'maxProcessesPerSecond': v['rate'],
        'maxProcessConcurrency': v['concurrency'],
      },
      'catalogRefreshTimeoutMs': v['catalogRefreshTimeoutMs'],
    }),
  );

  /// Original `HostTerminalsPage`: "Terminal agents" (title only while
  /// offline, as the original card renders nothing), then terminal profiles.
  Widget _terminals(BuildContext context) {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, 'settings.host.terminalProfiles.$key', args: args);
    final rawProfiles = config['terminalProfiles'];
    final profiles = (rawProfiles is List ? rawProfiles : defaultTerminalProfiles)
        .map(jsonMap)
        .toList();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SettingsSection(
          title: 'Terminal agents',
          card: !disconnected,
          children: [
            if (!disconnected)
              toggle(
                'enableTerminalAgentHooks',
                'Enable terminal agent hooks',
                hint:
                    'Get notifications and status from terminal agents. This installs hooks in your agent config files.',
              ),
          ],
        ),
        if (disconnected)
          SettingsEmptyCard(t('unavailable'))
        else
          SettingsSection(
            title: t('sectionTitle'),
            card: false,
            trailing: settingsIconButton(
              context,
              LucideIcons.plus300,
              manage ? () => editTerminalProfile(profiles) : null,
              label: t('addProfileTitle'),
            ),
            children: [
              if (profiles.isEmpty)
                SettingsEmptyCard(t('emptyState'))
              else
                SettingsRowsCard(
                  children: [
                    for (var i = 0; i < profiles.length; i++)
                      _TerminalProfileRow(
                        profile: profiles[i],
                        onMoveUp: !manage || i == 0
                            ? null
                            : () => saveTerminalProfiles(
                                [...profiles]..insert(i - 1, profiles[i])..removeAt(i + 1),
                              ),
                        onMoveDown: !manage || i == profiles.length - 1
                            ? null
                            : () => saveTerminalProfiles(
                                [...profiles]..insert(i + 2, profiles[i])..removeAt(i),
                              ),
                        onEdit: manage ? () => editTerminalProfile(profiles, profiles[i]) : null,
                        onRemove: !manage
                            ? null
                            : () async {
                                if (!await confirmAction(
                                  context,
                                  title: t('removeConfirmTitle'),
                                  message: t(
                                    'removeConfirmMessage',
                                    {'name': stringValue(profiles[i]['name'])},
                                  ),
                                  confirmLabel: t('remove'),
                                  destructive: true,
                                )) {
                                  return;
                                }
                                await saveTerminalProfiles(
                                  profiles.where((p) => p['id'] != profiles[i]['id']).toList(),
                                );
                              },
                      ),
                  ],
                ),
            ],
          ),
      ],
    );
  }
}

/// Original `TerminalProfileRow`: 56px settings row, 8px gaps, a 16px muted
/// profile glyph (provider icon or SquareTerminal), base name over a one-line
/// muted command hint, and four ghost sm icon buttons with no gap.
class _TerminalProfileRow extends StatelessWidget {
  const _TerminalProfileRow({
    required this.profile,
    this.onMoveUp,
    this.onMoveDown,
    this.onEdit,
    this.onRemove,
  });
  final JsonMap profile;
  final VoidCallback? onMoveUp, onMoveDown, onEdit, onRemove;
  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.terminalProfiles.$key');
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final args = jsonList(profile['args']);
    final command = [stringValue(profile['command']), ...args].join(' ');
    final icon = terminalProfileIcon(profile);
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 56),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Row(
          spacing: 8,
          children: [
            SizedBox(
              width: 16,
              child: Center(
                child: icon == null
                    ? Icon(LucideIcons.squareTerminal300, size: 16, color: colors.muted)
                    : ProviderIcon(provider: icon, size: 16, color: colors.muted),
              ),
            ),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.only(right: 12),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      stringValue(profile['name']),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
                    ),
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: Text(
                        command,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodySmall!.copyWith(
                          color: colors.muted,
                          fontSize: type.uiSize(12),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                settingsIconButton(context, LucideIcons.arrowUp300, onMoveUp, label: t('moveUp')),
                settingsIconButton(
                  context,
                  LucideIcons.arrowDown300,
                  onMoveDown,
                  label: t('moveDown'),
                ),
                settingsIconButton(context, LucideIcons.pencil300, onEdit, label: t('editProfile')),
                settingsIconButton(
                  context,
                  LucideIcons.trash2300,
                  onRemove,
                  label: t('remove'),
                  destructive: true,
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

typedef _ProfileDraft = ({String name, String command, String args});

/// Original `TerminalProfileEditModal` (480px): Name, Command and Arguments
/// fields (16px apart, required-field errors), a base red-300 submit error and
/// stretched secondary Cancel / default Save buttons 8px below.
class _TerminalProfileSheet extends StatefulWidget {
  const _TerminalProfileSheet({required this.profile, required this.onSave});
  final JsonMap? profile;
  final Future<void> Function(_ProfileDraft draft) onSave;
  @override
  State<_TerminalProfileSheet> createState() => _TerminalProfileSheetState();
}

class _TerminalProfileSheetState extends State<_TerminalProfileSheet> {
  late final name = TextEditingController(text: stringValue(widget.profile?['name']));
  late final command = TextEditingController(text: stringValue(widget.profile?['command']));
  late final args = TextEditingController(
    text: jsonList(widget.profile?['args']).join(' '),
  );
  final commandFocus = FocusNode(), argsFocus = FocusNode();
  String? nameError, commandError, submitError;
  bool pending = false;

  @override
  void dispose() {
    name.dispose();
    command.dispose();
    args.dispose();
    commandFocus.dispose();
    argsFocus.dispose();
    super.dispose();
  }

  String t(String key) => tr(context, 'settings.host.terminalProfiles.$key');

  Future<void> save() async {
    if (pending) return;
    setState(() {
      submitError = null;
      nameError = name.text.trim().isEmpty ? t('nameRequired') : null;
      commandError = command.text.trim().isEmpty ? t('commandRequired') : null;
    });
    if (nameError != null || commandError != null) return;
    setState(() => pending = true);
    try {
      await widget.onSave((name: name.text.trim(), command: command.text.trim(), args: args.text));
      if (mounted) Navigator.pop(context);
    } catch (e) {
      if (mounted) {
        final message = errorMessage(e);
        setState(
          () => submitError = message.isEmpty ? tr(context, 'common.errors.unableToSave') : message,
        );
      }
    } finally {
      if (mounted) setState(() => pending = false);
    }
  }

  Widget field(
    TextEditingController controller,
    String placeholder, {
    FocusNode? focus,
    bool autofocus = false,
    TextInputAction action = TextInputAction.next,
    ValueChanged<String>? onSubmitted,
    VoidCallback? onChanged,
  }) => TextField(
    controller: controller,
    focusNode: focus,
    autofocus: autofocus,
    enabled: !pending,
    autocorrect: false,
    textInputAction: action,
    style: Theme.of(context).textTheme.bodyMedium!.copyWith(
      fontSize: PaseoTypography.of(context).uiSize(14),
    ),
    decoration: paseoInputDecoration(context, hintText: placeholder),
    onChanged: onChanged == null ? null : (_) => onChanged(),
    onSubmitted: onSubmitted,
  );

  @override
  Widget build(BuildContext context) {
    final type = PaseoTypography.of(context);
    return SheetContents(
      title: t(widget.profile == null ? 'addProfileTitle' : 'editProfileTitle'),
      child: Padding(
        padding: const EdgeInsets.only(bottom: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 16,
          children: [
            PaseoFormField(
              label: t('nameLabel'),
              error: nameError,
              child: field(
                name,
                t('namePlaceholder'),
                autofocus: true,
                onChanged: () => setState(() => nameError = null),
                onSubmitted: (_) => commandFocus.requestFocus(),
              ),
            ),
            PaseoFormField(
              label: t('commandLabel'),
              error: commandError,
              child: field(
                command,
                t('commandPlaceholder'),
                focus: commandFocus,
                onChanged: () => setState(() => commandError = null),
                onSubmitted: (_) => argsFocus.requestFocus(),
              ),
            ),
            PaseoFormField(
              label: t('argsLabel'),
              hint: t('argsHint'),
              child: field(
                args,
                t('argsPlaceholder'),
                focus: argsFocus,
                action: TextInputAction.done,
                onSubmitted: (_) => save(),
              ),
            ),
            if (submitError != null)
              Text(
                submitError!,
                style: Theme.of(context).textTheme.bodyMedium!.copyWith(
                  color: paseoFieldErrorColor,
                  fontSize: type.uiSize(14),
                ),
              ),
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Row(
                spacing: 8,
                children: [
                  Expanded(
                    child: PaseoButton(
                      label: tr(context, 'common.actions.cancel'),
                      expand: true,
                      onPressed: pending ? null : () => Navigator.pop(context),
                    ),
                  ),
                  Expanded(
                    child: PaseoButton(
                      label: t(pending ? 'saving' : 'save'),
                      variant: PaseoButtonVariant.primary,
                      expand: true,
                      onPressed: pending ? null : save,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Original `AppendSystemPromptCard` editing sheet.
class _SystemPromptSheet extends StatefulWidget {
  const _SystemPromptSheet({required this.initial, required this.onSave});
  final String initial;
  final Future<void> Function(String value) onSave;
  @override
  State<_SystemPromptSheet> createState() => _SystemPromptSheetState();
}

class _SystemPromptSheetState extends State<_SystemPromptSheet> {
  late final controller = TextEditingController(text: widget.initial);
  bool saving = false;

  @override
  void dispose() {
    controller.dispose();
    super.dispose();
  }

  Future<void> save() async {
    setState(() => saving = true);
    try {
      await widget.onSave(controller.text);
      if (mounted) Navigator.pop(context);
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.orchestration.systemPrompt.$key');
    final changed = controller.text != widget.initial;
    return SheetContents(
      title: t('sheetTitle'),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          SettingsTextAreaCard(
            controller: controller,
            semanticLabel: t('accessibilityLabel'),
            placeholder: t('placeholder'),
            enabled: !saving,
            onChanged: (_) => setState(() {}),
          ),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            spacing: 8,
            children: [
              PaseoButton(
                label: t('reset'),
                size: PaseoControlSize.sm,
                variant: PaseoButtonVariant.ghost,
                onPressed: !changed || saving
                    ? null
                    : () => setState(() => controller.text = widget.initial),
              ),
              PaseoButton(
                label: t(saving ? 'saving' : 'save'),
                size: PaseoControlSize.sm,
                variant: PaseoButtonVariant.primary,
                onPressed: !changed || saving ? null : save,
              ),
            ],
          ),
        ],
      ),
    );
  }
}

bool _isReconnectFailure(Object error) =>
    error is DaemonException &&
    const {
      'DAEMON_CONNECTION_LOST',
      'DAEMON_REQUEST_TIMEOUT',
      'DAEMON_NOT_CONNECTED',
    }.contains(error.code);

/// Original `readSelectedWorker`: the worker status, refusing another daemon.
Future<JsonMap> _readSelectedWorker(PaseoClient client, String serverId) async {
  final status = await settingsRequest(
    client,
    'daemon.get_status.request',
    {},
    responseType: 'daemon.get_status.response',
  );
  if (stringValue(client.serverInfo['serverId'], serverId) != serverId) {
    throw StateError('Daemon identity changed');
  }
  return status;
}

/// Original `observeReplacement`: poll until a worker with a new PID answers.
/// Like the original it only reads status and lets the client's own reconnect
/// schedule bring the session back; an offline poll just retries in 250ms.
/// [client] is re-read every poll so a host whose client was replaced keeps
/// observing the live session.
Future<JsonMap> _observeReplacement(
  PaseoClient Function() client,
  String serverId,
  JsonMap previous,
) async {
  final deadline = DateTime.now().add(const Duration(minutes: 10));
  while (DateTime.now().isBefore(deadline)) {
    try {
      final current = await _readSelectedWorker(client(), serverId);
      if (current['pid'] != previous['pid']) return current;
    } catch (e) {
      if (!_isReconnectFailure(e)) rethrow;
    }
    await Future<void>.delayed(const Duration(milliseconds: 250));
  }
  throw StateError('Replacement worker could not be confirmed. Check daemon status and logs.');
}

/// Rebuilds a host card whenever its client's connection or server info moves.
mixin _HostClientListener<T extends StatefulWidget> on State<T> {
  PaseoClient get hostClient;
  late PaseoClient _listened;

  @override
  void initState() {
    super.initState();
    _listened = hostClient..addListener(_changed);
  }

  @override
  void didUpdateWidget(T oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!identical(_listened, hostClient)) {
      _listened.removeListener(_changed);
      _listened = hostClient..addListener(_changed);
    }
  }

  @override
  void dispose() {
    _listened.removeListener(_changed);
    super.dispose();
  }

  void _changed() {
    if (mounted) setState(() {});
  }
}

/// Original `RestartDaemonCard`: a settings card row with an outline sm
/// Restart button (RotateCw), disabled while restarting or offline.
class RestartDaemonCard extends StatefulWidget {
  const RestartDaemonCard({super.key, required this.host});
  final HostState host;
  @override
  State<RestartDaemonCard> createState() => _RestartDaemonCardState();
}

class _RestartDaemonCardState extends State<RestartDaemonCard>
    with _HostClientListener<RestartDaemonCard> {
  bool restarting = false;
  @override
  PaseoClient get hostClient => widget.host.client;

  Future<void> restart() async {
    String t(String key) => tr(context, 'settings.host.daemon.restart.$key');
    final client = hostClient;
    final serverId = widget.host.serverId;
    if (client.status != ConnectionStatus.connected) {
      showError(context, t('offlineMessage'));
      return;
    }
    final confirmed = await confirmAction(
      context,
      title: tr(
        context,
        'settings.host.daemon.restart.confirmTitle',
        args: {'name': widget.host.label},
      ),
      message: t('confirmMessage'),
      confirmLabel: t('confirm'),
      destructive: true,
    );
    if (!confirmed || !mounted) return;
    setState(() => restarting = true);
    try {
      final previous = await _readSelectedWorker(client, serverId);
      var acknowledged = false;
      try {
        await settingsRequest(
          client,
          'restart_server_request',
          {'reason': 'settings_daemon_restart_$serverId'},
          responseType: 'status',
        );
        acknowledged = true;
      } catch (e) {
        if (!_isReconnectFailure(e)) rethrow;
      }
      try {
        await _observeReplacement(() => widget.host.client, serverId, previous);
      } catch (e) {
        throw StateError('Restart acknowledged: $acknowledged. ${errorMessage(e)}');
      }
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => restarting = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.daemon.restart.$key');
    final connected = hostClient.status == ConnectionStatus.connected;
    return SettingsCard(
      children: [
        SettingsRow(
          label: t('title'),
          hint: t('hint'),
          control: PaseoButton(
            label: t(restarting ? 'restarting' : 'confirm'),
            leading: Icon(
              LucideIcons.rotateCw300,
              size: densityIcon(context, 'sm'),
              color: Theme.of(context).colorScheme.onSurface,
            ),
            size: PaseoControlSize.sm,
            variant: PaseoButtonVariant.outline,
            onPressed: restarting || !connected ? null : restart,
          ),
        ),
      ],
    );
  }
}

/// Original `UpdateDaemonCard`: shown only for a version mismatch on a
/// self-updating or desktop-managed daemon (or after an update completes);
/// an outline sm Update button (ArrowUpToLine) carrying the progress phase,
/// then a success or error inline alert.
class UpdateDaemonCard extends StatefulWidget {
  const UpdateDaemonCard({super.key, required this.host});
  final HostState host;
  @override
  State<UpdateDaemonCard> createState() => _UpdateDaemonCardState();
}

class _UpdateDaemonCardState extends State<UpdateDaemonCard>
    with _HostClientListener<UpdateDaemonCard> {
  static const _appVersion = String.fromEnvironment('PASEO_VERSION');

  /// `idle`, `updating`, `complete` or `failed`.
  String state = 'idle';
  String phase = '', workerVersion = '', failureTitle = '', failureMessage = '';
  StreamSubscription<JsonMap>? progress;

  @override
  PaseoClient get hostClient => widget.host.client;

  @override
  void dispose() {
    progress?.cancel();
    super.dispose();
  }

  static String? _normalize(Object? version) {
    final value = stringValue(version).trim();
    return value.isEmpty ? null : value.replaceFirst(RegExp('^v', caseSensitive: false), '');
  }

  void fail(String title, String message) => setState(() {
    state = 'failed';
    failureTitle = title;
    failureMessage = message;
  });

  Future<void> update() async {
    String t(String key, [Map<String, Object?> args = const {}]) =>
        tr(context, 'settings.host.daemon.update.$key', args: args);
    final client = hostClient;
    final serverId = widget.host.serverId;
    if (client.status != ConnectionStatus.connected) {
      fail(t('offlineTitle'), t('offlineMessage'));
      return;
    }
    final confirmed = await confirmAction(
      context,
      title: t('confirmTitle', {'name': widget.host.label}),
      message: t('confirmMessage'),
      confirmLabel: t('confirm'),
    );
    if (!confirmed || !mounted) return;
    final phases = {
      'starting': t('phaseStarting'),
      'downloading': t('phaseDownloading'),
      'installing': t('phaseInstalling'),
      'complete': t('phaseComplete'),
    };
    setState(() {
      state = 'updating';
      phase = phases['starting']!;
    });
    final requestId = 'settings_daemon_update_${serverId}_${DateTime.now().microsecondsSinceEpoch}';
    progress = client.events.listen((event) {
      if (event['type'] != 'daemon.update.progress') return;
      final payload = jsonMap(event['payload']);
      if (payload['requestId'] != requestId || !mounted) return;
      final next = phases[payload['phase']];
      if (next != null) setState(() => phase = next);
    });
    try {
      final previous = await _readSelectedWorker(client, serverId);
      final installed = await client.request(
        'daemon.update.request',
        {'requestId': requestId},
        responseType: 'daemon.update.response',
        timeout: const Duration(minutes: 10),
      );
      if (installed['success'] != true) {
        throw StateError(stringValue(installed['error'], 'Package installation failed'));
      }
      final newVersion = stringValue(installed['newVersion']);
      final String version;
      try {
        final worker = await _observeReplacement(
          () => widget.host.client,
          serverId,
          previous,
        );
        version = stringValue(
          widget.host.client.serverInfo['version'],
          stringValue(worker['version']),
        );
        if (version.isEmpty || newVersion.isEmpty || version != newVersion) {
          throw StateError(
            'Expected installed version ${newVersion.isEmpty ? 'unknown' : newVersion}; observed worker $version.',
          );
        }
      } catch (e) {
        throw StateError(
          'Package installed; replacement worker version was not confirmed. ${errorMessage(e)}',
        );
      }
      if (mounted) {
        setState(() {
          state = 'complete';
          workerVersion = version;
        });
      }
    } catch (e) {
      if (mounted) {
        fail(t('requestFailedTitle'), t('requestFailedMessage', {'error': errorMessage(e)}));
      }
    } finally {
      await progress?.cancel();
      progress = null;
    }
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, 'settings.host.daemon.update.$key');
    final info = hostClient.serverInfo;
    final desktopManaged = info['desktopManaged'] == true;
    final selfUpdate = jsonMap(info['features'])['daemonSelfUpdate'] == true;
    final app = _normalize(_appVersion), daemon = _normalize(info['version']);
    final mismatch = app != null && daemon != null && app != daemon;
    if (!(mismatch && (selfUpdate || desktopManaged)) && state != 'complete') {
      return const SizedBox.shrink();
    }
    final updating = state == 'updating';
    final connected = hostClient.status == ConnectionStatus.connected;
    // Original: a bare card directly above the danger-zone section.
    return SettingsRowsCard(
      children: [
          Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              SettingsRow(
                label: t('title'),
                hint: t(desktopManaged ? 'desktopManagedHint' : 'hint'),
                control: PaseoButton(
                  label: updating ? phase : t('confirm'),
                  leading: Icon(
                    LucideIcons.arrowUpToLine300,
                    size: densityIcon(context, 'sm'),
                    color: Theme.of(context).colorScheme.onSurface,
                  ),
                  size: PaseoControlSize.sm,
                  variant: PaseoButtonVariant.outline,
                  onPressed: desktopManaged || updating || !connected ? null : update,
                ),
              ),
              if (state == 'complete')
                SettingsInlineAlert(
                  success: true,
                  title: tr(
                    context,
                    'desktop.daemon.lifecycle.workerUpdated',
                    args: {'version': workerVersion},
                  ),
                  description: tr(context, 'desktop.daemon.lifecycle.supervisorRefresh'),
                ),
              if (state == 'failed')
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                  child: SettingsInlineAlert(title: failureTitle, description: failureMessage),
                ),
            ],
          ),
      ],
    );
  }
}

class ConnectedHostsSettings extends StatefulWidget {
  final PaseoClient client;
  const ConnectedHostsSettings({super.key, required this.client});
  @override
  State<ConnectedHostsSettings> createState() => _ConnectedHostsSettingsState();
}

/// Daemon-owned host registrations (not part of the original app), as a
/// settings section of pressable rows with destructive trash buttons.
class _ConnectedHostsSettingsState extends State<ConnectedHostsSettings> {
  List<JsonMap> hosts = [];
  Object? error;
  bool loading = true;
  @override
  void initState() {
    super.initState();
    load();
  }

  Future<void> load() async {
    setState(() => loading = true);
    try {
      final r = await settingsRequest(
        widget.client,
        'hosts.list.request',
        {},
        responseType: 'hosts.list.response',
      );
      if (mounted) {
        setState(() {
          hosts = jsonList(jsonMap(r['directory'])['hosts'])
              .map(jsonMap)
              .toList();
          error = null;
          loading = false;
        });
      }
    } catch (e) {
      if (mounted) {
        setState(() {
          error = e;
          loading = false;
        });
      }
    }
  }

  Future<void> edit([JsonMap? host]) async {
    await editSettingsForm(
      context,
      title: host == null ? 'Register connected host' : 'Edit connected host',
      description: 'These registrations belong to this daemon. They are separate from the app’s saved host connections.',
      fields: [
        SettingField('label', 'Label', value: host?['label']),
        SettingField(
          'connection',
          'Connection link or endpoint${host == null ? '' : ' (blank preserves existing)'}',
          required: host == null,
          secret: true,
        ),
        if (host != null)
          SettingField('enabled', 'Enabled', value: host['enabled'] != false),
        if (host != null)
          const SettingField(
            'replaceIdentity',
            'Explicitly replace registered daemon identity',
            value: false,
          ),
        SettingField(
          'metadata',
          'Metadata (key=value, one per line)',
          value: jsonMap(host?['metadata']).entries
              .map((e) => '${e.key}=${e.value}')
              .join('\n'),
          multiline: true,
          parse: (s) {
            final result = <String, String>{};
            for (final line in textLines(s)) {
              final i = line.indexOf('=');
              if (i < 1) throw const FormatException('Use key=value');
              result[line.substring(0, i)] = line.substring(i + 1);
            }
            return result;
          },
        ),
      ],
      onSave: (v) async {
        final fields = <String, dynamic>{...v};
        if (stringValue(fields['connection']).isEmpty) {
          fields.remove('connection');
        }
        await settingsRequest(
          widget.client,
          host == null ? 'hosts.add.request' : 'hosts.update.request',
          host == null
              ? {'input': fields}
              : {'registrationId': host['registrationId'], 'patch': fields},
          responseType: host == null
              ? 'hosts.add.response'
              : 'hosts.update.response',
        );
        await load();
      },
    );
  }

  Future<void> remove(JsonMap host) async {
    if (!await confirmAction(
      context,
      title: 'Remove connected host?',
      message: 'Remove this daemon-owned registration and its routes?',
      confirmLabel: tr(context, 'settings.host.connections.removeAction'),
      destructive: true,
    )) {
      return;
    }
    try {
      await settingsRequest(widget.client, 'hosts.remove.request', {
        'registrationId': host['registrationId'],
      }, responseType: 'hosts.remove.response');
      await load();
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  @override
  Widget build(BuildContext context) => SettingsSection(
    title: 'Daemon-connected hosts',
    info: 'Daemon-owned host routing registrations, not local app saved hosts.',
    card: false,
    trailing: Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        settingsIconButton(
          context,
          LucideIcons.rotateCw300,
          loading ? null : load,
          label: tr(context, 'usage.refresh'),
        ),
        settingsIconButton(context, LucideIcons.plus300, edit, label: 'Register host'),
      ],
    ),
    children: [
      if (error != null)
        SettingsInlineAlert(title: errorMessage(error!))
      else if (hosts.isEmpty)
        SettingsEmptyCard(loading ? tr(context, 'settings.providers.loading') : 'No connected hosts')
      else
        SettingsRowsCard(
          children: [
            for (final host in hosts)
              SettingsRow(
                label: stringValue(
                  host['label'],
                  stringValue(host['serverId'], stringValue(host['registrationId'])),
                ),
                hint: [
                  '${host['status']} · ${host['targetDescription']}',
                  ?jsonMap(host['error'])['message']?.toString(),
                ].join('\n'),
                onTap: () => edit(host),
                control: settingsIconButton(
                  context,
                  LucideIcons.trash2300,
                  () => remove(host),
                  label: tr(context, 'settings.host.connections.removeAction'),
                  destructive: true,
                ),
              ),
          ],
        ),
    ],
  );
}

/// Original `agent-skills/index.tsx` `AgentSkillsSection`: a Skills docs link
/// in the header, one card row with a 16px Blocks glyph beside the title, a
/// ghost sm Choose-skills glyph and the install / update / uninstall actions.
class SkillsSettings extends StatefulWidget {
  final PaseoClient client;
  final bool canManage, connected, supported;
  const SkillsSettings({
    super.key,
    required this.client,
    required this.canManage,
    required this.connected,
    required this.supported,
  });
  @override
  State<SkillsSettings> createState() => _SkillsSettingsState();
}

class _SkillsSettingsState extends State<SkillsSettings> {
  static const _docsUrl = 'https://paseo.sh/docs/skills';
  static const _opOrder = {'add': 0, 'update': 1, 'delete': 2};
  JsonMap? status;
  bool working = false;

  @override
  void initState() {
    super.initState();
    if (widget.connected && widget.supported) load();
  }

  @override
  void didUpdateWidget(SkillsSettings old) {
    super.didUpdateWidget(old);
    if (!identical(old.client, widget.client) ||
        (!old.connected && widget.connected) ||
        (!old.supported && widget.supported)) {
      if (widget.connected && widget.supported) load();
    }
  }

  String t(String key, [Map<String, Object?> args = const {}]) =>
      tr(context, 'settings.host.skills.$key', args: args);

  Future<void> load() async {
    try {
      final result = await settingsRequest(
        widget.client,
        'agent.skills.get_status.request',
        {},
        responseType: 'agent.skills.get_status.response',
      );
      if (mounted) setState(() => status = result);
    } catch (_) {
      if (mounted) showError(context, t('statusFailed'));
    }
  }

  Future<JsonMap> request(String name, [JsonMap fields = const {}]) => settingsRequest(
    widget.client,
    'agent.skills.$name.request',
    fields,
    responseType: 'agent.skills.$name.response',
  );

  Future<void> mutate(String name, String failure) async {
    if (working) return;
    setState(() => working = true);
    try {
      await request(name);
      await load();
    } catch (_) {
      if (mounted) showError(context, t(failure));
    } finally {
      if (mounted) setState(() => working = false);
    }
  }

  List<JsonMap> get maintenanceOps => jsonList(status?['ops'])
      .map(jsonMap)
      .where((op) => op['kind'] != 'delete')
      .toList();

  Future<void> reconcile() async {
    if (working) return;
    final ops = [...maintenanceOps]
      ..sort((a, b) {
        final kind = (_opOrder[a['kind']] ?? 0) - (_opOrder[b['kind']] ?? 0);
        return kind != 0
            ? kind
            : stringValue(a['name']).compareTo(stringValue(b['name']));
      });
    final confirmed = await confirmAction(
      context,
      title: t('updateTitle'),
      message: ops.isEmpty
          ? t('updateFallback')
          : [for (final op in ops) '${t('operations.${op['kind']}')} ${op['name']}'].join('\n'),
      confirmLabel: t('actions.update'),
    );
    if (confirmed) await mutate('reconcile', 'updateFailed');
  }

  Future<void> uninstall() async {
    if (working) return;
    final confirmed = await confirmAction(
      context,
      title: t('uninstallTitle'),
      message: t('uninstallMessage'),
      confirmLabel: t('actions.uninstall'),
      destructive: true,
    );
    if (confirmed) await mutate('uninstall', 'uninstallFailed');
  }

  Future<JsonMap> saveSelection(JsonMap selection, List<Object?> removals) async {
    setState(() => working = true);
    try {
      final result = await request('save_selection', {
        'selection': selection,
        'confirmedRemovals': removals,
      });
      await load();
      return result;
    } catch (_) {
      if (mounted) showError(context, t('saveSelectionFailed'));
      rethrow;
    } finally {
      if (mounted) setState(() => working = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final docs = SettingsExternalLink(
      label: t('docs'),
      url: _docsUrl,
      semanticLabel: t('openDocs'),
      leadingIcon: true,
    );
    if (!widget.connected || !widget.supported) {
      return SettingsSection(
        title: t('sectionTitle'),
        trailing: docs,
        card: false,
        children: [
          SettingsRowsCard(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  t(widget.connected ? 'unsupported' : 'unavailable'),
                  style: theme.textTheme.bodySmall!.copyWith(
                    color: colors.muted,
                    fontSize: type.uiSize(12),
                  ),
                ),
              ),
            ],
          ),
        ],
      );
    }
    final current = status;
    final selection = jsonMap(current?['selection']);
    final available = jsonList(current?['available']);
    final rawState = stringValue(current?['state']);
    final state = rawState == 'drift' && maintenanceOps.isEmpty ? 'up-to-date' : rawState;
    final hasSelected =
        (selection['mode'] == 'all' && available.isNotEmpty) ||
        (selection['mode'] == 'custom' &&
            jsonList(selection['skills']).any(available.contains));
    final enabled = widget.canManage && !working;
    Widget outline(String label, VoidCallback onPressed) => PaseoButton(
      label: label,
      size: PaseoControlSize.sm,
      variant: PaseoButtonVariant.outline,
      onPressed: enabled ? onPressed : null,
    );
    final small = theme.textTheme.bodySmall!.copyWith(
      color: colors.muted,
      fontSize: type.uiSize(12),
    );
    final actions = <Widget>[
      settingsIconButton(
        context,
        LucideIcons.settings2300,
        current == null || working
            ? null
            : () => showAdaptiveSheet<void>(
                context: context,
                builder: (context) => _SkillSelectionSheet(
                  available: [for (final name in available) name.toString()],
                  selection: selection,
                  onSave: saveSelection,
                ),
              ),
        label: t('choose'),
      ),
      if (hasSelected)
        if (state == 'up-to-date') ...[
          Row(
            mainAxisSize: MainAxisSize.min,
            spacing: 4,
            children: [
              Icon(LucideIcons.check300, size: 14, color: colors.muted),
              Text(t('actions.installed'), style: small),
            ],
          ),
          outline(t('actions.uninstall'), uninstall),
        ] else if (state == 'drift') ...[
          outline(t(working ? 'actions.working' : 'actions.update'), reconcile),
          outline(t('actions.uninstall'), uninstall),
        ] else
          outline(
            t(working ? 'actions.installing' : 'actions.install'),
            () => mutate('reconcile', 'updateFailed'),
          ),
    ];
    return SettingsSection(
      title: t('sectionTitle'),
      trailing: docs,
      card: false,
      children: [
        SettingsRowsCard(
          children: [
            Padding(
              padding: const EdgeInsets.all(16),
              child: Row(
                children: [
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.only(right: 12),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Row(
                            spacing: 8,
                            children: [
                              Icon(
                                LucideIcons.blocks300,
                                size: 16,
                                color: theme.colorScheme.onSurface,
                              ),
                              Flexible(
                                child: Text(
                                  t('title'),
                                  style: theme.textTheme.bodyMedium!.copyWith(
                                    fontSize: type.uiSize(14),
                                  ),
                                ),
                              ),
                            ],
                          ),
                          Padding(
                            padding: const EdgeInsets.only(top: 4),
                            child: Text(
                              t(state == 'drift' ? 'updateAvailable' : 'description'),
                              style: small,
                            ),
                          ),
                        ],
                      ),
                    ),
                  ),
                  Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: actions),
                ],
              ),
            ),
          ],
        ),
      ],
    );
  }
}

/// Original `SkillSelectionSheet`: the All-skills switch card, a flush
/// "Bundled skills" section of checkbox rows, a danger save error, and a
/// stretched secondary Cancel / default Save footer (12px apart).
class _SkillSelectionSheet extends StatefulWidget {
  const _SkillSelectionSheet({
    required this.available,
    required this.selection,
    required this.onSave,
  });
  final List<String> available;
  final JsonMap selection;
  final Future<JsonMap> Function(JsonMap selection, List<Object?> removals) onSave;
  @override
  State<_SkillSelectionSheet> createState() => _SkillSelectionSheetState();
}

class _SkillSelectionSheetState extends State<_SkillSelectionSheet> {
  late bool all = widget.selection['mode'] == 'all';
  late List<String> skills = [
    for (final name in jsonList(widget.selection['skills'])) name.toString(),
  ];
  String? saveError;
  bool saving = false;

  String t(String key, [Map<String, Object?> args = const {}]) =>
      tr(context, 'settings.host.skills.$key', args: args);

  JsonMap get draft => all ? {'mode': 'all'} : {'mode': 'custom', 'skills': skills};

  Future<void> save() async {
    setState(() {
      saveError = null;
      saving = true;
    });
    try {
      final attempt = await widget.onSave(draft, const []);
      final removals = jsonList(jsonMap(attempt['confirmationRequired'])['removals']);
      if (attempt['confirmationRequired'] != null) {
        if (!mounted) return;
        final confirmed = await confirmAction(
          context,
          title: t('removeTitle'),
          message: t('removeMessage', {'skills': removals.join(', ')}),
          confirmLabel: t('actions.remove'),
          destructive: true,
        );
        if (!confirmed) return;
        final retry = await widget.onSave(draft, removals);
        if (retry['confirmationRequired'] != null) {
          if (mounted) setState(() => saveError = t('saveFailed'));
          return;
        }
      }
      if (mounted) Navigator.pop(context);
    } catch (_) {
      if (mounted) setState(() => saveError = t('saveFailed'));
    } finally {
      if (mounted) setState(() => saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(fontSize: type.uiSize(12));
    return SheetContents(
      title: t('choose'),
      stretchActions: true,
      actions: [
        PaseoButton(
          label: tr(context, 'common.actions.cancel'),
          expand: true,
          onPressed: saving ? null : () => Navigator.pop(context),
        ),
        PaseoButton(
          label: t(saving ? 'actions.saving' : 'actions.save'),
          variant: PaseoButtonVariant.primary,
          expand: true,
          loading: saving,
          onPressed: save,
        ),
      ],
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 16,
        children: [
          SettingsCard(
            children: [
              SettingsSwitchRow(
                label: t('chooseAll'),
                hint: t('chooseAllHint'),
                value: all,
                onChanged: saving
                    ? null
                    : (next) => setState(() {
                        all = next;
                        if (!next) skills = [...widget.available];
                      }),
              ),
            ],
          ),
          SettingsSection(
            title: t('chooseList'),
            card: false,
            flush: true,
            children: [
              SettingsRowsCard(
                children: [
                  if (widget.available.isEmpty)
                    Padding(
                      padding: const EdgeInsets.all(16),
                      child: Text(t('chooseEmpty'), style: small.copyWith(color: colors.muted)),
                    )
                  else
                    for (final name in widget.available)
                      SettingsCheckboxRow(
                        label: name,
                        checked: all || skills.contains(name),
                        onChanged: all || saving
                            ? null
                            : (_) => setState(() {
                                skills = skills.contains(name)
                                    ? skills.where((skill) => skill != name).toList()
                                    : ([...skills, name]..sort());
                              }),
                      ),
                ],
              ),
            ],
          ),
          if (saveError != null)
            Text(saveError!, style: small.copyWith(color: colors.danger)),
        ],
      ),
    );
  }
}

const daemonPermissions = [
  'daemon.read',
  'daemon.manage',
  'tunnel.manage',
  'access.manage',
  'workspace.read',
  'workspace.write',
  'workspace.manage',
  'automation.manage',
  'hub.execute',
];

class HubAccessSettings extends StatefulWidget {
  final PaseoClient client;
  const HubAccessSettings({super.key, required this.client});
  @override
  State<HubAccessSettings> createState() => _HubAccessSettingsState();
}

/// Hub relationship and permission grants (not part of the original app), as
/// a settings card: relationship row with its actions, then checkbox rows.
class _HubAccessSettingsState extends State<HubAccessSettings> {
  JsonMap status = {};
  Object? error;
  @override
  void initState() {
    super.initState();
    load();
  }

  Future<void> request(String action, JsonMap fields) async {
    try {
      final r = await settingsRequest(
        widget.client,
        'hub.management.daemon.$action.request',
        fields,
        responseType: 'hub.management.daemon.$action.response',
      );
      if (mounted) {
        setState(() {
          status = jsonMap(r['status']);
          error = null;
        });
      }
    } catch (e) {
      if (mounted) setState(() => error = e);
    }
  }

  Future<void> load() => request('get_status', {});

  @override
  Widget build(BuildContext context) {
    final editable = status.isNotEmpty && status['state'] != 'not_connected';
    final problem = error == null
        ? status['lastError']?.toString()
        : errorMessage(error!);
    return SettingsSection(
      title: 'Hub access & permissions',
      card: false,
      trailing: settingsIconButton(
        context,
        LucideIcons.rotateCw300,
        load,
        label: tr(context, 'usage.refresh'),
      ),
      children: [
        SettingsRowsCard(
          children: [
            SettingsRow(
              label: 'Relationship',
              hint: [
                stringValue(status['state'], tr(context, 'settings.providers.loading')),
                ?status['hubOrigin']?.toString(),
              ].join('\n'),
              error: problem,
              control: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 8,
                children: [
                  settingsButton(
                    context,
                    'Connect hub',
                    () => editSettingsForm(
                      context,
                      title: 'Connect hub',
                      fields: const [
                        SettingField('hubUrl', 'Hub URL', required: true),
                        SettingField(
                          'token',
                          'Registration token',
                          required: true,
                          secret: true,
                        ),
                      ],
                      onSave: (v) => request('connect', {...v, 'permissions': []}),
                    ),
                  ),
                  settingsButton(
                    context,
                    'Disconnect',
                    !editable
                        ? null
                        : () async {
                            if (await confirmAction(
                              context,
                              title: 'Disconnect hub?',
                              message: 'Revoke this hub relationship?',
                              confirmLabel: 'Disconnect',
                              destructive: true,
                            )) {
                              await request('disconnect', {});
                            }
                          },
                  ),
                ],
              ),
            ),
            for (final permission in daemonPermissions)
              SettingsCheckboxRow(
                label: permission,
                checked: jsonList(status['permissions']).contains(permission),
                onChanged: !editable
                    ? null
                    : (v) => request(
                        'permissions.update',
                        v
                            ? {
                                'grant': [permission],
                              }
                            : {
                                'revoke': [permission],
                              },
                      ),
              ),
          ],
        ),
      ],
    );
  }
}
