import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../ui/command_center.dart';
import '../../ui/common.dart';
import '../../ui/controls.dart';
import '../../ui/localization.dart';
import '../../ui/shell_pages.dart';
import '../../ui/theme.dart';
import '../chat/provider_icon.dart';
import '../settings/settings_host_widgets.dart';
import 'navigation_actions.dart';

/// The original History screen (`screens/sessions-screen.tsx`), pushed as a page so the
/// platform back gesture returns to the previous surface. [allHosts] opens it the way
/// the original route does, on "All hosts"; otherwise it starts on [serverId]. [cwd] /
/// [projectKey] narrow it to one project.
Future<void> showSessionHistory(
  BuildContext context, {
  required AppStore store,
  required String serverId,
  String cwd = '',
  String projectKey = '',
  bool allHosts = false,
}) => ShellPages.open(
  context,
  _SessionsScreen(
    store: store,
    serverId: serverId,
    cwd: cwd,
    projectKey: projectKey,
    allHosts: allHosts,
  ),
);

/// The original import-session sheet (`components/import-session-sheet.tsx`).
Future<void> showImportSessions(
  BuildContext context, {
  required AppStore store,
  required String serverId,
  String cwd = '',
}) => showAdaptiveSheet<void>(
  context: context,
  maxWidth: 560,
  builder: (_) => _ImportSessionsSheet(store: store, serverId: serverId, cwd: cwd),
);

/// Original palette red-300: the host-errors banner text.
const _red300 = Color(0xfffca5a5);

/// `ALL_HOSTS_OPTION_ID`.
const _allHosts = '';

/// Long enough that a typed word is one request, short enough to feel live.
const _searchDebounce = Duration(milliseconds: 200);

/// `AGENT_HISTORY_PAGE_LIMIT`.
const _historyPageLimit = 200;

DateTime? _parseDate(String value) => value.isEmpty ? null : DateTime.tryParse(value);

enum _DateSection { today, yesterday, thisWeek, thisMonth, older }

/// Original `deriveDateSectionKey`: calendar days, not elapsed time.
_DateSection _dateSection(DateTime? lastActivityAt) {
  if (lastActivityAt == null) return _DateSection.older;
  final now = DateTime.now();
  final today = DateTime(now.year, now.month, now.day);
  final yesterday = today.subtract(const Duration(days: 1));
  final local = lastActivityAt.toLocal();
  final day = DateTime(local.year, local.month, local.day);
  if (!day.isBefore(today)) return _DateSection.today;
  if (!day.isBefore(yesterday)) return _DateSection.yesterday;
  final days = today.difference(day).inHours ~/ 24;
  if (days <= 7) return _DateSection.thisWeek;
  if (days <= 30) return _DateSection.thisMonth;
  return _DateSection.older;
}

/// One history agent: its host, the daemon's agent record (with its project placement
/// under `project`) and when it was last active.
class _HistoryAgent {
  _HistoryAgent(this.serverId, this.agent)
    : lastActivityAt = _parseDate(agent.text('lastActivityAt', agent.text('updatedAt')));
  final String serverId;
  final JsonMap agent;
  final DateTime? lastActivityAt;
  String get key => '$serverId:${agent.text('id')}';
}

class _SessionsScreen extends StatefulWidget {
  const _SessionsScreen({
    required this.store,
    required this.serverId,
    required this.cwd,
    required this.projectKey,
    required this.allHosts,
  });
  final AppStore store;
  final String serverId;
  final String cwd;
  final String projectKey;
  final bool allHosts;
  @override
  State<_SessionsScreen> createState() => _SessionsScreenState();
}

class _SessionsScreenState extends State<_SessionsScreen> {
  final _searchInput = TextEditingController();
  late String _selectedHost =
      widget.allHosts && widget.cwd.isEmpty && widget.projectKey.isEmpty
      ? _allHosts
      : widget.serverId;
  String _search = '';
  Timer? _debounce;
  Timer? _clock;

  // `use-agent-history.ts`
  List<_HistoryAgent> _agents = [];
  Map<String, String> _cursors = {};
  List<(String, String)> _hostErrors = [];
  bool _initialLoad = true;
  bool _loadingMore = false;
  bool _isError = false;
  bool _searchTruncated = false;
  int _generation = 0;
  String? _fetchedHosts;

  AppStore get _store => widget.store;

  @override
  void initState() {
    super.initState();
    _store.addListener(_storeChanged);
    // Relative times keep themselves current.
    _clock = Timer.periodic(const Duration(minutes: 1), (_) {
      if (mounted) setState(() {});
    });
    if (_selectedHost != _allHosts && _store.hostFor(_selectedHost) == null) {
      _selectedHost = _allHosts;
    }
    scheduleMicrotask(() {
      if (mounted) _fetch();
    });
  }

  @override
  void dispose() {
    _generation++;
    _debounce?.cancel();
    _clock?.cancel();
    _store.removeListener(_storeChanged);
    _searchInput.dispose();
    super.dispose();
  }

  List<HostState> get _targetHosts => _selectedHost == _allHosts
      ? _store.hosts
      : _store.hosts.where((host) => host.serverId == _selectedHost).toList();

  List<HostState> get _reachableHosts => _targetHosts
      .where((host) => host.client.status == ConnectionStatus.connected)
      .toList();

  /// One gate: a fleet where any host predates history search has no search field.
  bool get _searchSupported {
    final hosts = _reachableHosts;
    return hosts.isNotEmpty && hosts.every((host) => host.client.supports('agentHistorySearch'));
  }

  bool get _isSearching => _searchSupported && _search.isNotEmpty;

  String _hostsKey(List<HostState> hosts) => hosts.map((host) => host.serverId).join('\u0000');

