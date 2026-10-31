import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart' show defaultTargetPlatform;
import 'package:flutter/material.dart';

import '../../../core/client.dart';
import '../../../core/json.dart';
import '../../../core/scope.dart';
import '../../../ui/common.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';
import '../../settings/usage_settings.dart' show UsageCard;
import '../chat_controller.dart' show requireSuccess;

/// context-window-meter.utils.ts formatTokenCount.
String formatTokenCount(num value) {
  if (value >= 1000000) return '${(value / 1000000).round()}m';
  if (value >= 1000) return '${(value / 1000).round()}k';
  return '${value.round()}';
}

/// context-window-meter.tsx formatSessionCost.
String? formatSessionCost(Object? value) {
  if (value is! num || !value.isFinite || value <= 0) return null;
  return value < .01 ? '\$${value.toStringAsFixed(4)}' : '\$${value.toStringAsFixed(2)}';
}

/// usage/queries.ts useHostReportsUsage: connected and advertising usage sources.
bool hostReportsUsage(PaseoClient client) =>
    client.status == ConnectionStatus.connected &&
    jsonMap(client.serverInfo['features'])['usageSources'] == true;

/// components/context-window-meter.tsx: a 28px slot holding a ring (glyph
/// size, 2px stroke, surface3 track; progress muted, amber-500 from 70%,
/// destructive above 90%, clockwise from twelve). Compact layouts open the
/// details in a content-sized sheet; native wide layouts show them in a
/// tooltip on press (no Refresh); desktop shows them in a hover card above
/// the meter. When the host reports usage the card is 300 wide (12px padding,
/// 12px gaps) and lists the agent's account usage under the context summary.
class ContextWindowMeter extends StatelessWidget {
  const ContextWindowMeter({
    super.key,
    required this.usage,
    required this.client,
    required this.agentId,
    this.glyphSize = 16,
  });

  /// The agent's `lastUsage`.
  final JsonMap usage;
  final PaseoClient client;
  final String agentId;
  final double glyphSize;

  double? get _percentage {
    final used = usage['contextWindowUsedTokens'], max = usage['contextWindowMaxTokens'];
    if (used is! num || max is! num || !max.isFinite || max <= 0 || !used.isFinite || used < 0) {
      return null;
    }
    return used / max * 100;
  }

  @override
  Widget build(BuildContext context) {
    final palette = PaseoColors.of(context);
    final colors = Theme.of(context).colorScheme;
    final percentage = _percentage;
    final value = percentage ?? 0;
    final progress = value > 90
        ? colors.error
        : value >= 70
        ? const Color(0xfff59e0b)
        : colors.onSurfaceVariant;
    final label = percentage == null
        ? tr(context, 'contextWindow.accessibilityNoData')
        : tr(context, 'contextWindow.accessibility', args: {'percentage': percentage.round()});
    final ring = SizedBox.square(
      dimension: 28,
      child: Center(
        child: CustomPaint(
          size: Size.square(glyphSize),
          painter: _ContextRingPainter(
            fraction: percentage == null ? null : (percentage / 100).clamp(0.0, 1.0),
            track: palette.surface3,
            progress: progress,
          ),
        ),
      ),
    );
    if (isCompactLayout(context)) {
      return Semantics(
        button: true,
        label: label,
        excludeSemantics: true,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: () => showContextWindowSheet(context, usage: usage, client: client, agentId: agentId),
          child: ring,
        ),
      );
    }
    final native = defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final dark = Theme.of(context).brightness == Brightness.dark;
    final showsUsage = hostReportsUsage(client);
    final width = math.min(300.0, MediaQuery.sizeOf(context).width - 24);
    final details = ContextWindowDetails(
      usage: usage,
      client: client,
      agentId: agentId,
      showTitle: true,
      gap: 12,
      // Native wide screens show the details in a tooltip that takes no
      // presses, so its cards have no Refresh.
      refreshable: !native,
    );
    return Semantics(
      image: true,
      label: label,
      excludeSemantics: true,
      child: Tooltip(
        triggerMode: native ? TooltipTriggerMode.tap : null,
        waitDuration: Duration.zero,
        // Long enough to move onto the hover card and press Refresh.
        exitDuration: const Duration(milliseconds: 250),
        preferBelow: false,
        verticalOffset: 22,
        padding: showsUsage
            ? const EdgeInsets.all(12)
            : const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(
          color: palette.surface1,
          border: Border.all(color: palette.borderAccent),
          borderRadius: BorderRadius.circular(8),
          boxShadow: [
            BoxShadow(
              color: dark ? const Color(0x33000000) : const Color(0x0a000000),
              offset: const Offset(0, 4),
              blurRadius: dark ? 8 : 16,
            ),
          ],
        ),
        richMessage: WidgetSpan(
          child: showsUsage ? SizedBox(width: width - 24, child: details) : details,
        ),
        child: MouseRegion(cursor: SystemMouseCursors.basic, child: ring),
      ),
    );
  }
}

