import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../../core/json.dart';
import '../../../ui/common.dart';
import '../../../ui/localization.dart';
import '../../../ui/theme.dart';
import '../../files/pane_chrome.dart' show FileDiffStat;
import '../provider_icon.dart';
import 'chrome_controls.dart';
import 'status_mark.dart';

/// composer/pill-styles.ts geometry.
const composerPillMinHeight = 32.0;
double composerPillClearance(bool compact) => compact ? 8 : 10;

/// resolveComposerTrackTailClearance: the transcript's tail inset while the
/// track bar floats over it.
double composerTrackTailClearance(bool compact) =>
    (compact ? 24 : 20) + composerPillMinHeight + composerPillClearance(compact);

/// resolveComposerTrackControlClearance: where floating stream controls sit
/// while the track bar is visible.
double composerTrackControlClearance(bool compact) =>
    composerPillClearance(compact) * 2 + composerPillMinHeight;

/// subagents/select.ts SubagentRow — a managed (Paseo) child agent or a
/// provider-native subagent, already filtered to one parent.
@immutable
class SubagentRowData {
  const SubagentRowData({
    required this.kind,
    required this.id,
    required this.serverId,
    required this.provider,
    required this.status,
    required this.createdAt,
    this.title,
    this.description,
    this.subtitle,
    this.turnOpen = false,
    this.workspaceId,
    this.cwd,
    this.parentAgentId,
    this.agent = const {},
  });

  /// 'paseo' or 'provider'.
  final String kind;
  final String id, serverId, provider;
  final String? status, title, description, subtitle;
  final bool turnOpen;
  final DateTime createdAt;
  final String? workspaceId, cwd, parentAgentId;

  /// The raw managed agent, for its facts line.
  final JsonMap agent;

  bool get isPaseo => kind == 'paseo';
  String get key => isPaseo ? 'paseo:$serverId:$id' : 'provider:$id';

  /// archive-finished.ts isFinishedSubagent.
  bool get isFinished => isPaseo
      ? status == 'idle' || status == 'error'
      : status == 'completed' || status == 'failed' || status == 'canceled';

  /// track-presentation.ts presentationStatus (provider-store lifecycle map).
  String? get presentationStatus {
    if (isPaseo) {
      if (turnOpen) return 'running';
      return status == 'running' ? 'idle' : status;
    }
    if (status == 'running') return 'running';
    if (status == 'failed') return 'error';
    return 'idle';
  }

  StatusBucket get bucket => deriveStatusBucket(status: presentationStatus);

  /// The task names the row when present; providers own the subtitle.
  String? get label => resolveRowLabel(description) ?? resolveRowLabel(title);
  String? get rowSubtitle => !isPaseo
      ? resolveRowLabel(subtitle) ??
            (resolveRowLabel(description) != null ? resolveRowLabel(title) : null)
      : (resolveRowLabel(description) != null ? resolveRowLabel(title) : null);
}

/// track-presentation.ts resolveRowLabel: a placeholder "New agent" title is no title.
String? resolveRowLabel(String? title) {
  final normalized = title?.trim() ?? '';
  if (normalized.isEmpty || normalized.toLowerCase() == 'new agent') return null;
  return normalized;
}

/// use-archive-finished ArchiveFinishedStatus.
@immutable
class ArchiveFinishedStatus {
  const ArchiveFinishedStatus.idle()
    : kind = 'idle',
      completed = 0,
      failed = 0,
      total = 0;
  const ArchiveFinishedStatus.archiving(this.completed, this.total)
    : kind = 'archiving',
      failed = 0;
  const ArchiveFinishedStatus.failed(this.failed, this.total)
    : kind = 'failed',
      completed = 0;
  final String kind;
  final int completed, failed, total;
}

/// One leading mark plus its text inside a track pill.
@immutable
class TrackPillSegment {
  const TrackPillSegment(this.bucket, this.text);
  final StatusBucket? bucket;
  final String text;
}