  void _storeChanged() {
    if (!mounted) return;
    // A host that disappeared drops the filter back to "All hosts".
    if (_selectedHost != _allHosts &&
        !_store.hosts.any((host) => host.serverId == _selectedHost)) {
      _selectedHost = _allHosts;
    }
    // The query is keyed on the reachable hosts, so a host (re)connecting refetches.
    if (_hostsKey(_reachableHosts) != _fetchedHosts) {
      _reset();
    } else {
      setState(() {});
    }
  }

  void _searchChanged(String value) {
    setState(() {});
    _debounce?.cancel();
    _debounce = Timer(_searchDebounce, () {
      if (!mounted) return;
      final next = _searchInput.text.trim();
      if (next == _search) return;
      _search = next;
      _reset();
    });
  }

  void _clearSearch() {
    _debounce?.cancel();
    _searchInput.clear();
    if (_search.isEmpty) {
      setState(() {});
      return;
    }
    _search = '';
    _reset();
  }

  void _selectHost(String serverId) {
    if (serverId == _selectedHost) return;
    _selectedHost = serverId;
    _reset();
  }

  /// A new query key: no data yet, so the screen shows its initial spinner again.
  void _reset() {
    setState(() {
      _agents = [];
      _cursors = {};
      _initialLoad = true;
      _isError = false;
      _searchTruncated = false;
    });
    _fetch();
  }

  Future<void> _refresh() => _fetch();

  void _loadMore() {
    if (_cursors.isEmpty || _loadingMore) return;
    _fetch(append: true);
  }

  /// Original `fetchAgentHistoryBatch`: every reachable target host in parallel (a next
  /// page only from hosts that reported more), merged most recent first. A host that
  /// fails, or cannot be reached, is named in the banner instead of silently missing.
  Future<void> _fetch({bool append = false}) async {
    final generation = ++_generation;
    final targets = _targetHosts;
    final reachable = _reachableHosts;
    _fetchedHosts = _hostsKey(reachable);
    final unreachable = [
      for (final host in targets)
        if (!reachable.contains(host)) (host.serverId, hostDisplayLabel(host)),
    ];
    final search = _searchSupported ? _search : '';
    final cursors = append ? _cursors : const <String, String>{};
    final hosts = append
        ? reachable.where((host) => cursors.containsKey(host.serverId)).toList()
        : reachable;
    if (hosts.isEmpty) {
      setState(() {
        _initialLoad = false;
        _loadingMore = false;
        if (!append) {
          _isError = false;
          _agents = [];
          _cursors = {};
          _hostErrors = unreachable;
        }
      });
      return;
    }
    setState(() {
      if (append) _loadingMore = true;
    });
    Future<(HostState, JsonMap?)> fetchHost(HostState host) async {
      try {
        final payload = await navigationRequest(
          _store,
          host.serverId,
          'fetch_agent_history_request',
          {
            'filter': {
              'includeArchived': true,
              if (widget.projectKey.isNotEmpty) 'projectKeys': [widget.projectKey],
            },
            if (search.isNotEmpty) 'search': search,
            'sort': [
              {'key': 'updated_at', 'direction': 'desc'},
            ],
            'page': {
              'limit': _historyPageLimit,
              if (cursors[host.serverId] != null) 'cursor': cursors[host.serverId],
            },
          },
          'fetch_agent_history_response',
        );
        return (host, payload);
      } catch (_) {
        return (host, null);
      }
    }

    final pages = await Future.wait(hosts.map(fetchHost));
    if (!mounted || generation != _generation) return;
    final failed = [
      for (final (host, payload) in pages)
        if (payload == null) (host.serverId, hostDisplayLabel(host)),
    ];
    final loaded = <(HostState, JsonMap)>[
      for (final (host, payload) in pages)
        if (payload != null) (host, payload),
    ];
    List<(String, String)> mergeErrors(List<(String, String)> base) {
      final byServer = <String, (String, String)>{
        for (final error in base) error.$1: error,
        for (final error in failed) error.$1: error,
      };
      return byServer.values.toList();
    }

    setState(() {
      _initialLoad = false;
      _loadingMore = false;
      if (loaded.isEmpty) {
        // "No connected hosts could load agent history": the query errors.
        if (!append) {
          _agents = [];
          _cursors = {};
          _isError = true;
          _hostErrors = mergeErrors(unreachable);
        } else {
          _hostErrors = mergeErrors(_hostErrors);
        }
        return;
      }
      final next = <String, String>{};
      var truncated = false;
      final rows = <_HistoryAgent>[];
      for (final (host, payload) in loaded) {
        for (final item in payload.array('entries')) {
          final entry = jsonMap(item);
          final agent = <String, dynamic>{
            ...entry.object('agent'),
            'project': entry.object('project'),
          };
          if (!_inScope(agent)) continue;
          rows.add(_HistoryAgent(host.serverId, agent));
        }
        final pageInfo = payload.object('pageInfo');
        final cursor = pageInfo['nextCursor'];
        if (pageInfo.flag('hasMore') && cursor is String && cursor.isNotEmpty) {
          next[host.serverId] = cursor;
        }
        truncated = truncated || payload.flag('searchTruncated');
      }
      final merged = <String, _HistoryAgent>{
        if (append)
          for (final agent in _agents) agent.key: agent,
        for (final agent in rows) agent.key: agent,
      };
      _agents = merged.values.toList()
        ..sort(
          (left, right) => (right.lastActivityAt?.millisecondsSinceEpoch ?? 0).compareTo(
            left.lastActivityAt?.millisecondsSinceEpoch ?? 0,
          ),
        );
      _cursors = next;
      _isError = false;
      _searchTruncated = search.isNotEmpty && ((append && _searchTruncated) || truncated);
      _hostErrors = mergeErrors(append ? _hostErrors : unreachable);
    });
  }