/// context-window-sheet.tsx: the compact details sheet, titled
/// "Context window", content-sized, with the details 16px apart below its
/// header.
Future<void> showContextWindowSheet(
  BuildContext context, {
  required JsonMap usage,
  required PaseoClient client,
  required String agentId,
}) => showAdaptiveSheet<void>(
  context: context,
  builder: (sheet) => SheetContents(
    title: tr(sheet, 'contextWindow.title'),
    child: ContextWindowDetails(
      usage: usage,
      client: client,
      agentId: agentId,
      showTitle: false,
      gap: 16,
      refreshable: true,
    ),
  ),
);

/// context-window-details.tsx: optional title, "N% used", "used / max
/// tokens" and the session cost, 6px apart (min 200 wide), then the agent's
/// account usage ([AgentUsageSection]) [gap] below.
class ContextWindowDetails extends StatelessWidget {
  const ContextWindowDetails({
    super.key,
    required this.usage,
    required this.client,
    required this.agentId,
    required this.showTitle,
    required this.gap,
    required this.refreshable,
  });
  final JsonMap usage;
  final PaseoClient client;
  final String agentId;
  final bool showTitle, refreshable;
  final double gap;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final base = text.bodyMedium!.copyWith(color: colors.onSurface);
    final detail = text.bodySmall!.copyWith(color: colors.onSurfaceVariant, height: 1.4);
    final used = usage['contextWindowUsedTokens'], max = usage['contextWindowMaxTokens'];
    final window = used is num && max is num && max > 0 && used >= 0
        ? (used: used, max: max)
        : null;
    final cost = formatSessionCost(usage['totalCostUsd']);
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: gap,
      children: [
        ConstrainedBox(
          constraints: const BoxConstraints(minWidth: 200),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            spacing: 6,
            children: [
              if (showTitle) Text(tr(context, 'contextWindow.title'), style: base),
              if (window != null) ...[
                Text(
                  tr(context, 'contextWindow.used', args: {
                    'percentage': (window.used / window.max * 100).round(),
                  }),
                  style: base.copyWith(height: 1.4),
                ),
                Text(
                  tr(context, 'contextWindow.tokens', args: {
                    'used': formatTokenCount(window.used),
                    'max': formatTokenCount(window.max),
                  }),
                  style: detail,
                ),
              ] else
                Text(tr(context, 'contextWindow.noData'), style: detail),
              if (cost != null)
                Text(tr(context, 'contextWindow.sessionCost', args: {'cost': cost}), style: detail),
            ],
          ),
        ),
        AgentUsageSection(client: client, agentId: agentId, gap: gap, refreshable: refreshable),
      ],
    );
  }
}

