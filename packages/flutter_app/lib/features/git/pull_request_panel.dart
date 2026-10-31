import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/theme.dart';
import '../chat/chat_markdown.dart';
import '../files/pane_chrome.dart';
import '../settings/settings_host_widgets.dart' show StatusBadge, StatusBadgeVariant;
import 'changes_panel.dart' show statusMergedColor;
import 'git_actions.dart' show CheckoutGitActionsStore, CheckoutGitState, GitActionStatus;
import 'git_controller.dart';
import '../../ui/panel_activity.dart';

/// Original `PullRequestPane` (git/pull-request-panel/pane.tsx) and its host
/// `PullRequestContent`: a surfaceSidebar scroll with the pane toolbar, the
/// title header, the checks (or GitLab pipeline) section, a divider and the
/// activity timeline.
class PullRequestPanel extends StatefulWidget {
  const PullRequestPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<PullRequestPanel> createState() => _PullRequestPanelState();
}

class _PullRequestPanelState extends State<PullRequestPanel> {
  CheckoutGitState? _git;
  PaseoClient? _client;
  StreamSubscription<(String, String)>? _invalidations;
  ConnectionStatus? _connectionStatus;
  bool _visible = true;

  // Activity (original usePrPaneData timeline query).
  String? _timelineKey;
  int _timelineRevision = -1;
  JsonMap? _timeline;
  bool _timelineFetching = false;
  int _timelineGeneration = 0;

  bool _checksOpen = true;
  bool _activityOpen = true;
  final Set<_Check> _collapsedGroups = {};
  final Set<String> _collapsedKeys = {}, _expandedKeys = {};

  // GitLab pipeline (original useGitLabPipeline).
  String _pipelineIdentity = '';
  JsonMap? _pipeline;
  Object? _pipelineError;
  bool _pipelineLoading = false;
  int _pipelineGeneration = 0;
  int _pipelineRevision = -1;
  Timer? _pipelinePoll;