  /// A project-scoped history without a project key keeps sessions under its root.
  bool _inScope(JsonMap agent) {
    if (widget.cwd.isEmpty || widget.projectKey.isNotEmpty) return true;
    final checkout = agent.object('project').object('checkout');
    return [
      agent.text('cwd'),
      checkout.text('cwd'),
      checkout.text('mainRepoRoot'),
      checkout.text('worktreeRoot'),
    ].contains(widget.cwd);
  }

  /// agent-list.tsx handleAgentPress: opening any row, archived or not, only
  /// navigates to the agent. refresh_agent_request is reserved for the
  /// explicit Unarchive / Reload actions — it rehydrates provider history and
  /// can replace the stored timeline.
  void _open(_HistoryAgent row) {
    openNavigationAgent(_store, row.serverId, row.agent);
    Navigator.pop(context);
  }

  /// Original long-press: a running agent, or one whose host is offline, asks first;
  /// anything else is archived straight away.
  Future<void> _longPress(_HistoryAgent row) async {
    final connected =
        _store.hostFor(row.serverId)?.client.status == ConnectionStatus.connected;
    if (row.agent.text('status') == 'running' || !connected) {
      final archive = await _showArchiveSheet(context, hostOffline: !connected);
      if (archive != true || !mounted) return;
    }
    await _archive(row);
  }

  Future<void> _archive(_HistoryAgent row) async {
    try {
      await navigationRequest(
        _store,
        row.serverId,
        'archive_agent_request',
        {'agentId': row.agent.text('id')},
        'agent_archived',
      );
      if (!mounted) return;
      setState(() => row.agent['archivedAt'] = DateTime.now().toUtc().toIso8601String());
    } catch (_) {
      // Timeout errors are swallowed — the daemon still processes the archive.
    }
  }

  @override
  Widget build(BuildContext context) {
    PaseoScope.of(context);
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final compact = isCompactLayout(context);
    final inset = compact ? 12.0 : 24.0;
    final hosts = _store.hosts;
    final showHostFilter = hosts.length > 1;
    final searchSupported = _searchSupported;
    final isSearching = _isSearching;
    final showLoadError = _isError && _agents.isEmpty;
    final Widget body;
    if (_initialLoad) {
      body = Center(
        child: SizedBox.square(
          dimension: 36,
          child: CircularProgressIndicator(color: colors.muted, strokeWidth: 3),
        ),
      );
    } else if (showLoadError) {
      body = _EmptyState(
        text: 'Unable to load sessions',
        actions: [
          TextButton(onPressed: _refresh, child: const Text('Try again')),
        ],
      );
    } else if (_agents.isEmpty) {
      body = _EmptyState(
        text: isSearching
            ? tr(context, 'sessions.noMatches')
            : _selectedHost == _allHosts
            ? tr(context, 'sessions.empty')
            : 'No sessions for this host',
        actions: [
          if (isSearching)
            TextButton(
              onPressed: _clearSearch,
              child: Text(tr(context, 'sessions.actions.clearSearch')),
            )
          else
            TextButton.icon(
              onPressed: () => Navigator.maybePop(context),
              icon: const Icon(LucideIcons.chevronLeft300, size: 16),
              label: const Text('Back'),
            ),
          TextButton.icon(
            onPressed: () => showImportSessions(
              context,
              store: _store,
              serverId: _selectedHost == _allHosts
                  ? (hosts.firstOrNull?.serverId ?? widget.serverId)
                  : _selectedHost,
            ),
            icon: const Icon(LucideIcons.import300, size: 16),
            label: Text(tr(context, 'importSession.title')),
          ),
        ],
      );
    } else {
      body = _list(context, compact: compact, inset: inset, search: isSearching ? _search : '');
    }
    return Scaffold(
      backgroundColor: colors.surface0,
      body: SafeArea(
        bottom: false,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _MenuHeader(
              title: tr(context, 'sessions.title'),
              compact: compact,
              // Original `SidebarMenuToggle`: beside the sidebar it shows or hides it;
              // over the shell (compact) revealing the agent list leaves this screen.
              onMenu: () {
                if (EmbeddedShellPage.of(context)) {
                  widget.store.showSidebar(!widget.store.sidebarVisible);
                  return;
                }
                widget.store.showSidebar(true);
                Navigator.of(context).maybePop();
              },
            ),
            if (showHostFilter || searchSupported)
              Padding(
                padding: EdgeInsets.fromLTRB(inset, 16, inset, 0),
                child: Row(
                  spacing: 8,
                  children: [
                    if (searchSupported)
                      Flexible(
                        child: ConstrainedBox(
                          constraints: const BoxConstraints(maxWidth: 420),
                          child: _SearchField(
                            controller: _searchInput,
                            placeholder: tr(context, 'sessions.searchPlaceholder'),
                            clearLabel: tr(context, 'sessions.actions.clearSearch'),
                            onChanged: _searchChanged,
                            onClear: _clearSearch,
                          ),
                        ),
                      ),
                    if (showHostFilter)
                      _HostFilter(
                        hosts: hosts,
                        value: _selectedHost,
                        onChanged: _selectHost,
                      ),
                  ],
                ),
              ),
            if (_hostErrors.isNotEmpty)
              Padding(
                padding: EdgeInsets.fromLTRB(inset, 12, inset, 0),
                child: Container(
                  padding: const EdgeInsets.all(12),
                  decoration: BoxDecoration(
                    border: Border.all(color: theme.dividerColor),
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 4,
                    children: [
                      for (final (_, label) in _hostErrors)
                        Text(
                          tr(context, 'sessions.hostLoadFailed', args: {'host': label}),
                          style: theme.textTheme.bodySmall!.copyWith(
                            color: _red300,
                            fontWeight: FontWeight.normal,
                          ),
                        ),
                    ],
                  ),
                ),
              ),
            Expanded(child: body),
          ],
        ),
      ),
    );
  }

  /// Original `AgentList`: date sections in fixed order, then the rows, with the
  /// load-more / too-many-matches footer.
  Widget _list(
    BuildContext context, {
    required bool compact,
    required double inset,
    required String search,
  }) {
    final colors = PaseoColors.of(context);
    final base = Theme.of(context).textTheme.bodyMedium!;
    final buckets = <_DateSection, List<_HistoryAgent>>{};
    for (final agent in _agents) {
      buckets.putIfAbsent(_dateSection(agent.lastActivityAt), () => []).add(agent);
    }
    final items = <Widget>[];
    for (final section in _DateSection.values) {
      final agents = buckets[section];
      if (agents == null || agents.isEmpty) continue;
      items.add(
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
          child: Text(
            tr(context, 'agentList.dateSections.${section.name}'),
            style: base.copyWith(color: colors.muted, fontWeight: FontWeight.w500),
          ),
        ),
      );
      for (final agent in agents) {
        items.add(
          _SessionRow(
            key: ValueKey(agent.key),
            agent: agent.agent,
            hostLabel: switch (_store.hostFor(agent.serverId)) {
              final host? => hostDisplayLabel(host),
              null => agent.serverId,
            },
            search: search,
            compact: compact,
            onPressed: () => _open(agent),
            onLongPressed: () => _longPress(agent),
          ),
        );
      }
    }
    final Widget? footer = _searchTruncated
        ? Padding(
            padding: const EdgeInsets.symmetric(vertical: 16),
            child: Text(
              tr(context, 'sessions.tooManyMatches'),
              textAlign: TextAlign.center,
              style: base.copyWith(color: colors.muted),
            ),
          )
        : _cursors.isNotEmpty
        ? Padding(
            padding: const EdgeInsets.symmetric(vertical: 16),
            child: Center(
              child: TextButton(
                onPressed: _loadingMore ? null : _loadMore,
                child: Text(
                  _loadingMore ? 'Loading...' : tr(context, 'sessions.actions.loadMore'),
                ),
              ),
            ),
          )
        : null;
    return RefreshIndicator(
      color: colors.muted,
      backgroundColor: colors.surface1,
      onRefresh: _refresh,
      child: ListView.separated(
        physics: const AlwaysScrollableScrollPhysics(),
        keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
        padding: EdgeInsets.fromLTRB(inset, 16, inset, 24),
        itemCount: items.length + (footer == null ? 0 : 1),
        separatorBuilder: (_, _) => const SizedBox(height: 4),
        itemBuilder: (_, index) => index < items.length ? items[index] : footer!,
      ),
    );
  }
}