/// panels/agent-tracks.tsx AgentTracks inside composer/tracks.tsx
/// ComposerTrackBar: the pane's ambient context — tasks, subagents and the
/// workspace diff — as one rail of pills (4px apart, contentMaxWidth, 16px
/// inset) floating over the transcript right above the composer with 8px
/// (compact) / 10px clearance and no background of its own.
class AgentTracks extends StatelessWidget {
  const AgentTracks({
    super.key,
    required this.serverId,
    required this.tasks,
    required this.rows,
    required this.archiveStatus,
    required this.onArchiveFinished,
    required this.onOpenRow,
    required this.onArchiveRow,
    required this.onDetachRow,
    required this.additions,
    required this.deletions,
    required this.onOpenChanges,
    required this.factsFor,
    required this.hostLabelFor,
    required this.providerSvgFor,
    required this.opensInSheet,
    this.canArchiveFinished = true,
  });
  final String serverId;
  final List<JsonMap> tasks;
  final List<SubagentRowData> rows;
  final ArchiveFinishedStatus archiveStatus;
  final VoidCallback onArchiveFinished;
  final ValueChanged<SubagentRowData> onOpenRow, onArchiveRow;

  /// Null when the host cannot detach agents (serverInfo.features.agentDetach).
  final ValueChanged<SubagentRowData>? onDetachRow;
  final int additions, deletions;
  final VoidCallback onOpenChanges;
  final String? Function(SubagentRowData row) factsFor;
  final String Function(String serverId) hostLabelFor;
  final String? Function(SubagentRowData row) providerSvgFor;

  /// A row that opens in a sheet keeps the list open under it (closeOnSelect).
  final bool Function(SubagentRowData row) opensInSheet;

  /// False in a provider-subagent panel's child track (no onArchiveFinished).
  final bool canArchiveFinished;

  /// agent-tracks.tsx hasAgentTracks (+ the diff-stat pill).
  static bool visible({
    required List<JsonMap> tasks,
    required List<SubagentRowData> rows,
    required ArchiveFinishedStatus archiveStatus,
    required bool hasDiff,
  }) =>
      hasDiff || rows.isNotEmpty || tasks.isNotEmpty || archiveStatus.kind != 'idle';

