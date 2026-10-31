import 'dart:async';
import 'dart:io' as io;
import 'package:flutter/foundation.dart' show kIsWeb;
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/shell_pages.dart';
import '../../ui/theme.dart';
import '../chat/agent_controls.dart' show formatThinkingOptionLabel;
import '../chat/provider_icon.dart';
import '../settings/settings_forms.dart';
import '../settings/settings_host_widgets.dart';

part 'schedule_form_sheet.dart';

/// Original `palette.red[300]`: the schedules banner and form error text.
const _red300 = Color(0xfffca5a5);

TextStyle _text(BuildContext context, double size, [Color? color]) =>
    Theme.of(context).textTheme.bodyMedium!.copyWith(
      fontSize: PaseoTypography.of(context).uiSize(size),
      fontWeight: FontWeight.normal,
      color: color,
    );

/// Original `screens/schedules-screen.tsx`: a `MenuHeader`, then a centered
/// spinner, load error, or empty state, or the filter row (host filter,
/// Active/Ended segmented control, outline "New schedule") over one
/// settings-style card of schedule rows.
class SchedulesScreen extends StatefulWidget {
  const SchedulesScreen({super.key});
  @override
  State<SchedulesScreen> createState() => _SchedulesScreenState();
}

class _SchedulesScreenState extends State<SchedulesScreen> {
  AppStore? store;
  List<JsonMap> schedules = [];

  /// Labels of connected hosts whose `schedule/list` failed.
  List<String> hostErrors = [];
  bool loading = false, loaded = false, loadFailed = false;
  int generation = 0;
  String hostSignature = '';
  String selectedHost = '';
  String statusFilter = 'runnable';
  Timer? timer;
  final watchedClients = <PaseoClient>{};

  /// Original per-row `ScheduleRowPending`: in-flight actions keyed by
  /// `serverId:scheduleId`, so the kebab shows "Pausing..." etc. on the acting
  /// row only.
  final pending = <String, Set<String>>{};