  JsonMap get _payload => _git?.prPayload ?? const {};
  JsonMap get status => jsonMap(_payload['status']);

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    _visible = PanelActivity.of(context).visible;
    if (_git == null) {
      final client = _client = PaseoScope.of(context).clientFor(widget.target.serverId);
      _connectionStatus = client.status;
      client.addListener(_connectionChanged);
      _git = CheckoutGitState.acquire(client, widget.target.serverId, widget.target.cwd)
        ..addListener(_gitChanged);
      CheckoutGitActionsStore.instance.addListener(_actionsChanged);
      // Original invalidateCheckoutGitQueries: actions (and refresh) mark the
      // activity and pipeline stale too.
      _invalidations = CheckoutInvalidations.stream.listen((checkout) {
        if (checkout.$1 == widget.target.serverId && checkout.$2 == widget.target.cwd) {
          _invalidateActivity();
        }
      });
    }
    _sync();
  }

  @override
  void dispose() {
    _timelineGeneration++;
    _pipelineGeneration++;
    _pipelinePoll?.cancel();
    _client?.removeListener(_connectionChanged);
    _invalidations?.cancel();
    CheckoutGitActionsStore.instance.removeListener(_actionsChanged);
    _git?.removeListener(_gitChanged);
    _git?.release();
    super.dispose();
  }

  void _actionsChanged() {
    if (mounted) setState(() {});
  }

  void _gitChanged() {
    if (!mounted) return;
    setState(() {});
    _sync();
  }

  /// Original reconnect: every checkout query of the host refetches.
  void _connectionChanged() {
    final client = _client!;
    if (!mounted || _connectionStatus == client.status) return;
    _connectionStatus = client.status;
    if (client.status == ConnectionStatus.connected) _invalidateActivity();
    setState(() {});
  }

  void _invalidateActivity() {
    _timelineKey = null;
    _pipelineRevision = -1;
    _sync();
  }

  bool get _connected => _client?.status == ConnectionStatus.connected;

  /// Original `shouldFetchTimelineFrom` + the query key: fetches when the
  /// change request (or its pushed PR status) changes, and after invalidation.
  void _sync() {
    final git = _git;
    if (git == null) return;
    final number = pullRequestNumber(status);
    final owner = status.text('repoOwner'), name = status.text('repoName');
    final fetch = shouldFetchPullRequestTimeline(
      connected: _connected,
      timelineEnabled: _visible,
      githubFeaturesEnabled: _payload['githubFeaturesEnabled'] != false,
      cwd: widget.target.cwd,
      prNumber: number,
      repoOwner: owner,
      repoName: name,
      unsupported: number != null &&
          _unsupportedTimelines.contains(_unsupportedKey(number)),
    );
    // A disabled query keeps its data; it refetches when enabled again only
    // if its key changed or it was invalidated meanwhile.
    final key = fetch ? '$number/$owner/$name' : null;
    if (key != null && (key != _timelineKey || git.prRevision != _timelineRevision)) {
      _timelineKey = key;
      _timelineRevision = git.prRevision;
      unawaited(_loadTimeline(number!, owner, name));
    }
    _syncPipeline();
  }

  String _unsupportedKey(int number) =>
      '${widget.target.serverId}\u0000${widget.target.cwd}\u0000$number';

  /// Original timeline query: retried up to three times (1s, 2s, 4s) unless
  /// the host does not know the request, which is remembered per PR.
  Future<void> _loadTimeline(int number, String owner, String name) async {
    final generation = ++_timelineGeneration;
    setState(() => _timelineFetching = true);
    for (var failures = 0; ; failures += 1) {
      try {
        final response = await checkoutPayload(_client!, 'pull_request_timeline_request', {
          'cwd': widget.target.cwd,
          'prNumber': number,
          'repoOwner': owner,
          'repoName': name,
        });
        if (!mounted || generation != _timelineGeneration) return;
        setState(() {
          _timeline = response;
          _timelineFetching = false;
        });
        return;
      } catch (error) {
        if (!mounted || generation != _timelineGeneration) return;
        if (isUnsupportedTimelineError(error)) _unsupportedTimelines.add(_unsupportedKey(number));
        final delay = timelineRetryDelay(failures, error);
        if (delay == null) {
          // The query keeps its last data; the pane shows no error for activity.
          setState(() => _timelineFetching = false);
          return;
        }
        await Future<void>.delayed(delay);
        if (!mounted || generation != _timelineGeneration) return;
      }
    }
  }

  /// Original `renderGitlabChecksSection` + `useGitLabPipeline`: the pipeline
  /// section needs `forgeProviders`; its jobs are fetched while the section is
  /// open on hosts with `forgeCheckDetails`, through the check-details RPC
  /// (pipeline id as checkRunId), polled every 15s while the pipeline is
  /// active, and never shown for another change request.
  void _syncPipeline() {
    final facts = status.object('forgeSpecific');
    final pipelineId = facts['pipelineId'];
    final section = _gitlabPipelineSection(facts);
    final number = pullRequestNumber(status);
    final identity = section ? '$number:$pipelineId' : '';
    if (identity != _pipelineIdentity) {
      _pipelineIdentity = identity;
      _pipeline = null;
      _pipelineError = null;
      _pipelineLoading = false;
      _pipelineRevision = -1;
      _pipelineGeneration++;
    }
    final fetch = section &&
        _checksOpen &&
        _connected &&
        widget.target.cwd.isNotEmpty &&
        (_client?.supports('forgeCheckDetails') ?? false);
    final live = fetch && _gitlabActiveStatuses.contains(facts.text('pipelineStatus'));
    if (live && _pipelinePoll == null) {
      _pipelinePoll = Timer.periodic(const Duration(seconds: 15), (_) => _fetchPipeline());
    } else if (!live) {
      _pipelinePoll?.cancel();
      _pipelinePoll = null;
    }
    final revision = _git?.prRevision ?? 0;
    if (!fetch || revision == _pipelineRevision) return;
    _pipelineRevision = revision;
    unawaited(_fetchPipeline());
  }

  bool _gitlabPipelineSection(JsonMap facts) =>
      facts['forge'] == 'gitlab' &&
      facts['pipelineId'] != null &&
      (_client?.supports('forgeProviders') ?? false);

  Future<void> _fetchPipeline() async {
    final facts = status.object('forgeSpecific');
    final pipelineId = facts['pipelineId'];
    if (pipelineId == null || _client == null) return;
    final generation = ++_pipelineGeneration;
    setState(() => _pipelineLoading = _pipeline == null);
    try {
      final response = await checkoutPayload(
        _client!,
        'checkout.forge.get_check_details.request',
        {
          'cwd': widget.target.cwd,
          'checkRunId': pipelineId,
          'changeRequestNumber': pullRequestNumber(status),
        },
      );
      if (response['success'] == false) {
        throw StateError(checkoutPayloadError(response) ?? 'Could not load pipeline jobs');
      }
      if (!mounted || generation != _pipelineGeneration) return;
      final pipeline = response.object('details')['pipeline'];
      setState(() {
        _pipeline = pipeline is Map ? jsonMap(pipeline) : null;
        _pipelineError = null;
      });
    } catch (e) {
      if (mounted && generation == _pipelineGeneration) setState(() => _pipelineError = e);
    } finally {
      if (mounted && generation == _pipelineGeneration) {
        setState(() => _pipelineLoading = false);
      }
    }
  }

  /// Original `useCheckoutGitActionsStore.refresh`: the host re-reads the
  /// checkout; its queries (PR status, activity, pipeline) then invalidate.
  void _refresh() {
    if (_refreshing) return;
    final failed = tr(context, 'workspace.git.diff.failedRefresh');
    unawaited(
      CheckoutGitActionsStore.instance
          .run(
            serverId: widget.target.serverId,
            cwd: widget.target.cwd,
            actionId: 'refresh',
            body: () => gitRequest(_client!, 'checkout.refresh.request', {'cwd': widget.target.cwd}),
          )
          .catchError((Object error) {
            if (!mounted) return;
            final message = errorMessage(error);
            showError(context, StateError(message.trim().isEmpty ? failed : message));
          }),
    );
  }

  bool get _refreshing =>
      CheckoutGitActionsStore.instance.status(widget.target.serverId, widget.target.cwd, 'refresh') ==
      GitActionStatus.pending;

  void _launch(String url) {
    final uri = Uri.tryParse(url);
    if (uri == null || !['https', 'http'].contains(uri.scheme)) return;
    unawaited(
      launchUrl(uri, mode: LaunchMode.externalApplication).then((opened) {
        if (!opened && mounted) showError(context, StateError('Unable to open $url'));
      }),
    );
  }

  void _toggleChecks() {
    setState(() => _checksOpen = !_checksOpen);
    _syncPipeline();
  }

  // Original activity-state.ts: resolved/outdated threads start collapsed; an
  // explicit toggle wins over the default for the current change request.
  String _entryKey(String id) => '${pullRequestNumber(status)}:$id';

  bool _isCollapsed(_Entry entry) {
    final key = _entryKey(entry.id);
    return _collapsedKeys.contains(key) ||
        (entry.collapsedByDefault && !_expandedKeys.contains(key));
  }

  void _toggleEntry(_Entry entry) {
    final key = _entryKey(entry.id);
    setState(() {
      if (_isCollapsed(entry)) {
        _collapsedKeys.remove(key);
        _expandedKeys.add(key);
      } else {
        _collapsedKeys.add(key);
        _expandedKeys.remove(key);
      }
    });
  }

  /// Original `resolvePullRequestContentState`: the pane once the PR has a
  /// number; otherwise the status error, loading, or the empty state.
  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final git = _git!;
    final pr = status;
    final number = pullRequestNumber(pr);
    final payloadError = checkoutPayloadError(_payload);
    final Widget content;
    if (number != null) {
      content = _pane(pr, number);
    } else if (_payload['error'] != null || payloadError != null || git.prError != null) {
      content = _PaneError(onRetry: _refresh);
    } else if (git.prLoading) {
      content = const _PaneSkeleton();
    } else {
      content = const _PaneEmpty();
    }
    return ColoredBox(color: colors.sidebar, child: content);
  }

  Widget _pane(JsonMap pr, int number) {
    final colors = PaseoColors.of(context);
    final forge = _payload.text('forge').isEmpty ? 'github' : _payload.text('forge');
    final brand = _forgeBrand(forge);
    final facts = pr.object('forgeSpecific');
    final gitlabPipeline = _gitlabPipelineSection(facts);
    final activities = pullRequestTimelineItems(_timeline, number).expand(_mapActivity).toList();
    final entries = _buildTimeline(activities);
    // Original `activityLoading`: the timeline query has no data yet.
    final activityLoading = _timelineFetching && _timeline == null;
    final approvals = activities
        .where((a) => a.text('kind') == 'review' && a.text('reviewState') == 'approved')
        .length;
    final changesRequested = activities
        .where((a) => a.text('kind') == 'review' && a.text('reviewState') == 'changes_requested')
        .length;
    final comments = activities
        .where(
          (a) =>
              a.text('kind') == 'comment' ||
              (a.text('kind') == 'review' && a.text('reviewState') == 'commented'),
        )
        .length;
    final callbacks = _TimelineCallbacks(
      brand: brand,
      isCollapsed: _isCollapsed,
      onToggle: _toggleEntry,
      onLaunch: _launch,
    );
    return ListView(
      padding: EdgeInsets.zero,
      children: [
        FilePaneToolbar(
          sidebar: true,
          padding: const EdgeInsets.symmetric(horizontal: 12),
          children: [
            _ViewButton(
              label: tr(context, 'workspace.git.pr.actions.viewPullRequest'),
              onPressed: () => _launch(pr.text('url')),
            ),
            const Spacer(),
            // Original: the refresh control exists only on hosts with `checkoutRefresh`.
            if (_client?.supports('checkoutRefresh') ?? false)
              PaneToolbarButton(
                tooltip: _refreshing
                    ? tr(context, 'workspace.git.diff.refreshing')
                    : tr(context, 'workspace.git.diff.refreshState', args: {'brand': brand}),
                onPressed: _refreshing ? null : _refresh,
                child: SizedBox.square(
                  dimension: 16,
                  child: Center(
                    child: _refreshing
                        ? SizedBox.square(
                            dimension: paneToolbarIconSize(context),
                            child: CircularProgressIndicator(strokeWidth: 1.5, color: colors.muted),
                          )
                        : Icon(
                            LucideIcons.rotateCw300,
                            size: paneToolbarIconSize(context),
                            color: colors.muted,
                          ),
                  ),
                ),
              ),
          ],
        ),
        _Header(pr: pr, forge: forge, onOpen: () => _launch(pr.text('url'))),
        if (gitlabPipeline)
          _GitlabPipelineSection(
            facts: facts,
            open: _checksOpen,
            onToggle: _toggleChecks,
            pipeline: _pipeline,
            loading: _pipelineLoading,
            error: _pipelineError,
            onLaunch: _launch,
          )
        else
          _ChecksSection(
            checks: pullRequestChecks(pr),
            fallbackUrl: pr.text('url'),
            open: _checksOpen,
            onToggle: _toggleChecks,
            collapsedGroups: _collapsedGroups,
            onToggleGroup: (group) => setState(() {
              if (!_collapsedGroups.remove(group)) _collapsedGroups.add(group);
            }),
            onLaunch: _launch,
          ),
        Divider(height: 1, thickness: 1, color: Theme.of(context).dividerColor),
        _Section(
          title: tr(context, 'workspace.git.pr.sections.activity'),
          open: _activityOpen,
          onToggle: () => setState(() => _activityOpen = !_activityOpen),
          summary: [
            _SummaryPill(
              count: approvals,
              icon: _checkIcon(context, _Check.success, 12),
              color: colors.success,
            ),
            _SummaryPill(
              count: changesRequested,
              icon: _checkIcon(context, _Check.failure, 12),
              color: colors.danger,
            ),
            _SummaryPill(
              count: comments,
              icon: Icon(LucideIcons.messageSquare300, size: 11, color: colors.muted),
              color: colors.muted,
            ),
          ],
          children: [
            if (activityLoading)
              const _ActivitySkeleton()
            else ...[
              if (entries.isEmpty) _EmptyText(tr(context, 'workspace.git.pr.empty.noActivity')),
              for (final entry in entries) _TimelineEntryCard(entry: entry, callbacks: callbacks),
            ],
          ],
        ),
      ],
    );
  }
}

/// Original `defaultUnsupportedTimelineRegistry`: (host, cwd, PR) whose host
/// does not know `pull_request_timeline_request`.
final _unsupportedTimelines = <String>{};

/// Original `mapPrPaneData` number: the status number, else the `/pull/<n>`
/// of its URL.
int? pullRequestNumber(JsonMap status) {
  if (status.isEmpty) return null;
  final number = status['number'];
  if (number is num) return number.toInt();
  final match = RegExp(r'/pull/(\d+)(?:/|$)').firstMatch(Uri.tryParse(status.text('url'))?.path ?? '');
  return match == null ? null : int.tryParse(match[1]!);
}

/// Original `shouldFetchTimelineFrom`.
bool shouldFetchPullRequestTimeline({
  required bool connected,
  required bool timelineEnabled,
  required bool githubFeaturesEnabled,
  required String cwd,
  required int? prNumber,
  required String repoOwner,
  required String repoName,
  required bool unsupported,
}) =>
    connected &&
    timelineEnabled &&
    githubFeaturesEnabled &&
    cwd.isNotEmpty &&
    prNumber != null &&
    repoOwner.isNotEmpty &&
    repoName.isNotEmpty &&
    !unsupported;

