import 'dart:async';

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import 'app_preferences.dart' show sidebarNavPreferences;
import 'settings_forms.dart';
import 'settings_host_widgets.dart';

/// Original `supportsUsageReports`.
bool supportsUsageReports(JsonMap features) =>
    features['usageSources'] == true || features['providerUsageList'] == true;

/// Original `laterFetched`: a copy fetched after the arriving one stays.
JsonMap _laterFetched(JsonMap shown, JsonMap arriving) {
  final a = DateTime.tryParse(stringValue(shown['fetchedAt']));
  final b = DateTime.tryParse(stringValue(arriving['fetchedAt']));
  return a != null && b != null && a.isAfter(b) ? shown : arriving;
}

/// Original `upsertReport`: a streamed report replaces its copy (unless that
/// copy is newer) or is appended.
List<JsonMap> upsertUsageReport(List<JsonMap>? reports, JsonMap report) {
  final current = reports ?? const <JsonMap>[];
  if (!current.any((entry) => entry['id'] == report['id'])) return [...current, report];
  return [for (final entry in current) entry['id'] == report['id'] ? _laterFetched(entry, report) : entry];
}

/// Original `settleReports`: shown reports keep their place, missing ones go,
/// new ones are appended, newer copies stay.
List<JsonMap> settleUsageReports(List<JsonMap>? shown, List<JsonMap> finished) {
  final remaining = {for (final report in finished) report['id']: report};
  final settled = <JsonMap>[];
  for (final report in shown ?? const <JsonMap>[]) {
    final arriving = remaining.remove(report['id']);
    if (arriving != null) settled.add(_laterFetched(report, arriving));
  }
  return [...settled, ...remaining.values];
}

/// Original `replaceReport`: null means the host no longer knows the report.
List<JsonMap> replaceUsageReport(List<JsonMap> reports, String reportId, JsonMap? refreshed) =>
    refreshed == null
        ? reports.where((report) => report['id'] != reportId).toList()
        : [for (final report in reports) report['id'] == reportId ? refreshed : report];

/// Original `displayPercent`.
num? usageDisplayPercent(JsonMap window, {required bool remaining}) {
  final used = window['usedPct'], left = window['remainingPct'];
  final num? usedPct = used is num ? used : left is num ? 100 - left : null;
  if (!remaining) return usedPct;
  if (left is num) return left;
  return usedPct == null ? null : 100 - usedPct;
}

/// Original `effectiveUsagePins`: the stored selection, or each available
/// report's summary windows (else its first window with a percent).
List<JsonMap> effectiveUsagePins(JsonMap preferences, List<JsonMap> reports) {
  if (preferences['pins'] is List) return jsonList(preferences['pins']).map(jsonMap).toList();
  final remaining = preferences['displayAs'] == 'remaining';
  final pins = <JsonMap>[];
  for (final entry in reports) {
    final report = jsonMap(entry['report']);
    if (report['status'] != 'available') continue;
    final withPercent = jsonList(report['windows'])
        .map(jsonMap)
        .where((window) => usageDisplayPercent(window, remaining: remaining) != null)
        .toList();
    final marked = withPercent.where((window) => window['summary'] == true).toList();
    for (final window in marked.isNotEmpty ? marked : withPercent.take(1)) {
      final pin = {'sourceId': entry['sourceId'], 'windowId': window['id']};
      if (!pins.any((p) => p['sourceId'] == pin['sourceId'] && p['windowId'] == pin['windowId'])) {
        pins.add(pin);
      }
    }
  }
  return pins;
}

/// Original `toggleUsagePin`: the first edit snapshots the shown defaults.
JsonMap toggleUsagePin(JsonMap preferences, JsonMap pin, List<JsonMap> reports) {
  bool same(JsonMap p) => p['sourceId'] == pin['sourceId'] && p['windowId'] == pin['windowId'];
  final selected = effectiveUsagePins(preferences, reports);
  return {
    ...preferences,
    'pins': selected.any(same)
        ? selected.where((p) => !same(p)).toList()
        : [...selected, {'sourceId': pin['sourceId'], 'windowId': pin['windowId']}],
  };
}

/// Original `listUsageReports` COMPAT(providerUsageList): a pre-usageSources
/// host's provider list as report entries.
List<JsonMap> legacyUsageReports(JsonMap payload) => [
  for (final provider in jsonList(payload['providers']).map(jsonMap))
    {
      'id': provider['providerId'],
      'sourceId': provider['providerId'],
      'sourceLabel': provider['displayName'],
      'account': const <String, dynamic>{},
      'fetchedAt': provider['fetchedAt'] ?? payload['fetchedAt'],
      'report': switch (provider['status']) {
        'available' => {
          'status': 'available',
          'windows': jsonList(provider['windows']),
          'balances': ?provider['balances'],
          'details': ?provider['details'],
          'planLabel': ?provider['planLabel'],
        },
        'error' => {'status': 'error', 'error': stringValue(provider['error'])},
        _ => {
          'status': 'unavailable',
          'problem': {'kind': 'no_quota', 'detail': stringValue(provider['error'])},
        },
      },
    },
];