  static String _rowKey(JsonMap schedule) => '${schedule['_serverId']}:${schedule['id']}';

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final next = PaseoScope.of(context);
    if (store != next) {
      store = next;
      timer ??= Timer.periodic(const Duration(seconds: 30), (_) {
        if (!loading) load();
      });
    }
    synchronizeClients();
  }

  @override
  void dispose() {
    ++generation;
    timer?.cancel();
    for (final client in watchedClients) {
      client.removeListener(connectionChanged);
    }
    watchedClients.clear();
    super.dispose();
  }

  void synchronizeClients() {
    final current = store!.hosts.map((host) => host.client).toSet();
    for (final client in watchedClients.difference(current)) {
      client.removeListener(connectionChanged);
    }
    for (final client in current.difference(watchedClients)) {
      client.addListener(connectionChanged);
    }
    watchedClients
      ..clear()
      ..addAll(current);
    connectionChanged();
  }

  void connectionChanged() {
    if (!mounted || store == null) return;
    final signature = store!.hosts
        .map(
          (host) =>
              '${host.serverId}:${identityHashCode(host.client)}:${host.client.status}',
        )
        .join('|');
    if (signature == hostSignature) return;
    hostSignature = signature;
    load();
  }

  /// Original `fetchAggregatedSchedules`: offline hosts are skipped, a failing
  /// connected host lands in the banner, and only when every connected host
  /// fails does the screen show the full load error. While hosts are still
  /// connecting and nothing loaded, the spinner stays up. Rows from the last
  /// load stay visible while a refresh is in flight.
  Future<void> load() async {
    if (!mounted || store == null) return;
    final own = ++generation;
    setState(() => loading = true);
    final hosts = store!.hosts.toList();
    final connected = hosts
        .where((host) => host.client.status == ConnectionStatus.connected)
        .toList();
    final settling = hosts.any(
      (host) => host.client.status == ConnectionStatus.connecting,
    );
    final rows = <JsonMap>[];
    final failures = <String>[];
    await Future.wait(
      connected.map((host) async {
        try {
          final result = await settingsRequest(
            host.client,
            'schedule/list',
            {},
            responseType: 'schedule/list/response',
          );
          for (final value in jsonList(result['schedules'])) {
            rows.add({
              ...jsonMap(value),
              '_serverId': host.serverId,
              '_hostLabel': host.label,
            });
          }
        } catch (_) {
          failures.add(host.label);
        }
      }),
    );
    if (!mounted || own != generation) return;
    rows.sort(
      (a, b) =>
          stringValue(b['createdAt']).compareTo(stringValue(a['createdAt'])),
    );
    final failed =
        connected.isNotEmpty && rows.isEmpty && failures.length == connected.length;
    setState(() {
      loading = false;
      loadFailed = failed;
      schedules = rows;
      hostErrors = failures;
      if (failed || rows.isNotEmpty || !settling) loaded = true;
    });
  }

  /// Original `schedule-derivation.ts` precedence: expired, target gone,
  /// finished, paused, active. A missing agent only counts as gone once the
  /// host's agent directory has loaded.
  String state(JsonMap schedule) {
    final expiry = DateTime.tryParse(stringValue(schedule['expiresAt']));
    if (expiry != null && !expiry.isAfter(DateTime.now())) return 'expired';
    final target = jsonMap(schedule['target']);
    if (target['type'] == 'agent') {
      final host = store?.hostFor(stringValue(schedule['_serverId']));
      if (host != null &&
          host.agents.isNotEmpty &&
          !host.agents.any((a) => a['id'] == target['agentId'])) {
        return 'targetGone';
      }
    }
    if (schedule['status'] == 'completed') return 'finished';
    return schedule['status'] == 'paused' ? 'paused' : 'active';
  }

  static String bucket(String state) =>
      state == 'active' || state == 'paused' ? 'runnable' : 'ended';

  /// Original `resolveTarget`: agent title, project name, or shortened cwd,
  /// plus the provider glyph.
  (String, String?) target(JsonMap schedule) {
    final target = jsonMap(schedule['target']);
    final host = store?.hostFor(stringValue(schedule['_serverId']));
    if (target['type'] == 'agent') {
      final agent = host?.agents
          .where((a) => a['id'] == target['agentId'])
          .firstOrNull;
      if (agent == null) return ('Agent unavailable', null);
      final title = stringValue(agent['title']).trim();
      final provider = stringValue(agent['provider']);
      return (
        title.isEmpty ? 'Untitled agent' : title,
        provider.isEmpty ? null : provider,
      );
    }
    final config = jsonMap(target['config']);
    final cwd = stringValue(config['cwd']).trim();
    final project = host?.projects
        .where((p) => stringValue(p['projectRootPath']).trim() == cwd)
        .firstOrNull;
    final name = stringValue(project?['projectDisplayName']);
    final provider = stringValue(config['provider']);
    return (
      name.isNotEmpty ? name : _shortenPath(cwd),
      provider.isEmpty ? null : provider,
    );
  }

  Future<void> edit([JsonMap? schedule]) async {
    final changed = await showAdaptiveSheet<bool>(
      context: context,
      builder: (context) => _ScheduleEditor(store: store!, schedule: schedule),
    );
    if (changed == true) await load();
  }

  /// Original `SchedulesTableRow` + `useScheduleMutations`: pause/resume
  /// optimistically flip the status, delete optimistically drops the row,
  /// each rolls back on failure (silently, as the original does) and every
  /// action refetches the list when it settles.
  Future<void> action(JsonMap schedule, String action) async {
    if (action == 'delete') {
      final product = _productName(schedule).toLowerCase();
      if (!await confirmAction(
        context,
        title: 'Delete $product',
        message: 'Delete "${_scheduleTitle(schedule)}"? This cannot be undone.',
        confirmLabel: 'Delete',
        destructive: true,
      )) {
        return;
      }
    }
    if (!mounted) return;
    final key = _rowKey(schedule);
    final previous = schedules;
    // Original `cancelQueries` before the optimistic write: an in-flight list
    // load must not overwrite it.
    ++generation;
    setState(() {
      pending.putIfAbsent(key, () => <String>{}).add(action);
      schedules = switch (action) {
        'pause' || 'resume' => [
          for (final row in schedules)
            if (_rowKey(row) == key)
              {
                ...row,
                'status': action == 'pause' ? 'paused' : 'active',
                'pausedAt': action == 'pause'
                    ? DateTime.now().toUtc().toIso8601String()
                    : null,
              }
            else
              row,
        ],
        'delete' => [
          for (final row in schedules)
            if (_rowKey(row) != key) row,
        ],
        _ => schedules,
      };
    });
    try {
      await settingsRequest(
        store!.clientFor(stringValue(schedule['_serverId'])),
        'schedule/$action',
        {'scheduleId': schedule['id']},
        responseType: 'schedule/$action/response',
      );
    } catch (_) {
      if (mounted && action != 'run-once') setState(() => schedules = previous);
    } finally {
      if (mounted) {
        setState(() {
          final actions = pending[key];
          actions?.remove(action);
          if (actions != null && actions.isEmpty) pending.remove(key);
        });
        await load();
      }
    }
  }

  Future<void> runs(JsonMap schedule) async {
    final serverId = stringValue(schedule['_serverId']);
    final JsonMap result;
    try {
      result = await settingsRequest(
        store!.clientFor(serverId),
        'schedule/logs',
        {'scheduleId': schedule['id']},
        responseType: 'schedule/logs/response',
      );
    } catch (e) {
      if (mounted) showError(context, e);
      return;
    }
    if (!mounted) return;
    final runs = jsonList(result['runs']).map(jsonMap).toList()
      ..sort(
        (a, b) =>
            stringValue(b['startedAt']).compareTo(stringValue(a['startedAt'])),
      );
    final agent = await showAdaptiveSheet<JsonMap>(
      context: context,
      builder: (context) => SheetContents(
        title: 'Run history',
        subtitle: Text(
          _scheduleTitle(schedule),
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
        child: _RunHistory(runs: runs),
      ),
    );
    if (agent == null || !mounted) return;
    final host = store!.hostFor(serverId);
    final known = host?.agents
        .where((a) => a['id'] == agent['agentId'])
        .firstOrNull;
    store!.open(
      PanelTarget(
        kind: 'agent',
        serverId: serverId,
        workspaceId: stringValue(
          agent['workspaceId'],
          stringValue(known?['workspaceId']),
        ),
        cwd: stringValue(
          jsonMap(jsonMap(schedule['target'])['config'])['cwd'],
        ),
        id: stringValue(agent['agentId']),
      ),
    );
    Navigator.of(context).maybePop();
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    return Scaffold(
      backgroundColor: colors.surface0,
      body: SafeArea(
        bottom: false,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _MenuHeader(
              title: 'Schedules',
              compact: compact,
              // Original `MenuHeader`'s `SidebarMenuToggle`: beside the sidebar it shows
              // or hides it; over the shell (compact) revealing the agent list leaves
              // this screen for the sidebar beneath it.
              onMenu: () {
                if (EmbeddedShellPage.of(context)) {
                  store?.showSidebar(!(store?.sidebarVisible ?? true));
                  return;
                }
                store?.showSidebar(true);
                Navigator.of(context).maybePop();
              },
            ),
            Expanded(child: _body(context, compact)),
          ],
        ),
      ),
    );
  }

  Widget _centered(List<Widget> children) => Center(
    child: SingleChildScrollView(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        spacing: 24,
        children: children,
      ),
    ),
  );

  Widget _body(BuildContext context, bool compact) {
    final colors = PaseoColors.of(context);
    final inset = compact ? 12.0 : 24.0;
    if (loadFailed) {
      return _centered([
        Text(
          'Unable to load schedules',
          textAlign: TextAlign.center,
          style: _text(context, 14, colors.muted),
        ),
        PaseoButton(
          label: 'Try again',
          variant: PaseoButtonVariant.ghost,
          onPressed: loading ? null : load,
        ),
      ]);
    }
    if (!loaded) {
      return Center(
        child: SizedBox.square(
          dimension: 36,
          child: CircularProgressIndicator(strokeWidth: 3, color: colors.muted),
        ),
      );
    }
    final banner = hostErrors.isEmpty ? null : _ErrorsBanner(hosts: hostErrors);
    if (schedules.isEmpty) {
      return _centered([
        ?banner,
        _EmptyState(onCreate: () => edit()),
      ]);
    }
    final hosts = store?.hosts ?? const <HostState>[];
    final host = hosts.any((h) => h.serverId == selectedHost) ? selectedHost : '';
    final singleHost = hosts.length <= 1;
    final rows = [
      for (final schedule in schedules)
        if ((host.isEmpty || schedule['_serverId'] == host) &&
            bucket(state(schedule)) == statusFilter)
          schedule,
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: EdgeInsets.fromLTRB(inset, 16, inset, 0),
          child: Row(
            spacing: 12,
            children: [
              Expanded(
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: Wrap(
                    spacing: 12,
                    runSpacing: 12,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: [
                      if (!singleHost)
                        _HostFilter(
                          hosts: hosts,
                          value: host,
                          onChanged: (value) => setState(() => selectedHost = value),
                        ),
                      PaseoSegmentedControl<String>(
                        size: PaseoControlSize.sm,
                        value: statusFilter,
                        segments: const [
                          PaseoSegment('runnable', 'Active'),
                          PaseoSegment('ended', 'Ended'),
                        ],
                        onChanged: (value) => setState(() => statusFilter = value),
                      ),
                    ],
                  ),
                ),
              ),
              PaseoButton(
                label: 'New schedule',
                icon: LucideIcons.plus300,
                variant: PaseoButtonVariant.outline,
                size: PaseoControlSize.sm,
                onPressed: () => edit(),
              ),
            ],
          ),
        ),
        Expanded(
          child: CustomScrollView(
            slivers: [
              SliverPadding(
                padding: const EdgeInsets.only(top: 16),
                sliver: SliverToBoxAdapter(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    spacing: 12,
                    children: [
                      if (banner != null)
                        Padding(
                          padding: EdgeInsets.symmetric(horizontal: inset),
                          child: banner,
                        ),
                      if (rows.isNotEmpty)
                        Padding(
                          padding: EdgeInsets.symmetric(horizontal: inset),
                          child: _ScheduleCard(
                            children: [
                              for (final (index, schedule) in rows.indexed)
                                _rowFor(schedule, first: index == 0, singleHost: singleHost),
                            ],
                          ),
                        ),
                    ],
                  ),
                ),
              ),
              if (rows.isEmpty)
                SliverFillRemaining(
                  hasScrollBody: false,
                  child: Padding(
                    padding: EdgeInsets.symmetric(horizontal: inset, vertical: 24),
                    child: Center(
                      child: statusFilter == 'ended'
                          ? const _EndedEmptyState()
                          : _EmptyState(onCreate: () => edit()),
                    ),
                  ),
                ),
              SliverToBoxAdapter(
                child: SizedBox(height: 24 + MediaQuery.paddingOf(context).bottom),
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _rowFor(JsonMap schedule, {required bool first, required bool singleHost}) {
    final current = state(schedule);
    final (label, provider) = target(schedule);
    return _ScheduleRow(
      key: ValueKey('${schedule['_serverId']}:${schedule['id']}'),
      schedule: schedule,
      state: current,
      targetLabel: label,
      provider: provider,
      hostLabel: singleHost ? null : stringValue(schedule['_hostLabel']),
      first: first,
      pending: pending[_rowKey(schedule)] ?? const <String>{},
      onEdit: () => edit(schedule),
      onAction: (value) => value == 'runs' ? runs(schedule) : action(schedule, value),
    );
  }
}

/// Original `MenuHeader` for a pushed screen: the phone `CompactHeader`
/// (12px insets, 8px top padding, 56px row, round island button, lg medium
/// title) or the wide `ScreenHeader` (36px bordered row, 12px insets, base
/// light title), both led by the `SidebarMenuToggle` (MobileMenuIcon on phones,
/// PanelLeft elsewhere).
class _MenuHeader extends StatelessWidget {
  const _MenuHeader({required this.title, required this.compact, required this.onMenu});
  final String title;
  final bool compact;
  final VoidCallback onMenu;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = PaseoColors.of(context).muted;
    final row = Row(
      spacing: 8,
      children: [
        _IslandButton(
          compact: compact,
          tooltip: tr(context, compact ? 'shell.menu.open' : 'shell.menu.toggleSidebar'),
          onPressed: onMenu,
          // Original MobileMenuIcon: 16/16/8 x 1.5 muted lines in a 16x12 box.
          child: compact
              ? SizedBox(
                  width: 16,
                  height: 12,
                  child: Column(
                    mainAxisAlignment: MainAxisAlignment.spaceBetween,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (final width in const [16.0, 16.0, 8.0])
                        Container(
                          width: width,
                          height: 1.5,
                          decoration: BoxDecoration(
                            color: muted,
                            borderRadius: BorderRadius.circular(1),
                          ),
                        ),
                    ],
                  ),
                )
              : Icon(LucideIcons.panelLeft300, size: 16, color: muted),
        ),
        Expanded(
          child: Text(
            title,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: _text(context, compact ? 16 : 14, theme.colorScheme.onSurface).copyWith(
              fontWeight: compact ? FontWeight.w500 : FontWeight.w300,
            ),
          ),
        ),
      ],
    );
    if (compact) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
        child: SizedBox(height: 56, child: row),
      );
    }
    return Container(
      height: 36,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: theme.dividerColor)),
      ),
      child: row,
    );
  }
}