/// Original `SessionRow`. Phones: workspace (or project) title and badges, then the
/// provider + agent title, then `project · branch · time · host`. Wider layouts: one
/// line of workspace › provider + title and badges, with fixed project (132), host (120,
/// right-aligned, 16px further in), branch (132) and time (72, right-aligned) columns.
class _SessionRow extends StatefulWidget {
  const _SessionRow({
    super.key,
    required this.agent,
    required this.hostLabel,
    required this.search,
    required this.compact,
    required this.onPressed,
    required this.onLongPressed,
  });
  final JsonMap agent;
  final String hostLabel;
  final String search;
  final bool compact;
  final VoidCallback onPressed;
  final VoidCallback onLongPressed;
  @override
  State<_SessionRow> createState() => _SessionRowState();
}

class _SessionRowState extends State<_SessionRow> {
  bool hovered = false, pressed = false;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final compact = widget.compact;
    final agent = widget.agent;
    final search = widget.search;
    final project = agent.object('project');
    final projectName = project.text('projectName');
    final workspaceName = project.text('workspaceName');
    final branch = project.object('checkout').text('currentBranch');
    final title = agent.text('title').isNotEmpty
        ? agent.text('title')
        : tr(context, 'agentList.fallbackTitle');
    final time = paseoTimeAgo(_parseDate(agent.text('lastActivityAt', agent.text('updatedAt'))));
    final pending = agent.array('pendingPermissions').length;
    final base = theme.textTheme.bodyMedium!;
    final muted = base.copyWith(color: colors.muted, fontWeight: FontWeight.normal);
    final separator = Opacity(opacity: .7, child: Text('·', style: muted));
    Widget highlighted(String text, TextStyle style) =>
        PaseoHighlightedText(text, query: search, style: style, typoTolerant: true);
    final agentTitle = Row(
      mainAxisSize: MainAxisSize.min,
      spacing: 8,
      children: [
        ProviderIcon(provider: agent.text('provider'), size: 14, color: colors.muted),
        Flexible(child: highlighted(title, muted)),
      ],
    );
    final badges = [
      if (agent['archivedAt'] != null)
        StatusBadge(
          label: tr(context, 'agentList.badges.archived'),
          leading: Icon(LucideIcons.archive300, size: 12, color: colors.muted),
        ),
      if (pending > 0)
        StatusBadge(
          label: tr(context, 'agentList.badges.pending', args: {'count': pending}),
          variant: StatusBadgeVariant.warning,
        ),
    ];
    final workspaceTitle = highlighted(
      workspaceName.isNotEmpty ? workspaceName : projectName,
      base,
    );
    // agent-list.tsx rowContent (`minWidth: 0`, `overflow: hidden`): the workspace
    // name keeps up to 320px, the session title shrinks, and the cell clips rather
    // than painting over the project column.
    final titleRow = ClipRect(
      child: LayoutBuilder(
        builder: (context, constraints) => Row(
          spacing: 8,
          children: [
            if (compact)
              Flexible(child: workspaceTitle)
            else
              ConstrainedBox(
                constraints: BoxConstraints(
                  maxWidth: constraints.maxWidth < 320 ? constraints.maxWidth : 320,
                ),
                child: workspaceTitle,
              ),
            if (!compact) ...[
              Icon(LucideIcons.chevronRight300, size: 12, color: colors.muted),
              Flexible(child: agentTitle),
            ],
            for (final badge in badges) Flexible(child: badge),
          ],
        ),
      ),
    );
    final Widget content;
    if (compact) {
      content = Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          titleRow,
          agentTitle,
          Padding(
            padding: const EdgeInsets.only(top: 2),
            child: Wrap(
              spacing: 4,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                highlighted(projectName, muted),
                separator,
                highlighted(branch, muted),
                separator,
                Text(time, maxLines: 1, style: muted),
                if (widget.hostLabel.isNotEmpty) ...[
                  separator,
                  Text(widget.hostLabel, maxLines: 1, overflow: TextOverflow.ellipsis, style: muted),
                ],
              ],
            ),
          ),
        ],
      );
    } else {
      content = Row(
        children: [
          Expanded(child: titleRow),
          // agent-list.tsx rowColumns: the columns keep spacing[3] from the content.
          const SizedBox(width: 12),
          SizedBox(width: 132, child: highlighted(projectName, muted)),
          const SizedBox(width: 12 + 16),
          SizedBox(
            width: 120,
            child: Text(
              widget.hostLabel,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              textAlign: TextAlign.right,
              style: muted,
            ),
          ),
          const SizedBox(width: 12),
          SizedBox(width: 132, child: highlighted(branch, muted)),
          const SizedBox(width: 12),
          SizedBox(
            width: 72,
            child: Text(time, maxLines: 1, textAlign: TextAlign.right, style: muted),
          ),
        ],
      );
    }
    return Padding(
      padding: EdgeInsets.only(bottom: compact ? 4 : 0),
      child: Semantics(
        button: true,
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
            onLongPress: () {
              setState(() => pressed = false);
              widget.onLongPressed();
            },
            child: Container(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              decoration: BoxDecoration(
                color: pressed
                    ? colors.surface2
                    : hovered
                    ? colors.surface1
                    : null,
                borderRadius: BorderRadius.circular(compact ? 8 : 0),
              ),
              child: content,
            ),
          ),
        ),
      ),
    );
  }
}