/// Original `usage/queries.ts` for one host: the report list every usage
/// surface on this device shares, re-read after a minute (the daemon caches
/// each report for five), streamed in as sources answer, and refreshed from
/// the sources only on an explicit refresh.
class HostUsageReports extends ChangeNotifier {
  HostUsageReports._(this.client);
  static final _hosts = Expando<HostUsageReports>();
  static HostUsageReports of(PaseoClient client) => _hosts[client] ??= HostUsageReports._(client);

  /// Original REPORTS_STALE_TIME_MS.
  static const staleTime = Duration(minutes: 1);

  final PaseoClient client;
  List<JsonMap>? data;
  Object? error;
  DateTime? _updatedAt;
  Future<void>? _inflight;
  bool get fetching => _inflight != null;

  /// Per-report refresh (original `useReportRefresh`): `pending` or `failed`.
  final refreshStates = <String, String>{};

  JsonMap get _features => jsonMap(client.serverInfo['features']);
  bool get supported => supportsUsageReports(_features);
  bool get _canFetch => client.status == ConnectionStatus.connected && supported;

  /// The query's mount/reconnect fetch: only when there is nothing or it is stale.
  void ensureFresh() {
    if (!_canFetch) return;
    final updatedAt = _updatedAt;
    if (data != null && updatedAt != null && DateTime.now().difference(updatedAt) < staleTime) return;
    unawaited(fetch().catchError((Object _) {}));
  }

  /// A fetch already running is joined, as the original query dedupes.
  Future<void> fetch({bool force = false}) {
    final running = _inflight;
    if (running != null) return running;
    final run = _fetch(force);
    _inflight = run;
    notifyListeners();
    return run.whenComplete(() {
      _inflight = null;
      notifyListeners();
    });
  }

  Future<void> _fetch(bool force) async {
    try {
      final finished = await _list(
        force: force,
        onReport: (report) {
          data = upsertUsageReport(data, report);
          _updatedAt = DateTime.now();
          notifyListeners();
        },
      );
      data = settleUsageReports(data, finished);
      _updatedAt = DateTime.now();
      error = null;
    } catch (e) {
      error = e;
      rethrow;
    }
  }

  /// Original `listUsageReports`.
  Future<List<JsonMap>> _list({
    bool force = false,
    List<String>? reportIds,
    void Function(JsonMap report)? onReport,
  }) async {
    if (client.status != ConnectionStatus.connected) {
      throw StateError('Host connection is not ready');
    }
    if (!supported) throw StateError('Update the host to see usage.');
    if (_features['usageSources'] != true) {
      final payload = await settingsRequest(
        client,
        'provider.usage.list.request',
        {},
        responseType: 'provider.usage.list.response',
      );
      return legacyUsageReports(payload)
          .where((report) => reportIds == null || reportIds.contains(report['id']))
          .toList();
    }
    final requestId = 'usage-${DateTime.now().microsecondsSinceEpoch}';
    final reports = <JsonMap>[];
    final subscription = client.events.listen((event) {
      if (event['type'] != 'usage.list_reports.update') return;
      final payload = jsonMap(event['payload']);
      if (payload['requestId'] != requestId) return;
      final report = jsonMap(payload['report']);
      reports.add(report);
      onReport?.call(report);
    });
    try {
      await settingsRequest(client, 'usage.list_reports.request', {
        'requestId': requestId,
        'forceRefresh': force,
        'reportIds': ?reportIds,
      }, responseType: 'usage.list_reports.response');
      return reports;
    } finally {
      await subscription.cancel();
    }
  }

  /// Original `refreshReports`: Refresh all asks every source again.
  Future<void> refreshAll() => fetch(force: true).catchError((Object _) {});

  /// Original `useReportRefresh`: one report asked again; its card keeps the
  /// previous copy until the answer replaces (or removes) it.
  Future<void> refreshReport(String reportId) async {
    refreshStates[reportId] = 'pending';
    notifyListeners();
    try {
      final reports = await _list(force: true, reportIds: [reportId]);
      final refreshed = reports.where((report) => report['id'] == reportId).firstOrNull;
      final current = data;
      if (current != null) data = replaceUsageReport(current, reportId, refreshed);
      refreshStates.remove(reportId);
    } catch (_) {
      refreshStates[reportId] = 'failed';
    }
    notifyListeners();
  }
}