/// usage/agent-usage.tsx AgentUsage + queries.ts useAgentUsage: the reports
/// of the account the agent runs under, fetched only while the details are
/// open (usage.list_reports.request scoped to the agent, cards streaming in as
/// `usage.list_reports.update` events arrive). A 1px borderAccent divider,
/// then the compact cards — or a small muted "Loading usage..." / "Unable to
/// load usage: …" line. Nothing when the host does not report usage or the
/// agent has no reports.
class AgentUsageSection extends StatefulWidget {
  const AgentUsageSection({
    super.key,
    required this.client,
    required this.agentId,
    required this.gap,
    this.refreshable = false,
  });
  final PaseoClient client;
  final String agentId;
  final double gap;

  /// Whether each card has a Refresh button (the surface can be pressed).
  final bool refreshable;

  @override
  State<AgentUsageSection> createState() => _AgentUsageSectionState();
}

class _AgentUsageSectionState extends State<AgentUsageSection> {
  List<JsonMap> reports = [];
  bool settled = false;
  Object? error;
  String? requestId;
  StreamSubscription<JsonMap>? subscription;
  final Set<Object?> refreshing = {}, refreshFailed = {};

  /// queries.ts useReportRefresh(serverId, reportId, agentId): re-list the
  /// agent's reports with forceRefresh and replace this card with the fresh
  /// copy; a failure keeps the card and says "Unable to refresh usage".
  Future<void> _refresh(Object? reportId) async {
    setState(() {
      refreshing.add(reportId);
      refreshFailed.remove(reportId);
    });
    try {
      final result = requireSuccess(await widget.client.request(
        'usage.list_reports.request',
        {
          'requestId': 'flutter-agent-usage-refresh-${DateTime.now().microsecondsSinceEpoch}',
          'agentId': widget.agentId,
          'forceRefresh': true,
        },
        responseType: 'usage.list_reports.response',
      ));
      final fresh = jsonList(result['reports'])
          .map(jsonMap)
          .where((report) => report['id'] == reportId)
          .firstOrNull;
      if (!mounted) return;
      setState(() {
        // model.ts replaceReport: a vanished report is dropped.
        reports = fresh == null
            ? reports.where((report) => report['id'] != reportId).toList()
            : [for (final report in reports) report['id'] == reportId ? fresh : report];
      });
    } catch (_) {
      if (mounted) setState(() => refreshFailed.add(reportId));
    } finally {
      if (mounted) setState(() => refreshing.remove(reportId));
    }
  }

  @override
  void initState() {
    super.initState();
    if (hostReportsUsage(widget.client)) unawaited(_load());
  }

  /// model.ts upsertReport: replace a report by id, keeping the later fetch.
  void _upsert(JsonMap report) {
    final id = report['id'];
    final index = reports.indexWhere((entry) => entry['id'] == id);
    if (index < 0) {
      reports = [...reports, report];
    } else if (stringValue(report['fetchedAt']).compareTo(stringValue(reports[index]['fetchedAt'])) >= 0) {
      reports = [...reports]..[index] = report;
    }
  }

  Future<void> _load() async {
    final own = requestId = 'flutter-agent-usage-${DateTime.now().microsecondsSinceEpoch}';
    subscription = widget.client.events.listen((event) {
      if (event['type'] != 'usage.list_reports.update' || !mounted) return;
      final payload = jsonMap(event['payload']);
      if (payload['requestId'] != own) return;
      setState(() => _upsert(jsonMap(payload['report'])));
    });
    try {
      final result = requireSuccess(await widget.client.request(
        'usage.list_reports.request',
        {'requestId': own, 'agentId': widget.agentId, 'forceRefresh': false},
        responseType: 'usage.list_reports.response',
      ));
      if (!mounted || own != requestId) return;
      // model.ts settleReports: keep shown order, drop missing, append new.
      final finished = jsonList(result['reports']).map(jsonMap).toList();
      final byId = {for (final report in finished) report['id']: report};
      final settledList = <JsonMap>[];
      for (final shown in reports) {
        final arriving = byId.remove(shown['id']);
        if (arriving == null) continue;
        settledList.add(
          stringValue(arriving['fetchedAt']).compareTo(stringValue(shown['fetchedAt'])) >= 0
              ? arriving
              : shown,
        );
      }
      setState(() {
        reports = [...settledList, ...byId.values];
        settled = true;
      });
    } catch (e) {
      if (mounted && own == requestId) setState(() => error = e);
    }
  }