/// Original `isUnsupportedTimelineError`: the host rejected the request type.
bool isUnsupportedTimelineError(Object error) =>
    error is DaemonException &&
    error.code == 'unknown_schema' &&
    error.requestType == 'pull_request_timeline_request';

/// Original timeline query retry (react-query defaults): up to three retries
/// after 1s, 2s and 4s, none for an unsupported host.
Duration? timelineRetryDelay(int failures, Object error) {
  if (isUnsupportedTimelineError(error) || failures >= 3) return null;
  return Duration(milliseconds: math.min(1000 * (1 << failures), 30000));
}

/// Original `mapPrPaneData` activity: only a timeline of this PR counts.
List<JsonMap> pullRequestTimelineItems(JsonMap? timeline, int number) {
  if (timeline == null || timeline['prNumber'] != number) return const [];
  return timeline.array('items').map(jsonMap).toList();
}

/// Original `mapChecks`: the status checks, else the forge's native fallback
/// (Gitea's combined commit status as one "CI" check).
List<JsonMap> pullRequestChecks(JsonMap status) {
  final checks = status.array('checks').map(jsonMap).toList();
  if (checks.isNotEmpty) return checks;
  final facts = status.object('forgeSpecific');
  final ciStatus = facts['ciStatus'];
  if (facts['forge'] != 'gitea' || ciStatus is! String) return const [];
  return [
    {
      'name': 'CI',
      'status': mapGiteaCommitState(ciStatus),
      if (ciStatus == 'warning') 'traits': ['warning'],
      'url': status.text('url'),
    },
  ];
}

/// Original protocol `mapGiteaCommitState`: warning is terminal and failing.
String mapGiteaCommitState(String state) => switch (state.toLowerCase()) {
  'success' => 'success',
  'failure' || 'error' || 'warning' => 'failure',
  'skipped' => 'skipped',
  _ => 'pending',
};

// ---------------------------------------------------------------------------
// Presentation helpers (original forge.ts, data.ts, check-presentation.ts).

const _gitlabActiveStatuses = {
  'created',
  'waiting_for_resource',
  'preparing',
  'pending',
  'running',
  'canceling',
  'scheduled',
};

String _forgeBrand(String forge) => switch (forge) {
  'gitlab' => 'GitLab',
  'gitea' => 'Gitea',
  'forgejo' => 'Forgejo',
  'codeberg' => 'Codeberg',
  _ => 'GitHub',
};

/// Original `formatAge`.
String _formatAge(int createdAt) {
  if (createdAt <= 0) return '';
  final seconds =
      math.max(0, DateTime.now().millisecondsSinceEpoch - createdAt) ~/ 1000;
  if (seconds < 60) return 'just now';
  final minutes = seconds ~/ 60;
  if (minutes < 60) return '${minutes}m ago';
  final hours = minutes ~/ 60;
  if (hours < 24) return '${hours}h ago';
  final days = hours ~/ 24;
  if (days < 30) return '${days}d ago';
  if (days < 365) return '${days ~/ 30}mo ago';
  return '${days ~/ 365}y ago';
}

/// Original `formatDuration` (utils/time.ts) for pipeline job durations.
String _formatDuration(int totalSeconds) {
  if (totalSeconds < 60) return '${totalSeconds}s';
  final minutes = totalSeconds ~/ 60;
  if (minutes < 60) {
    final seconds = totalSeconds % 60;
    return seconds == 0 ? '${minutes}m' : '${minutes}m ${seconds}s';
  }
  final hours = minutes ~/ 60;
  final rem = minutes % 60;
  return rem == 0 ? '${hours}h' : '${hours}h ${rem}m';
}

/// Original `CheckPresentation`, declared in the checks section's reading order
/// (`PRESENTATION_ORDER`): what needs action, then what is running, then what
/// needs no attention.
enum _Check { actionRequired, warning, failure, pending, manual, success, ignored }

/// Original `mapCheckStatus`.
String _checkStatus(String status) =>
    const {'success', 'failure', 'pending', 'skipped', 'cancelled'}.contains(status)
    ? status
    : 'pending';

/// Original `classifyCheck`.
_Check _classify(String rawStatus, Iterable<String> traits) {
  final status = _checkStatus(rawStatus);
  if (traits.contains('action_required')) return _Check.actionRequired;
  if (traits.contains('warning')) return _Check.warning;
  if (traits.contains('manual')) return _Check.manual;
  if (status == 'success') return _Check.success;
  if (status == 'failure') return _Check.failure;
  if (status == 'skipped' || status == 'cancelled') return _Check.ignored;
  return _Check.pending;
}

_Check _classifyCheck(JsonMap check) =>
    _classify(check.text('status'), check.array('traits').map((t) => '$t'));

/// Original `classifyGitlabPipelineJob`.
_Check _classifyGitlabJob(String status, bool allowFailure) {
  final mapped = switch (status) {
    'success' || 'passed' => 'success',
    'failed' => 'failure',
    'canceled' || 'cancelled' || 'skipped' => 'skipped',
    _ => 'pending',
  };
  if (status == 'manual') {
    return _classify(mapped, allowFailure ? const ['manual'] : const ['manual', 'action_required']);
  }
  if (mapped == 'failure' && allowFailure) return _classify(mapped, const ['warning']);
  return _classify(mapped, const []);
}

Color _checkColor(BuildContext context, _Check check) {
  final colors = PaseoColors.of(context);
  return switch (check) {
    _Check.success => colors.success,
    _Check.failure => colors.danger,
    _Check.warning || _Check.actionRequired || _Check.pending => colors.warning,
    _Check.manual || _Check.ignored => colors.muted,
  };
}

const _manualGearPath =
    'M10.5 7.63V6.37l-.787-.13c-.044-.175-.132-.349-.263-.61l.481-.652-.918-.913-.657.478a2.346 2.346 0 0 0-.612-.26L7.656 3.5H6.388l-.132.783c-.219.043-.394.13-.612.26l-.657-.478-.918.913.437.652c-.131.218-.175.392-.262.61l-.744.086v1.261l.787.13c.044.218.132.392.263.61l-.438.651.92.913.655-.434c.175.086.394.173.613.26l.131.783h1.313l.131-.783c.219-.043.394-.13.613-.26l.656.478.918-.913-.48-.652c.13-.218.218-.435.262-.61l.656-.13zM7 8.283a1.285 1.285 0 0 1-1.313-1.305c0-.739.57-1.304 1.313-1.304.744 0 1.313.565 1.313 1.304 0 .74-.57 1.305-1.313 1.305z';

/// Original `CheckPresentationIcon`: circle-check, circle-x (danger or warning),
/// circle-dot, circle-slash and the GitLab manual gear.
Widget _checkIcon(BuildContext context, _Check check, double size) {
  final color = _checkColor(context, check);
  return switch (check) {
    _Check.success => Icon(LucideIcons.circleCheck300, size: size, color: color),
    _Check.failure || _Check.warning => Icon(LucideIcons.circleX300, size: size, color: color),
    _Check.pending => Icon(LucideIcons.circleDot300, size: size, color: color),
    _Check.ignored => Icon(LucideIcons.circleSlash300, size: size, color: color),
    _Check.actionRequired || _Check.manual => SvgPicture.string(
      '<svg xmlns="http://www.w3.org/2000/svg" viewBox="-0.5 -0.5 15 15">'
      '<circle cx="7" cy="7" r="6.25" fill="none" stroke="#000" stroke-width="1.25"/>'
      '<path d="$_manualGearPath" fill="#000"/></svg>',
      width: size,
      height: size,
      colorFilter: ColorFilter.mode(color, BlendMode.srcIn),
    ),
  };
}

TextStyle _baseText(BuildContext context) =>
    Theme.of(context).textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.normal);

TextStyle _smallText(BuildContext context, [Color? color]) =>
    Theme.of(context).textTheme.bodySmall!.copyWith(
      fontWeight: FontWeight.normal,
      color: color ?? PaseoColors.of(context).muted,
    );

bool _isTouchPlatform(BuildContext context) =>
    const {TargetPlatform.android, TargetPlatform.iOS}.contains(Theme.of(context).platform);

// ---------------------------------------------------------------------------
// Pane states (pane-error.tsx, pane-skeleton.tsx, PullRequestContent empty).

class _PaneError extends StatelessWidget {
  const _PaneError({required this.onRetry});
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        spacing: 12,
        children: [
          Text(
            tr(context, 'workspace.git.diff.failedRefresh'),
            textAlign: TextAlign.center,
            style: _smallText(context),
          ),
          PaseoButton(
            label: tr(context, 'common.actions.retry'),
            icon: LucideIcons.rotateCw300,
            variant: PaseoButtonVariant.ghost,
            size: PaseoControlSize.xs,
            onPressed: onRetry,
          ),
        ],
      ),
    ),
  );
}