const _providerUsageLabels = {
  'session': ('Session', 'session'),
  'weekly': ('Weekly', 'weekly'),
  'monthly': ('Monthly', 'monthly'),
  'code_review': ('Code review', 'codeReview'),
  'credits': ('Credits', 'credits'),
  'monthly_credits': ('Monthly credits', 'monthlyCredits'),
};

/// Original `formatProviderUsageLabel`: the daemon's English labels for the
/// standard windows are translated; any other label shows as sent.
String formatProviderUsageLabel(BuildContext context, String id, String label) {
  const reviewPrefix = 'Code review · ';
  if (id.startsWith('code_review:') && label.startsWith(reviewPrefix)) {
    return '${tr(context, 'providerUsage.labels.codeReview')} · '
        '${formatProviderUsageLabel(context, id.substring('code_review:'.length), label.substring(reviewPrefix.length))}';
  }
  final standard = _providerUsageLabels[id] ?? (id == 'five_hour' ? _providerUsageLabels['session'] : null);
  return standard != null && standard.$1 == label ? tr(context, 'providerUsage.labels.${standard.$2}') : label;
}

/// Original `UsageView` kinds.
enum UsageViewKind { unavailable, loading, error, ready }

/// Original `resolveUsageView`.
UsageViewKind resolveUsageView({
  required bool connected,
  required bool supported,
  required List<JsonMap>? data,
  required Object? error,
}) {
  if (!connected || !supported) return UsageViewKind.unavailable;
  if (data != null) return UsageViewKind.ready;
  if (error != null) return UsageViewKind.error;
  return UsageViewKind.loading;
}

/// Original `usageCopy.problem` / `usageProblemText`.
String usageProblemText(BuildContext context, JsonMap report) {
  if (report['status'] == 'error') return stringValue(report['error']);
  if (report['status'] != 'unavailable') return '';
  final problem = jsonMap(report['problem']);
  if (problem['kind'] == 'no_quota') return stringValue(problem['detail']);
  final refreshedBy = stringValue(problem['refreshedBy']);
  final remedy = refreshedBy.isNotEmpty
      ? tr(context, 'providerUsage.runToRefresh', args: {'command': refreshedBy})
      : tr(context, 'providerUsage.signInAgain');
  if (problem['kind'] == 'rejected') {
    return tr(context, 'providerUsage.loginRejected', args: {'status': problem['status'], 'remedy': remedy});
  }
  return tr(context, 'providerUsage.loginExpired', args: {
    'relativeTime': settingsRelativeTime(context, problem['expiresAt'] as String?, ago: true) ?? '',
    'remedy': remedy,
  });
}

class UsageSettings extends StatefulWidget {
  final PaseoClient client;
  final String serverId;
  const UsageSettings({
    super.key,
    required this.client,
    required this.serverId,
  });
  @override
  State<UsageSettings> createState() => _UsageSettingsState();
}

/// Original `HostUsageSection` / `UsageSection`.
class _UsageSettingsState extends State<UsageSettings> {
  late HostUsageReports usage;
  late ConnectionStatus connectionStatus;

  @override
  void initState() {
    super.initState();
    bind();
  }

  void bind() {
    usage = HostUsageReports.of(widget.client)..addListener(changed);
    connectionStatus = widget.client.status;
    widget.client.addListener(connectionChanged);
    usage.ensureFresh();
  }

  void unbind(PaseoClient client) {
    usage.removeListener(changed);
    client.removeListener(connectionChanged);
  }

  void changed() {
    if (mounted) setState(() {});
  }

  void connectionChanged() {
    if (!mounted || connectionStatus == widget.client.status) return;
    connectionStatus = widget.client.status;
    usage.ensureFresh();
    setState(() {});
  }

  @override
  void didUpdateWidget(UsageSettings old) {
    super.didUpdateWidget(old);
    if (old.client != widget.client) {
      unbind(old.client);
      bind();
    }
  }

  @override
  void dispose() {
    unbind(widget.client);
    super.dispose();
  }

  JsonMap get preferences => jsonMap(PaseoScope.of(context).preferences['usage']);

  /// Original `useUsageInSidebar`: the Usage row of Settings › Sidebar.
  bool get inSidebar => jsonList(PaseoScope.of(context).preferences['sidebarFooterItems'])
      .map(jsonMap)
      .any((item) => item['key'] == 'usage' && item['visible'] == true);

  Future<void> savePreference(String key, Object value) async {
    final store = PaseoScope.of(context);
    final before = store.preferences[key];
    store.preferences[key] = value;
    if (mounted) setState(() {});
    try {
      await store.savePreferences();
    } catch (e) {
      if (before == null) {
        store.preferences.values.remove(key);
      } else {
        store.preferences[key] = before;
      }
      if (mounted) {
        setState(() {});
        showError(context, e);
      }
    }
  }