  @override
  Widget build(BuildContext context) {
    final compact = isCompactLayout(context);
    final hasDiff = additions > 0 || deletions > 0;
    if (!visible(tasks: tasks, rows: rows, archiveStatus: archiveStatus, hasDiff: hasDiff)) {
      return const SizedBox.shrink();
    }
    return Padding(
      padding: EdgeInsets.fromLTRB(16, 0, 16, composerPillClearance(compact)),
      child: Align(
        alignment: Alignment.bottomCenter,
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxWidth: PaseoTypography.of(context).contentMaxWidth,
          ),
          child: SizedBox(
            width: double.infinity,
            child: Row(
              spacing: 4,
              children: [
                if (tasks.isNotEmpty)
                  Flexible(key: const ValueKey('tasks'), child: _taskPill(context)),
                if (rows.isNotEmpty || archiveStatus.kind != 'idle')
                  Flexible(key: const ValueKey('subagents'), child: _subagentPill(context)),
                if (hasDiff)
                  _DiffStatPill(
                    additions: additions,
                    deletions: deletions,
                    onPressed: onOpenChanges,
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// composer/task-list: "2/7 tasks", the full list in the panel.
  Widget _taskPill(BuildContext context) {
    final completed = tasks
        .where((task) => task['completed'] == true || task['status'] == 'completed')
        .length;
    final label = tr(
      context,
      'message.todo.tasksProgress',
      args: {'completed': completed, 'total': tasks.length},
    );
    return ComposerTrackPill(
      segments: [TrackPillSegment(null, label)],
      panelTitle: tr(context, 'message.todo.title'),
      panelBuilder: (context) => [
        for (final task in tasks)
          ComposerTrackRow(builder: (_) => TaskListRow(task: task)),
      ],
    );
  }

  /// subagents/track.tsx SubagentsTrack.
  Widget _subagentPill(BuildContext context) {
    final pill = buildSubagentPill(context, rows);
    final finished = rows.where((row) => row.isFinished).length;
    final archiving = archiveStatus.kind == 'archiving';
    final showArchiveFinished =
        canArchiveFinished && (finished > 0 || archiveStatus.kind != 'idle');
    return ComposerTrackPill(
      segments: pill.segments,
      semanticsLabel: pill.accessibilityLabel,
      panelTitle: tr(context, 'subagents.title'),
      panelBuilder: (context) => [
        if (showArchiveFinished) ...[
          ComposerTrackRow(
            semanticsLabel: tr(context, 'subagents.archiveFinishedAction'),
            disabled: archiving,
            closeOnSelect: false,
            onPressed: onArchiveFinished,
            builder: (active) => _ArchiveFinishedRow(status: archiveStatus, active: active),
          ),
          if (rows.isNotEmpty) const _TrackSeparator(),
        ],
        for (final row in rows)
          ComposerTrackRow(
            key: ValueKey(row.key),
            semanticsLabel: row.label ?? tr(context, 'common.states.loading'),
            closeOnSelect: !opensInSheet(row),
            onPressed: () => onOpenRow(row),
            builder: (active) => _SubagentRowContent(
              row: row,
              active: active,
              facts: row.isPaseo ? factsFor(row) : null,
              hostLabel: row.isPaseo && row.serverId != serverId
                  ? hostLabelFor(row.serverId)
                  : null,
              providerSvg: providerSvgFor(row),
              onArchive: row.isPaseo ? () => onArchiveRow(row) : null,
              onDetach: row.isPaseo && onDetachRow != null ? () => onDetachRow!(row) : null,
            ),
          ),
      ],
    );
  }
}

/// track-presentation.ts buildSubagentPillPresentation: every non-done state
/// gets its own mark and count, in sidebar group order; a fan-out where nothing
/// is happening falls back to naming what the pill opens.
({List<TrackPillSegment> segments, String accessibilityLabel}) buildSubagentPill(
  BuildContext context,
  List<SubagentRowData> rows,
) {
  final buckets = rows.map((row) => row.bucket).toList();
  final segments = <TrackPillSegment>[];
  for (final bucket in statusBucketOrder) {
    if (bucket == StatusBucket.done) continue;
    final count = buckets.where((candidate) => candidate == bucket).length;
    if (count == 0) continue;
    final text = switch (bucket) {
      StatusBucket.running => tr(context, 'subagents.pillLabelWorking', args: {'count': count}),
      StatusBucket.failed => tr(context, 'subagents.pillLabelFailed', args: {'count': count}),
      StatusBucket.needsInput => count == 1
          ? tr(context, 'subagents.pillLabelNeedsInputOne')
          : tr(context, 'subagents.pillLabelNeedsInputMany', args: {'count': count}),
      _ => tr(context, 'subagents.pillLabelReadyToReview', args: {'count': count}),
    };
    segments.add(TrackPillSegment(bucket, text));
  }
  if (segments.isEmpty) {
    final label = rows.length == 1
        ? tr(context, 'subagents.pillLabelOne')
        : tr(context, 'subagents.pillLabelMany', args: {'count': rows.length});
    return (segments: [TrackPillSegment(null, label)], accessibilityLabel: label);
  }
  return (
    segments: segments,
    accessibilityLabel: segments.map((segment) => segment.text).join(', '),
  );
}

/// composer/tracks.tsx ComposerTrackPill: a 32px pill (12/4 padding, 16px
/// radius, 1px borderAccent, surface1 → surface2 while hovered, pressed or
/// open) of status segments 16px apart, each a mark 8px from its 12px muted
/// label (foreground while open). It opens its panel as a bottom sheet on
/// compact layouts and a popover above the pill elsewhere (12px offset,
/// 280–620px wide, at most 440px tall, scrollable).
class ComposerTrackPill extends StatefulWidget {
  const ComposerTrackPill({
    super.key,
    required this.segments,
    required this.panelTitle,
    required this.panelBuilder,
    this.semanticsLabel,
  });
  final List<TrackPillSegment> segments;
  final String panelTitle;
  final List<Widget> Function(BuildContext context) panelBuilder;
  final String? semanticsLabel;

  @override
  State<ComposerTrackPill> createState() => _ComposerTrackPillState();
}

class _ComposerTrackPillState extends State<ComposerTrackPill> {
  bool _hovered = false, _pressed = false, _open = false;
  late final ValueNotifier<List<Widget> Function(BuildContext)> _panel =
      ValueNotifier(widget.panelBuilder);
  NavigatorState? _navigator;
  ModalRoute<Object?>? _panelRoute;

  @override
  void didUpdateWidget(ComposerTrackPill old) {
    super.didUpdateWidget(old);
    // The open panel is another route; hand it the fresh rows after this frame.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _panel.value = widget.panelBuilder;
    });
  }

  @override
  void dispose() {
    final route = _panelRoute, navigator = _navigator;
    if (_open && route != null && navigator != null) {
      // The pill left with its panel open, e.g. archive-finished emptied the
      // track. The original popover lives inside the track, so the panel goes
      // with it rather than freezing on rows nothing updates any more. Its rows
      // listen to [_panel] until the route is gone, so it is not disposed here.
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!route.isActive) return;
        route.isCurrent ? navigator.pop() : navigator.removeRoute(route);
      });
    } else {
      _panel.dispose();
    }
    super.dispose();
  }

  Future<void> _openPanel() async {
    setState(() => _open = true);
    _navigator = Navigator.of(context);
    final colors = PaseoColors.of(context);
    Widget rows(BuildContext context) {
      _panelRoute = ModalRoute.of(context);
      return ValueListenableBuilder(
        valueListenable: _panel,
        builder: (context, build, _) => Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: build(context),
        ),
      );
    }
    if (isCompactLayout(context)) {
      await showModalBottomSheet<void>(
        context: context,
        isScrollControlled: true,
        backgroundColor: colors.surface0,
        barrierColor: const Color(0x73000000),
        shape: RoundedRectangleBorder(
          borderRadius: const BorderRadius.vertical(top: Radius.circular(12)),
          side: BorderSide(color: Theme.of(context).dividerColor),
        ),
        builder: (sheet) => ConstrainedBox(
          constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(sheet).height * .8),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Center(
                child: Container(
                  margin: const EdgeInsets.symmetric(vertical: 10),
                  width: 36,
                  height: 4,
                  decoration: BoxDecoration(
                    color: colors.surface2,
                    borderRadius: BorderRadius.circular(2),
                  ),
                ),
              ),
              Padding(
                padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
                child: Text(
                  widget.panelTitle,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(sheet).textTheme.bodyMedium!.copyWith(
                    fontWeight: FontWeight.w500,
                  ),
                ),
              ),
              Flexible(
                child: SingleChildScrollView(
                  padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(sheet).bottom + 4),
                  child: rows(sheet),
                ),
              ),
            ],
          ),
        ),
      );
    } else {
      final anchor = paseoAnchorRect(context);
      final dark = Theme.of(context).brightness == Brightness.dark;
      await showGeneralDialog<void>(
        context: context,
        barrierDismissible: true,
        barrierLabel: MaterialLocalizations.of(context).modalBarrierDismissLabel,
        barrierColor: Colors.transparent,
        transitionDuration: const Duration(milliseconds: 120),
        transitionBuilder: (_, animation, _, child) =>
            FadeTransition(opacity: animation, child: child),
        pageBuilder: (popover, _, _) => CustomSingleChildLayout(
          delegate: _AbovePillLayout(anchor, MediaQuery.paddingOf(popover)),
          child: Semantics(
            label: widget.panelTitle,
            child: Material(
              color: colors.surface1,
              shape: RoundedRectangleBorder(
                borderRadius: BorderRadius.circular(8),
                side: BorderSide(color: colors.borderAccent),
              ),
              clipBehavior: Clip.antiAlias,
              elevation: 0,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  boxShadow: [
                    BoxShadow(
                      color: dark ? const Color(0x33000000) : const Color(0x0a000000),
                      offset: const Offset(0, 4),
                      blurRadius: dark ? 8 : 16,
                    ),
                  ],
                ),
                child: SingleChildScrollView(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: IntrinsicWidth(child: rows(popover)),
                ),
              ),
            ),
          ),
        ),
      );
    }
    _panelRoute = null;
    if (mounted) setState(() => _open = false);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final active = _hovered || _pressed || _open;
    final label = theme.textTheme.bodySmall!.copyWith(
      fontSize: PaseoTypography.of(context).uiSize(12),
      color: _open ? theme.colorScheme.onSurface : colors.muted,
    );
    // container: its own node sized to the pill; a plain Semantics merges
    // into the enclosing node and reports the whole stream area.
    return Semantics(
      container: true,
      button: true,
      expanded: _open,
      label: widget.semanticsLabel ?? widget.segments.map((s) => s.text).join(' '),
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: (_) => setState(() => _pressed = true),
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: _openPanel,
          child: Container(
            constraints: const BoxConstraints(minHeight: composerPillMinHeight),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
            decoration: BoxDecoration(
              color: active ? colors.surface2 : colors.surface1,
              border: Border.all(color: colors.borderAccent),
              borderRadius: BorderRadius.circular(16),
            ),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              spacing: 16,
              children: [
                for (final segment in widget.segments)
                  Flexible(
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      spacing: 8,
                      children: [
                        if (segment.bucket != null) _TrackMark(bucket: segment.bucket!),
                        Flexible(
                          child: Text(
                            segment.text,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: label,
                          ),
                        ),
                      ],
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// MenuSurface side="top" align="start" offset=12, clamped to the window.
class _AbovePillLayout extends SingleChildLayoutDelegate {
  const _AbovePillLayout(this.anchor, this.padding);
  final Rect anchor;
  final EdgeInsets padding;

  @override
  BoxConstraints getConstraintsForChild(BoxConstraints constraints) {
    final maxWidth = (constraints.maxWidth - 16).clamp(0.0, 620.0);
    return BoxConstraints(
      minWidth: maxWidth < 280 ? maxWidth : 280,
      maxWidth: maxWidth,
      maxHeight: (constraints.maxHeight - padding.vertical - 16).clamp(0.0, 440.0),
    );
  }

  @override
  Offset getPositionForChild(Size size, Size child) {
    final above = anchor.top - 12 - child.height;
    final y = above >= padding.top + 8 ? above : anchor.bottom + 12;
    return Offset(
      anchor.left.clamp(8.0, (size.width - child.width - 8).clamp(8.0, double.infinity)),
      y.clamp(padding.top + 8, (size.height - child.height - 8).clamp(padding.top + 8, double.infinity)),
    );
  }

  @override
  bool shouldRelayout(_AbovePillLayout old) => anchor != old.anchor || padding != old.padding;
}

/// ComposerTrackMark: the running ring (halo trimmed) or a 6px status dot,
/// done drawn in the border colour.
class _TrackMark extends StatelessWidget {
  const _TrackMark({required this.bucket});
  final StatusBucket bucket;

  @override
  Widget build(BuildContext context) {
    if (bucket == StatusBucket.running) return const StatusRing(halo: false);
    return Container(
      width: statusFilledDotSize,
      height: statusFilledDotSize,
      decoration: BoxDecoration(
        color: statusDotColor(context, bucket, showDoneAsInactive: true),
        shape: BoxShape.circle,
      ),
    );
  }
}

/// MenuSeparator between the bulk action and the rows.
class _TrackSeparator extends StatelessWidget {
  const _TrackSeparator();

  @override
  Widget build(BuildContext context) => Container(
    height: 1,
    margin: const EdgeInsets.symmetric(vertical: 4),
    color: PaseoColors.of(context).borderAccent,
  );
}

/// ComposerTrackRow: one line in a track panel — 4px from the panel edge, 8px
/// horizontal padding, 40px tall with 8px vertical padding on compact and 32/4
/// on pointer layouts, 6px radius, items 8px apart, surface2 while hovered or
/// pressed. Choosing a row dismisses the panel unless [closeOnSelect] is false.
class ComposerTrackRow extends StatefulWidget {
  const ComposerTrackRow({
    super.key,
    required this.builder,
    this.onPressed,
    this.closeOnSelect = true,
    this.disabled = false,
    this.semanticsLabel,
  });
  final Widget Function(bool active) builder;
  final VoidCallback? onPressed;
  final bool closeOnSelect, disabled;
  final String? semanticsLabel;

  @override
  State<ComposerTrackRow> createState() => _ComposerTrackRowState();
}

class _ComposerTrackRowState extends State<ComposerTrackRow> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final compact = isCompactLayout(context);
    final pressable = widget.onPressed != null;
    final active = pressable && (_hovered || _pressed);
    final row = Container(
      constraints: BoxConstraints(minHeight: compact ? 40 : 32),
      margin: const EdgeInsets.symmetric(horizontal: 4),
      padding: EdgeInsets.symmetric(horizontal: 8, vertical: compact ? 8 : 4),
      decoration: BoxDecoration(
        color: active ? PaseoColors.of(context).surface2 : null,
        borderRadius: BorderRadius.circular(6),
      ),
      child: Row(spacing: 8, children: [Expanded(child: widget.builder(active))]),
    );
    if (!pressable) {
      return Semantics(label: widget.semanticsLabel, child: row);
    }
    return Semantics(
      button: true,
      enabled: !widget.disabled,
      label: widget.semanticsLabel,
      child: MouseRegion(
        cursor: widget.disabled ? MouseCursor.defer : SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTapDown: widget.disabled ? null : (_) => setState(() => _pressed = true),
          onTapCancel: () => setState(() => _pressed = false),
          onTapUp: (_) => setState(() => _pressed = false),
          onTap: widget.disabled
              ? null
              : () {
                  if (widget.closeOnSelect) Navigator.of(context).pop();
                  widget.onPressed!();
                },
          child: row,
        ),
      ),
    );
  }
}

/// components/task-list-row.tsx: a 16px status glyph 8px from base-size text —
/// CircleCheck (extra-muted) when done, CircleDot (running blue) while in
/// progress showing its active form in foreground, a plain extra-muted Circle
/// otherwise; completed text is struck through in extra-muted.
class TaskListRow extends StatelessWidget {
  const TaskListRow({super.key, required this.task});
  final JsonMap task;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final completed = task['completed'] == true || task['status'] == 'completed';
    final running = !completed && task['status'] == 'in_progress';
    final activeForm = stringValue(task['activeForm']);
    final text = running && activeForm.isNotEmpty ? activeForm : stringValue(task['text']);
    final base = theme.textTheme.bodyMedium!;
    return Semantics(
      label: text,
      excludeSemantics: true,
      child: Row(
        spacing: 8,
        children: [
          Icon(
            completed
                ? LucideIcons.circleCheck300
                : running
                ? LucideIcons.circleDot300
                : LucideIcons.circle300,
            size: 16,
            color: running ? colors.running : colors.extraMuted,
          ),
          Flexible(
            child: Text(
              text,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: base.copyWith(
                color: completed
                    ? colors.extraMuted
                    : running
                    ? theme.colorScheme.onSurface
                    : colors.muted,
                decoration: completed ? TextDecoration.lineThrough : null,
                decorationColor: colors.extraMuted,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// subagents/track.tsx ArchiveFinishedRow: 14px Archive glyph (foreground when
/// active), the action name, and "n/m" progress or the retry count trailing.
class _ArchiveFinishedRow extends StatelessWidget {
  const _ArchiveFinishedRow({required this.status, required this.active});
  final ArchiveFinishedStatus status;
  final bool active;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final small = theme.textTheme.bodySmall!.copyWith(
      fontSize: PaseoTypography.of(context).uiSize(12),
      color: colors.muted,
    );
    return Row(
      spacing: 8,
      children: [
        Icon(
          LucideIcons.archive300,
          size: 14,
          color: active ? theme.colorScheme.onSurface : colors.muted,
        ),
        Flexible(
          flex: 2,
          child: Text(
            tr(context, 'subagents.archiveFinishedAction'),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: theme.textTheme.bodyMedium!.copyWith(color: theme.colorScheme.onSurface),
          ),
        ),
        if (status.kind == 'archiving')
          Flexible(child: Text('${status.completed}/${status.total}', style: small)),
        if (status.kind == 'failed')
          Flexible(
            child: Text(
              tr(
                context,
                'subagents.archiveFinishedRetry',
                args: {'failed': status.failed, 'total': status.total},
              ),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: small,
            ),
          ),
      ],
    );
  }
}

/// SubagentsTrackRow: the provider icon carrying the row's status, the label
/// over its 12px muted facts (host label when on another host, the agent's
/// model facts, or the provider subtitle), and the detach/archive toolbar
/// buttons — always shown on touch layouts, revealed by hover elsewhere.
class _SubagentRowContent extends StatelessWidget {
  const _SubagentRowContent({
    required this.row,
    required this.active,
    required this.facts,
    required this.hostLabel,
    required this.providerSvg,
    required this.onArchive,
    required this.onDetach,
  });
  final SubagentRowData row;
  final bool active;
  final String? facts, hostLabel, providerSvg;
  final VoidCallback? onArchive, onDetach;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final compact = isCompactLayout(context);
    final label = row.label ?? tr(context, 'common.states.loading');
    final factsStyle = theme.textTheme.bodySmall!.copyWith(
      fontSize: PaseoTypography.of(context).uiSize(12),
      color: colors.muted,
    );
    Widget factsLine(String? text) => text == null || text.isEmpty
        ? const SizedBox.shrink()
        : Text(text, maxLines: 1, overflow: TextOverflow.ellipsis, style: factsStyle);
    final actionsVisible = compact ||
        Theme.of(context).platform == TargetPlatform.android ||
        Theme.of(context).platform == TargetPlatform.iOS ||
        active;
    return Row(
      spacing: 8,
      children: [
        StatusIcon(
          bucket: row.bucket,
          backdrop: active ? colors.surface2 : colors.surface1,
          child: ProviderIcon(
            provider: row.provider,
            snapshotSvg: providerSvg,
            size: 14,
            color: colors.muted,
          ),
        ),
        Expanded(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodyMedium!.copyWith(color: theme.colorScheme.onSurface),
              ),
              if (row.isPaseo) ...[
                factsLine(hostLabel),
                factsLine(facts),
              ] else
                factsLine(row.rowSubtitle),
            ],
          ),
        ),
        if (onArchive != null)
          IgnorePointer(
            ignoring: !actionsVisible,
            child: Opacity(
              opacity: actionsVisible ? 1 : 0,
              child: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 4,
                children: [
                  if (onDetach != null)
                    ChromeToolbarButton(
                      label: tr(context, 'subagents.detachAction', args: {'label': label}),
                      tooltip: tr(context, 'subagents.detachTooltip'),
                      icon: LucideIcons.unlink300,
                      onPressed: onDetach,
                    ),
                  ChromeToolbarButton(
                    label: tr(context, 'subagents.archiveAction', args: {'label': label}),
                    tooltip: tr(context, 'subagents.archiveTooltip'),
                    icon: LucideIcons.archive300,
                    onPressed: onArchive,
                  ),
                ],
              ),
            ),
          ),
      ],
    );
  }
}

/// composer/diff-stat-pill.tsx: the pill chrome around the +n/-m diff stat,
/// filled surface2 on hover; it opens the workspace's Changes view.
class _DiffStatPill extends StatefulWidget {
  const _DiffStatPill({
    required this.additions,
    required this.deletions,
    required this.onPressed,
  });
  final int additions, deletions;
  final VoidCallback onPressed;

  @override
  State<_DiffStatPill> createState() => _DiffStatPillState();
}

class _DiffStatPillState extends State<_DiffStatPill> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Semantics(
      container: true,
      button: true,
      label: tr(context, 'workspace.git.diff.openChangesTab'),
      excludeSemantics: true,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: Container(
            constraints: const BoxConstraints(minHeight: composerPillMinHeight),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
            decoration: BoxDecoration(
              color: _hovered ? colors.surface2 : colors.surface1,
              border: Border.all(color: colors.borderAccent),
              borderRadius: BorderRadius.circular(16),
            ),
            child: FileDiffStat(additions: widget.additions, deletions: widget.deletions),
          ),
        ),
      ),
    );
  }
}