class _PaneEmpty extends StatelessWidget {
  const _PaneEmpty();

  @override
  Widget build(BuildContext context) => Center(
    child: Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        spacing: 8,
        children: [
          Text(
            tr(context, 'panels.pullRequest.emptyTitle'),
            textAlign: TextAlign.center,
            style: _baseText(context).copyWith(
              color: Theme.of(context).colorScheme.onSurface,
            ),
          ),
          Text(
            tr(context, 'panels.pullRequest.emptyDescription'),
            textAlign: TextAlign.center,
            style: _baseText(context).copyWith(color: PaseoColors.of(context).muted),
          ),
        ],
      ),
    ),
  );
}

/// Original `useSkeletonPulse`: opacity 0.4 ↔ 0.8 over 1s each way.
class _Pulse extends StatefulWidget {
  const _Pulse({required this.child});
  final Widget child;
  @override
  State<_Pulse> createState() => _PulseState();
}

class _PulseState extends State<_Pulse> with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(seconds: 1),
  )..repeat(reverse: true);

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => FadeTransition(
    opacity: _controller.drive(Tween(begin: .4, end: .8)),
    child: widget.child,
  );
}

Widget _skeletonBlock(
  BuildContext context, {
  double? width,
  double? widthFactor,
  required double height,
  double radius = 2,
}) {
  final block = Container(
    width: width,
    height: height,
    decoration: BoxDecoration(
      color: PaseoColors.of(context).surface2,
      borderRadius: BorderRadius.circular(radius),
    ),
  );
  return widthFactor == null
      ? block
      : FractionallySizedBox(alignment: Alignment.centerLeft, widthFactor: widthFactor, child: block);
}

class _ActivitySkeleton extends StatelessWidget {
  const _ActivitySkeleton();

  @override
  Widget build(BuildContext context) => _Pulse(
    child: Padding(
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Column(
        spacing: 12,
        children: [
          for (var i = 0; i < 3; i++)
            Row(
              spacing: 8,
              children: [
                _skeletonBlock(context, width: 20, height: 20, radius: 10),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 4,
                    children: [
                      _skeletonBlock(context, widthFactor: .7, height: 12),
                      _skeletonBlock(context, widthFactor: .45, height: 10),
                    ],
                  ),
                ),
              ],
            ),
        ],
      ),
    ),
  );
}

class _PaneSkeleton extends StatelessWidget {
  const _PaneSkeleton();

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: EdgeInsets.zero,
      children: [
        _Pulse(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 8,
              children: [
                _skeletonBlock(context, widthFactor: .75, height: 16),
                _skeletonBlock(context, widthFactor: .4, height: 12),
              ],
            ),
          ),
        ),
        FilePaneToolbar(
          sidebar: true,
          padding: const EdgeInsets.symmetric(horizontal: 12),
          children: [
            _Pulse(
              child: Row(
                spacing: 8,
                children: [
                  _skeletonBlock(context, width: 96, height: 24, radius: 8),
                  _skeletonBlock(context, width: 96, height: 24, radius: 8),
                ],
              ),
            ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                child: Text(
                  tr(context, 'workspace.git.pr.sections.checks'),
                  style: _smallText(context).copyWith(fontWeight: FontWeight.w500),
                ),
              ),
              _Pulse(
                child: Column(
                  spacing: 4,
                  children: [
                    for (var i = 0; i < 3; i++)
                      Container(
                        constraints: const BoxConstraints(minHeight: 32),
                        padding: const EdgeInsets.symmetric(horizontal: 12),
                        child: Row(
                          spacing: 8,
                          children: [
                            _skeletonBlock(context, width: 14, height: 14, radius: 7),
                            Expanded(child: _skeletonBlock(context, widthFactor: .6, height: 12)),
                          ],
                        ),
                      ),
                  ],
                ),
              ),
            ],
          ),
        ),
        const Padding(
          padding: EdgeInsets.symmetric(vertical: 8),
          child: _ActivitySkeleton(),
        ),
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// Shared interaction primitives.

/// A pressable region that reports pointer hover to [builder].
class _Hover extends StatefulWidget {
  const _Hover({required this.builder, this.onTap, this.semanticLabel});
  final Widget Function(BuildContext context, bool hovered) builder;
  final VoidCallback? onTap;
  final String? semanticLabel;
  @override
  State<_Hover> createState() => _HoverState();
}

class _HoverState extends State<_Hover> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) => Semantics(
    button: widget.onTap != null,
    label: widget.semanticLabel,
    child: MouseRegion(
      cursor: widget.onTap == null ? MouseCursor.defer : SystemMouseCursors.click,
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onTap,
        child: widget.builder(context, _hovered && widget.onTap != null),
      ),
    ),
  );
}

/// Original `Button variant="ghost" size="xs"` with the pane's `viewButton`
/// override: 24px tall, 4px inset, 4px radius and gap, a 12px leading glyph and
/// 12px muted text that turns foreground on hover.
class _ViewButton extends StatefulWidget {
  const _ViewButton({required this.label, required this.onPressed});
  final String label;
  final VoidCallback onPressed;
  @override
  State<_ViewButton> createState() => _ViewButtonState();
}