  void togglePin(JsonMap entry, JsonMap window) => savePreference(
    'usage',
    toggleUsagePin(
      preferences,
      {'sourceId': entry['sourceId'], 'windowId': window['id']},
      usage.data ?? const [],
    ),
  );

  void setInSidebar(bool visible) {
    final stored = jsonList(PaseoScope.of(context).preferences['sidebarFooterItems']).map(jsonMap).toList();
    savePreference('sidebarFooterItems', sidebarNavPreferences([
      {'key': 'usage', 'visible': visible},
    ], stored));
  }

  /// Original `UsageOptions`: the sidebar summary switch and the
  /// used/remaining choice.
  Future<void> openOptions() async {
    String t(String key) => tr(context, key);
    await showAdaptiveSheet<void>(
      context: context,
      maxWidth: 340,
      builder: (sheet) => StatefulBuilder(
        builder: (sheet, refresh) => SheetContents(
          title: t('providerUsage.options'),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              SettingsSwitchRow(
                label: t('providerUsage.showInSidebar'),
                hint: t('providerUsage.showInSidebarHint'),
                value: inSidebar,
                onChanged: (v) async {
                  setInSidebar(v);
                  refresh(() {});
                },
              ),
              SettingsRow(
                label: t('providerUsage.displayAs'),
                control: PaseoSegmentedControl<String>(
                  size: PaseoControlSize.sm,
                  value: preferences['displayAs'] == 'remaining' ? 'remaining' : 'used',
                  segments: [
                    PaseoSegment('used', t('providerUsage.displayUsed')),
                    PaseoSegment('remaining', t('providerUsage.displayRemaining')),
                  ],
                  onChanged: (v) {
                    savePreference('usage', {...preferences, 'displayAs': v});
                    refresh(() {});
                  },
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// Original `UsageCard` with `resolveUsageReport`: the current outcome owns
  /// the messages; only a successful poll owns values and freshness.
  Widget card(JsonMap entry, {required bool pinnable}) {
    final pref = preferences;
    final report = jsonMap(entry['report']);
    final available = report['status'] == 'available';
    final snapshot = available || entry['lastSuccessful'] == null ? null : jsonMap(entry['lastSuccessful']);
    final content = available ? report : jsonMap(snapshot?['report']);
    final loginErrors = entry['loginErrors'];
    final id = stringValue(entry['id']);
    final pins = effectiveUsagePins(pref, usage.data ?? const []);
    return UsageCard(
      title: stringValue(entry['sourceLabel'], id),
      icon: entry['icon'] as String?,
      status: stringValue(report['status']),
      planLabel: stringValue(content['planLabel']),
      windows: jsonList(content['windows']).map(jsonMap).toList(),
      balances: jsonList(content['balances']).map(jsonMap).toList(),
      details: jsonList(content['details']).map(jsonMap).toList(),
      messages: [
        if (!available)
          if (loginErrors is List)
            for (final login in loginErrors.map(jsonMap))
              '${login['harness']}: ${usageProblemText(context, jsonMap(login['report']))}'
          else
            usageProblemText(context, report),
      ],
      footer: jsonMap(entry['account'])['label'] as String?,
      successfulAt: available
          ? stringValue(entry['successfulAt'], stringValue(entry['fetchedAt']))
          : snapshot?['fetchedAt'] as String?,
      retained: snapshot != null,
      displayRemaining: pref['displayAs'] == 'remaining',
      isPinned: pinnable
          ? (window) => pins.any((p) => p['sourceId'] == entry['sourceId'] && p['windowId'] == window['id'])
          : null,
      onTogglePin: pinnable ? (window) => togglePin(entry, window) : null,
      onRefresh: () => unawaited(usage.refreshReport(id)),
      refreshing: usage.refreshStates[id] == 'pending',
      refreshFailed: usage.refreshStates[id] == 'failed',
    );
  }

  @override
  Widget build(BuildContext context) {
    String t(String key) => tr(context, key);
    final colors = PaseoColors.of(context);
    final icon = densityIcon(context, 'sm');
    final hostLabel = PaseoScope.of(context)
            .hosts
            .where((host) => host.serverId == widget.serverId)
            .firstOrNull
            ?.label ??
        widget.serverId;
    final connected = widget.client.status == ConnectionStatus.connected;
    final view = resolveUsageView(
      connected: connected,
      supported: usage.supported,
      data: usage.data,
      error: usage.error,
    );
    final busy = view == UsageViewKind.loading || (view == UsageViewKind.ready && usage.fetching);
    final reports = usage.data ?? const <JsonMap>[];
    final pinnable = inSidebar;
    final buttonSize = isCompactLayout(context) ? PaseoControlSize.sm : PaseoControlSize.xs;
    return SettingsSection(
      title: t('providerUsage.title'),
      card: false,
      trailing: view == UsageViewKind.unavailable
          ? null
          : Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 8,
              children: [
                PaseoButton(
                  tooltip: t('providerUsage.refreshAll'),
                  semanticLabel: t('providerUsage.refreshAll'),
                  size: buttonSize,
                  variant: PaseoButtonVariant.ghost,
                  leading: busy
                      ? SettingsSpinner(size: icon, color: colors.extraMuted)
                      : Icon(LucideIcons.rotateCw300, size: icon, color: colors.extraMuted),
                  onPressed: busy ? null : usage.refreshAll,
                ),
                PaseoButton(
                  tooltip: t('providerUsage.options'),
                  semanticLabel: t('providerUsage.options'),
                  size: buttonSize,
                  variant: PaseoButtonVariant.ghost,
                  leading: Icon(LucideIcons.settings300, size: icon, color: colors.extraMuted),
                  onPressed: openOptions,
                ),
              ],
            ),
      children: [
        switch (view) {
          UsageViewKind.unavailable => SettingsEmptyCard(tr(
            context,
            connected ? 'providerUsage.hostUpgradeRequired' : 'providerUsage.hostUnavailable',
            args: {'host': hostLabel},
          )),
          UsageViewKind.loading => SettingsEmptyCard(t('providerUsage.loading')),
          UsageViewKind.error => _UsageAlert(
            title: t('providerUsage.errorTitle'),
            description: errorMessage(usage.error!),
            action: PaseoButton(
              label: t('providerUsage.retry'),
              size: PaseoControlSize.sm,
              variant: PaseoButtonVariant.outline,
              onPressed: usage.refreshAll,
            ),
          ),
          UsageViewKind.ready when reports.isEmpty => SettingsEmptyCard(t('providerUsage.empty')),
          UsageViewKind.ready => Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            spacing: 12,
            children: [for (final entry in reports) card(entry, pinnable: pinnable)],
          ),
        },
      ],
    );
  }
}


/// Original `components/ui/alert.tsx` (size sm, variant error): a danger-tinted
/// rounded box with a medium title, muted description and an action below.
class _UsageAlert extends StatelessWidget {
  const _UsageAlert({required this.title, required this.description, this.action});
  final String title, description;
  final Widget? action;
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final tint = theme.brightness == Brightness.dark ? .16 : .12;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        color: colors.danger.withValues(alpha: tint),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        spacing: 4,
        children: [
          Text(
            title,
            style: theme.textTheme.bodyMedium!.copyWith(
              fontSize: type.uiSize(14),
              fontWeight: FontWeight.w500,
              color: colors.danger,
            ),
          ),
          SelectableText(
            description,
            style: theme.textTheme.bodySmall!.copyWith(
              fontSize: type.uiSize(12),
              color: colors.muted,
            ),
          ),
          if (action != null)
            Padding(padding: const EdgeInsets.only(top: 8), child: action),
        ],
      ),
    );
  }
}

/// Original `useTimeAgo` / reset-duration wording ("2 hours ago", "in 3 days").
String? settingsRelativeTime(BuildContext context, String? iso, {required bool ago}) {
  final at = iso == null ? null : DateTime.tryParse(iso);
  if (at == null) return null;
  final diff = ago ? DateTime.now().difference(at) : at.difference(DateTime.now());
  String unit(String key, int value) =>
      tr(context, 'providerUsage.${ago ? 'timing' : 'duration'}.$key', args: {'value': value});
  if (ago && diff.inSeconds < 60) return tr(context, 'providerUsage.timing.justNow');
  if (!ago && diff <= Duration.zero) return '';
  if (diff.inDays > 0) return unit(ago ? 'daysAgo' : 'days', diff.inDays);
  if (diff.inHours > 0) return unit(ago ? 'hoursAgo' : 'hours', diff.inHours);
  return unit(ago ? 'minutesAgo' : 'minutes', diff.inMinutes);
}

/// Original `usage/card.tsx` inside a settings card: 16px padding, 12px gaps,
/// a 14px source icon + base name + xs plan badge header with a status dot,
/// 4px-spaced window bars and detail rows, a muted 12px account / freshness
/// footer and 12px danger messages.
///
/// [compact] is the card's `compact` form (the agent's context-window
/// details): no frame or padding, just the 12px-gapped blocks. [onRefresh]
/// adds the header's Refresh button (RotateCw, extra-muted; a spinner while
/// [refreshing]); where hover exists its tooltip carries the freshness, which
/// otherwise stays in the footer. [refreshFailed] adds "Unable to refresh
/// usage" under the footer. Both default off, so Settings is unchanged.
class UsageCard extends StatelessWidget {
  const UsageCard({
    super.key,
    required this.title,
    required this.status,
    required this.windows,
    required this.balances,
    required this.details,
    required this.messages,
    required this.retained,
    required this.displayRemaining,
    this.icon,
    this.planLabel = '',
    this.footer,
    this.successfulAt,
    this.isPinned,
    this.onTogglePin,
    this.compact = false,
    this.onRefresh,
    this.refreshing = false,
    this.refreshFailed = false,
  });
  final String title, status, planLabel;
  final String? icon, footer, successfulAt;
  final List<JsonMap> windows, balances, details;
  final List<String> messages;
  final bool retained, displayRemaining;
  final bool Function(JsonMap window)? isPinned;
  final ValueChanged<JsonMap>? onTogglePin;
  final bool compact, refreshing, refreshFailed;
  final VoidCallback? onRefresh;

