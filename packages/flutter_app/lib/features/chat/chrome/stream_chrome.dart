part of '../agent_panel.dart';

String? _optionalString(Object? value) => value is String ? value : null;

/// Panel-local state of the conversation chrome around the stream: sync
/// status, history pagination, subagent tracks, chat find and the outline.
class _StreamChromeState {
  ChatController? owner;

  /// The first authoritative history has been applied (hasHydratedHistoryBefore).
  bool hydrated = false;
  bool loadingOlder = false;

  /// A detached reader's reading position is being held this frame.
  bool holdScheduled = false;

  /// The held row (mvcp's first visible item): the content offset of its
  /// newest (bottom) edge and the scroll offset both were taken at.
  ({int seq, double offset, double pixels})? hold;

  /// A jump-to-bottom request is scrolling back (pendingRequest).
  bool anchoring = false;

  /// use-agent-screen-state-machine.ts hadInitialSyncFailure: the first
  /// catch-up failed before any history landed, so a later catch-up of the
  /// still-empty chat stays silent instead of covering it with the overlay.
  bool initialSyncFailed = false;

  /// Failed older-history pages; each one owns a transient toast.
  int olderFailures = 0;

  /// archive_agent_request in flight for this agent (isArchivingCurrentAgent).
  bool archiving = false;
  ArchiveFinishedStatus archiveFinished = const ArchiveFinishedStatus.idle();
  final Set<String> pendingArchive = {}, hiddenProviderSubagents = {};

  ChatFindController? find;

  /// The transcript viewport, for the outline's reading position.
  final GlobalKey viewportKey = GlobalKey();
  bool findOpen = false;

  List<JsonMap> prompts = const [];
  String? promptsEpoch;
  int promptsLatestSeq = -1, promptsRequest = 0;
  int? activePromptSeq;
  bool readingScheduled = false;

  /// A provider-subagent panel asked for its parent's subagent list.
  bool childrenRequested = false;

  /// Toast ids the reader already dismissed while their source persists.
  final Set<Object> dismissedToasts = {};

  void reset(ChatController? next) {
    owner = next;
    hydrated = false;
    hold = null;
    anchoring = false;
    loadingOlder = false;
    initialSyncFailed = false;
    archiving = false;
    archiveFinished = const ArchiveFinishedStatus.idle();
    pendingArchive.clear();
    hiddenProviderSubagents.clear();
    find?.dispose();
    find = null;
    findOpen = false;
    prompts = const [];
    promptsEpoch = null;
    promptsLatestSeq = -1;
    promptsRequest++;
    activePromptSeq = null;
    childrenRequested = false;
  }

  void dispose() {
    find?.dispose();
    find = null;
  }
}

typedef _HoldShift = double Function(ScrollPosition position, double min, double max);

/// The stream's scroll controller: its position applies the detached
/// reader's hold ([_StreamChrome._readingHoldShift]) as part of the list's
/// layout, as maintainVisibleContentPosition does natively.
class _StreamScrollController extends ScrollController {
  _StreamScrollController(this.holdShift);
  final _HoldShift holdShift;

  @override
  ScrollPosition createScrollPosition(
    ScrollPhysics physics,
    ScrollContext context,
    ScrollPosition? oldPosition,
  ) => _StreamScrollPosition(
    holdShift,
    physics: physics,
    context: context,
    initialPixels: initialScrollOffset,
    keepScrollOffset: keepScrollOffset,
    oldPosition: oldPosition,
    debugLabel: debugLabel,
  );
}

class _StreamScrollPosition extends ScrollPositionWithSingleContext {
  _StreamScrollPosition(
    this.holdShift, {
    required super.physics,
    required super.context,
    super.initialPixels,
    super.keepScrollOffset,
    super.oldPosition,
    super.debugLabel,
  });
  final _HoldShift holdShift;

  /// The viewport calls this once its slivers are laid out and before
  /// anything paints; a correction makes it lay them out again within the
  /// same frame (a fling picks up from the corrected offset).
  @override
  bool applyContentDimensions(double minScrollExtent, double maxScrollExtent) {
    final shift = holdShift(this, minScrollExtent, maxScrollExtent);
    if (shift != 0) {
      correctBy(shift);
      return false;
    }
    return super.applyContentDimensions(minScrollExtent, maxScrollExtent);
  }
}

class _PanelFindOperations implements ChatFindOperations {
  _PanelFindOperations(this.state);
  final _AgentPanelState state;

  ChatController get _c => state.controller!;

  @override
  String? get epoch => _c.epoch;

  @override
  Future<JsonMap> search(String query, int? cursor) => _c.action(
    'agent.timeline.search.request',
    {'query': query, 'cursor': ?cursor},
    responseType: 'agent.timeline.search.response',
  );

  @override
  List<JsonMap> get rows => _c.timeline;

  /// model.ts load: older history until the hit's row is loaded; a failed
  /// page is the caller's connection failure.
  @override
  Future<void> load(String epoch, int seq) => _c.reveal(seq);