class _ViewButtonState extends State<_ViewButton> {
  bool _hovered = false, _pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final type = PaseoTypography.of(context);
    final fg = _hovered ? Theme.of(context).colorScheme.onSurface : colors.muted;
    return Semantics(
      button: true,
      label: widget.label,
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
          onTap: widget.onPressed,
          child: Opacity(
            opacity: _pressed ? .85 : 1,
            child: Container(
              height: 24,
              padding: const EdgeInsets.symmetric(horizontal: 4),
              decoration: BoxDecoration(
                border: Border.all(color: Colors.transparent),
                borderRadius: BorderRadius.circular(4),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                spacing: 4,
                children: [
                  Icon(LucideIcons.externalLink300, size: 12, color: fg),
                  Text(
                    widget.label,
                    style: _baseText(context).copyWith(
                      color: fg,
                      fontSize: type.uiSize(12),
                      height: 1.25,
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// Header (pane.tsx `header`).

class _Header extends StatelessWidget {
  const _Header({required this.pr, required this.forge, required this.onOpen});
  final JsonMap pr;
  final String forge;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final state = pr.text('state').toLowerCase();
    // Original `derivePrState`.
    final (String stateKey, IconData icon, Color color) =
        pr.flag('isMerged') || state == 'merged'
        ? ('merged', LucideIcons.gitMerge300, statusMergedColor(context))
        : state != 'open'
        ? ('closed', LucideIcons.gitPullRequestClosed300, colors.danger)
        : pr.flag('isDraft')
        ? ('draft', LucideIcons.gitPullRequestDraft300, colors.muted)
        : ('open', LucideIcons.gitPullRequest300, colors.success);
    final facts = pr.object('forgeSpecific');
    final approvalsRequired = facts['forge'] == 'gitlab' ? facts.integer('approvalsRequired') : 0;
    final approvalsGiven = facts.integer('approvalsGiven');
    final repoIdentity = pr.text('projectPath').isNotEmpty
        ? pr.text('projectPath')
        : pr.text('repoOwner').isNotEmpty && pr.text('repoName').isNotEmpty
        ? '${pr.text('repoOwner')}/${pr.text('repoName')}'
        : '';
    final base = _baseText(context);
    return _Hover(
      onTap: onOpen,
      builder: (context, hovered) => Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          spacing: 8,
          children: [
            Text.rich(
              TextSpan(
                children: [
                  TextSpan(text: pr.text('title')),
                  TextSpan(
                    text: ' ${forge == 'gitlab' ? '!' : '#'}${pr.integer('number')}',
                    style: TextStyle(color: colors.muted),
                  ),
                ],
              ),
              style: base.copyWith(
                color: theme.colorScheme.onSurface,
                height: 22 / 14,
              ),
            ),
            ConstrainedBox(
              constraints: const BoxConstraints(minHeight: 16),
              child: Row(
                spacing: 4,
                children: [
                  Icon(icon, size: 14, color: color),
                  Text(
                    tr(context, 'workspace.git.pr.states.$stateKey'),
                    style: _smallText(context, color),
                  ),
                  if (approvalsRequired > 0)
                    Row(
                      mainAxisSize: MainAxisSize.min,
                      spacing: 4,
                      children: [
                        Icon(
                          LucideIcons.circleCheck300,
                          size: 11,
                          color: approvalsGiven >= approvalsRequired ? colors.success : colors.muted,
                        ),
                        Text(
                          tr(
                            context,
                            'workspace.git.pr.approvals',
                            args: {'given': approvalsGiven, 'required': approvalsRequired},
                          ),
                          style: _smallText(context),
                        ),
                      ],
                    ),
                  if (repoIdentity.isNotEmpty)
                    Flexible(
                      child: Padding(
                        padding: const EdgeInsets.only(left: 4),
                        child: Text(
                          repoIdentity,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: _smallText(context),
                        ),
                      ),
                    ),
                  Padding(
                    padding: const EdgeInsets.only(left: 4),
                    child: Opacity(
                      opacity: hovered ? 1 : 0,
                      child: Icon(LucideIcons.externalLink300, size: 12, color: colors.muted),
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

// ---------------------------------------------------------------------------
// Section kit (section-kit.tsx).

class _Section extends StatelessWidget {
  const _Section({
    required this.title,
    required this.open,
    required this.onToggle,
    required this.summary,
    required this.children,
  });
  final String title;
  final bool open;
  final VoidCallback onToggle;
  final List<Widget> summary;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Semantics(
          button: true,
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: onToggle,
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
                child: Row(
                  spacing: 8,
                  children: [
                    Icon(
                      open ? LucideIcons.chevronDown300 : LucideIcons.chevronRight300,
                      size: 14,
                      color: colors.muted,
                    ),
                    Text(title, style: _smallText(context).copyWith(fontWeight: FontWeight.w500)),
                    const Spacer(),
                    Row(mainAxisSize: MainAxisSize.min, spacing: 8, children: summary),
                  ],
                ),
              ),
            ),
          ),
        ),
        if (open)
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children),
          ),
      ],
    );
  }
}

class _SummaryPill extends StatelessWidget {
  const _SummaryPill({required this.count, required this.icon, required this.color});
  final int count;
  final Widget icon;
  final Color color;

  @override
  Widget build(BuildContext context) => count == 0
      ? const SizedBox.shrink()
      : Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 3,
          children: [icon, Text('$count', style: _smallText(context, color))],
        );
}

/// Original `sectionKitStyles.emptyText`: small muted copy inset 12/8.
class _EmptyText extends StatelessWidget {
  const _EmptyText(this.text);
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
    child: Text(text, style: _smallText(context)),
  );
}

/// Original `sectionKitStyles.checkRow`: a fixed 32px single-line row (12px
/// inset, 8px gap) with a 14px status glyph, base name, small muted detail and a
/// trailing small muted duration, hover-filled.
class _CheckRow extends StatelessWidget {
  const _CheckRow({
    required this.icon,
    required this.name,
    this.detail = '',
    this.trailing,
    this.onTap,
    this.hoverColor,
    this.height = 32,
    this.padding = const EdgeInsets.symmetric(horizontal: 12),
  });
  final Widget icon;
  final String name, detail;
  final Widget? trailing;
  final VoidCallback? onTap;
  final Color? hoverColor;
  final double? height;
  final EdgeInsets padding;

  @override
  Widget build(BuildContext context) => _Hover(
    onTap: onTap,
    builder: (context, hovered) => Container(
      height: height,
      constraints: height == null ? const BoxConstraints(minHeight: 32) : null,
      padding: padding,
      color: hovered ? hoverColor ?? sidebarHoverColor(context) : null,
      child: Row(
        spacing: 8,
        children: [
          icon,
          Expanded(
            child: Row(
              spacing: 8,
              children: [
                Flexible(
                  flex: 3,
                  child: Text(
                    name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: _baseText(context).copyWith(
                      color: Theme.of(context).colorScheme.onSurface,
                    ),
                  ),
                ),
                if (detail.isNotEmpty)
                  Flexible(
                    flex: 2,
                    child: Text(
                      detail,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: _smallText(context),
                    ),
                  ),
              ],
            ),
          ),
          ?trailing,
        ],
      ),
    ),
  );
}

// ---------------------------------------------------------------------------
// Checks (checks-section.tsx, checks-summary.ts, checks-ring.tsx).

class _ChecksSection extends StatelessWidget {
  const _ChecksSection({
    required this.checks,
    required this.fallbackUrl,
    required this.open,
    required this.onToggle,
    required this.collapsedGroups,
    required this.onToggleGroup,
    required this.onLaunch,
  });
  final List<JsonMap> checks;
  final String fallbackUrl;
  final bool open;
  final VoidCallback onToggle;
  final Set<_Check> collapsedGroups;
  final ValueChanged<_Check> onToggleGroup;
  final ValueChanged<String> onLaunch;

  static const _copy = 'workspace.git.pr.checksOverview';

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = PaseoColors.of(context);
    final groups = <(_Check, List<JsonMap>)>[
      for (final presentation in _Check.values)
        (presentation, checks.where((c) => _classifyCheck(c) == presentation).toList()),
    ].where((group) => group.$2.isNotEmpty).toList();
    // Original `selectOutcome`: a failure outranks anything still running.
    final outcome = checks.isEmpty
        ? 'none'
        : groups.any((g) => g.$1 == _Check.actionRequired)
        ? 'actionRequired'
        : groups.any((g) => g.$1 == _Check.failure || g.$1 == _Check.warning)
        ? 'failure'
        : checks.any((c) => _checkStatus(c.text('status')) == 'pending')
        ? 'pending'
        : 'success';
    final parts = [
      for (final (presentation, matching) in groups)
        tr(context, '$_copy.count.${presentation.name}', args: {'count': matching.length}),
    ].join(', ');
    final headline = tr(context, '$_copy.headline.$outcome');
    final detail = parts.isEmpty
        ? ''
        : tr(
            context,
            checks.length == 1 ? '$_copy.detailOne' : '$_copy.detailMany',
            args: {'parts': parts},
          );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _Hover(
          onTap: onToggle,
          semanticLabel: detail.isEmpty ? headline : '$headline. $detail',
          builder: (context, hovered) => Container(
            color: hovered ? sidebarHoverColor(context) : null,
            padding: const EdgeInsets.all(12),
            child: Row(
              spacing: 8,
              children: [
                SizedBox.square(
                  dimension: 20,
                  child: CustomPaint(
                    painter: _ChecksRingPainter(
                      parts: [
                        for (final (presentation, matching) in groups)
                          (_ringColor(context, presentation), matching.length),
                      ],
                      total: checks.length,
                      track: colors.surface3,
                    ),
                  ),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 2,
                    children: [
                      Text(
                        headline,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: _baseText(context).copyWith(
                          fontWeight: FontWeight.w500,
                          color: theme.colorScheme.onSurface,
                        ),
                      ),
                      if (detail.isNotEmpty)
                        Text(
                          detail,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: _smallText(context),
                        ),
                    ],
                  ),
                ),
                Icon(
                  open ? LucideIcons.chevronDown300 : LucideIcons.chevronRight300,
                  size: 14,
                  color: colors.muted,
                ),
              ],
            ),
          ),
        ),
        if (open && groups.isNotEmpty)
          ConstrainedBox(
            // Roughly eight rows; past this the list scrolls in place.
            constraints: const BoxConstraints(maxHeight: 268),
            child: SingleChildScrollView(
              primary: false,
              padding: const EdgeInsets.only(bottom: 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  for (final (presentation, matching) in groups) ...[
                    _groupHeader(context, presentation, matching.length),
                    if (!collapsedGroups.contains(presentation))
                      for (final check in matching)
                        _CheckRow(
                          icon: _checkIcon(context, presentation, 14),
                          name: check.text('name'),
                          detail: check.text('workflow'),
                          trailing: _timing(context, check),
                          onTap: () => onLaunch(check.text('url', fallbackUrl)),
                        ),
                  ],
                ],
              ),
            ),
          ),
      ],
    );
  }

  Widget _groupHeader(BuildContext context, _Check presentation, int count) {
    final collapsed = collapsedGroups.contains(presentation);
    return Semantics(
      button: true,
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: () => onToggleGroup(presentation),
        child: MouseRegion(
          cursor: SystemMouseCursors.click,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
            child: Row(
              spacing: 4,
              children: [
                Text(
                  tr(
                    context,
                    '$_copy.${count == 1 ? 'groupOne' : 'groupMany'}.${presentation.name}',
                    args: {'count': count},
                  ),
                  style: _smallText(context).copyWith(fontWeight: FontWeight.w500),
                ),
                Icon(
                  collapsed ? LucideIcons.chevronRight300 : LucideIcons.chevronDown300,
                  size: 12,
                  color: PaseoColors.of(context).muted,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// Original `formatCheckTiming`: a running check says how long it has run.
  Widget? _timing(BuildContext context, JsonMap check) {
    final duration = check.text('duration');
    if (duration.isEmpty) return null;
    final running = _checkStatus(check.text('status')) == 'pending';
    return Text(running ? 'running $duration' : duration, style: _smallText(context));
  }

  Color _ringColor(BuildContext context, _Check presentation) =>
      presentation == _Check.ignored ? PaseoColors.of(context).extraMuted : _checkColor(context, presentation);
}

/// Original `ChecksRing`: a 20-unit ring (3-unit stroke on a surface3 track)
/// split clockwise from twelve into per-status arcs, each 2 units short of its
/// share with a 1.5-unit floor; a single status owns the whole circle.
class _ChecksRingPainter extends CustomPainter {
  _ChecksRingPainter({required this.parts, required this.total, required this.track});
  final List<(Color, int)> parts;
  final int total;
  final Color track;

  static const _viewBox = 20.0, _stroke = 3.0, _gap = 2.0, _minSegment = 1.5;

  @override
  void paint(Canvas canvas, Size size) {
    final scale = size.width / _viewBox;
    final radius = (_viewBox - _stroke) / 2;
    final circumference = 2 * math.pi * radius;
    final center = size.center(Offset.zero);
    final rect = Rect.fromCircle(center: center, radius: radius * scale);
    Paint stroke(Color color) => Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = _stroke * scale
      ..color = color;
    canvas.drawCircle(center, radius * scale, stroke(track));
    if (total == 0) return;
    var offset = 0.0;
    for (final (color, count) in parts) {
      final arc = count / total * circumference;
      final dash = parts.length == 1 ? circumference : math.max(arc - _gap, _minSegment);
      canvas.drawArc(
        rect,
        -math.pi / 2 + offset / circumference * 2 * math.pi,
        dash / circumference * 2 * math.pi,
        false,
        stroke(color),
      );
      offset += arc;
    }
  }

  @override
  bool shouldRepaint(_ChecksRingPainter old) =>
      old.total != total || old.track != track || old.parts.toString() != parts.toString();
}

// ---------------------------------------------------------------------------
// GitLab pipeline section (git/forges/gitlab.view.tsx).

class _GitlabPipelineSection extends StatelessWidget {
  const _GitlabPipelineSection({
    required this.facts,
    required this.open,
    required this.onToggle,
    required this.pipeline,
    required this.loading,
    required this.error,
    required this.onLaunch,
  });
  final JsonMap facts;
  final bool open;
  final VoidCallback onToggle;
  final JsonMap? pipeline;
  final bool loading;
  final Object? error;
  final ValueChanged<String> onLaunch;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final rawStatus = facts.text('pipelineStatus');
    final url = facts.text('pipelineUrl');
    final stages = pipeline?.array('stages').map(jsonMap).toList() ?? const <JsonMap>[];
    final counts = <_Check, int>{};
    for (final job in stages.expand((stage) => stage.array('jobs').map(jsonMap))) {
      final presentation = _classifyGitlabJob(job.text('status'), job.flag('allowFailure'));
      if (presentation != _Check.ignored) counts[presentation] = (counts[presentation] ?? 0) + 1;
    }
    final showBreakdown = counts.isNotEmpty;
    Widget statusIcon(String status, [bool allowFailure = false]) =>
        _checkIcon(context, _classifyGitlabJob(status, allowFailure), 14);
    return _Section(
      title: tr(context, 'workspace.git.pr.sections.pipeline'),
      open: open,
      onToggle: onToggle,
      summary: [
        // Original `COUNTED_CHECK_PRESENTATIONS` order.
        for (final presentation in const [
          _Check.success,
          _Check.failure,
          _Check.warning,
          _Check.actionRequired,
          _Check.manual,
          _Check.pending,
        ])
          _SummaryPill(
            count: counts[presentation] ?? 0,
            icon: _checkIcon(context, presentation, 12),
            color: _checkColor(context, presentation),
          ),
        if (!showBreakdown) statusIcon(rawStatus),
      ],
      children: [
        _CheckRow(
          icon: statusIcon(rawStatus),
          name: 'Pipeline #${facts.integer('pipelineId')}',
          detail: rawStatus,
          hoverColor: colors.surface1,
          trailing: url.isEmpty
              ? null
              : Icon(LucideIcons.externalLink300, size: 12, color: colors.muted),
          onTap: url.isEmpty ? null : () => onLaunch(url),
        ),
        if (loading)
          _EmptyText(tr(context, 'workspace.git.pr.empty.loadingPipeline'))
        else if (pipeline != null && stages.isEmpty)
          _EmptyText(tr(context, 'workspace.git.pr.empty.noJobs'))
        else if (pipeline == null && error != null)
          _EmptyText(tr(context, 'workspace.git.pr.empty.pipelineJobsLoadFailed')),
        if (!loading)
          for (final stage in stages) ...[
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
              child: Row(
                spacing: 8,
                children: [
                  statusIcon(stage.text('status')),
                  Flexible(
                    child: Text(
                      stage.text('name').toUpperCase(),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: _smallText(context).copyWith(
                        fontWeight: FontWeight.w500,
                        letterSpacing: .5,
                      ),
                    ),
                  ),
                ],
              ),
            ),
            for (final job in stage.array('jobs').map(jsonMap))
              _CheckRow(
                icon: statusIcon(job.text('status'), job.flag('allowFailure')),
                name: job.text('name'),
                detail: job.flag('allowFailure')
                    ? tr(context, 'workspace.git.pr.empty.allowedToFail')
                    : '',
                hoverColor: colors.surface1,
                height: null,
                padding: const EdgeInsets.fromLTRB(24, 8, 12, 8),
                trailing: job.integer('durationSeconds') > 0
                    ? Text(
                        _formatDuration(job.integer('durationSeconds')),
                        style: _smallText(context),
                      )
                    : null,
                onTap: job.text('url').isEmpty ? null : () => onLaunch(job.text('url')),
              ),
          ],
      ],
    );
  }
}

// ---------------------------------------------------------------------------
// Activity timeline (timeline.ts, activity-state.ts, pane.tsx cards).

/// Original `mapActivity`: blank comments and blank "commented" reviews drop out.
Iterable<JsonMap> _mapActivity(JsonMap item) {
  final body = item.text('body');
  if (item.text('kind') == 'review') {
    if (item.text('reviewState') == 'commented' && body.trim().isEmpty) return const [];
    return [item];
  }
  if (body.trim().isEmpty) return const [];
  return [
    {...item, 'kind': 'comment'},
  ];
}

sealed class _Entry {
  String get id;
  bool get collapsedByDefault => false;
}

class _SingleEntry extends _Entry {
  _SingleEntry(this.activity);
  final JsonMap activity;
  @override
  String get id => activity.text('id');
  @override
  bool get collapsedByDefault {
    final location = activity.object('location');
    return location.flag('isResolved') || location.flag('isOutdated');
  }
}

class _ThreadEntry extends _Entry {
  _ThreadEntry(this.threadId, this.location, this.isResolved, this.comments);
  final String threadId;
  final JsonMap location;
  final bool? isResolved;
  final List<JsonMap> comments;
  @override
  String get id => 'thread:$threadId';
  @override
  bool get collapsedByDefault => isResolved == true || location.flag('isOutdated');
}

class _ReviewEntry extends _Entry {
  _ReviewEntry(this.review, this.threads);
  final JsonMap review;
  final List<_ThreadEntry> threads;
  @override
  String get id => review.text('id');
}

/// Original `buildPrTimeline`: comments sharing a thread id form one thread at
/// the first comment's position; threads whose root carries a review id nest
/// under that review; everything else stays standalone in order.
List<_Entry> _buildTimeline(List<JsonMap> activities) {
  final entries = <_Entry>[];
  final threads = <String, _ThreadEntry>{};
  for (final activity in activities) {
    final location = activity.object('location');
    final threadId = activity.text('threadId', location.text('threadId'));
    if (threadId.isEmpty) {
      entries.add(_SingleEntry(activity));
      continue;
    }
    final existing = threads[threadId];
    if (existing != null) {
      existing.comments.add(activity);
      continue;
    }
    final resolved = location['isResolved'] is bool
        ? location['isResolved'] as bool
        : activity['threadIsResolved'] is bool
        ? activity['threadIsResolved'] as bool
        : null;
    final thread = _ThreadEntry(threadId, location, resolved, [activity]);
    threads[threadId] = thread;
    entries.add(thread);
  }
  final reviewIds = {
    for (final entry in entries)
      if (entry is _SingleEntry && entry.activity.text('kind') == 'review') entry.activity.text('id'),
  };
  final byReview = <String, List<_ThreadEntry>>{};
  for (final entry in entries.whereType<_ThreadEntry>()) {
    final reviewId = entry.comments.first.text('reviewId');
    if (reviewId.isEmpty || !reviewIds.contains(reviewId)) continue;
    (byReview[reviewId] ??= []).add(entry);
  }
  if (byReview.isEmpty) return entries;
  final nested = byReview.values.expand((list) => list).toSet();
  return [
    for (final entry in entries)
      if (entry is _ThreadEntry && nested.contains(entry))
        ...const <_Entry>[]
      else if (entry is _SingleEntry && byReview.containsKey(entry.id))
        _ReviewEntry(entry.activity, byReview[entry.id]!)
      else
        entry,
  ];
}

/// Original `formatPullRequestThreadPath`.
String _threadPath(JsonMap location) {
  final path = location.text('path');
  if (location['line'] != null && location['startLine'] != null) {
    return '$path:${location.integer('startLine')}-${location.integer('line')}';
  }
  if (location['line'] != null) return '$path:${location.integer('line')}';
  return path;
}

class _TimelineCallbacks {
  const _TimelineCallbacks({
    required this.brand,
    required this.isCollapsed,
    required this.onToggle,
    required this.onLaunch,
  });
  final String brand;
  final bool Function(_Entry entry) isCollapsed;
  final void Function(_Entry entry) onToggle;
  final ValueChanged<String> onLaunch;
}

class _TimelineEntryCard extends StatelessWidget {
  const _TimelineEntryCard({required this.entry, required this.callbacks});
  final _Entry entry;
  final _TimelineCallbacks callbacks;

  @override
  Widget build(BuildContext context) => switch (entry) {
    final _ThreadEntry thread => _Card(
      child: _ThreadBlock(thread: thread, callbacks: callbacks),
    ),
    final _ReviewEntry review => _ReviewCard(entry: review, callbacks: callbacks),
    final _SingleEntry single => _SingleActivityCard(entry: single, callbacks: callbacks),
  };
}

/// Original `card`: 12px side inset, 12px below, 1px border, 8px radius.
class _Card extends StatelessWidget {
  const _Card({required this.child});
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 12),
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        color: colors.sidebar,
        border: Border.all(color: Theme.of(context).dividerColor),
        borderRadius: BorderRadius.circular(8),
      ),
      child: child,
    );
  }
}

/// Original `useRevealOnHover`: row actions show while hovered or while their
/// menu is open, and always on touch platforms and compact layouts.
class _RevealOnHover extends StatefulWidget {
  const _RevealOnHover({required this.builder});
  final Widget Function(BuildContext context, bool visible, ValueChanged<bool> onMenuOpen) builder;
  @override
  State<_RevealOnHover> createState() => _RevealOnHoverState();
}

class _RevealOnHoverState extends State<_RevealOnHover> {
  bool _hovered = false, _menuOpen = false;