  /// card.tsx UsageRefreshButton: a small toolbar button (20px frame, 14px
  /// glyph; 32 / 18 on compact layouts) whose hitbox overhangs the header to
  /// the right and vertically instead of pushing it.
  Widget _refreshButton(BuildContext context, String freshness, bool inlineFreshness) {
    final colors = PaseoColors.of(context);
    final compactLayout = isCompactLayout(context);
    final frame = compactLayout ? 32.0 : 20.0, glyph = compactLayout ? 18.0 : 14.0;
    final button = Semantics(
      button: true,
      enabled: !refreshing,
      label: '${tr(context, 'providerUsage.refresh')} $title',
      excludeSemantics: true,
      child: Material(
        type: MaterialType.transparency,
        child: InkWell(
          customBorder: const CircleBorder(),
          hoverColor: colors.surface2,
          onTap: refreshing ? null : onRefresh,
          child: SizedBox.square(
            dimension: frame,
            child: Center(
              child: refreshing
                  ? SettingsSpinner(size: glyph, color: colors.extraMuted)
                  : Icon(LucideIcons.rotateCw300, size: glyph, color: colors.extraMuted),
            ),
          ),
        ),
      ),
    );
    return SizedBox.square(
      dimension: glyph,
      child: OverflowBox(
        maxWidth: frame,
        maxHeight: frame,
        child: inlineFreshness
            ? button
            : Tooltip(
                message: freshness,
                waitDuration: Duration.zero,
                preferBelow: false,
                child: button,
              ),
      ),
    );
  }
  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(fontSize: type.uiSize(12));
    final mutedSmall = small.copyWith(color: colors.muted);
    final statusText = status == 'available' || status.isEmpty
        ? null
        : tr(context, status == 'error' ? 'providerUsage.status.error' : 'providerUsage.status.unavailable');
    final ago = settingsRelativeTime(context, successfulAt, ago: true);
    final freshness = ago == null
        ? tr(context, 'providerUsage.timing.noSuccessfulUpdate')
        : tr(
            context,
            retained ? 'providerUsage.timing.lastUpdated' : 'providerUsage.timing.updated',
            args: {'relativeTime': ago},
          );
    final svg = icon == null || icon!.isEmpty ? null : icon;
    // Window rows bleed 8px into the 16px card padding for their highlight, so
    // the card pads 8px and every other block re-insets by 8px.
    final native = defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    // card.tsx showsFreshnessInline: no hover (native, compact) or no Refresh.
    final inlineFreshness = native || isCompactLayout(context) || onRefresh == null;
    Widget inset(Widget child) =>
        Padding(padding: const EdgeInsets.symmetric(horizontal: 8), child: child);
    return DecoratedBox(
      decoration: compact
          ? const BoxDecoration()
          : BoxDecoration(
              color: colors.surface1,
              border: Border.all(color: theme.dividerColor),
              borderRadius: BorderRadius.circular(8),
            ),
      child: Material(
        type: MaterialType.transparency,
        child: Padding(
        padding: compact
            ? EdgeInsets.zero
            : const EdgeInsets.symmetric(horizontal: 8, vertical: 16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 12,
          children: [
            inset(Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              spacing: 8,
              children: [
                Flexible(
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    spacing: 8,
                    children: [
                      if (svg != null)
                        SvgPicture.string(
                          svg,
                          width: 14,
                          height: 14,
                          theme: SvgTheme(currentColor: theme.colorScheme.onSurface),
                        ),
                      Flexible(
                        child: Text(
                          title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodyMedium!.copyWith(fontSize: type.uiSize(14)),
                        ),
                      ),
                      if (planLabel.isNotEmpty) StatusBadge(label: planLabel, xs: true),
                    ],
                  ),
                ),
                if (statusText != null || onRefresh != null)
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    spacing: 8,
                    children: [
                      if (statusText != null)
                        Row(
                          mainAxisSize: MainAxisSize.min,
                          spacing: 6,
                          children: [
                            Container(
                              width: 8,
                              height: 8,
                              decoration: BoxDecoration(
                                shape: BoxShape.circle,
                                color: status == 'error' ? colors.danger : colors.muted,
                              ),
                            ),
                            Text(statusText, style: mutedSmall),
                          ],
                        ),
                      if (onRefresh != null) _refreshButton(context, freshness, inlineFreshness),
                    ],
                  ),
              ],
            )),
            if (messages.isNotEmpty)
              inset(SelectableText(
                messages.join('\n'),
                style: small.copyWith(color: colors.danger, height: 1.4),
              )),
            if (windows.isNotEmpty || balances.isNotEmpty)
              Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 4,
                children: [
                  for (final window in windows) _windowBar(context, window),
                  for (final balance in balances) _balanceBar(context, balance),
                ],
              ),
            if (details.isNotEmpty)
              inset(Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 4,
                children: [
                  for (final detail in details)
                    Row(
                      spacing: 8,
                      children: [
                        Expanded(
                          child: Text(
                            stringValue(detail['label']),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: mutedSmall,
                          ),
                        ),
                        Text(
                          '${detail['value'] ?? ''}',
                          maxLines: 1,
                          style: small,
                        ),
                      ],
                    ),
                ],
              )),
            if ((footer ?? '').isNotEmpty || inlineFreshness)
              inset(Row(
                spacing: 8,
                children: [
                  Expanded(
                    child: Text(
                      footer ?? '',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: mutedSmall,
                    ),
                  ),
                  if (inlineFreshness) Text(freshness, maxLines: 1, style: mutedSmall),
                ],
              )),
            if (refreshFailed)
              inset(Text(
                tr(context, 'providerUsage.refreshFailed'),
                style: small.copyWith(color: colors.danger, height: 1.4),
              )),
          ],
        ),
      ),
      ),
    );
  }

  static Color _tone(PaseoColors colors, Object? tone, num? used) {
    final resolved = tone is String
        ? tone
        : used == null
        ? 'default'
        : used > 90
        ? 'danger'
        : used >= 70
        ? 'warning'
        : 'default';
    return switch (resolved) {
      'ok' => colors.success,
      'warning' => colors.warning,
      'danger' => colors.danger,
      _ => colors.muted,
    };
  }

  /// Original `UsageMeter`: a 4px surface3 track with a tone-coloured fill.
  static Widget _meter(BuildContext context, num percent, Color fill) {
    final colors = PaseoColors.of(context);
    return ClipRRect(
      borderRadius: BorderRadius.circular(2),
      child: Container(
        height: 4,
        color: colors.surface3,
        alignment: Alignment.centerLeft,
        child: FractionallySizedBox(
          widthFactor: (percent / 100).clamp(0.0, 1.0).toDouble(),
          heightFactor: 1,
          child: DecoratedBox(
            decoration: BoxDecoration(
              color: fill,
              borderRadius: BorderRadius.circular(999),
            ),
          ),
        ),
      ),
    );
  }

  /// Original `UsageWindowBar`: muted 12px label, medium 12px value with a
  /// muted (danger when at risk) " · resets …" tail, the meter 3px below; the
  /// row bleeds 8px into the card padding with 6px vertical padding, and a
  /// pinnable row shows its 12px pin glyph and a surface2 pinned highlight.
  Widget _windowBar(BuildContext context, JsonMap window) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(fontSize: type.uiSize(12));
    final usedRaw = window['usedPct'];
    final leftRaw = window['remainingPct'];
    final num? used = usedRaw is num
        ? usedRaw
        : leftRaw is num
        ? 100 - leftRaw
        : null;
    final num? shown = displayRemaining
        ? (leftRaw is num ? leftRaw : used == null ? null : 100 - used)
        : used;
    final pct = shown == null ? null : '${shown.clamp(0, 100).round()}%';
    final value = pct == null
        ? '—'
        : displayRemaining
        ? tr(context, 'providerUsage.values.remaining', args: {'amount': pct})
        : tr(context, 'providerUsage.values.used', args: {'percentage': pct});
    final runsOut = window['runsOutAt'] as String?;
    final atRisk = runsOut != null &&
        window['shortfallPct'] != null &&
        (!retained || (DateTime.tryParse(runsOut)?.isAfter(DateTime.now()) ?? false));
    String? trailing;
    if (atRisk) {
      final d = settingsRelativeTime(context, runsOut, ago: false);
      trailing = d == null
          ? null
          : d.isEmpty
          ? tr(context, 'providerUsage.timing.runsOutNow')
          : tr(context, 'providerUsage.timing.runsOutIn', args: {'duration': d});
    } else {
      final d = settingsRelativeTime(context, window['resetsAt'] as String?, ago: false);
      trailing = d == null
          ? null
          : d.isEmpty
          ? tr(context, retained ? 'providerUsage.timing.resetTimePassed' : 'providerUsage.timing.resettingNow')
          : tr(context, 'providerUsage.timing.resetsIn', args: {'duration': d});
    }
    final pinnable = onTogglePin != null;
    final pinned = isPinned?.call(window) ?? false;
    final content = Row(
      spacing: 8,
      children: [
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            spacing: 3,
            children: [
              LayoutBuilder(
                builder: (context, constraints) => Row(
                  spacing: 8,
                  children: [
                    Expanded(
                      child: Text(
                        formatProviderUsageLabel(
                          context,
                          stringValue(window['id']),
                          stringValue(window['label'], stringValue(window['id'])),
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: small.copyWith(color: colors.muted),
                      ),
                    ),
                    // A flex-shrink value keeps its natural width before the
                    // label grows; two Flutter flex children split the width.
                    ConstrainedBox(
                      constraints: BoxConstraints(maxWidth: (constraints.maxWidth - 8).clamp(0.0, double.infinity)),
                      child: Text.rich(
                        TextSpan(
                          text: value,
                          children: [
                            if (trailing != null)
                              TextSpan(
                                text: ' · $trailing',
                                style: TextStyle(
                                  color: atRisk ? colors.danger : colors.muted,
                                  fontWeight: FontWeight.normal,
                                ),
                              ),
                          ],
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        textAlign: TextAlign.end,
                        style: small.copyWith(fontWeight: FontWeight.w500),
                      ),
                    ),
                  ],
                ),
              ),
              _meter(context, used ?? 0, _tone(colors, window['tone'], used)),
            ],
          ),
        ),
        if (pinnable)
          Tooltip(
            message: tr(context, pinned ? 'providerUsage.unpin' : 'providerUsage.pin'),
            child: Icon(
              pinned ? LucideIcons.pinOff300 : LucideIcons.pin300,
              size: 12,
              color: colors.muted,
            ),
          ),
      ],
    );
    final row = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
      decoration: BoxDecoration(
        color: pinned ? colors.surface2 : null,
        borderRadius: BorderRadius.circular(6),
      ),
      child: content,
    );
    if (!pinnable) return row;
    // Pinned rows carry the surface2 highlight; hovering an unpinned row
    // previews it at half strength.
    return Semantics(
      checked: pinned,
      child: InkWell(
        borderRadius: BorderRadius.circular(6),
        hoverColor: pinned ? Colors.transparent : colors.surface2.withValues(alpha: .5),
        highlightColor: Colors.transparent,
        splashColor: Colors.transparent,
        onTap: () => onTogglePin!(window),
        child: row,
      ),
    );
  }

  /// Original `UsageBalanceBar`: label / "used / limit" amount row, then a
  /// meter when a limit is known.
  Widget _balanceBar(BuildContext context, JsonMap balance) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(fontSize: type.uiSize(12));
    final unit = stringValue(balance['unit']);
    String format(num value) => unit.isEmpty ? '$value' : '$value $unit';
    final limit = balance['limit'];
    final usedRaw = balance['used'];
    final remaining = balance['remaining'];
    String amount;
    num? usedPct;
    if (limit is num && limit > 0) {
      final used = usedRaw is num ? usedRaw : remaining is num ? limit - remaining : null;
      usedPct = used == null ? null : used / limit * 100;
      amount = '${used == null ? '—' : format(used)} / ${format(limit)}';
    } else if (remaining is num) {
      amount = tr(context, 'providerUsage.values.remaining', args: {'amount': format(remaining)});
    } else if (usedRaw is num) {
      amount = format(usedRaw);
    } else {
      amount = '—';
    }
    final reset = settingsRelativeTime(context, balance['resetsAt'] as String?, ago: false);
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 3,
        children: [
          LayoutBuilder(
            builder: (context, constraints) => Row(
              spacing: 8,
              children: [
                Expanded(
                  child: Text(
                    formatProviderUsageLabel(
                      context,
                      stringValue(balance['id']),
                      stringValue(balance['label']),
                    ),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: small.copyWith(color: colors.muted),
                  ),
                ),
                ConstrainedBox(
                  constraints: BoxConstraints(maxWidth: (constraints.maxWidth - 8).clamp(0.0, double.infinity)),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      Text(
                        amount,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        textAlign: TextAlign.end,
                        style: small.copyWith(fontWeight: FontWeight.w500),
                      ),
                      if (reset != null && reset.isNotEmpty)
                        Text(
                          tr(context, 'providerUsage.timing.resetsIn', args: {'duration': reset}),
                          style: small.copyWith(color: colors.muted),
                          textAlign: TextAlign.end,
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          if (usedPct != null) _meter(context, usedPct, _tone(colors, balance['tone'], usedPct)),
        ],
      ),
    );
  }
}