/// Original header-island button: a round 32px (26px wide) frame around a
/// muted 16px glyph, filled with the interaction highlight on hover/press.
class _IslandButton extends StatefulWidget {
  const _IslandButton({
    required this.compact,
    required this.tooltip,
    required this.onPressed,
    required this.child,
  });
  final bool compact;
  final String tooltip;
  final VoidCallback onPressed;
  final Widget child;
  @override
  State<_IslandButton> createState() => _IslandButtonState();
}

class _IslandButtonState extends State<_IslandButton> {
  bool hovered = false, pressed = false;
  @override
  Widget build(BuildContext context) {
    final extent = widget.compact ? 32.0 : 26.0;
    Widget button = Semantics(
      button: true,
      label: widget.tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onPressed,
          child: Container(
            width: extent,
            height: extent,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              shape: BoxShape.circle,
              color: hovered || pressed ? paseoInteractionHighlight(context) : null,
            ),
            child: widget.child,
          ),
        ),
      ),
    );
    if (!widget.compact) button = Tooltip(message: widget.tooltip, child: button);
    return button;
  }
}

/// Original `HostStatusDot` in its 14px `HostStatusDotSlot`.
class _HostStatusDot extends StatelessWidget {
  const _HostStatusDot({required this.status});
  final ConnectionStatus status;
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return SizedBox.square(
      dimension: 14,
      child: Center(
        child: Container(
          width: 8,
          height: 8,
          decoration: BoxDecoration(
            shape: BoxShape.circle,
            color: switch (status) {
              ConnectionStatus.connected => colors.success,
              ConnectionStatus.connecting => colors.warning,
              _ => colors.danger,
            },
          ),
        ),
      ),
    );
  }
}