  @override
  Widget build(BuildContext context) {
    final visible =
        _hovered || _menuOpen || _isTouchPlatform(context) || isCompactLayout(context);
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: widget.builder(context, visible, (open) {
        if (mounted) setState(() => _menuOpen = open);
      }),
    );
  }
}

/// Original `ActivityKebab` slot: 22px wide, hidden (and inert) until revealed;
/// a 2px-padded 4px-radius trigger with a 14px more-horizontal glyph, muted
/// until hovered, on a surface2 hover fill. Opens a 200px end-aligned menu.
class _Kebab extends StatefulWidget {
  const _Kebab({
    required this.visible,
    required this.semanticLabel,
    required this.entries,
    required this.onSelected,
    required this.onMenuOpen,
  });
  final bool visible;
  final String semanticLabel;
  final List<PaseoMenuEntry<String>> entries;
  final ValueChanged<String> onSelected;
  final ValueChanged<bool> onMenuOpen;
  @override
  State<_Kebab> createState() => _KebabState();
}

class _KebabState extends State<_Kebab> {
  bool _hovered = false;

  Future<void> _open(BuildContext anchor) async {
    widget.onMenuOpen(true);
    final value = await showPaseoMenu<String>(
      context,
      anchor: paseoAnchorRect(anchor),
      alignEnd: true,
      width: 200,
      entries: widget.entries,
    );
    widget.onMenuOpen(false);
    if (value != null) widget.onSelected(value);
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return SizedBox(
      width: 22,
      child: Center(
        child: IgnorePointer(
          ignoring: !widget.visible,
          child: Opacity(
            opacity: widget.visible ? 1 : 0,
            child: Builder(
              builder: (anchor) => Semantics(
                button: true,
                label: widget.semanticLabel,
                excludeSemantics: true,
                child: MouseRegion(
                  cursor: SystemMouseCursors.click,
                  onEnter: (_) => setState(() => _hovered = true),
                  onExit: (_) => setState(() => _hovered = false),
                  child: GestureDetector(
                    behavior: HitTestBehavior.opaque,
                    onTap: () => _open(anchor),
                    child: Container(
                      padding: const EdgeInsets.all(2),
                      decoration: BoxDecoration(
                        color: _hovered ? colors.surface2 : null,
                        borderRadius: BorderRadius.circular(4),
                      ),
                      child: Icon(
                        LucideIcons.ellipsis300,
                        size: 14,
                        color: _hovered ? Theme.of(context).colorScheme.onSurface : colors.muted,
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// The activity kebab's Copy / Open on forge menu.
Widget _activityKebab(
  BuildContext context,
  JsonMap activity,
  _TimelineCallbacks callbacks,
  bool visible,
  ValueChanged<bool> onMenuOpen,
) => _Kebab(
  visible: visible,
  semanticLabel: tr(context, 'workspace.git.pr.accessibility.commentActions'),
  onMenuOpen: onMenuOpen,
  entries: [
    if (activity.text('body').trim().isNotEmpty)
      const PaseoMenuEntry.item(value: 'copy', label: 'Copy', icon: LucideIcons.copy300),
    PaseoMenuEntry.item(
      value: 'open',
      label: tr(context, 'workspace.git.pr.actions.openOn', args: {'brand': callbacks.brand}),
      icon: LucideIcons.externalLink300,
    ),
  ],
  onSelected: (value) {
    if (value == 'copy') {
      unawaited(Clipboard.setData(ClipboardData(text: activity.text('body'))));
    } else {
      callbacks.onLaunch(activity.text('url'));
    }
  },
);

/// IDENTITY_COLORS fills (styles/identity-colors.ts); order is load-bearing.
const _avatarFills = [
  Color(0xff7a6aa8),
  Color(0xff3d7ea6),
  Color(0xff388068),
  Color(0xffa4673a),
  Color(0xffb05c80),
  Color(0xff6a70b8),
  Color(0xff368080),
  Color(0xffb06260),
  Color(0xff8f7838),
  Color(0xff5179b0),
];

/// Original `ActivityAvatar`: a round avatar image, or the author's white 10px
/// initial on their case-insensitive identity colour.
class _Avatar extends StatelessWidget {
  const _Avatar({required this.activity, required this.size});
  final JsonMap activity;
  final double size;

  @override
  Widget build(BuildContext context) {
    final author = activity.text('author');
    var hash = 0;
    for (final rune in author.toLowerCase().runes) {
      hash = (hash * 31 + String.fromCharCode(rune).codeUnitAt(0)) & 0xffffffff;
    }
    final initial = Text(
      author.isEmpty ? '' : String.fromCharCode(author.runes.first).toUpperCase(),
      style: TextStyle(
        fontSize: PaseoTypography.of(context).uiSize(10),
        fontWeight: FontWeight.normal,
        color: Colors.white,
        height: 1,
      ),
    );
    final url = activity.text('avatarUrl');
    return ClipOval(
      child: Container(
        width: size,
        height: size,
        alignment: Alignment.center,
        color: _avatarFills[hash % _avatarFills.length],
        child: url.isEmpty
            ? initial
            : Image.network(
                url,
                width: size,
                height: size,
                fit: BoxFit.cover,
                errorBuilder: (_, _, _) => initial,
              ),
      ),
    );
  }
}

/// Original `ActivityHeader`: avatar, base author, the verb (approved / changes
/// requested carry a 12px status glyph), then the trailing age and actions.
class _ActivityHeader extends StatelessWidget {
  const _ActivityHeader({required this.activity, required this.avatarSize, required this.trailing});
  final JsonMap activity;
  final double avatarSize;
  final List<Widget> trailing;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final review = activity.text('kind') == 'review';
    final reviewState = activity.text('reviewState');
    final verbKey = !review
        ? 'commented'
        : switch (reviewState) {
            'approved' => 'approved',
            'changes_requested' => 'requestedChanges',
            _ => 'reviewed',
          };
    final verb = tr(context, 'workspace.git.pr.activity.$verbKey').toLowerCase();
    final (IconData? verbIcon, Color verbColor) = !review
        ? (null, colors.muted)
        : switch (reviewState) {
            'approved' => (LucideIcons.circleCheck300, colors.success),
            'changes_requested' => (LucideIcons.circleX300, colors.danger),
            _ => (null, colors.muted),
          };
    return Row(
      spacing: 8,
      children: [
        _Avatar(activity: activity, size: avatarSize),
        Flexible(
          child: Text(
            activity.text('author'),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: _baseText(context).copyWith(color: Theme.of(context).colorScheme.onSurface),
          ),
        ),
        Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 4,
          children: [
            if (verbIcon != null) Icon(verbIcon, size: 12, color: verbColor),
            Text(verb, style: _smallText(context, verbColor)),
          ],
        ),
        const Spacer(),
        Row(
          mainAxisSize: MainAxisSize.min,
          spacing: 8,
          children: [
            Text(_formatAge(activity.integer('createdAt')), style: _smallText(context)),
            ...trailing,
          ],
        ),
      ],
    );
  }
}

/// Original `entryHeaderPressable`: 8px gap row inset 12/8, min 36px, filled
/// with surfaceSidebarHover while hovered.
Widget _entryHeader({required VoidCallback onTap, required Widget child}) => _Hover(
  onTap: onTap,
  builder: (context, hovered) => Container(
    constraints: const BoxConstraints(minHeight: 36),
    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
    alignment: Alignment.centerLeft,
    color: hovered ? sidebarHoverColor(context) : null,
    child: child,
  ),
);

Widget _markdownBody(String body, _TimelineCallbacks callbacks, EdgeInsets padding) => Padding(
  padding: padding,
  child: PaseoMarkdown(data: body, compact: true, onTapLink: callbacks.onLaunch),
);

Widget _threadCount(BuildContext context, int count) => Row(
  mainAxisSize: MainAxisSize.min,
  spacing: 3,
  children: [
    Icon(LucideIcons.messageSquare300, size: 11, color: PaseoColors.of(context).muted),
    Text('$count', style: _smallText(context)),
  ],
);

class _SingleActivityCard extends StatelessWidget {
  const _SingleActivityCard({required this.entry, required this.callbacks});
  final _SingleEntry entry;
  final _TimelineCallbacks callbacks;

  @override
  Widget build(BuildContext context) {
    final activity = entry.activity;
    final hasBody = activity.text('body').trim().isNotEmpty;
    final collapsed = callbacks.isCollapsed(entry);
    return _RevealOnHover(
      builder: (context, visible, onMenuOpen) {
        final header = _entryHeader(
          onTap: hasBody
              ? () => callbacks.onToggle(entry)
              : () => callbacks.onLaunch(activity.text('url')),
          child: _ActivityHeader(
            activity: activity,
            avatarSize: 20,
            trailing: [_activityKebab(context, activity, callbacks, visible, onMenuOpen)],
          ),
        );
        if (!hasBody) {
          // Original `eventRow`: an unbordered header row, 8px below.
          return Padding(padding: const EdgeInsets.fromLTRB(12, 0, 12, 8), child: header);
        }
        return _Card(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              header,
              if (!collapsed)
                _markdownBody(
                  activity.text('body'),
                  callbacks,
                  const EdgeInsets.fromLTRB(12, 8, 12, 12),
                ),
            ],
          ),
        );
      },
    );
  }
}

class _ReviewCard extends StatelessWidget {
  const _ReviewCard({required this.entry, required this.callbacks});
  final _ReviewEntry entry;
  final _TimelineCallbacks callbacks;

  @override
  Widget build(BuildContext context) {
    final review = entry.review;
    final collapsed = callbacks.isCollapsed(entry);
    final hasBody = review.text('body').trim().isNotEmpty;
    return _RevealOnHover(
      builder: (context, visible, onMenuOpen) => _Card(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _entryHeader(
              onTap: () => callbacks.onToggle(entry),
              child: _ActivityHeader(
                activity: review,
                avatarSize: 20,
                trailing: [
                  if (collapsed) _threadCount(context, entry.threads.length),
                  _activityKebab(context, review, callbacks, visible, onMenuOpen),
                ],
              ),
            ),
            if (!collapsed) ...[
              if (hasBody)
                _markdownBody(
                  review.text('body'),
                  callbacks,
                  const EdgeInsets.fromLTRB(12, 8, 12, 12),
                ),
              if (entry.threads.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 4),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      for (final thread in entry.threads)
                        Container(
                          margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
                          clipBehavior: Clip.antiAlias,
                          decoration: BoxDecoration(
                            border: Border.all(color: Theme.of(context).dividerColor),
                            borderRadius: BorderRadius.circular(6),
                          ),
                          child: _ThreadBlock(thread: thread, callbacks: callbacks),
                        ),
                    ],
                  ),
                ),
            ],
          ],
        ),
      ),
    );
  }
}

/// Original `ThreadBlock`: a surface1 header (mono path or "Discussion thread",
/// resolved/outdated badges, collapsed comment count, kebab), then the root
/// comment and a rail of bordered surface1 reply cards.
class _ThreadBlock extends StatelessWidget {
  const _ThreadBlock({required this.thread, required this.callbacks});
  final _ThreadEntry thread;
  final _TimelineCallbacks callbacks;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final collapsed = callbacks.isCollapsed(thread);
    final type = PaseoTypography.of(context);
    final root = thread.comments.first;
    final replies = thread.comments.skip(1).toList();
    return _RevealOnHover(
      builder: (context, visible, onMenuOpen) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Hover(
            onTap: () => callbacks.onToggle(thread),
            builder: (context, hovered) => Container(
              constraints: const BoxConstraints(minHeight: 36),
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              color: hovered ? sidebarHoverColor(context) : colors.surface1,
              child: Row(
                spacing: 8,
                children: [
                  Flexible(
                    child: Text(
                      thread.location.text('path').isNotEmpty
                          ? _threadPath(thread.location)
                          : tr(context, 'workspace.git.pr.thread.discussion'),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: type.codeStyle.copyWith(
                        fontSize: type.uiSize(12),
                        height: null,
                        color: Theme.of(context).colorScheme.onSurface,
                      ),
                    ),
                  ),
                  if (thread.isResolved == true)
                    StatusBadge(
                      label: tr(context, 'workspace.git.pr.thread.resolved'),
                      variant: StatusBadgeVariant.success,
                    ),
                  if (thread.location.flag('isOutdated'))
                    StatusBadge(label: tr(context, 'workspace.git.pr.thread.outdated')),
                  const Spacer(),
                  if (collapsed) _threadCount(context, thread.comments.length),
                  _Kebab(
                    visible: visible,
                    semanticLabel: tr(context, 'workspace.git.pr.accessibility.threadActions'),
                    onMenuOpen: onMenuOpen,
                    entries: [
                      PaseoMenuEntry.item(
                        value: 'open',
                        label: tr(
                          context,
                          'workspace.git.pr.actions.openOn',
                          args: {'brand': callbacks.brand},
                        ),
                        icon: LucideIcons.externalLink300,
                      ),
                    ],
                    onSelected: (_) => callbacks.onLaunch(root.text('url')),
                  ),
                ],
              ),
            ),
          ),
          if (!collapsed) ...[
            _ThreadComment(comment: root, callbacks: callbacks, topBorder: true),
            if (replies.isNotEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    for (final reply in replies)
                      Container(
                        margin: const EdgeInsets.only(bottom: 8),
                        clipBehavior: Clip.antiAlias,
                        decoration: BoxDecoration(
                          color: colors.surface1,
                          border: Border.all(color: Theme.of(context).dividerColor),
                          borderRadius: BorderRadius.circular(6),
                        ),
                        child: _ThreadComment(comment: reply, callbacks: callbacks, topBorder: false),
                      ),
                  ],
                ),
              ),
          ],
        ],
      ),
    );
  }
}

/// Original `ThreadComment`: a top-bordered block (8px below) with a 16px-avatar
/// header inset 12/8/4, min 32px, and the compact markdown body 8px under it.
class _ThreadComment extends StatelessWidget {
  const _ThreadComment({required this.comment, required this.callbacks, required this.topBorder});
  final JsonMap comment;
  final _TimelineCallbacks callbacks;
  final bool topBorder;

  @override
  Widget build(BuildContext context) => _RevealOnHover(
    builder: (context, visible, onMenuOpen) => Container(
      padding: const EdgeInsets.only(bottom: 8),
      decoration: topBorder
          ? BoxDecoration(border: Border(top: BorderSide(color: Theme.of(context).dividerColor)))
          : null,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Container(
            constraints: const BoxConstraints(minHeight: 32),
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
            alignment: Alignment.centerLeft,
            child: _ActivityHeader(
              activity: comment,
              avatarSize: 16,
              trailing: [_activityKebab(context, comment, callbacks, visible, onMenuOpen)],
            ),
          ),
          if (comment.text('body').trim().isNotEmpty)
            _markdownBody(comment.text('body'), callbacks, const EdgeInsets.fromLTRB(12, 8, 12, 0)),
        ],
      ),
    ),
  );
}