  @override
  void scrollTo(int rowSeqStart) {
    state._refresh(() => state.highlighted = '$rowSeqStart');
    // Like an outline jump, a find reveal leaves sticky-bottom so new rows
    // do not pull the reader back to the latest edge.
    state._detach();
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      final row = await state._rowContext(rowSeqStart);
      if (row == null || !row.mounted) return;
      final find = state._chrome.find;
      if (find != null && chatFindScrollToMatch(row, clearance: find.matchClearance)) return;
      unawaited(Scrollable.ensureVisible(
        row,
        duration: const Duration(milliseconds: 250),
        alignment: .8,
      ));
    });
  }
}

extension _StreamChrome on _AgentPanelState {
  bool get _nativeStream =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  bool get _connected => client.status == ConnectionStatus.connected;

  // ── scroll, bottom anchoring and history pagination ────────────────────

  /// strategy-native.tsx on the inverted list (offset 0 is the newest edge):
  /// the reader is "near the bottom" within 32px of it (the scroll-to-bottom
  /// button's rule); within 96px of the history start (the far end) the next
  /// older page loads. bottom-anchor-controller.ts handleScrollNearBottomChange:
  /// a scroll the reader is not driving (wheel, a reveal, a prompt jump) that
  /// leaves the bottom detaches a sticky stream — in the inverted list
  /// nothing else moves offset 0, since growth at the newest edge keeps it.
  void _onStreamScroll() {
    if (!scroll.hasClients) return;
    final position = scroll.position;
    final atBottom = position.pixels - position.minScrollExtent <= 32;
    if (atBottom != _atBottom) {
      // shouldDetachFromScrollAway: not while a jump-to-bottom request runs.
      if (!atBottom && _sticky && !_userScrolling && !_chrome.anchoring) _detach();
      _refresh(() => _atBottom = atBottom);
    }
    if (position.extentAfter <= 96) unawaited(_loadOlder());
    // The reader's (or a jump's) own scrolling moves what it reads; only
    // what the content does afterwards is held against.
    final held = _chrome.hold;
    if (held != null) _chrome.hold = (seq: held.seq, offset: held.offset, pixels: position.pixels);
    _scheduleReadingPosition();
    _holdReadingPosition();
  }

  /// bottom-anchor-controller.ts detachByUser.
  void _detach() {
    if (!_sticky) return;
    _sticky = false;
    _chrome.hold = null;
    _holdReadingPosition();
  }

  /// beginUserScroll / endUserScroll (strategy-native.tsx
  /// onScrollBeginDrag … onScrollEndDrag/onMomentumScrollEnd): ending within
  /// 32px of the bottom re-sticks (and settles at offset 0); ending away from
  /// it detaches. A scroll's end covers its fling, as the momentum end does.
  void _streamScrollEnded() {
    _userScrolling = false;
    _chrome.anchoring = false;
    if (!scroll.hasClients) return;
    final position = scroll.position;
    final nearBottom = position.pixels - position.minScrollExtent <= 32;
    if (nearBottom) {
      _sticky = true;
      _chrome.hold = null;
      _bottom();
    } else {
      _detach();
    }
  }

  /// history-start-pagination.ts: older rows extend the far end of the
  /// inverted list, so what the reader sees (and a running fling) is
  /// untouched by the prepend. use-load-older-agent-history.ts: a failed page
  /// is a 2.2s "Couldn't load older history" toast, not a sync error.
  Future<void> _loadOlder() async {
    final c = controller;
    if (c == null || _chrome.loadingOlder || !c.hasOlder || c.loading) return;
    _refresh(() => _chrome.loadingOlder = true);
    try {
      await c.older();
    } catch (_) {
      _refresh(() => _chrome.olderFailures++);
    } finally {
      _refresh(() => _chrome.loadingOlder = false);
    }
  }