  @override
  void dispose() {
    requestId = null;
    subscription?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (!hostReportsUsage(widget.client)) return const SizedBox.shrink();
    final palette = PaseoColors.of(context);
    final colors = Theme.of(context).colorScheme;
    final message = Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.onSurfaceVariant);
    final List<Widget> children;
    if (error != null) {
      children = [
        Text(
          tr(context, 'providerUsage.agentError', args: {'reason': '$error'}),
          style: message,
        ),
      ];
    } else if (!settled && reports.isEmpty) {
      children = [Text(tr(context, 'providerUsage.loading'), style: message)];
    } else if (reports.isEmpty) {
      return const SizedBox.shrink();
    } else {
      final displayRemaining =
          jsonMap(PaseoScope.of(context).preferences['usage'])['displayAs'] == 'remaining';
      children = [for (final entry in reports) _card(entry, displayRemaining)];
    }
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      spacing: widget.gap,
      children: [
        Container(height: 1, color: palette.borderAccent),
        ...children,
      ],
    );
  }

  /// usage_settings.dart card(): a failed report keeps its last successful
  /// windows, marked retained; not pinnable here (pinnable={false}).
  Widget _card(JsonMap entry, bool displayRemaining) {
    final report = jsonMap(entry['report']);
    final last = entry['lastSuccessful'] == null ? null : jsonMap(entry['lastSuccessful']);
    final retained = report['status'] != 'available' && last != null && jsonMap(last['report']).isNotEmpty;
    final shown = retained ? jsonMap(last['report']) : report;
    return UsageCard(
      title: stringValue(entry['sourceLabel'], stringValue(entry['id'])),
      icon: entry['icon'] as String?,
      status: stringValue(report['status']),
      planLabel: stringValue(shown['planLabel'], stringValue(report['planLabel'])),
      windows: jsonList(shown['windows']).map(jsonMap).toList(),
      balances: jsonList(shown['balances']).map(jsonMap).toList(),
      details: jsonList(shown['details']).map(jsonMap).toList(),
      messages: [
        if (report['error'] != null) '${report['error']}',
        for (final login in jsonList(entry['loginErrors']).map(jsonMap))
          '${login['harness']}: ${jsonMap(login['report'])['error'] ?? jsonMap(login['report'])['problem']}',
      ],
      footer: jsonMap(entry['account'])['label'] as String?,
      successfulAt: retained ? stringValue(last['fetchedAt']) : entry['fetchedAt'] as String?,
      retained: retained,
      displayRemaining: displayRemaining,
      compact: true,
      onRefresh: widget.refreshable ? () => _refresh(entry['id']) : null,
      refreshing: refreshing.contains(entry['id']),
      refreshFailed: refreshFailed.contains(entry['id']),
    );
  }
}

class _ContextRingPainter extends CustomPainter {
  const _ContextRingPainter({required this.fraction, required this.track, required this.progress});
  final double? fraction;
  final Color track, progress;

  @override
  void paint(Canvas canvas, Size size) {
    const stroke = 2.0;
    final rect = Rect.fromLTWH(stroke / 2, stroke / 2, size.width - stroke, size.height - stroke);
    final paint = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = stroke
      ..color = track;
    canvas.drawArc(rect, 0, 2 * math.pi, false, paint);
    final value = fraction;
    if (value != null && value > 0) {
      canvas.drawArc(
        rect,
        -math.pi / 2,
        2 * math.pi * value,
        false,
        paint
          ..color = progress
          ..strokeCap = StrokeCap.round,
      );
    }
  }

  @override
  bool shouldRepaint(_ContextRingPainter old) =>
      old.fraction != fraction || old.track != track || old.progress != progress;
}