/// The original running/offline archive sheet: a fading 35% scrim and a surface2 sheet
/// (16px top radius, 24px sides, 16px top, ≥24px bottom, 16px gaps) with a muted 40×4
/// handle, a centred semibold title and Cancel / Archive buttons (16px tall padding,
/// 8px radius); Archive is disabled while the host is offline.
Future<bool?> _showArchiveSheet(BuildContext context, {required bool hostOffline}) =>
    showGeneralDialog<bool>(
      context: context,
      barrierDismissible: true,
      barrierLabel: 'Dismiss',
      barrierColor: const Color(0x59000000),
      transitionDuration: const Duration(milliseconds: 200),
      transitionBuilder: (_, animation, _, child) =>
          FadeTransition(opacity: animation, child: child),
      pageBuilder: (context, _, _) {
        final colors = PaseoColors.of(context);
        final theme = Theme.of(context);
        final base = theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w600);
        final bottom = MediaQuery.paddingOf(context).bottom;
        Widget button({
          required String label,
          required Color color,
          required Color textColor,
          VoidCallback? onPressed,
        }) => Expanded(
          child: Material(
            color: color,
            borderRadius: BorderRadius.circular(8),
            clipBehavior: Clip.antiAlias,
            child: InkWell(
              onTap: onPressed,
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 16),
                child: Center(
                  child: Opacity(
                    opacity: onPressed == null ? .5 : 1,
                    child: Text(label, style: base.copyWith(color: textColor)),
                  ),
                ),
              ),
            ),
          ),
        );
        return Align(
          alignment: Alignment.bottomCenter,
          child: Material(
            color: colors.surface2,
            borderRadius: const BorderRadius.vertical(top: Radius.circular(16)),
            child: Padding(
              padding: EdgeInsets.fromLTRB(24, 16, 24, bottom > 24 ? bottom : 24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                spacing: 16,
                children: [
                  Center(
                    child: Container(
                      width: 40,
                      height: 4,
                      decoration: BoxDecoration(
                        color: colors.muted.withValues(alpha: .3),
                        borderRadius: BorderRadius.circular(999),
                      ),
                    ),
                  ),
                  Text(
                    tr(
                      context,
                      hostOffline
                          ? 'agentList.archiveSheet.hostOffline'
                          : 'agentList.archiveSheet.runningAgent',
                    ),
                    textAlign: TextAlign.center,
                    style: base.copyWith(color: theme.colorScheme.onSurface),
                  ),
                  Row(
                    spacing: 12,
                    children: [
                      button(
                        label: tr(context, 'common.actions.cancel'),
                        color: colors.surface1,
                        textColor: theme.colorScheme.onSurface,
                        onPressed: () => Navigator.pop(context, false),
                      ),
                      button(
                        label: tr(context, 'agentList.archiveSheet.archive'),
                        color: theme.colorScheme.primary,
                        textColor: theme.colorScheme.onPrimary,
                        onPressed: hostOffline ? null : () => Navigator.pop(context, true),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        );
      },
    );

/// Original empty/error state: centred muted base text over ghost actions, 24px gaps
/// and padding.
class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.text, required this.actions});
  final String text;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          spacing: 24,
          children: [
            Text(
              text,
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodyMedium!.copyWith(color: colors.muted),
            ),
            ...actions,
          ],
        ),
      ),
    );
  }
}