/// Original `HostFilter` pill: surface1, 1px border, 6px radius, 12/6
/// padding, 6px gaps, a muted 14px server glyph (or the host's status dot),
/// a medium base label and a muted 14px chevron; surface2 hovered, surface3
/// pressed. Opens the "Filter by host" picker.
class _HostFilter extends StatefulWidget {
  const _HostFilter({required this.hosts, required this.value, required this.onChanged});
  final List<HostState> hosts;
  final String value;
  final ValueChanged<String> onChanged;
  @override
  State<_HostFilter> createState() => _HostFilterState();
}

class _HostFilterState extends State<_HostFilter> {
  bool hovered = false, pressed = false;

  Future<void> open(BuildContext anchor) async {
    final colors = PaseoColors.of(context);
    final picked = await showPaseoSelect<String>(
      context: anchor,
      title: 'Filter by host',
      value: widget.value,
      options: [
        PaseoSelectOption(
          '',
          'All hosts',
          leading: Icon(LucideIcons.server300, size: 14, color: colors.muted),
        ),
        for (final host in widget.hosts)
          PaseoSelectOption(
            host.serverId,
            host.label,
            leading: _HostStatusDot(status: host.client.status),
          ),
      ],
    );
    if (picked != null && picked != widget.value) widget.onChanged(picked);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final host = widget.hosts.where((h) => h.serverId == widget.value).firstOrNull;
    final label = host?.label ?? 'All hosts';
    return Semantics(
      button: true,
      label: 'Filter: $label',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: Builder(
          builder: (anchor) => GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTapDown: (_) => setState(() => pressed = true),
            onTapCancel: () => setState(() => pressed = false),
            onTapUp: (_) => setState(() => pressed = false),
            onTap: () => open(anchor),
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              decoration: BoxDecoration(
                color: pressed
                    ? colors.surface3
                    : hovered
                    ? colors.surface2
                    : colors.surface1,
                border: Border.all(color: theme.dividerColor),
                borderRadius: BorderRadius.circular(6),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 6,
                children: [
                  if (host == null)
                    Icon(LucideIcons.server300, size: 14, color: colors.muted)
                  else
                    _HostStatusDot(status: host.client.status),
                  Flexible(
                    child: Text(
                      label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: _text(context, 14, theme.colorScheme.onSurface).copyWith(
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                  Icon(LucideIcons.chevronDown300, size: 14, color: colors.muted),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `ScheduleHostErrorsBanner`: 1px border, 8px radius, 12px padding,
/// 4px gaps, red-300 small text per failing host.
class _ErrorsBanner extends StatelessWidget {
  const _ErrorsBanner({required this.hosts});
  final List<String> hosts;
  @override
  Widget build(BuildContext context) => Container(
    width: double.infinity,
    padding: const EdgeInsets.all(12),
    decoration: BoxDecoration(
      border: Border.all(color: Theme.of(context).dividerColor),
      borderRadius: BorderRadius.circular(8),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      spacing: 4,
      children: [
        for (final host in hosts)
          Text('$host: Could not load schedules', style: _text(context, 12, _red300)),
      ],
    ),
  );
}

/// Original `SchedulesEmptyState`: muted 20px CalendarClock, a centered base
/// title and muted description with the "See docs" link (8px stack), then the
/// outline New schedule button; 16px gaps, 420px max width.
class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.onCreate});
  final VoidCallback onCreate;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 420),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        spacing: 16,
        children: [
          Icon(LucideIcons.calendarClock300, size: 20, color: colors.muted),
          Column(
            spacing: 8,
            children: [
              Text(
                'No active schedules',
                textAlign: TextAlign.center,
                style: _text(context, 14, theme.colorScheme.onSurface),
              ),
              Text(
                'Schedules run agents on a cadence.',
                textAlign: TextAlign.center,
                style: _text(context, 14, colors.muted),
              ),
              const _ExternalLink(
                href: 'https://paseo.sh/docs/schedules',
                label: 'See docs',
              ),
            ],
          ),
          PaseoButton(
            label: 'New schedule',
            icon: LucideIcons.plus300,
            variant: PaseoButtonVariant.outline,
            onPressed: onCreate,
          ),
        ],
      ),
    );
  }
}

/// Original `SchedulesEndedEmptyState`: icon over "No ended schedules", 12px gap.
class _EndedEmptyState extends StatelessWidget {
  const _EndedEmptyState();
  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    spacing: 12,
    children: [
      Icon(LucideIcons.calendarClock300, size: 20, color: PaseoColors.of(context).muted),
      Text(
        'No ended schedules',
        textAlign: TextAlign.center,
        style: _text(context, 14, Theme.of(context).colorScheme.onSurface),
      ),
    ],
  );
}

/// Original `ExternalLink`: muted small label + 12px ArrowUpRight, 4px gap.
class _ExternalLink extends StatelessWidget {
  const _ExternalLink({required this.href, required this.label});
  final String href, label;
  @override
  Widget build(BuildContext context) {
    final muted = PaseoColors.of(context).muted;
    return Semantics(
      link: true,
      label: label,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () async {
            try {
              await launchUrl(Uri.parse(href), mode: LaunchMode.externalApplication);
            } catch (e) {
              if (context.mounted) showError(context, e);
            }
          },
          child: Padding(
            padding: const EdgeInsets.all(8),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 4,
              children: [
                Text(label, style: _text(context, 12, muted)),
                Icon(LucideIcons.arrowUpRight300, size: 12, color: muted),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `settingsStyles.card`: surface1, 1px border, 8px radius, clipped.
class _ScheduleCard extends StatelessWidget {
  const _ScheduleCard({required this.children});
  final List<Widget> children;
  @override
  Widget build(BuildContext context) => DecoratedBox(
    decoration: BoxDecoration(
      color: PaseoColors.of(context).surface1,
      border: Border.all(color: Theme.of(context).dividerColor),
      borderRadius: BorderRadius.circular(8),
    ),
    child: ClipRRect(
      borderRadius: BorderRadius.circular(7),
      child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children),
    ),
  );
}

/// Original `ScheduleRow`: a 16px settings row (top border after the first),
/// a 16x20 provider-glyph slot, then title (base), target (muted base, 4px
/// above) and meta (muted small, 4px above), with the StatusBadge and kebab
/// trailing (8px gap). Surface2 while hovered on wide layouts, surface3 while
/// pressed. Tapping the row edits the schedule.
class _ScheduleRow extends StatefulWidget {
  const _ScheduleRow({
    super.key,
    required this.schedule,
    required this.state,
    required this.targetLabel,
    required this.provider,
    required this.hostLabel,
    required this.first,
    required this.onEdit,
    required this.onAction,
    this.pending = const <String>{},
  });
  final JsonMap schedule;
  final String state, targetLabel;
  final String? provider, hostLabel;
  final bool first;
  final VoidCallback onEdit;
  final ValueChanged<String> onAction;

  /// In-flight row actions (`pause`, `resume`, `run-once`, `delete`).
  final Set<String> pending;
  @override
  State<_ScheduleRow> createState() => _ScheduleRowState();
}

class _ScheduleRowState extends State<_ScheduleRow> {
  bool hovered = false, pressed = false;

  /// Original `buildMeta`: host (multi-host only), cadence, created, last run,
  /// and the next run while active.
  String meta() {
    final schedule = widget.schedule;
    final next = widget.state == 'active' ? _formatNextRun(schedule['nextRunAt']) : '';
    return [
      ?widget.hostLabel,
      _formatCadence(jsonMap(schedule['cadence'])),
      'Created ${_timeAgo(schedule['createdAt'])}',
      schedule['lastRunAt'] == null
          ? 'Never run'
          : 'Last run ${_timeAgo(schedule['lastRunAt'])}',
      if (next.isNotEmpty) 'Next run $next',
    ].join(' · ');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final schedule = widget.schedule;
    final (badge, variant) = switch (widget.state) {
      'active' => ('Active', StatusBadgeVariant.success),
      'paused' => ('Paused', StatusBadgeVariant.muted),
      'expired' => ('Expired', StatusBadgeVariant.muted),
      'finished' => ('Finished', StatusBadgeVariant.muted),
      _ => ('Target gone', StatusBadgeVariant.error),
    };
    final title = _scheduleTitle(schedule);
    return MouseRegion(
      cursor: SystemMouseCursors.click,
      onEnter: (_) => setState(() => hovered = true),
      onExit: (_) => setState(() => hovered = false),
      child: Semantics(
        button: true,
        label: 'Edit ${_productName(schedule).toLowerCase()} $title',
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => pressed = true),
          onTapCancel: () => setState(() => pressed = false),
          onTapUp: (_) => setState(() => pressed = false),
          onTap: widget.onEdit,
          child: Container(
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              color: pressed
                  ? colors.surface3
                  : hovered && !compact
                  ? colors.surface2
                  : null,
              border: widget.first
                  ? null
                  : Border(top: BorderSide(color: theme.dividerColor)),
            ),
            child: Row(
              spacing: 12,
              children: [
                Expanded(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 12,
                    children: [
                      SizedBox(
                        width: 16,
                        height: 20,
                        child: widget.provider == null
                            ? null
                            : Center(
                                child: ProviderIcon(
                                  provider: widget.provider!,
                                  size: 16,
                                  color: colors.muted,
                                ),
                              ),
                      ),
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              title,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: _text(context, 14, theme.colorScheme.onSurface),
                            ),
                            const SizedBox(height: 4),
                            Text(
                              widget.targetLabel,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: _text(context, 14, colors.muted),
                            ),
                            const SizedBox(height: 4),
                            Text(
                              meta(),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: _text(context, 12, colors.muted),
                            ),
                          ],
                        ),
                      ),
                    ],
                  ),
                ),
                Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    StatusBadge(label: badge, variant: variant),
                    _KebabButton(
                      schedule: schedule,
                      state: widget.state,
                      pending: widget.pending,
                      onSelected: widget.onAction,
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Original `ScheduleKebabMenu` trigger: 4px padding, 4px radius, muted 14px
/// MoreVertical turning foreground on a surface2 hover. Opens the 220px
/// end-aligned dropdown (a bottom sheet on compact layouts). The 8px gap to
/// the badge belongs to its hit area, as the original 8px hitSlop does.
class _KebabButton extends StatefulWidget {
  const _KebabButton({
    required this.schedule,
    required this.state,
    required this.onSelected,
    this.pending = const <String>{},
  });
  final JsonMap schedule;
  final String state;
  final Set<String> pending;
  final ValueChanged<String> onSelected;
  @override
  State<_KebabButton> createState() => _KebabButtonState();
}

class _KebabButtonState extends State<_KebabButton> {
  final frame = GlobalKey();
  bool hovered = false, open = false;

  Future<void> show() async {
    final colors = PaseoColors.of(context);
    final destructive = Theme.of(context).colorScheme.error;
    final schedule = widget.schedule;
    final product = _productName(schedule);
    final lower = product.toLowerCase();
    final canRun = jsonMap(schedule['target'])['type'] == 'new-agent' &&
        (widget.state == 'active' || widget.state == 'paused');
    Widget glyph(IconData icon, [Color? color]) =>
        Icon(icon, size: 14, color: color ?? colors.muted);
    // Original `DropdownMenuItem status="pending"`: the pending label, disabled.
    final busy = widget.pending;
    setState(() => open = true);
    final value = await showPaseoMenu<String>(
      context,
      title: '$product actions',
      anchor: paseoAnchorRect(frame.currentContext ?? context),
      alignEnd: true,
      entries: [
        PaseoMenuEntry.item(
          value: 'edit',
          label: 'Edit $lower',
          leading: glyph(LucideIcons.pencil300),
        ),
        if (jsonMap(schedule['target'])['type'] != 'agent') ...[
          if (schedule['status'] == 'paused')
            PaseoMenuEntry.item(
              value: 'resume',
              label: busy.contains('resume') ? 'Resuming...' : 'Resume schedule',
              leading: glyph(LucideIcons.play300),
              enabled: canRun && !busy.contains('resume'),
            )
          else
            PaseoMenuEntry.item(
              value: 'pause',
              label: busy.contains('pause') ? 'Pausing...' : 'Pause schedule',
              leading: glyph(LucideIcons.pause300),
              enabled: schedule['status'] != 'completed' && canRun && !busy.contains('pause'),
            ),
          PaseoMenuEntry.item(
            value: 'run-once',
            label: busy.contains('run-once') ? 'Starting...' : 'Run now',
            leading: glyph(LucideIcons.rotateCw300),
            enabled: canRun && !busy.contains('run-once'),
          ),
        ],
        PaseoMenuEntry.item(
          value: 'runs',
          label: 'Run history',
          leading: glyph(LucideIcons.history300),
        ),
        const PaseoMenuEntry.separator(),
        PaseoMenuEntry.item(
          value: 'delete',
          label: busy.contains('delete') ? 'Deleting...' : 'Delete $lower',
          leading: glyph(LucideIcons.trash2300, destructive),
          destructive: true,
          enabled: !busy.contains('delete'),
        ),
      ],
    );
    if (!mounted) return;
    setState(() => open = false);
    if (value != null) widget.onSelected(value);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final active = hovered || open;
    return Semantics(
      button: true,
      label: '${_productName(widget.schedule)} actions',
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => hovered = true),
        onExit: (_) => setState(() => hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: show,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(8, 8, 0, 8),
            child: Container(
              key: frame,
              padding: const EdgeInsets.all(4),
              decoration: BoxDecoration(
                color: active ? colors.surface2 : null,
                borderRadius: BorderRadius.circular(4),
              ),
              child: Icon(
                LucideIcons.moreVertical300,
                size: 14,
                color: active ? theme.colorScheme.onSurface : colors.muted,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Run history for one schedule (`schedule/logs`), newest first: settings
/// rows with a status badge and start time, the run output in the code font
/// and any error. Pops with the run when "Open session" is pressed.
class _RunHistory extends StatelessWidget {
  const _RunHistory({required this.runs});
  final List<JsonMap> runs;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    if (runs.isEmpty) {
      return Padding(
        padding: const EdgeInsets.symmetric(vertical: 24),
        child: Text(
          'No runs yet',
          textAlign: TextAlign.center,
          style: _text(context, 14, colors.muted),
        ),
      );
    }
    return _ScheduleCard(
      children: [
        for (final (index, run) in runs.indexed)
          Container(
            padding: const EdgeInsets.all(16),
            decoration: BoxDecoration(
              border: index == 0
                  ? null
                  : Border(top: BorderSide(color: theme.dividerColor)),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 8,
              children: [
                Row(
                  spacing: 8,
                  children: [
                    switch (run['status']) {
                      'succeeded' => const StatusBadge(
                        label: 'Succeeded',
                        variant: StatusBadgeVariant.success,
                        xs: true,
                      ),
                      'failed' => const StatusBadge(
                        label: 'Failed',
                        variant: StatusBadgeVariant.error,
                        xs: true,
                      ),
                      _ => const StatusBadge(
                        label: 'Running',
                        variant: StatusBadgeVariant.warning,
                        xs: true,
                      ),
                    },
                    Expanded(
                      child: Text(
                        'Started ${_timeAgo(run['startedAt'])}',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: _text(context, 12, colors.muted),
                      ),
                    ),
                    if (stringValue(run['agentId']).isNotEmpty)
                      PaseoButton(
                        label: 'Open session',
                        icon: LucideIcons.messageSquare300,
                        variant: PaseoButtonVariant.ghost,
                        size: PaseoControlSize.xs,
                        onPressed: () => Navigator.pop(context, run),
                      ),
                  ],
                ),
                if (stringValue(run['output']).trim().isNotEmpty)
                  Container(
                    constraints: const BoxConstraints(maxHeight: 200),
                    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                    decoration: BoxDecoration(
                      color: colors.surface2,
                      borderRadius: BorderRadius.circular(6),
                    ),
                    child: SingleChildScrollView(
                      child: SelectableText(
                        stringValue(run['output']).trimRight(),
                        style: type.codeStyle.copyWith(color: theme.colorScheme.onSurface),
                      ),
                    ),
                  ),
                if (stringValue(run['error']).isNotEmpty)
                  SelectableText(
                    stringValue(run['error']),
                    style: _text(context, 12, colors.danger),
                  ),
              ],
            ),
          ),
      ],
    );
  }
}

// --- Original `utils/schedule-format.ts` ------------------------------------

String _productName(JsonMap schedule) =>
    jsonMap(schedule['target'])['type'] == 'agent' ? 'Heartbeat' : 'Schedule';

/// Original `resolveScheduleTitle`.
String _scheduleTitle(JsonMap schedule) {
  final name = stringValue(schedule['name']).trim();
  if (name.isNotEmpty) return name;
  final target = jsonMap(schedule['target']);
  if (target['type'] == 'new-agent') {
    final configTitle = stringValue(jsonMap(target['config'])['title']).trim();
    if (configTitle.isNotEmpty) return configTitle;
  }
  final line = stringValue(schedule['prompt'])
      .split('\n')
      .map((l) => l.trim())
      .where((l) => l.isNotEmpty)
      .firstOrNull;
  return line ?? 'Untitled ${_productName(schedule).toLowerCase()}';
}

/// Original `shortenPath`.
String _shortenPath(String path) => path.replaceFirst(RegExp(r'^/(?:Users|home)/[^/]+'), '~');

String _pad2(int value) => value.toString().padLeft(2, '0');

const _msPerMinute = 60000, _msPerHour = 3600000, _msPerDay = 86400000;

/// Original `everyMsToParts`: (value, unit noun).
(int, String) _everyMsToParts(num? ms) {
  if (ms == null || !ms.isFinite || ms <= 0) return (1, 'hour');
  final whole = ms.round();
  if (ms == whole && whole % _msPerDay == 0) return (whole ~/ _msPerDay, 'day');
  if (ms == whole && whole % _msPerHour == 0) return (whole ~/ _msPerHour, 'hour');
  final minutes = (ms / _msPerMinute).round();
  return (minutes < 1 ? 1 : minutes, 'minute');
}

/// Original `formatCadence`.
String _formatCadence(JsonMap cadence) {
  if (cadence['type'] == 'every') {
    final (value, noun) = _everyMsToParts(cadence['everyMs'] as num?);
    return 'Every ${value == 1 ? '1 $noun' : '$value ${noun}s'}';
  }
  final expression = stringValue(cadence['expression']);
  return _describeCron(expression, stringValue(cadence['timezone'], 'UTC')) ?? expression;
}

/// Original `describeCron`: humanizes the fixed-time family, null otherwise.
/// A null [timezone] omits the zone suffix.
String? _describeCron(String expression, String? timezone) {
  final trimmed = expression.trim();
  if (_validateCron(trimmed) != null) return null;
  final [minute, hour, dayOfMonth, month, dayOfWeek] = trimmed.split(RegExp(r'\s+'));
  final digits = RegExp(r'^\d+$');
  if (minute == '*' && hour == '*' && month == '*' && dayOfMonth == '*' && dayOfWeek == '*') {
    return 'Every minute';
  }
  if (!digits.hasMatch(minute) || month != '*' || dayOfMonth != '*') return null;
  final minuteNum = int.parse(minute);
  if (hour == '*') {
    if (dayOfWeek != '*') return null;
    return minuteNum == 0 ? 'Every hour' : 'Every hour at :${_pad2(minuteNum)}';
  }
  if (!digits.hasMatch(hour)) return null;
  const days = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
  final dayLabel = switch (dayOfWeek) {
    '*' => 'Daily',
    '1-5' => 'Weekdays',
    '0,6' || '6,0' => 'Weekends',
    _ when RegExp(r'^\d$').hasMatch(dayOfWeek) && int.parse(dayOfWeek) < 7 =>
      '${days[int.parse(dayOfWeek)]}s',
    _ => null,
  };
  if (dayLabel == null) return null;
  final time = '${_pad2(int.parse(hour))}:${_pad2(minuteNum)}';
  return '$dayLabel at $time${timezone == null ? '' : ' $timezone'}';
}

/// Original `validateCron` over the protocol's `validateCronExpression`.
String? _validateCron(String expression) {
  final trimmed = expression.trim();
  if (trimmed.isEmpty) return 'Enter a cron expression';
  final parts = trimmed.split(RegExp(r'\s+'));
  if (parts.length != 5) return 'Cron expressions must have 5 fields';
  const bounds = [
    (0, 59, 'minute'),
    (0, 23, 'hour'),
    (1, 31, 'day-of-month'),
    (1, 12, 'month'),
    (0, 6, 'day-of-week'),
  ];
  for (var i = 0; i < 5; i++) {
    final (min, max, name) = bounds[i];
    final error = _cronFieldError(parts[i], min, max, name);
    if (error != null) return error;
  }
  return null;
}

String? _cronFieldError(String source, int min, int max, String name) {
  final digits = RegExp(r'^\d+$');
  for (final raw in source.split(',')) {
    final part = raw.trim();
    if (part.isEmpty) return 'Invalid $name field';
    final steps = part.split('/');
    if (steps.length > 2) return 'Invalid $name step';
    if (steps.length == 2) {
      final source = steps[1].trim();
      final step = int.tryParse(source);
      if (step == null || step <= 0 || '$step' != source) return 'Invalid $name step';
    }
    final base = steps[0];
    if (base == '*') continue;
    final range = RegExp(r'^(\d+)-(\d+)$').firstMatch(base);
    if (range != null) {
      final start = int.tryParse(range[1]!), end = int.tryParse(range[2]!);
      if (start == null || end == null || start > end || start < min || end > max) {
        return 'Invalid $name range';
      }
      continue;
    }
    final value = digits.hasMatch(base) ? int.tryParse(base) : null;
    if (value == null || value < min || value > max) return 'Invalid $name value';
  }
  return null;
}

/// Original `formatNextRun`: "soon", "in 5m", "in 3h", "in 2d".
String _formatNextRun(Object? iso) {
  final target = DateTime.tryParse(stringValue(iso));
  if (target == null) return '';
  final diff = target.difference(DateTime.now()).inMilliseconds;
  if (diff < _msPerMinute) return 'soon';
  if (diff < _msPerHour) return 'in ${(diff / _msPerMinute).round()}m';
  if (diff < _msPerDay) return 'in ${(diff / _msPerHour).round()}h';
  return 'in ${(diff / _msPerDay).round()}d';
}

/// Original `describeTimeAgo`: "just now", "5m ago", "2h ago", "3d ago", then
/// "Jan 15" past a week.
String _timeAgo(Object? iso) {
  final time = DateTime.tryParse(stringValue(iso))?.toLocal();
  if (time == null) return '';
  final elapsed = DateTime.now().difference(time).inMilliseconds;
  if (elapsed < _msPerMinute) return 'just now';
  if (elapsed < _msPerHour) return '${elapsed ~/ _msPerMinute}m ago';
  if (elapsed < _msPerDay) return '${elapsed ~/ _msPerHour}h ago';
  if (elapsed < 7 * _msPerDay) return '${elapsed ~/ _msPerDay}d ago';
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  return '${months[time.month - 1]} ${time.day}';
}

// --- Original `schedule-cadence-options.ts` ---------------------------------

const _cadencePresets = [
  ('every-minute', 'Every minute', '* * * * *'),
  ('every-hour', 'Every hour', '0 * * * *'),
  ('daily-9', 'Daily 9:00', '0 9 * * *'),
  ('weekdays-9', 'Weekdays 9:00', '0 9 * * 1-5'),
  ('mondays-9', 'Mondays 9:00', '0 9 * * 1'),
];

/// Original `normalizeScheduleFormCadence`: interval cadences become the
/// equivalent cron expression.
String _cronExpressionFor(JsonMap cadence) {
  if (cadence['type'] == 'cron') return stringValue(cadence['expression']);
  final (value, unit) = _everyMsToParts(cadence['everyMs'] as num?);
  return switch (unit) {
    'minute' => value == 1 ? '* * * * *' : '*/${value.clamp(1, 59)} * * * *',
    'hour' => value == 1 ? '0 * * * *' : '0 */${value.clamp(1, 23)} * * *',
    _ => value == 1 ? '0 9 * * *' : '0 9 */${value.clamp(1, 31)} * *',
  };
}