  /// strategy-native.tsx maintainVisibleContentPosition
  /// ({minIndexForVisible: 0}, Android: only while detached): the newest
  /// visible row keeps its newest (bottom) edge where it was, so rows
  /// arriving or growing below a detached reader — including streamed text
  /// revealed between controller updates — never move what it reads. That
  /// row's own growth extends away from the edge, as in the inverted
  /// FlatList. The correction is part of the list's layout
  /// ([_readingHoldShift]), so no frame paints the pushed rows; after a
  /// frame the hold moves on to the row that is now the newest on screen.
  void _holdReadingPosition() {
    if (_sticky || _chrome.holdScheduled) return;
    _chrome.holdScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _chrome.holdScheduled = false;
      _chrome.hold = mounted && !_sticky && scroll.hasClients ? _readingAnchor() : null;
    });
  }

  /// _StreamScrollPosition's layout hook, run once the list is laid out and
  /// before it paints: how far the offset moves so the held row stays where
  /// it was on screen. Its content offset is compared with where it was
  /// held, less what the offset silently moved since (a sliver's own scroll
  /// correction moves both alike); the reader's scrolling rebases the hold
  /// in [_onStreamScroll] instead of being undone here.
  double _readingHoldShift(ScrollPosition position, double min, double max) {
    final held = _chrome.hold;
    if (!mounted || _sticky || held == null) return 0;
    final row = _streamRow(held.seq);
    if (row == null) return 0;
    final shift = (row.offset - held.offset) - (position.pixels - held.pixels);
    if (shift.abs() < .5) return 0;
    final target = (position.pixels + shift).clamp(min, max).toDouble();
    _chrome.hold = (seq: held.seq, offset: row.offset, pixels: target);
    return target - position.pixels;
  }

  /// [seq]'s row as the stream's sliver laid it out: the list child holding
  /// it and the content offset of its newest (bottom) edge, 0 being the
  /// inverted list's newest edge. Content offsets only change with layout,
  /// so an offset change not laid out yet never reads as the row moving.
  ({RenderBox box, double offset})? _streamRow(int seq) {
    var node = rowKeys[seq]?.currentContext?.findRenderObject();
    while (node != null && node.parent is! RenderSliverMultiBoxAdaptor) {
      node = node.parent;
    }
    final data = node?.parentData;
    if (node is! RenderBox || !node.attached || data is! SliverMultiBoxAdaptorParentData) return null;
    final offset = data.layoutOffset;
    return offset == null ? null : (box: node, offset: offset);
  }

  /// The newest row on screen (mvcp's first visible item).
  ({int seq, double offset, double pixels})? _readingAnchor() {
    final position = scroll.position;
    int? anchor;
    var offset = 0.0;
    for (final seq in rowKeys.keys) {
      if (anchor != null && seq <= anchor) continue;
      final row = _streamRow(seq);
      if (row == null || !row.box.hasSize) continue;
      // Wholly below the bottom or wholly above the top.
      if (row.offset + row.box.size.height <= position.pixels ||
          row.offset >= position.pixels + position.viewportDimension) {
        continue;
      }
      anchor = seq;
      offset = row.offset;
    }
    return anchor == null ? null : (seq: anchor, offset: offset, pixels: position.pixels);
  }

  /// view.tsx scrollToBottom("jump-to-bottom"): animated when the bottom is
  /// within the native list's overscan window, a jump beyond it. The request
  /// re-sticks at once and reports near-bottom (strategy-native.tsx
  /// scrollToBottom → onNearBottomChange(true)); until its scroll ends it is
  /// pending, so the way back never detaches.
  void _jumpToLatest() {
    if (!scroll.hasClients) return;
    final position = scroll.position;
    final distance = position.pixels - position.minScrollExtent;
    _refresh(() {
      _sticky = true;
      _atBottom = true;
      _chrome.hold = null;
      _chrome.anchoring = distance > 0;
    });
    if (distance <= position.viewportDimension * 4.5) {
      unawaited(scroll
          .animateTo(
            position.minScrollExtent,
            duration: const Duration(milliseconds: 300),
            curve: Curves.easeInOut,
          )
          .then((_) => _bottom()));
    } else {
      _bottom();
    }
  }

  /// scroll-keyboard-dismiss/model.ts: a flick into history faster than
  /// 1.5 points/ms dismisses the keyboard; a slow reading scroll keeps it.
  /// The transcript's own scrolls (not a code block's) drive the anchor.
  bool _onStreamScrollNotification(ScrollNotification notification) {
    if (notification.depth != 0) return false;
    if ((notification is ScrollStartNotification && notification.dragDetails != null) ||
        (notification is ScrollUpdateNotification && notification.dragDetails != null)) {
      _userScrolling = true;
    } else if (notification is ScrollEndNotification) {
      if (_nativeStream) {
        final velocity = notification.dragDetails?.primaryVelocity ?? 0;
        if (velocity > 1500) FocusManager.instance.primaryFocus?.unfocus();
      }
      _streamScrollEnded();
    }
    return false;
  }

  /// The list builds lazily: a row far from the viewport has no context
  /// until the list scrolls near it, so page toward it (older rows lie at
  /// larger inverted offsets) a viewport at a time.
  Future<BuildContext?> _rowContext(int seq) async {
    for (var attempt = 0; attempt < 400 && mounted; attempt++) {
      final built = rowKeys[seq]?.currentContext;
      if (built != null && built.mounted) return built;
      if (!scroll.hasClients) return null;
      final laidOut = [
        for (final entry in rowKeys.entries)
          if (entry.value.currentContext != null) entry.key,
      ]..sort();
      if (laidOut.isEmpty) return null;
      final older = seq < laidOut.first;
      if (!older && seq < laidOut.last) return null;
      final position = scroll.position;
      final step = position.viewportDimension * (older ? 1 : -1);
      final target = (position.pixels + step)
          .clamp(position.minScrollExtent, position.maxScrollExtent)
          .toDouble();
      if (target == position.pixels) return null;
      scroll.jumpTo(target);
      await WidgetsBinding.instance.endOfFrame;
    }
    return null;
  }

  // ── controller changes: sync state, catch-up, prompt index ─────────────

  void _streamChanged() {
    final c = controller;
    if (!identical(c, _chrome.owner)) {
      _chrome.reset(c);
      // bottom-anchor-controller.ts resetForAgent.
      _sticky = true;
      _userScrolling = false;
    }
    if (c == null) return;
    // use-agent-screen-state-machine.ts updateInitialSyncFailureMemory.
    if (c.synced) {
      _chrome.hydrated = true;
      _chrome.initialSyncFailed = false;
    } else if (c.syncStatus == TimelineSyncStatus.error && c.error != null) {
      _chrome.initialSyncFailed = true;
    }
    _chrome.find?.historyChanged(c.epoch);
    if (isSubagent &&
        !_chrome.childrenRequested &&
        jsonMap(client.serverInfo['features'])['providerSubagentNesting'] == true) {
      _chrome.childrenRequested = true;
      unawaited(c.loadSubagents().catchError((Object _) {}));
    }
    _refreshPrompts();
    // onNearHistoryStart also fires without a scroll: a transcript shorter
    // than the viewport keeps paging older history in until it fills.
    if (c.hasOlder && !c.loading && !_chrome.loadingOlder) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!mounted || !scroll.hasClients) return;
        if (scroll.position.extentAfter <= 96) unawaited(_loadOlder());
      });
    }
  }

  // ── outline ─────────────────────────────────────────────────────────────

  /// use-chat-outline.ts: the daemon's prompt index for the served epoch,
  /// refreshed whenever a newer prompt arrives.
  void _refreshPrompts() {
    final c = controller;
    if (c == null ||
        isSubagent ||
        !chatOutlineSupported() ||
        // view.tsx supportsChatOutline: hosts serving the prompt index only.
        jsonMap(client.serverInfo['features'])['agentTimelinePromptIndex'] != true ||
        c.epoch == null ||
        PaseoScope.of(context).preferences['chatOutlineEnabled'] == false) {
      return;
    }
    var latest = -1;
    for (final row in c.timeline) {
      if (jsonMap(row['item'])['type'] == 'user_message') {
        latest = intValue(row['seqStart']) > latest ? intValue(row['seqStart']) : latest;
      }
    }
    if (_chrome.promptsEpoch == c.epoch && latest <= _chrome.promptsLatestSeq) return;
    _chrome.promptsLatestSeq = latest;
    final epoch = c.epoch;
    if (_chrome.promptsEpoch != epoch) _chrome.prompts = const [];
    _chrome.promptsEpoch = epoch;
    final request = ++_chrome.promptsRequest;
    unawaited(c
        .action(
          'agent.timeline.list_prompts.request',
          {},
          responseType: 'agent.timeline.list_prompts.response',
        )
        .then((payload) {
          if (!mounted || request != _chrome.promptsRequest) return;
          if (stringValue(payload['epoch']) != controller?.epoch) return;
          _refresh(() => _chrome.prompts = maps(payload['prompts']));
          _scheduleReadingPosition();
        })
        .catchError((Object _) {}));
  }

  void _scheduleReadingPosition() {
    if (_chrome.readingScheduled || _chrome.prompts.length < 2) return;
    _chrome.readingScheduled = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _chrome.readingScheduled = false;
      if (!mounted) return;
      final viewport = _chrome.viewportKey.currentContext?.findRenderObject();
      if (viewport is! RenderBox || !viewport.attached) return;
      final top = viewport.localToGlobal(Offset.zero).dy;
      int? anchor;
      final keys = rowKeys.keys.toList()..sort();
      for (final seq in keys) {
        final box = rowKeys[seq]?.currentContext?.findRenderObject();
        if (box is! RenderBox || !box.attached) continue;
        final bottom = box.localToGlobal(Offset.zero).dy + box.size.height;
        if (bottom > top) {
          anchor = seq;
          break;
        }
      }
      final active = resolveActivePromptSeq(_chrome.prompts, anchor);
      if (active != _chrome.activePromptSeq) {
        _refresh(() => _chrome.activePromptSeq = active);
      }
    });
  }

  Future<void> _jumpToPrompt(int seq) async {
    final c = controller;
    if (c == null) return;
    int? loaded() => c.timeline
        .where((row) =>
            jsonMap(row['item'])['type'] == 'user_message' &&
            intValue(row['seqStart']) <= seq &&
            intValue(row['seqEnd'], intValue(row['seqStart'])) >= seq)
        .map((row) => intValue(row['seqStart']))
        .firstOrNull;
    var row = loaded();
    if (row == null) {
      try {
        await c.reveal(seq);
      } catch (_) {
        // view.tsx handleTimelineHistoryLoadError.
        if (mounted) _showFailure(StateError(tr(context, 'agentStream.historyLoadFailed')));
        return;
      }
      row = loaded();
    }
    if (row == null || !mounted) return;
    _detach();
    final target = row;
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      final revealed = await _rowContext(target);
      if (revealed != null && revealed.mounted) {
        unawaited(Scrollable.ensureVisible(
          revealed,
          duration: const Duration(milliseconds: 250),
          // Row top at the viewport top, counted from the inverted bottom.
          alignment: 1,
        ));
      }
    });
  }

  /// Prompt outline on demand (conversation.outline): the same index as the
  /// rail, listed in the adaptive menu.
  Future<void> _showOutline() async {
    final c = controller;
    if (c == null || _nativeStream) return;
    try {
      final payload = await c.action(
        'agent.timeline.list_prompts.request',
        {},
        responseType: 'agent.timeline.list_prompts.response',
      );
      final prompts = maps(payload['prompts']);
      if (!mounted || prompts.isEmpty) return;
      final seq = await showPaseoMenu<int>(
        context,
        title: tr(context, 'settings.appearance.chatOutline.title'),
        width: 320,
        entries: [
          for (final prompt in prompts)
            PaseoMenuEntry.item(
              value: intValue(prompt['seq']),
              label: stringValue(prompt['preview']),
              selected: intValue(prompt['seq']) == _chrome.activePromptSeq,
            ),
        ],
      );
      if (seq != null) await _jumpToPrompt(seq);
    } catch (e) {
      _showFailure(e);
    }
  }

  // ── find ────────────────────────────────────────────────────────────────

  void _openFind() {
    // chat-find/index.tsx: native has no chat find.
    if (controller == null || _nativeStream) return;
    var find = _chrome.find;
    if (find == null) {
      final created = find = _chrome.find = ChatFindController(_PanelFindOperations(this));
      // The selected message is the highlighted row; a new query clears it.
      created.addListener(() {
        final next = created.selectedRow == null ? null : '${created.selectedRow}';
        // Rows re-read the query and the current occurrence on every change.
        _refresh(() => highlighted = next);
      });
    }
    find.show();
    _refresh(() => _chrome.findOpen = true);
  }

  void _closeFind() => _refresh(() {
    _chrome.findOpen = false;
    highlighted = null;
  });

  // ── subagents ───────────────────────────────────────────────────────────

  /// subagents/select.ts useSubagentsForParent: managed children (by their
  /// parent labels, unarchived, not mid-archive) and the provider's own
  /// top-level subagents not dismissed from the track, oldest first.
  List<SubagentRowData> _subagentRows() {
    final c = controller;
    if (c == null || isDraft) return const [];
    // provider-subagent-panel.tsx ProviderSubagentChildTrack: a provider
    // subagent's own children (hosts with providerSubagentNesting only).
    final nesting =
        jsonMap(client.serverInfo['features'])['providerSubagentNesting'] == true;
    if (isSubagent && !nesting) return const [];
    final store = PaseoScope.of(context);
    final rows = <SubagentRowData>[];
    for (final host in store.hosts.where((_) => !isSubagent)) {
      for (final raw in host.agents) {
        final child = raw['agent'] is Map ? jsonMap(raw['agent']) : raw;
        final labels = jsonMap(child['labels']);
        final id = stringValue(child['id']);
        if (labels['paseo.parent-agent-id'] != widget.target.id ||
            stringValue(labels['paseo.parent-server-id'], host.serverId) !=
                widget.target.serverId ||
            child['archivedAt'] != null ||
            _chrome.pendingArchive.contains('${host.serverId}:$id')) {
          continue;
        }
        rows.add(SubagentRowData(
          kind: 'paseo',
          id: id,
          serverId: host.serverId,
          provider: stringValue(child['provider']),
          status: _optionalString(child['status']),
          title: _optionalString(child['title']),
          turnOpen: jsonMap(child['turn'])['phase'] == 'open' || child['activeTurn'] != null,
          createdAt: DateTime.tryParse(stringValue(child['createdAt'])) ?? DateTime(0),
          workspaceId: _optionalString(child['workspaceId']),
          cwd: _optionalString(child['cwd']),
          agent: child,
        ));
      }
    }
    for (final child in c.subagents) {
      final id = stringValue(child['id']);
      final parent = child['parentSubagentId'];
      // provider-store.ts: a dismissed subagent that runs again returns to the track.
      if (child['status'] == 'running') _chrome.hiddenProviderSubagents.remove(id);
      if (_chrome.hiddenProviderSubagents.contains(id) ||
          (isSubagent ? parent != widget.target.id : parent != null && nesting)) {
        continue;
      }
      rows.add(SubagentRowData(
        kind: 'provider',
        id: id,
        serverId: widget.target.serverId,
        provider: stringValue(child['provider'], stringValue(c.agent['provider'])),
        status: _optionalString(child['status']),
        title: _optionalString(child['title']),
        description: _optionalString(child['description']),
        subtitle: _optionalString(child['subtitle']),
        createdAt: DateTime.tryParse(stringValue(child['createdAt'])) ?? DateTime(0),
        parentAgentId: c.agentId,
        cwd: _optionalString(child['cwd']),
      ));
    }
    rows.sort((a, b) => a.createdAt.compareTo(b.createdAt));
    return rows;
  }

  PanelTarget _subagentTarget(SubagentRowData row) => row.isPaseo
      ? PanelTarget(
          kind: 'agent',
          serverId: row.serverId,
          workspaceId: row.workspaceId ?? widget.target.workspaceId,
          cwd: row.cwd ?? widget.target.cwd,
          id: row.id,
          title: row.label ?? 'Subagent',
        )
      : PanelTarget(
          kind: 'provider_subagent',
          serverId: widget.target.serverId,
          workspaceId: widget.target.workspaceId,
          cwd: row.cwd ?? widget.target.cwd,
          id: row.id,
          title: row.label ?? 'Subagent',
          data: {'parentAgentId': row.parentAgentId},
        );

  bool _crossWorkspace(SubagentRowData row) =>
      row.isPaseo &&
      (row.serverId != widget.target.serverId ||
          (row.workspaceId != null && row.workspaceId != widget.target.workspaceId));

  /// subagents/opens-in-sheet.ts + workspace-layout-store resolveSheetPlacement:
  /// on compact layouts a subagent in this workspace that is not already open
  /// as a tab opens in a sheet over the pane instead.
  bool _subagentOpensInSheet(SubagentRowData row) =>
      isCompactLayout(context) &&
      !_crossWorkspace(row) &&
      PaseoScope.of(context).targetFor(_subagentTarget(row).key) == null;

  /// agent-tracks.tsx handleOpenSubagent: a child in another host or
  /// workspace lives in that workspace's tabs.
  void _openSubagent(SubagentRowData row) {
    final store = PaseoScope.of(context);
    if (_crossWorkspace(row)) {
      store.openAgent(row.serverId, row.id).catchError(_showFailure);
      return;
    }
    final target = _subagentTarget(row);
    if (_subagentOpensInSheet(row)) {
      unawaited(_showSubagentSheet(target, row));
      return;
    }
    store.open(target);
  }

  /// screens/workspace/sheet-tabs.tsx SheetTabSheet: a 92% bottom sheet
  /// (45% scrim, pan-down to close) whose header is the provider icon, the
  /// tab label, the Maximize2 "Open as tab" button and close; the agent shows
  /// without its composer, under its one-line facts subtitle.
  Future<void> _showSubagentSheet(PanelTarget target, SubagentRowData row) {
    final colors = PaseoColors.of(context);
    final store = PaseoScope.of(context);
    final svg = providers
        .where((entry) => entry['provider'] == row.provider)
        .map((entry) => _optionalString(entry['iconSvg']))
        .firstOrNull;
    return showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      enableDrag: true,
      backgroundColor: colors.surface0,
      barrierColor: const Color(0x73000000),
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(16)),
      ),
      builder: (sheet) => SizedBox(
        height: MediaQuery.sizeOf(sheet).height * .92,
        child: SheetContents(
          title: row.label ?? tr(sheet, 'common.states.loading'),
          scroll: false,
          maxHeightFactor: .92,
          leading: ProviderIcon(
            provider: row.provider,
            snapshotSvg: svg,
            size: 16,
            color: colors.muted,
          ),
          headerActions: [
            IconButton(
              tooltip: tr(sheet, 'workspace.sheet.openAsTab'),
              style: IconButton.styleFrom(
                padding: const EdgeInsets.all(8),
                minimumSize: Size.zero,
                tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                foregroundColor: colors.muted,
                shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
              ),
              onPressed: () {
                Navigator.pop(sheet);
                store.open(target);
              },
              icon: const Icon(LucideIcons.maximize2300, size: 16),
            ),
          ],
          child: Padding(
            padding: EdgeInsets.only(bottom: MediaQuery.paddingOf(sheet).bottom),
            child: AgentPanel(
              target: PanelTarget(
                kind: target.kind,
                serverId: target.serverId,
                workspaceId: target.workspaceId,
                cwd: target.cwd,
                id: target.id,
                title: target.title,
                data: {...target.data, 'paneHost': 'sheet'},
              ),
            ),
          ),
        ),
      ),
    );
  }

  /// subagents/archive-subagent.ts requestArchiveSubagent.
  Future<void> _archiveSubagent(SubagentRowData row) async {
    final label = resolveRowLabel(row.title) ?? 'this subagent';
    final running = row.status == 'running';
    if (!await confirmAction(
      context,
      title: running ? 'Archive running subagent?' : 'Archive subagent?',
      message: running
          ? '$label is still running. Archiving it will stop the subagent and remove it from the track.'
          : 'Remove $label from the track. The subagent will be archived.',
      confirmLabel: 'Archive',
      destructive: true,
    )) {
      return;
    }
    if (!mounted) return;
    final key = '${row.serverId}:${row.id}';
    _refresh(() => _chrome.pendingArchive.add(key));
    try {
      requireSuccess(await PaseoScope.of(context).clientFor(row.serverId).request(
        'archive_agent_request',
        {'agentId': row.id},
        responseType: 'agent_archived',
      ));
    } catch (e) {
      _refresh(() => _chrome.pendingArchive.remove(key));
      _showFailure(e);
    }
  }

  /// subagents/detach-subagent.ts requestDetachSubagent: detach, then open the
  /// now standalone agent.
  Future<void> _detachSubagent(SubagentRowData row) async {
    final label = resolveRowLabel(row.title) ?? 'This subagent';
    if (!await confirmAction(
      context,
      title: 'Detach subagent?',
      message: '$label will leave this track and continue as a standalone agent.',
      confirmLabel: 'Detach',
    )) {
      return;
    }
    if (!mounted) return;
    final store = PaseoScope.of(context);
    try {
      final childClient = store.clientFor(row.serverId);
      if (childClient.status != ConnectionStatus.connected) {
        throw StateError(tr(context, 'workspaceSetup.errors.hostDisconnected'));
      }
      requireSuccess(await childClient.request(
        'agent.detach.request',
        {'agentId': row.id},
        responseType: 'agent.detach.response',
      ));
      await store.openAgent(row.serverId, row.id);
    } catch (e) {
      _showFailure(e);
    }
  }

  /// archive-finished.ts archiveFinished: archive every idle/errored managed
  /// child and dismiss finished provider rows, with n/m progress and a retry
  /// count for failures. No confirmation.
  Future<void> _archiveFinishedSubagents() async {
    if (_chrome.archiveFinished.kind == 'archiving') return;
    final eligible = _subagentRows().where((row) => row.isFinished).toList();
    if (eligible.isEmpty) return;
    final total = eligible.length;
    var completed = 0, failed = 0;
    _refresh(() => _chrome.archiveFinished = ArchiveFinishedStatus.archiving(0, total));
    final store = PaseoScope.of(context);
    for (final row in eligible) {
      if (row.isPaseo) {
        try {
          requireSuccess(await store.clientFor(row.serverId).request(
            'archive_agent_request',
            {'agentId': row.id},
            responseType: 'agent_archived',
          ));
          _chrome.pendingArchive.add('${row.serverId}:${row.id}');
        } catch (_) {
          failed++;
        }
      } else {
        _chrome.hiddenProviderSubagents.add(row.id);
      }
      completed++;
      if (!mounted) return;
      _refresh(() => _chrome.archiveFinished = ArchiveFinishedStatus.archiving(completed, total));
    }
    _refresh(() => _chrome.archiveFinished = failed > 0
        ? ArchiveFinishedStatus.failed(failed, failed)
        : const ArchiveFinishedStatus.idle());
  }

  /// panels/agent-facts-line.ts buildAgentFactsLine: model · thinking · mode ·
  /// context usage, each resolved as the composer's controls resolve it.
  String? _agentFactsLine(SubagentRowData row) {
    final agent = row.agent;
    final runtime = jsonMap(agent['runtimeInfo']);
    final runtimeModel = stringValue(runtime['model']).trim();
    final configuredModel = stringValue(agent['model']).trim();
    if (runtimeModel.isEmpty && configuredModel.isEmpty) return null;
    final models = maps(providers
        .where((entry) => entry['provider'] == agent['provider'])
        .firstOrNull?['models']);
    final model = models.where((m) => m['id'] == runtimeModel).firstOrNull ??
        models.where((m) => m['id'] == configuredModel).firstOrNull;
    final parts = <String>[
      model == null
          ? (runtimeModel.isNotEmpty ? runtimeModel : configuredModel)
          : AgentControlsModel.modelLabel(model),
    ];
    final thinkingId = stringValue(
      runtime['thinkingOptionId'],
      stringValue(agent['thinkingOptionId']),
    );
    final options = maps(model?['thinkingOptions']);
    final thinking = options.where((o) => o['id'] == thinkingId).firstOrNull ??
        (thinkingId.isEmpty ? options.where((o) => o['isDefault'] == true).firstOrNull : null);
    if (thinking != null) {
      parts.add(formatThinkingOptionLabel(context, thinking));
    } else if (thinkingId.isNotEmpty) {
      parts.add(thinkingId);
    }
    final modes = maps(agent['availableModes']);
    final mode = modes.where((m) => m['id'] == agent['currentModeId']).firstOrNull ??
        modes.firstOrNull;
    if (mode != null) parts.add(formatAgentModeLabel(mode));
    final usage = jsonMap(agent['lastUsage']);
    final used = usage['contextWindowUsedTokens'], max = usage['contextWindowMaxTokens'];
    if (used is num && max is num && used >= 0 && max > 0) {
      String tokens(num value) => value >= 1000000
          ? '${(value / 1000000).round()}m'
          : value >= 1000
          ? '${(value / 1000).round()}k'
          : '${value.round()}';
      final percentage = (used / max * 100).clamp(0, 100).round();
      parts.add('${tokens(used)} / ${tokens(max)} tokens ($percentage%)');
    }
    return parts.join(' · ');
  }

  /// AgentTracks over the stream's bottom edge (agent-panel.tsx streamContent).
  Widget _agentTracks(List<SubagentRowData> rows, List<JsonMap> tasks, JsonMap diffStat) {
    final store = PaseoScope.of(context);
    final canDetach =
        jsonMap(client.serverInfo['features'])['agentDetach'] == true;
    final svg = {
      for (final entry in providers)
        stringValue(entry['provider']): _optionalString(entry['iconSvg']),
    };
    return AgentTracks(
      serverId: widget.target.serverId,
      tasks: tasks,
      rows: rows,
      archiveStatus: _chrome.archiveFinished,
      onArchiveFinished: () => unawaited(_archiveFinishedSubagents()),
      onOpenRow: _openSubagent,
      onArchiveRow: (row) => unawaited(_archiveSubagent(row)),
      onDetachRow: canDetach ? (row) => unawaited(_detachSubagent(row)) : null,
      additions: diffStat.integer('additions'),
      deletions: diffStat.integer('deletions'),
      onOpenChanges: () => store.open(PanelTarget(
        kind: 'changes_tree',
        serverId: widget.target.serverId,
        workspaceId: widget.target.workspaceId,
        cwd: widget.target.cwd,
      )),
      factsFor: _agentFactsLine,
      hostLabelFor: (serverId) =>
          store.hosts.where((host) => host.serverId == serverId).firstOrNull?.label ?? serverId,
      providerSvgFor: (row) => svg[row.provider],
      opensInSheet: _subagentOpensInSheet,
      canArchiveFinished: !isSubagent,
    );
  }

  /// agent-panel.tsx SheetAgentFacts: the composer controls' facts as the
  /// pane subtitle, or nothing until the agent has a model.
  Widget? _sheetFacts(JsonMap agent) {
    final text = _agentFactsLine(SubagentRowData(
      kind: 'paseo',
      id: stringValue(agent['id']),
      serverId: widget.target.serverId,
      provider: stringValue(agent['provider']),
      status: _optionalString(agent['status']),
      createdAt: DateTime(0),
      agent: agent,
    ));
    return text == null ? null : PaneSubtitle(text: text);
  }

  /// The agent's latest task list (session agentTasks).
  List<JsonMap> _latestTasks() {
    final todo = controller?.timeline.reversed
        .map((entry) => jsonMap(entry['item']))
        .where((item) => item['type'] == 'todo')
        .firstOrNull;
    return maps(todo?['items']);
  }

  /// composer/workspace-diff-stat.ts: the workspace's diffStat.
  JsonMap _workspaceDiffStat() {
    final host = PaseoScope.of(context).hosts
        .where((candidate) => candidate.serverId == widget.target.serverId)
        .firstOrNull;
    return jsonMap(host?.workspaces
        .where((workspace) => workspace['id'] == widget.target.workspaceId)
        .firstOrNull?['diffStat']);
  }

  // ── states and toasts ───────────────────────────────────────────────────

  /// agent-panel.tsx renderChatAgentNonReadyView / AgentSessionUnavailableState.
  Widget? _nonReadyView() {
    final c = controller;
    if (c == null || isDraft || isSubagent) return null;
    // An optimistic create handoff renders the ready view before any snapshot.
    if (c.agent.isNotEmpty || _chrome.hydrated || c.createHandoff) return null;
    if (!_connected) {
      final host = PaseoScope.of(context).hosts
          .where((candidate) => candidate.serverId == widget.target.serverId)
          .firstOrNull;
      final hostError = host?.error;
      return AgentSessionUnavailableState(
        serverLabel: host?.label ?? widget.target.serverId,
        connecting: client.status == ConnectionStatus.connecting,
        lastError: hostError == null ? null : errorMessage(hostError),
      );
    }
    // use-agent-screen-state-machine.ts: a failed first catch-up is the error
    // view; its retry (status retrying) shows the loading view until it settles.
    final failure = c.error;
    if (failure != null && c.syncStatus == TimelineSyncStatus.error) {
      final message = errorMessage(failure);
      if (RegExp('agent not found|not found', caseSensitive: false).hasMatch(message)) {
        return const AgentNotFoundView();
      }
      return AgentLoadErrorView(message: message, onRetry: c.retrySync);
    }
    return const AgentLoadingView();
  }

  /// The panel's ToastViewport: an action's toast wins over the sync status
  /// (agent-panel.tsx `toast ?? syncToast`).
  PanelToastData? _panelToast() {
    final failure = error;
    if (failure != null) {
      return PanelToastData(
        id: 'error:${identityHashCode(failure)}',
        message: errorMessage(failure),
        variant: PanelToastVariant.error,
        duration: const Duration(milliseconds: 3200),
      );
    }
    final persistence = drafts.persistenceError;
    if (persistence != null) {
      final id = 'persist:${errorMessage(persistence)}';
      if (!_chrome.dismissedToasts.contains(id)) {
        return PanelToastData(
          id: id,
          message: errorMessage(persistence),
          variant: PanelToastVariant.error,
          duration: const Duration(milliseconds: 3200),
        );
      }
    }
    final c = controller;
    if (c?.notice != null) {
      return PanelToastData(id: 'notice:${c!.notice}', message: c.notice!);
    }
    final olderFailure = 'older-history:${_chrome.olderFailures}';
    if (_chrome.olderFailures > 0 && !_chrome.dismissedToasts.contains(olderFailure)) {
      return PanelToastData(
        id: olderFailure,
        message: tr(context, 'loadOlderHistory.failed'),
        duration: const Duration(milliseconds: 2200),
      );
    }
    if (c == null || isDraft || controller?.agent['archivedAt'] != null) return null;
    final spinner = ChromeSpinner(size: 18, color: Theme.of(context).colorScheme.onSurface);
    if (!_connected) {
      return PanelToastData(
        id: 'reconnecting',
        message: tr(context, 'agentPanel.states.reconnecting'),
        icon: spinner,
        duration: null,
      );
    }
    // timeline/sync-status.tsx: "Updating messages" while a chat that already
    // showed history catches up (resolveCatchingUpUi "status").
    if (c.syncStatus == TimelineSyncStatus.pending && _chrome.hydrated && !c.createHandoff) {
      return PanelToastData(
        id: 'updating',
        message: tr(context, 'agentPanel.states.updating'),
        icon: spinner,
        duration: null,
      );
    }
    return null;
  }

  void _dismissPanelToast(Object? id) {
    if (id is! String) return;
    _refresh(() {
      if (id.startsWith('error:')) {
        error = null;
      } else if (id.startsWith('notice:')) {
        controller?.notice = null;
      } else {
        _chrome.dismissedToasts.add(id);
      }
    });
  }

  /// archived-agent-callout.tsx handleUnarchive: client.refreshAgent.
  Future<void> _unarchive() async {
    final c = controller!;
    await c.action('refresh_agent_request', {}, responseType: 'status');
    await c.reload();
  }
}