/// Original `SearchField`: the host pill's chrome (surface1, 1px border, 6px radius,
/// 12/6 padding, 8px gaps) around a muted 14px search glyph, a 20px base input and a
/// muted 14px clear X; focus only moves the border to borderAccent on surface2.
class _SearchField extends StatefulWidget {
  const _SearchField({
    required this.controller,
    required this.placeholder,
    required this.clearLabel,
    required this.onChanged,
    required this.onClear,
  });
  final TextEditingController controller;
  final String placeholder;
  final String clearLabel;
  final ValueChanged<String> onChanged;
  final VoidCallback onClear;
  @override
  State<_SearchField> createState() => _SearchFieldState();
}

class _SearchFieldState extends State<_SearchField> {
  final _focus = FocusNode();

  @override
  void initState() {
    super.initState();
    _focus.addListener(_focusChanged);
  }

  void _focusChanged() => setState(() {});

  @override
  void dispose() {
    _focus.removeListener(_focusChanged);
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final focused = _focus.hasFocus;
    final base = theme.textTheme.bodyMedium!;
    final style = base.copyWith(height: 20 / base.fontSize!);
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      decoration: BoxDecoration(
        color: focused ? colors.surface2 : colors.surface1,
        borderRadius: BorderRadius.circular(6),
        border: Border.all(color: focused ? colors.borderAccent : theme.dividerColor),
      ),
      child: Row(
        spacing: 8,
        children: [
          Icon(LucideIcons.search300, size: 14, color: colors.muted),
          Expanded(
            child: TextField(
              controller: widget.controller,
              focusNode: _focus,
              autocorrect: false,
              enableSuggestions: false,
              textInputAction: TextInputAction.search,
              style: style,
              cursorColor: theme.colorScheme.onSurface,
              decoration: InputDecoration(
                hintText: widget.placeholder,
                hintStyle: style.copyWith(color: colors.muted),
                hintMaxLines: 1,
                isDense: true,
                isCollapsed: true,
                filled: false,
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                contentPadding: EdgeInsets.zero,
              ),
              onChanged: widget.onChanged,
            ),
          ),
          if (widget.controller.text.isNotEmpty)
            Semantics(
              button: true,
              label: widget.clearLabel,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onTap: widget.onClear,
                child: Padding(
                  padding: const EdgeInsets.all(8),
                  child: Icon(LucideIcons.x300, size: 14, color: colors.muted),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Original `HostFilter` pill: surface1, 1px border, 6px radius, 12/6 padding, 6px gaps,
/// a muted 14px server glyph for "All hosts" (or the host's status dot), the medium base
/// label and a muted 14px chevron; surface2 hovered, surface3 pressed. Opens the
/// "Filter by host" picker.
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

  Future<void> _open(BuildContext anchor) async {
    final colors = PaseoColors.of(context);
    final picked = await showPaseoSelect<String>(
      context: anchor,
      title: 'Filter by host',
      value: widget.value,
      options: [
        PaseoSelectOption(
          _allHosts,
          'All hosts',
          leading: Icon(LucideIcons.server300, size: 14, color: colors.muted),
        ),
        for (final host in widget.hosts)
          PaseoSelectOption(
            host.serverId,
            hostDisplayLabel(host),
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
    final host = widget.hosts.where((host) => host.serverId == widget.value).firstOrNull;
    final label = host == null ? 'All hosts' : hostDisplayLabel(host);
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
            onTap: () => _open(anchor),
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
                  ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 180),
                    child: Text(
                      label,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: theme.textTheme.bodyMedium!.copyWith(
                        fontWeight: FontWeight.w500,
                        height: 20 / theme.textTheme.bodyMedium!.fontSize!,
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

/// Original `HostStatusDot` in its 14px `HostStatusDotSlot`: an 8px success / warning /
/// danger dot for online / connecting / offline.
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

/// Original `MenuHeader` for a pushed screen: the phone `CompactHeader` (12px insets,
/// 8px top padding, 56px row, round 32px island button, lg medium title) or the wide
/// `ScreenHeader` (36px bordered row, 12px insets, base light title), both led by the
/// `SidebarMenuToggle` (MobileMenuIcon on phones, PanelLeft elsewhere).
class _MenuHeader extends StatelessWidget {
  const _MenuHeader({required this.title, required this.compact, required this.onMenu});
  final String title;
  final bool compact;
  final VoidCallback onMenu;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final type = PaseoTypography.of(context);
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
            style: theme.textTheme.bodyMedium!.copyWith(
              fontSize: type.uiSize(compact ? 16 : 14),
              fontWeight: compact ? FontWeight.w500 : FontWeight.w300,
              color: theme.colorScheme.onSurface,
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

/// Original header-island button: a round 32px (26px wide) frame around a muted 16px
/// glyph, filled with the interaction highlight on hover/press.
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

/// The original import-session sheet: recent provider sessions on a host (or under one
/// workspace), filterable by provider, imported into Paseo on tap.
class _ImportSessionsSheet extends StatefulWidget {
  const _ImportSessionsSheet({
    required this.store,
    required this.serverId,
    required this.cwd,
  });
  final AppStore store;
  final String serverId;
  final String cwd;
  @override
  State<_ImportSessionsSheet> createState() => _ImportSessionsSheetState();
}

class _ImportSessionsSheetState extends State<_ImportSessionsSheet> {
  /// The import sheet grows its page, as the original does.
  int _limit = 50;
  final _search = TextEditingController();
  late final String _serverId = widget.serverId;
  late String _scopeCwd = widget.cwd;
  final _entries = <JsonMap>[];
  final _providers = <String>{};
  String? _provider;
  bool _more = false;
  bool _busy = false;
  bool _loaded = false;
  String? _error;
  String? _notice;
  String? _importing;
  int _generation = 0;
  Timer? _debounce;

  @override
  void initState() {
    super.initState();
    scheduleMicrotask(() {
      if (mounted) _load();
    });
  }

  @override
  void dispose() {
    _generation++;
    _debounce?.cancel();
    _search.dispose();
    super.dispose();
  }

  void _searchChanged(String _) {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 300), () {
      if (mounted) _load(force: true);
    });
    setState(() {});
  }

  Future<void> _load({bool force = false}) async {
    if (_busy && !force) return;
    final generation = ++_generation;
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
    });
    try {
      final result = await navigationRequest(
        widget.store,
        _serverId,
        'fetch_recent_provider_sessions_request',
        {
          if (_scopeCwd.isNotEmpty) 'cwd': _scopeCwd,
          if (_provider != null) 'providers': [_provider],
          'query': _search.text.trim(),
          'limit': _limit,
        },
        'fetch_recent_provider_sessions_response',
      );
      String? catalogError;
      if (_providers.isEmpty) {
        try {
          final catalog = await navigationRequest(
            widget.store,
            _serverId,
            'list_available_providers_request',
            {},
            'list_available_providers_response',
          );
          if (!mounted || generation != _generation) return;
          _providers.addAll(
            catalog
                .array('providers')
                .map(jsonMap)
                .map((item) => item.text('provider'))
                .where((id) => id.isNotEmpty),
          );
        } catch (error) {
          catalogError = 'Provider list: $error';
        }
      }
      if (!mounted || generation != _generation) return;
      final rows = result.array('entries').map(jsonMap).toList();
      setState(() {
        _entries.clear();
        for (final row in rows) {
          final key = _key(row);
          _entries.removeWhere((old) => _key(old) == key);
          _entries.add(row);
          _providers.add(row.text('providerId'));
        }
        _more = rows.length >= _limit;
        final notices = <String>[?catalogError];
        for (final item in result.array('providerErrors')) {
          final error = jsonMap(item);
          notices.add(
            tr(context, 'importSession.status.failedProvider', args: {
              'provider': error.text('provider'),
            }),
          );
        }
        _notice = notices.isEmpty ? null : notices.join('\n');
      });
    } catch (error) {
      if (mounted && generation == _generation) {
        setState(() => _error = error.toString());
      }
    } finally {
      if (mounted && generation == _generation) {
        setState(() {
          _busy = false;
          _loaded = true;
        });
      }
    }
  }

  String _key(JsonMap row) => '${row.text('providerId')}:${row.text('providerHandleId')}';

  Future<void> _open(JsonMap row) async {
    setState(() {
      _busy = true;
      _error = null;
      _importing = _key(row);
    });
    try {
      if (row.text('cwd').isEmpty) {
        throw StateError('Provider session has no working directory.');
      }
      final response = await navigationRequest(
        widget.store,
        _serverId,
        'import_agent_request',
        {
          'providerId': row.text('providerId'),
          'providerHandleId': row.text('providerHandleId'),
          'cwd': row.text('cwd'),
        },
        'status',
      );
      final agent = response.object('agent');
      if (agent.text('id').isEmpty) {
        if (!mounted) return;
        throw StateError(tr(context, 'importSession.status.failedImport'));
      }
      await widget.store.refreshHost(_serverId);
      final hosts = widget.store.hostFor(_serverId);
      final matching = hosts?.agents.where(
        (item) => item.text('id') == agent.text('id'),
      );
      openNavigationAgent(
        widget.store,
        _serverId,
        matching != null && matching.isNotEmpty ? matching.first : agent,
      );
      if (mounted) Navigator.pop(context);
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) {
        setState(() {
          _busy = false;
          _importing = null;
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    PaseoScope.of(context);
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final host = widget.store.hostFor(_serverId);
    final hostLabel = host == null ? _serverId : hostDisplayLabel(host);
    final muted = theme.textTheme.bodyMedium!.copyWith(color: colors.muted);
    final providers = _providers.toList()..sort();
    final providerLabel = _provider ?? tr(context, 'importSession.filters.all');
    return SheetContents(
      title: tr(context, 'importSession.title'),
      subtitle: Row(
        spacing: 8,
        children: [
          Flexible(
            child: Text(
              _scopeCwd.isNotEmpty
                  ? tr(context, 'importSession.scope.workspace')
                  : tr(context, 'importSession.scope.host', args: {'host': hostLabel}),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          if (_scopeCwd.isNotEmpty)
            TextButton(
              style: TextButton.styleFrom(
                minimumSize: Size.zero,
                padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                tapTargetSize: MaterialTapTargetSize.shrinkWrap,
              ),
              onPressed: () {
                setState(() => _scopeCwd = '');
                _load(force: true);
              },
              child: Text(tr(context, 'importSession.actions.showAll')),
            ),
        ],
      ),
      headerActions: [
        IconButton(
          tooltip: tr(context, 'importSession.actions.refresh'),
          style: IconButton.styleFrom(
            padding: const EdgeInsets.all(8),
            minimumSize: Size.zero,
            tapTargetSize: MaterialTapTargetSize.shrinkWrap,
            foregroundColor: colors.muted,
          ),
          onPressed: _busy ? null : () => _load(force: true),
          icon: _busy
              ? const SizedBox.square(dimension: 16, child: CircularProgressIndicator(strokeWidth: 1.5))
              : const Icon(LucideIcons.rotateCw300, size: 16),
        ),
      ],
      header: Padding(
        padding: const EdgeInsets.fromLTRB(24, 0, 24, 12),
        child: Row(
          spacing: 8,
          children: [
            Icon(LucideIcons.search300, size: 16, color: colors.muted),
            Expanded(
              child: TextField(
                controller: _search,
                autocorrect: false,
                style: theme.textTheme.bodyMedium,
                decoration: InputDecoration(
                  hintText: tr(context, 'importSession.searchPlaceholder'),
                  isDense: true,
                  filled: false,
                  border: InputBorder.none,
                  enabledBorder: InputBorder.none,
                  focusedBorder: InputBorder.none,
                  contentPadding: const EdgeInsets.symmetric(vertical: 8),
                ),
                onChanged: _searchChanged,
              ),
            ),
          ],
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 4,
        children: [
          if (providers.length > 1)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: Align(
                alignment: Alignment.centerLeft,
                child: PopupMenuButton<String>(
                  tooltip: 'Filter by provider',
                  onSelected: (value) {
                    setState(() => _provider = value.isEmpty ? null : value);
                    _load(force: true);
                  },
                  itemBuilder: (_) => [
                    PopupMenuItem(value: '', height: 36, child: Text(tr(context, 'importSession.filters.all'))),
                    for (final provider in providers)
                      PopupMenuItem(
                        value: provider,
                        height: 36,
                        child: Row(
                          spacing: 8,
                          children: [
                            ProviderIcon(provider: provider, size: 14, color: colors.muted),
                            Text(provider),
                          ],
                        ),
                      ),
                  ],
                  child: Container(
                    padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
                    decoration: BoxDecoration(
                      color: colors.surface1,
                      borderRadius: BorderRadius.circular(6),
                      border: Border.all(color: theme.dividerColor),
                    ),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      spacing: 6,
                      children: [
                        _provider == null
                            ? Icon(LucideIcons.layers300, size: 14, color: colors.muted)
                            : ProviderIcon(provider: _provider!, size: 14, color: colors.muted),
                        Text(providerLabel, style: theme.textTheme.bodyMedium!.copyWith(fontWeight: FontWeight.w500)),
                        Icon(LucideIcons.chevronDown300, size: 14, color: colors.muted),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          if (_busy && _entries.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 8),
              child: Row(
                spacing: 8,
                children: [
                  const SizedBox.square(dimension: 14, child: CircularProgressIndicator(strokeWidth: 1.5)),
                  Text(tr(context, 'importSession.status.loading'), style: muted),
                ],
              ),
            ),
          if (_error != null)
            Text(
              _error!,
              style: theme.textTheme.bodySmall!.copyWith(color: colors.danger),
            ),
          if (_notice != null)
            Container(
              margin: const EdgeInsets.only(bottom: 8),
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
              decoration: BoxDecoration(
                border: Border.all(color: theme.dividerColor),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Text(_notice!, style: theme.textTheme.bodySmall!.copyWith(color: colors.danger)),
            ),
          for (final row in _entries) _importRow(context, row),
          if (_more)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Center(
                child: TextButton(
                  onPressed: _busy
                      ? null
                      : () {
                          _limit += 50;
                          _load(force: true);
                        },
                  child: Text(tr(context, 'importSession.actions.loadMore')),
                ),
              ),
            ),
          if (_loaded && !_busy && _entries.isEmpty && _error == null)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 32, horizontal: 16),
              child: Column(
                spacing: 8,
                children: [
                  Opacity(
                    opacity: .6,
                    child: Icon(LucideIcons.inbox300, size: 20, color: colors.muted),
                  ),
                  Text(
                    tr(context, _search.text.trim().isEmpty
                        ? 'importSession.empty.noRecent'
                        : 'sessions.noMatches'),
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium,
                  ),
                ],
              ),
            ),
        ],
      ),
    );
  }

  /// Original `ImportSessionSheetRow`.
  Widget _importRow(BuildContext context, JsonMap row) {
    final colors = PaseoColors.of(context);
    final theme = Theme.of(context);
    final title = row.text('title').isNotEmpty
        ? row.text('title')
        : tr(context, 'importSession.preview.untitledSession');
    final preview = row.text('firstPromptPreview', row.text('promptPreview'));
    final importing = _importing == _key(row);
    return Transform.translate(
      offset: const Offset(-8, 0),
      child: Material(
        color: Colors.transparent,
        borderRadius: BorderRadius.circular(8),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          hoverColor: colors.surface1,
          highlightColor: colors.surface2,
          onTap: _busy ? null : () => _open(row),
          child: Padding(
            padding: const EdgeInsets.all(8),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              spacing: 8,
              children: [
                Padding(
                  padding: const EdgeInsets.only(top: 2),
                  child: ProviderIcon(provider: row.text('providerId'), size: 16, color: colors.muted),
                ),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    spacing: 4,
                    children: [
                      Row(
                        crossAxisAlignment: CrossAxisAlignment.baseline,
                        textBaseline: TextBaseline.alphabetic,
                        spacing: 8,
                        children: [
                          Expanded(
                            child: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis,
                                style: theme.textTheme.bodyMedium),
                          ),
                          Text(
                            importing
                                ? 'Importing…'
                                : paseoTimeAgo(_parseDate(row.text('lastActivityAt'))),
                            style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
                          ),
                        ],
                      ),
                      Text(
                        preview.isEmpty ? tr(context, 'importSession.preview.noPrompt') : preview,
                        maxLines: 2,
                        overflow: TextOverflow.ellipsis,
                        style: theme.textTheme.bodyMedium!.copyWith(color: colors.muted, height: 20 / 14),
                      ),
                      if (_scopeCwd.isEmpty && row.text('cwd').isNotEmpty)
                        Text(
                          row.text('cwd'),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: theme.textTheme.bodySmall!.copyWith(color: colors.muted),
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
