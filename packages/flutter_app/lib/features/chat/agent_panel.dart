import 'dart:async';
import 'dart:convert';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart';
import 'package:collection/collection.dart' show DeepCollectionEquality;
import 'package:flutter/foundation.dart' show defaultTargetPlatform, listEquals;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show RenderSliverMultiBoxAdaptor, SliverMultiBoxAdaptorParentData;
import 'package:flutter/services.dart';
import 'package:uuid/uuid.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/client.dart';
import '../../core/commands.dart';
import '../../core/json.dart';
import '../../core/scope.dart';
import '../../core/target.dart';
import '../../ui/command_center.dart' show formatShortcutStep;
import '../../ui/common.dart';
import '../../ui/indicator_clock.dart';
import '../../ui/localization.dart';
import '../../ui/panel_activity.dart';
import '../../ui/shortcuts.dart' show ShortcutBinding, ShortcutCatalog;
import '../../ui/theme.dart';
import '../audio/audio_service.dart';
import '../files/remote_file_service.dart';
import '../settings/provider_catalog.dart' show ProviderSettingsSnapshot, showProviderSettingsSheet;
import '../settings/profile_settings.dart' show ProfileEditor, ProfileSettings;
import '../settings/settings_forms.dart';
import '../settings/settings_screen.dart' show SettingsScreen;
import 'agent_attention.dart';
import 'agent_controls.dart';
import 'attachment_pill.dart';
import 'chat_controller.dart';
import 'chat_markdown.dart' show ChatFindTarget;
import 'chat_drafts.dart';
import 'chat_delivery.dart';
import 'composer/autocomplete.dart';
import 'composer/composer_tooltip.dart';
import 'composer/context_window_meter.dart';
import 'composer/forge_picker.dart';
import 'composer/forge_icons.dart';
import 'composer/queue_track.dart';
import 'composer/send_button.dart';
import 'composer/agent_form.dart';
import 'composer/attachment_files.dart';
import 'composer/forge_auto_attach.dart';
import 'permission_card.dart';
import 'stream/message_actions.dart';
import 'stream/stream_layout.dart';
import 'timeline_item.dart';
import 'forge_attachments.dart';
import 'image_clipboard.dart';
import 'creation_recovery.dart';
import 'provider_icon.dart';
import 'chrome/chat_find.dart';
import 'chrome/chat_outline.dart';
import 'chrome/chrome_controls.dart';
import 'chrome/composer_tracks.dart';
import 'chrome/stream_states.dart';

part 'chrome/stream_chrome.dart';

/// One entry of the virtualized stream list: its stable identity and the
/// builder that runs only when the list lays the entry out.
typedef _StreamItem = ({Key key, Widget Function() build});

/// A conversation's own actions (find, outline, tool details, history sync,
/// rename, fork, reload, usage, detach, archive, delete). Its workspace tab
/// menu offers them, so the pane carries no second title row of its own.
class ConversationMenus {
  ConversationMenus._();
  static final _menus = <String, ({
    Object owner,
    List<(String, String)> Function() actions,
    void Function(String) run,
  })>{};

  static void register(
    String tabKey,
    Object owner,
    List<(String, String)> Function() actions,
    void Function(String) run,
  ) => _menus[tabKey] = (owner: owner, actions: actions, run: run);

  static void unregister(String tabKey, Object owner) {
    if (identical(_menus[tabKey]?.owner, owner)) _menus.remove(tabKey);
  }

  /// The open conversation's (value, label) actions and their runner.
  static ({List<(String, String)> Function() actions, void Function(String) run})? of(String tabKey) {
    final menu = _menus[tabKey];
    return menu == null ? null : (actions: menu.actions, run: menu.run);
  }
}

class AgentPanel extends StatefulWidget {
  const AgentPanel({super.key, required this.target});
  final PanelTarget target;
  @override
  State<AgentPanel> createState() => _AgentPanelState();
}

class _AgentPanelState extends State<AgentPanel> {
  final editor = TextEditingController();
  final focus = FocusNode(debugLabel: 'message-input');
  final fullscreenFocus = FocusNode(debugLabel: 'message-input');
  late final scroll = _StreamScrollController(_readingHoldShift);
  final _chrome = _StreamChromeState();

  /// setState for the stream chrome extension (chrome/stream_chrome.dart).
  void _refresh([VoidCallback? change]) {
    if (mounted) setState(change ?? () {});
  }
  final drafts = ChatDrafts.instance;
  final Map<int, GlobalKey> rowKeys = {};

  /// Each row's widget with the inputs it was built from: a rebuild that
  /// leaves a row's inputs alone (a status update, a chunk streaming into
  /// another row, the composer) hands back the identical widget, so its
  /// subtree is skipped.
  final Map<int, ({List<Object?> deps, JsonMap capabilities, Widget widget})> _rowWidgets = {};

  /// The stream's row models and list items with what they derive from.
  ({
    List<JsonMap> timeline,
    Object? pending,
    Object? queue,
    bool overview,
    String provider,
    bool running,
    List<StreamRow> models,
    List<_StreamItem> items,
  })? _streamCache;
  PaseoClient? _client;
  ChatController? controller;
  VoidCallback? _removeCloseGuard, _removeCommands;
  bool initialized = false,
      sending = false,
      uploading = false,
      expandedTools = false;
  bool _syncingEditor = false, _atBottom = true, _fullscreenOpen = false;

  /// bottom-anchor-controller.ts mode: "sticky-bottom" follows new rows to
  /// offset 0; "detached" (the reader scrolled away) holds the reading
  /// position instead. [_atBottom] is only the 32px geometry that shows the
  /// scroll-to-bottom button.
  bool _sticky = true;

  /// beginUserScroll/endUserScroll: while a finger drives the list (drag
  /// and its fling) nothing scrolls it programmatically.
  bool _userScrolling = false;

  /// Set while a keystroke publishes its own text to [drafts]: every panel's
  /// draft listener skips that rebuild, since only the editor's own
  /// listeners (send control, line count) depend on the text.
  static bool _ownTextEdit = false;

  /// The last full build, reused while this retained panel is hidden so
  /// offstage tabs do no work for store, draft or stream changes.
  Widget? _lastBuild;

  /// hooks/use-agent-attention-clear.ts for this panel's agent.
  AgentAttentionClear? _attention;
  bool _attentionFocused = false;
  Object? error;
  List<JsonMap> profiles = [], commands = [], providers = [],
      draftFeatures = [];
  late final autocomplete = ComposerAutocomplete(loadFiles: _directorySuggestions);
  StreamSubscription<JsonMap>? _providerUpdates;

  /// The daemon config and this cwd's provider catalog, published to the
  /// open model sheet and the provider settings sheet as they change.
  JsonMap _daemonConfig = {};
  final _providerState = ValueNotifier<ProviderSettingsSnapshot>((
    config: const <String, dynamic>{},
    providers: const <JsonMap>[],
  ));

  void _setProviders(List<JsonMap> next) {
    setState(() => providers = next);
    _providerState.value = (config: _daemonConfig, providers: next);
  }

  /// use-providers-snapshot.ts pushEvent "providers_snapshot_update": the
  /// catalog loads asynchronously, so pushes for this cwd's scope replace
  /// [providers] (or trigger a refetch when they only announce a hash).
  void _providersPushed(JsonMap event) {
    if (!mounted || event['type'] != 'providers_snapshot_update') return;
    final payload = jsonMap(event['payload']);
    String scope(Object? cwd) => stringValue(cwd).trim().replaceFirst(RegExp(r'/+$'), '');
    if (scope(payload['cwd']) != scope(widget.target.cwd)) return;
    if (payload.containsKey('entries') || payload.containsKey('compactSnapshot')) {
      _setProviders(providerEntries(payload));
      return;
    }
    unawaited(_reloadProviders());
  }

  Future<void> _reloadProviders() async {
    try {
      final snapshot = requireSuccess(await client.request(
        'get_providers_snapshot_request',
        {'cwd': widget.target.cwd},
        responseType: 'get_providers_snapshot_response',
      ));
      if (mounted) _setProviders(providerEntries(snapshot));
    } catch (e) {
      _showFailure(e);
    }
  }

  /// use-providers-snapshot.ts refetchIfStale on selector open: a provider
  /// that is missing or still loading refetches at once.
  void _refetchIfLoading() {
    final provider = _config()['provider'];
    final entry = providers.where((p) => p['provider'] == provider).firstOrNull;
    if (entry == null || entry['status'] == 'loading') unawaited(_reloadProviders());
  }

  /// model-browser.tsx ProviderSettingsAction → the provider settings sheet.
  void _openProviderSettings(BuildContext context, String provider) {
    final manage = !client.serverInfo.containsKey('permissions') ||
        jsonList(client.serverInfo['permissions']).contains('daemon.manage');
    unawaited(showProviderSettingsSheet(
      context,
      client: client,
      provider: provider,
      snapshot: _providerState,
      patch: (value) async {
        if (!manage) {
          throw StateError('This connection does not have daemon.manage permission.');
        }
        final result = await settingsRequest(client, 'set_daemon_config_request', {
          'config': value,
        }, responseType: 'set_daemon_config_response');
        if (!mounted) return;
        _daemonConfig = jsonMap(result['config']);
        _providerState.value = (config: _daemonConfig, providers: providers);
      },
      refresh: (ids) async {
        await settingsRequest(client, 'refresh_providers_snapshot_request', {
          'providers': ids,
          if (widget.target.cwd.trim().isNotEmpty) 'cwd': widget.target.cwd,
        }, responseType: 'refresh_providers_snapshot_response');
        await _reloadProviders();
      },
    ));
  }

  /// index.tsx useEditAgentProfilesNavigation: settings › host › agents, when
  /// the host supports agent profiles.
  VoidCallback? get _editProfiles =>
      jsonMap(client.serverInfo['features'])['agentProfiles'] == true
          ? () => unawaited(Navigator.of(context).push(MaterialPageRoute<void>(
                builder: (_) => SettingsScreen(
                  initialSection: 'host/agents',
                  initialServerId: widget.target.serverId,
                ),
              )))
          : null;

  /// agent-profiles/agent-profile-editor.tsx openCreateFromModel / openEdit:
  /// the profile editor sheet over the composer, seeded from a model row
  /// ([seed]: provider, model, name) or editing [profile]; saving merges it
  /// into the host's current `agentProfiles`.
  void _openProfileEditor({JsonMap? profile, JsonMap? seed}) {
    unawaited(showAdaptiveSheet<JsonMap>(
      context: context,
      builder: (_) => ProfileEditor(
        client: client,
        profile: profile,
        seed: seed,
        onSave: (next) async {
          final fresh = await settingsRequest(
            client,
            'get_daemon_config_request',
            {},
            responseType: 'get_daemon_config_response',
          );
          final saved = maps(jsonMap(fresh['config'])['agentProfiles'])
              .map((value) => <String, dynamic>{...value})
              .toList();
          final index = saved.indexWhere((p) => p['id'] == next['id']);
          if (profile != null && index < 0) {
            throw StateError(
              'This profile was removed on the host while the editor was open. Close the editor and create a new profile instead.',
            );
          }
          if (next['cliDefault'] == true) {
            for (final p in saved) {
              p['cliDefault'] = false;
            }
          }
          if (index < 0) {
            saved.add(next);
          } else {
            saved[index] = next;
          }
          final result = await settingsRequest(client, 'set_daemon_config_request', {
            'config': {'agentProfiles': saved},
          }, responseType: 'set_daemon_config_response');
          if (!mounted) return;
          _daemonConfig = jsonMap(result['config']);
          setState(() => profiles = maps(_daemonConfig['agentProfiles']));
          _providerState.value = (config: _daemonConfig, providers: providers);
        },
      ),
    ));
  }

  bool get _profilesSupported => ProfileSettings.supportsAgentProfiles(client);

  /// composer/index.tsx sendError: the last failed send, red text above the input.
  String? sendError;
  int commandGeneration = 0;

  /// input/state.ts resolveActiveSendBehavior over the live app setting:
  /// queueing behind a pending permission would strand the message, so a
  /// queue preference interrupts while one is open.
  String get behavior {
    final saved = stringValue(PaseoScope.of(context).preferences['sendBehavior'], 'steer');
    final value = const ['interrupt', 'steer', 'queue'].contains(saved) ? saved : 'steer';
    if (value == 'queue' && (controller?.permissions.isNotEmpty ?? false)) return 'interrupt';
    return value;
  }
  int uploadSent = 0, uploadTotal = 0;
  String? uploadName;
  String? highlighted;
  bool get isDraft => widget.target.kind == 'draft';
  bool get isSubagent => widget.target.kind == 'provider_subagent';

  /// pane-context host "sheet": the workspace sheet is for looking, so the
  /// agent shows without its composer (Open as tab brings it back).
  bool get isSheet => widget.target.data['paneHost'] == 'sheet';
  String get draftKey => ChatDrafts.key(
    widget.target.serverId,
    widget.target.workspaceId,
    widget.target.id ?? 'draft',
  );
  JsonMap get draft => drafts.draft(draftKey);
  PaseoClient get client => _client!;

  void _installCloseGuard() {
    _removeCloseGuard?.call();
    _removeCloseGuard = PaseoScope.of(context)
        .registerCloseGuard(widget.target.key, () async {
          if (_fullscreenOpen) {
            if (mounted) {
              showError(
                context,
                StateError(
                  'Close the expanded editor before closing this session.',
                ),
              );
            }
            return false;
          }
          if (!sending && !uploading) {
            try {
              await drafts.flush();
              return true;
            } catch (e) {
              if (mounted) showError(context, e);
              return false;
            }
          }
          if (mounted) {
            showError(
              context,
              StateError(
                'Wait for the current send or upload to finish before closing this session.',
              ),
            );
          }
          return false;
        });
    _removeCommands?.call();
    _removeCommands = PaseoCommands.instance.register(widget.target.key, {
      'conversation.tools': (_) => _conversationAction('tools'),
      if (!isDraft) 'conversation.sync': (_) => _conversationAction('sync'),
      if (!isDraft && !isSubagent)
        for (final action in ['find', 'outline', 'rename', 'fork', 'refresh', 'usage', 'detach', 'archive', 'delete'])
          'conversation.$action': (_) => _conversationAction(action),
      if (!isSubagent) ...{
        'message-input.focus': (_) => focus.requestFocus(),
        'message-input.send': (_) => _submit(),
        'message-input.mode-cycle': (_) => _cycleMode(),
        if (!isDraft) 'agent.interrupt': (_) => _cancelAgent(),
      },
    });
  }

  Future<void> _cycleMode() async {
    try {
      var modes = maps(controller?.agent['availableModes']);
      final config = _config();
      if (isDraft) {
        final result = requireSuccess(
          await client.request('get_providers_snapshot_request', {
            'cwd': widget.target.cwd,
          }, responseType: 'get_providers_snapshot_response'),
        );
        final entry = providerEntries(result)
            .where((p) => p['provider'] == config['provider'])
            .firstOrNull;
        modes = maps(entry?['modes']);
      }
      if (modes.isEmpty) {
        throw StateError('This provider has no available modes.');
      }
      final index = modes.indexWhere((m) => m['id'] == config['modeId']);
      await _saveConfig({
        ...config,
        'modeId': modes[(index + 1) % modes.length]['id'],
      });
    } catch (e) {
      _showFailure(e);
    }
  }

  @override
  void initState() {
    super.initState();
    drafts.addListener(_draftChanged);
    editor.addListener(_editorChanged);
    focus.onKeyEvent = _composerKey;
    focus.addListener(_inputFocusChanged);
    ConversationMenus.register(widget.target.key, this, _conversationActions, _conversationAction);
    fullscreenFocus.addListener(_inputFocusChanged);
    scroll.addListener(_onStreamScroll);
    _appLifecycle = AppLifecycleListener(onStateChange: _appLifecycleChanged);
  }

  /// viewed-timeline-sync.ts setActive (app-visibility.ts getIsAppVisible):
  /// returning to the foreground catches the conversation up.
  AppLifecycleListener? _appLifecycle;
  void _appLifecycleChanged(AppLifecycleState state) {
    controller?.setAppVisible(
      state != AppLifecycleState.hidden &&
          state != AppLifecycleState.paused &&
          state != AppLifecycleState.detached,
    );
  }

  /// composer/index.tsx handleFocusChange → onAttentionInputFocus.
  void _inputFocusChanged() {
    if (focus.hasFocus || fullscreenFocus.hasFocus) _attention?.clearOnInputFocus();
  }

  /// The facts use-agent-attention-clear.ts reads: the agent directory's
  /// attention (the fetched agent until the directory has it), the host
  /// connection, and whether this panel is the focused pane's shown tab
  /// while the app is in the foreground.
  AgentAttentionState _attentionState() {
    final activity = PanelActivity.of(context);
    final id = isDraft || isSubagent ? null : widget.target.id;
    final row = id == null
        ? null
        : PaseoScope.of(context)
              .hostFor(widget.target.serverId)
              ?.agents
              .where((agent) => agent['id'] == id)
              .firstOrNull;
    final agent = row ?? controller?.agent ?? const <String, dynamic>{};
    return (
      agentId: id,
      connected: _client?.status == ConnectionStatus.connected,
      requiresAttention: agent['requiresAttention'] == true,
      attentionReason: agent['attentionReason'] is String
          ? agent['attentionReason'] as String
          : null,
      screenFocused: activity.focused,
      appVisible: activity.appActive,
    );
  }

  /// Runs the hook's effects for the current facts, then agent-panel.tsx's
  /// blur effect (leaving a focused panel clears too).
  void _syncAttention() {
    if (!mounted || _client == null) return;
    final next = _attentionState();
    final attention = _attention;
    if (attention == null) {
      _attention = AgentAttentionClear(next, _clearAttention);
    } else {
      attention.update(next);
    }
    if (_attentionFocused && !next.screenFocused) _attention!.clearOnAgentBlur();
    _attentionFocused = next.screenFocused;
  }

  /// daemon-client clearAgentAttention; failures are ignored as the original.
  void _clearAttention(String agentId) {
    final target = _client;
    if (target == null) return;
    unawaited(target
        .request('clear_agent_attention', {'agentId': agentId},
            responseType: 'clear_agent_attention_response')
        .then<void>((_) {}, onError: (Object _) {}));
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final next = PaseoScope.of(context).clientFor(widget.target.serverId);
    if (_client != next) {
      controller?.removeListener(_changed);
      controller?.dispose();
      _client = next;
      _providerUpdates?.cancel();
      _providerUpdates = client.events.listen(_providersPushed);
      ChatDelivery.attach(client, widget.target.serverId);
      AudioService.forClient(client);
      autocomplete
        ..isDraft = isDraft
        ..canLoadFiles = widget.target.cwd.trim().isNotEmpty;
      expandedTools = false;
      _rowWidgets.clear();
      _streamCache = null;
      if (!isDraft) {
        controller = ChatController(client, widget.target)
          ..addListener(_changed);
        unawaited(controller!.start());
      }
      unawaited(_initialize());
    }
    _installCloseGuard();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _takeAttachments();
    });
    _syncAttention();
    // chat-find/index.web.tsx: an inactive pane closes its find.
    if (_chrome.findOpen && !PanelActivity.of(context).visible) {
      _chrome.find?.close();
      _chrome.findOpen = false;
      highlighted = null;
    }
  }

  @override
  void didUpdateWidget(AgentPanel old) {
    super.didUpdateWidget(old);
    if (old.target.key != widget.target.key) {
      ConversationMenus.unregister(old.target.key, this);
      ConversationMenus.register(widget.target.key, this, _conversationActions, _conversationAction);
      _installCloseGuard();
      if (_attentionFocused) _attention?.clearOnAgentBlur();
      _attention = null;
      _attentionFocused = false;
      controller?.removeListener(_changed);
      controller?.dispose();
      controller = null;
      _rowWidgets.clear();
      _streamCache = null;
      if (!isDraft) {
        controller = ChatController(client, widget.target)
          ..addListener(_changed);
        unawaited(controller!.start());
      }
      initialized = false;
      unawaited(_initialize());
      _syncAttention();
    } else if (old.target.data['attachmentIntentId'] !=
        widget.target.data['attachmentIntentId']) {
      _targetAttachments();
    }
  }

  Future<void> _initialize() async {
    await drafts.initialize();
    if (!mounted) return;
    final hydrationKey = draftKey;
    try {
      final server = widget.target.serverId;
      final id = widget.target.id;
      await drafts.hydrateLegacy(
        hydrationKey,
        jsonMap(PaseoScope.of(context).preferences['legacyDrafts']),
        sourceKeys: isDraft
            ? [
                if (id != null) 'draft:$server:$id',
                if (id == null || id == 'draft') ...['new-workspace', 'createModalDraft'],
              ]
            : ['agent:$server:$id'],
        legacyForge: jsonMap(client.serverInfo['features'])['forgeAttachments'] == false,
      );
    } catch (e) {
      _showFailure(e);
    }
    if (!mounted || hydrationKey != draftKey) return;
    if (jsonMap(draft['config']).isEmpty &&
        widget.target.data['config'] is Map) {
      drafts.update(draftKey, {
        'config': jsonMap(widget.target.data['config']),
      });
    }
    if (draft['text'] == '' && widget.target.data['initialPrompt'] is String) {
      drafts.update(draftKey, {'text': widget.target.data['initialPrompt']});
    }
    _targetAttachments();
    _takeAttachments();
    initialized = true;
    _draftChanged();
    if (widget.target.data['submissionError'] != null) {
      _showFailure(
        StateError(stringValue(widget.target.data['submissionError'])),
      );
    }
    try {
      final config = requireSuccess(
        await client.request(
          'get_daemon_config_request',
          {},
          responseType: 'get_daemon_config_response',
        ),
      );
      if (mounted) {
        _daemonConfig = jsonMap(config['config']);
        setState(() => profiles = maps(_daemonConfig['agentProfiles']));
        _providerState.value = (config: _daemonConfig, providers: providers);
      }
    } catch (e) {
      _showFailure(e);
    }
    await _reloadProviders();
    _resolveDraftConfig();
    if (isDraft) await _refreshDraftFeatures();
    await _loadCommands();
    unawaited(_loadRemoteUrl());
  }

  /// use-agent-form-state.ts: a draft's agent selection resolves once the
  /// provider catalog is known, from its initial values (a /clear or fork
  /// setup) and the remembered create-agent preferences.
  void _resolveDraftConfig() {
    if (!mounted || !isDraft || draft['configResolved'] == true) return;
    final resolved = resolveInitialAgentConfig(
      initial: jsonMap(draft['config']),
      preferences: _formPreferences,
      providers: providers,
    );
    drafts.update(draftKey, {
      'config': {...resolved, 'cwd': widget.target.cwd},
      'configResolved': true,
    });
  }

  JsonMap get _formPreferences =>
      parseFormPreferences(PaseoScope.of(context).preferences[formPreferencesKey]);

  void _rememberPreferences(JsonMap next) {
    final store = PaseoScope.of(context);
    store.preferences[formPreferencesKey] = next;
    unawaited(store.savePreferences());
  }

  /// index.tsx useCheckoutStatusQuery: the checkout's remote, which scopes the
  /// forge URLs the composer auto-attaches.
  String? _remoteUrl;
  late final _forgeAutoAttach = ComposerForgeAutoAttach(
    search: (cwd, query) => searchForgeItems(client, cwd, query),
    attach: _autoAttachForge,
  );

  Future<void> _loadRemoteUrl() async {
    final cwd = widget.target.cwd.trim();
    if (isSubagent || cwd.isEmpty || client.status != ConnectionStatus.connected) return;
    try {
      final status = await client.request('checkout_status_request', {
        'cwd': cwd,
      }, responseType: 'checkout_status_response');
      if (!mounted) return;
      final url = stringValue(status['remoteUrl']);
      _remoteUrl = url.isEmpty ? null : url;
      _syncForgeAutoAttach();
    } catch (_) {
      // Without a remote nothing auto-attaches (extractForgeRefs).
    }
  }

  void _syncForgeAutoAttach() {
    if (isSubagent || _client == null) return;
    _forgeAutoAttach.update((
      text: editor.text,
      remoteUrl: _remoteUrl,
      attachments: maps(draft['attachments']),
      connected: client.status == ConnectionStatus.connected,
      serverId: widget.target.serverId,
      cwd: widget.target.cwd,
    ));
  }

  /// forge-auto-attach.ts drainOutcomes → toggleForgeAttachment.
  void _autoAttachForge(JsonMap item) {
    if (!mounted) return;
    final current = maps(draft['attachments']);
    if (current.any((attachment) => forgeAttachmentKey(attachment) == forgeItemKey(item))) return;
    drafts.update(draftKey, {
      'attachments': [...current, forgeAttachment(item, legacy: usesLegacyForge(client))],
    });
  }

  void _targetAttachments() {
    final attachments = maps(widget.target.data['attachments']);
    if (attachments.isNotEmpty) {
      drafts.addDraftAttachments(
        draftKey,
        attachments,
        intentId: stringValue(
          widget.target.data['attachmentIntentId'],
          '${widget.target.key}:${jsonEncode(attachments)}',
        ),
      );
    }
  }

  void _takeAttachments() {
    if (isSubagent ||
        PaseoScope.of(context).selected?.key != widget.target.key) {
      return;
    }
    drafts.takeAttachments(
      widget.target.serverId,
      widget.target.workspaceId,
      draftKey,
    );
  }

  void _draftChanged() {
    if (!mounted) return;
    if (initialized) _syncForgeAutoAttach();
    final text = stringValue(draft['text']);
    if (editor.text != text) {
      _syncingEditor = true;
      editor.value = TextEditingValue(
        text: text,
        selection: TextSelection.collapsed(offset: text.length),
      );
      _syncingEditor = false;
    } else if (_ownTextEdit) {
      return;
    }
    setState(() {});
  }

  void _editorChanged() {
    autocomplete.update(editor.text, editor.selection.baseOffset);
    if (_syncingEditor) return;
    _ownTextEdit = true;
    try {
      drafts.update(draftKey, {'text': editor.text});
    } finally {
      _ownTextEdit = false;
    }
  }

  void _changed() {
    if (!mounted) return;
    final pending = jsonMap(draft['pending']);
    if (pending.isNotEmpty &&
        controller!.timeline.any(
          (row) =>
              jsonMap(row['item'])['clientMessageId'] == pending['messageId'],
        )) {
      drafts.acknowledge(draftKey, pending);
    }
    _holdReadingPosition();
    _streamChanged();
    _syncAttention();
    setState(() {});
    if (_sticky) _bottom();
  }

  /// strategy-native.tsx sticky bottom: the inverted list's newest edge is
  /// offset 0. bottom-anchor-controller.ts evaluate: no attempt while the
  /// reader drives the list or once it detached.
  void _bottom() => WidgetsBinding.instance.addPostFrameCallback((_) {
    if (mounted &&
        _sticky &&
        !_userScrolling &&
        scroll.hasClients &&
        scroll.position.pixels != scroll.position.minScrollExtent) {
      scroll.jumpTo(scroll.position.minScrollExtent);
    }
  });
  void _showFailure(Object e) {
    if (mounted) setState(() => error = e);
  }

  JsonMap _config() {
    if (isDraft) return {...jsonMap(draft['config']), 'cwd': widget.target.cwd};
    final a = controller?.agent ?? {};
    return {
      'provider': a['provider'],
      'cwd': widget.target.cwd,
      if (a['model'] != null) 'model': a['model'],
      if (a['currentModeId'] != null) 'modeId': a['currentModeId'],
      if (a['thinkingOptionId'] != null)
        'thinkingOptionId': a['thinkingOptionId'],
      'featureValues': {
        for (final f in maps(a['features'])) stringValue(f['id']): f['value'],
      },
    };
  }

  /// Draft: use-agent-form-state.ts — choosing a provider or model resolves
  /// its mode and thinking option (resolve-agent-form.ts) and every user
  /// choice is remembered for the next new agent. Running agent:
  /// agent-controls/index.tsx and mode-control.tsx — each changed field is
  /// its own RPC (model, mode, thinking, feature), remembered per provider;
  /// a failure toasts.
  Future<void> _saveConfig(JsonMap value) async {
    final preferences = _formPreferences;
    final previous = _config();
    if (isDraft) {
      var next = Map<String, dynamic>.of(value);
      final provider = stringValue(value['provider']);
      if (provider.isNotEmpty &&
          (provider != previous['provider'] || value['model'] != previous['model'])) {
        next = {
          ...applyUserModelChoice(
            previous: previous,
            provider: provider,
            modelId: stringValue(value['model']),
            preferences: preferences,
            providers: providers,
          ),
          'cwd': widget.target.cwd,
        };
      }
      drafts.update(draftKey, {'config': next, 'configResolved': true});
      _rememberPreferences(rememberUserSelection(preferences, previous, next));
      unawaited(_refreshDraftFeatures());
      await _loadCommands();
      return;
    }
    final provider = stringValue(controller?.agent['provider']);
    if (value['provider'] != provider) {
      _showFailure(
        StateError(
          'A running session cannot change provider. Fork into a new session instead.',
        ),
      );
      return;
    }
    var remembered = preferences;
    final requests = <(String, JsonMap)>[];
    final model = stringValue(value['model']);
    if (value['model'] != previous['model']) {
      if (model.isNotEmpty) remembered = mergeProviderPreferences(remembered, provider, model: model);
      requests.add(('set_agent_model', {'modelId': value['model']}));
    }
    if (value['modeId'] != previous['modeId']) {
      final mode = stringValue(value['modeId']);
      remembered = mergeProviderPreferences(
        remembered,
        provider,
        mode: mode.isEmpty ? null : mode,
      );
      requests.add(('set_agent_mode', {'modeId': mode}));
    }
    if (value['thinkingOptionId'] != previous['thinkingOptionId']) {
      final thinking = stringValue(value['thinkingOptionId']);
      final activeModel = stringValue(
        value['model'],
        stringValue(jsonMap(controller?.agent['runtimeInfo'])['model']),
      );
      if (activeModel.isNotEmpty) {
        remembered = mergeProviderPreferences(
          remembered,
          provider,
          model: activeModel,
          thinkingByModel: {activeModel: thinking},
        );
      }
      requests.add(('set_agent_thinking', {'thinkingOptionId': value['thinkingOptionId']}));
    }
    final before = jsonMap(previous['featureValues']);
    for (final entry in jsonMap(value['featureValues']).entries) {
      if (jsonEncode(before[entry.key]) == jsonEncode(entry.value)) continue;
      remembered = mergeProviderPreferences(
        remembered,
        provider,
        featureValues: {entry.key: entry.value},
      );
      requests.add(('set_agent_feature', {'featureId': entry.key, 'value': entry.value}));
    }
    if (requests.isEmpty) return;
    _rememberPreferences(remembered);
    await _applyAgentRequests(requests);
  }

  Future<void> _applyAgentRequests(List<(String, JsonMap)> requests) async {
    for (final (type, fields) in requests) {
      try {
        await controller!.action('${type}_request', fields, responseType: '${type}_response');
      } catch (e) {
        if (mounted) showToast(context, errorMessage(e), variant: ToastVariant.error);
      }
    }
    if (!mounted) return;
    await controller!.reload();
    await _loadCommands();
  }

  /// agent-profiles use-agent-profile-picker.ts applyProfile. A draft takes
  /// the profile through the form (resolve-agent-form.ts applyProfile,
  /// preferences.ts applyAgentProfilePreferences). A running agent applies
  /// the materialized profile as one bundle with only its set fields — its
  /// mode only when the provider offers it — once the catalog lists the
  /// provider, and remembers it (mergeCreateAgentSelectionPreferences).
  Future<void> _applyProfile(JsonMap profile) async {
    final preferences = _formPreferences;
    final materialized = {
      for (final key in ['provider', 'model', 'modeId', 'thinkingOptionId'])
        key: stringValue(profile[key]).trim(),
      'featureValues': jsonMap(profile['featureValues']),
    };
    if (isDraft) {
      final previous = _config();
      final next = {
        ...applyProfileChoice(
          profile: materialized,
          preferences: preferences,
          providers: providers,
        ),
        'cwd': widget.target.cwd,
      };
      drafts.update(draftKey, {'config': next, 'configResolved': true});
      _rememberPreferences(rememberProfileSelection(
        preferences,
        previousProvider: stringValue(previous['provider']),
        providers: providers,
        config: next,
        profileFeatureValues: jsonMap(materialized['featureValues']),
      ));
      unawaited(_refreshDraftFeatures());
      await _loadCommands();
      return;
    }
    final provider = stringValue(controller?.agent['provider']);
    final entry = providers.where((p) => p['provider'] == provider).firstOrNull;
    if (entry == null || materialized['provider'] != provider) return;
    final modeIds = maps(entry['modes']).map((mode) => stringValue(mode['id']));
    final model = materialized['model'] as String;
    var mode = materialized['modeId'] as String;
    if (mode.isNotEmpty && !modeIds.contains(mode)) mode = '';
    final thinking = materialized['thinkingOptionId'] as String;
    final features = jsonMap(materialized['featureValues']);
    _rememberPreferences(mergeProviderPreferences(
      preferences,
      provider,
      model: model.isEmpty ? null : model,
      clearMode: mode.isEmpty,
      mode: mode.isEmpty ? null : mode,
      thinkingByModel: model.isNotEmpty && thinking.isNotEmpty ? {model: thinking} : null,
      featureValues: features.isEmpty ? null : features,
    ));
    await _applyAgentRequests([
      (
        'agent.config.apply',
        {
          'config': {
            if (model.isNotEmpty) 'modelId': model,
            if (mode.isNotEmpty) 'modeId': mode,
            if (thinking.isNotEmpty) 'thinkingOptionId': thinking,
            if (features.isNotEmpty) 'featureValues': features,
          },
        },
      ),
    ]);
  }

  /// use-agent-commands-query: provider commands for the agent, or for a
  /// draft's complete config (none until it has a provider).
  Future<void> _loadCommands() async {
    final own = ++commandGeneration;
    if (_client == null || isSubagent) return;
    final config = _config();
    autocomplete
      ..isDraft = isDraft
      ..draftEmptyKey = isDraft && stringValue(config['provider']).isEmpty
          ? 'agentAutocomplete.chooseModelForCommands'
          : null;
    if (isDraft && stringValue(config['provider']).isEmpty) {
      autocomplete.setCommands(const []);
      return;
    }
    autocomplete.setCommands(commands, loading: commands.isEmpty);
    try {
      final result = requireSuccess(
        await client.request('list_commands_request', {
          'agentId': widget.target.id ?? 'draft',
          if (isDraft) 'draftConfig': config,
        }, responseType: 'list_commands_response'),
      );
      if (mounted && own == commandGeneration) {
        setState(() => commands = maps(result['commands']));
        autocomplete.setCommands(commands);
      }
    } catch (e) {
      if (own == commandGeneration) {
        autocomplete.setCommands(commands, error: e is ChatRequestRejected ? e.message : '$e');
      }
    }
  }

  /// use-agent-autocomplete.ts file query: up to 50 workspace files and
  /// directories for the @mention.
  Future<List<JsonMap>> _directorySuggestions(String query) async {
    final result = requireSuccess(
      await client.request('directory_suggestions_request', {
        'cwd': widget.target.cwd,
        'query': query,
        'limit': 50,
        'includeFiles': true,
        'includeDirectories': true,
      }, responseType: 'directory_suggestions_response'),
    );
    final entries = maps(result['entries']);
    if (entries.isNotEmpty) return entries;
    return [
      for (final path in jsonList(result['directories']))
        {'path': '$path', 'kind': 'directory'},
    ];
  }

  /// An autocomplete row was chosen (tap, Enter or Tab).
  void _applyAutocomplete(AutocompleteOption option) {
    final hasAttachments = maps(draft['attachments']).isNotEmpty || maps(draft['images']).isNotEmpty;
    if (option.clientCommand && !hasAttachments && !isDraft) {
      unawaited(_clientCommand('/${option.id}'));
      return;
    }
    final next = autocomplete.applied(option);
    if (next == null) return;
    editor.value = TextEditingValue(
      text: next,
      selection: TextSelection.collapsed(offset: next.length),
    );
    focus.requestFocus();
  }

  /// Hardware keys on the composer: autocomplete navigation first, then
  /// input.tsx handleDesktopKeyPress (Enter sends on wide non-native layouts,
  /// Shift+Enter is a newline, Cmd/Ctrl+Enter runs the alternate send while
  /// the agent runs) and the iOS hardware-keyboard submit.
  KeyEventResult _composerKey(FocusNode node, KeyEvent event) {
    // isImeComposingKeyboardEvent: keys that commit an IME composition (CJK
    // input) never send or drive the autocomplete.
    final composing = editor.value.composing;
    if (composing.isValid && !composing.isCollapsed) return KeyEventResult.ignored;
    final complete = autocomplete.handleKey(event);
    if (complete.handled) {
      if (complete.select != null) _applyAutocomplete(complete.select!);
      if (complete.escape) editor.clear();
      return KeyEventResult.handled;
    }
    if (event is! KeyDownEvent) return KeyEventResult.ignored;
    final key = event.logicalKey;
    final keyboard = HardwareKeyboard.instance;
    if ((keyboard.isControlPressed || keyboard.isMetaPressed) && key == LogicalKeyboardKey.keyV) {
      _paste();
      return KeyEventResult.handled;
    }
    if (key != LogicalKeyboardKey.enter && key != LogicalKeyboardKey.numpadEnter) {
      return KeyEventResult.ignored;
    }
    if (keyboard.isShiftPressed) return KeyEventResult.ignored;
    final ios = defaultTargetPlatform == TargetPlatform.iOS;
    final native = ios || defaultTargetPlatform == TargetPlatform.android;
    final submitOnEnter = (!native && !isCompactLayout(context)) || ios;
    if (!submitOnEnter) return KeyEventResult.ignored;
    // handleDesktopKeyPressImpl: while the submit is disabled or loading the
    // key is left alone (a plain newline).
    if (sending || uploading || !initialized) return KeyEventResult.ignored;
    final running = !isDraft && controller?.running == true;
    // Cmd/Ctrl+Enter is the alternate send only while the agent runs;
    // otherwise it is the default send (the iOS hardware-keyboard submit
    // ignores modifiers).
    if ((keyboard.isControlPressed || keyboard.isMetaPressed) && running && !ios) {
      _alternateSend();
      return KeyEventResult.handled;
    }
    _submit();
    return KeyEventResult.handled;
  }

  /// input/state.ts runAlternateSendAction: with the queue preference the
  /// alternate sends now (interrupting); otherwise it queues.
  void _alternateSend() {
    if (behavior == 'queue') {
      _submit(overrideBehavior: 'interrupt');
    } else {
      _submit(overrideBehavior: 'queue');
    }
  }

  void _insert(String text) {
    final selection = editor.selection;
    final start = selection.isValid ? selection.start : editor.text.length;
    final end = selection.isValid ? selection.end : start;
    editor.value = TextEditingValue(
      text: editor.text.replaceRange(start, end, text),
      selection: TextSelection.collapsed(offset: start + text.length),
    );
    focus.requestFocus();
  }

  Future<void> _submit({JsonMap? retry, String? overrideBehavior}) async {
    if (sending || uploading || isSubagent) return;
    if (retry == null && jsonList(draft['unresolvedLegacyAttachments']).isNotEmpty) {
      await _reviewLegacyAttachments();
      return;
    }
    if (retry == null &&
        (editor.text.trim().isEmpty &&
            jsonList(draft['attachments']).isEmpty &&
            jsonList(draft['images']).isEmpty)) {
      return;
    }
    if (retry == null && jsonMap(draft['pending']).isNotEmpty) {
      _showFailure(
        StateError(
          'Resolve the unacknowledged send before sending another message.',
        ),
      );
      return;
    }
    if (isDraft) {
      // provider-selection.ts resolveSubmissionReadiness.
      final blocker = draftSubmissionBlocker(
        config: _config(),
        providers: providers,
        connected: client.status == ConnectionStatus.connected,
        catalogLoading: providers.isEmpty && client.status == ConnectionStatus.connected,
      );
      if (blocker != null) {
        setState(() => sendError = tr(context, blocker));
        return;
      }
    }
    if (retry == null && !isDraft) {
      // client-slash-commands resolveClientSlashCommand: /exit (/quit, /q)
      // and /clear (/new) run immediately when nothing is attached.
      final command = resolveClientSlashCommand(
        editor.text,
        hasAttachments: jsonList(draft['attachments']).isNotEmpty ||
            jsonList(draft['images']).isNotEmpty,
      );
      if (command != null) {
        await _clientCommand('/$command');
        return;
      }
    }
    final choice = overrideBehavior ?? behavior;
    // composer/index.tsx submitMessage: a send during a turn steers only
    // with the steer preference; otherwise it interrupts.
    final message =
        retry ??
        {
          ...drafts.snapshot(draftKey),
          'activeTurnBehavior': choice == 'steer' ? 'steer' : 'interrupt',
        };
    if (!isDraft && controller!.running && choice == 'queue' && retry == null) {
      final queue = maps(draft['queue'])..add({...message, 'state': 'queued'});
      drafts.update(draftKey, {'queue': queue});
      drafts.acknowledge(draftKey, message);
      return;
    }
    setState(() {
      sending = true;
      error = null;
      sendError = null;
    });
    // submit.ts: the composer clears at once, so the submitted row and the
    // composer agree; a failure puts the message back.
    drafts.update(draftKey, {
      'pending': message,
      if (retry == null) ...{
        'text': '',
        'attachments': <JsonMap>[],
        'images': <JsonMap>[],
      },
    });
    try {
      await drafts.flush();
      if (isDraft) {
        await _create(message);
      } else {
        await _send(message);
        drafts.acknowledge(draftKey, message);
      }
      _sticky = true;
      _bottom();
    } catch (e) {
      drafts.update(draftKey, {
        if (definitelyRejected(e)) ...{
          'pending': null,
          if (isDraft) ...{'creationKey': null, 'creationRequest': null},
        },
        if (retry == null) ...{
          'text': message['text'],
          'attachments': List.of(jsonList(message['attachments'])),
          'images': List.of(jsonList(message['images'])),
        },
      });
      if (mounted) setState(() => sendError = _sendErrorMessage(e));
    } finally {
      if (mounted) setState(() => sending = false);
    }
  }

  /// submit.ts: the error's own message, else composer.errors.failedToSend.
  String _sendErrorMessage(Object e) {
    final message = errorMessage(e);
    return message.trim().isEmpty ? tr(context, 'composer.errors.failedToSend') : message;
  }

  /// composer/index.tsx sendAgentMessage: a dispatched prompt then clears
  /// the agent's attention (onAttentionPromptSend).
  Future<void> _send(JsonMap message) async {
    SentUserMessages.remember(stringValue(widget.target.id), message);
    requireSuccess(
      await client.request(
        'send_agent_message_request',
        agentMessageFields(stringValue(widget.target.id), message),
        responseType: 'send_agent_message_response',
      ),
    );
    if (mounted) _attention?.clearOnPromptSend();
  }

  Future<void> _create(JsonMap message) async {
    // provider-selection.ts resolveEffectiveComposer*: the selected or
    // default model, an offered mode, the selected or default thinking.
    final config = effectiveSubmitConfig(_config(), providers);
    // use-draft-agent-features.ts featureValues: offered features only, the
    // draft's own values over the remembered ones.
    final featureValues = resolveFeatureValues(
      draftFeatures,
      rememberedFeatureValues(_formPreferences, stringValue(config['provider'])),
      jsonMap(config.remove('featureValues')),
    );
    if (featureValues.isNotEmpty) config['featureValues'] = featureValues;
    if (stringValue(config['provider']).isEmpty) {
      throw const ChatRequestRejected(
        'Select a provider before creating a session.',
      );
    }
    // workspace-tab.tsx onBeforeSubmit: persistFormPreferences.
    _rememberPreferences(rememberSubmittedSelection(_formPreferences, config, providers));
    var workspaceId = widget.target.workspaceId.isEmpty
        ? stringValue(draft['creationWorkspaceId'])
        : widget.target.workspaceId;
    final recovering = draft['creationKey'] is String;
    final key = draft['creationKey'] as String? ?? const Uuid().v4();
    drafts.update(draftKey, {'creationKey': key});
    await drafts.flush();
    if (workspaceId.isEmpty) {
      final workspace = await createWithReceipt(client, 'workspace', {
        'idempotencyKey': '$key:workspace',
        'source': {'kind': 'directory', 'path': widget.target.cwd},
        'subscribe': false,
      }, recover: recovering);
      workspaceId = stringValue(jsonMap(workspace['workspace'])['id']);
      if (workspaceId.isEmpty) {
        throw StateError('Workspace creation returned no workspace identity.');
      }
      drafts.update(draftKey, {'creationWorkspaceId': workspaceId});
    }
    final original = jsonMap(draft['creationRequest']);
    final fields = original.isNotEmpty
        ? original
        : <String, dynamic>{
            'idempotencyKey': key,
            'workspaceId': workspaceId,
            'config': config,
            'initialPrompt': message['text'],
            'clientMessageId': message['messageId'],
            'images': jsonList(message['images']),
            'attachments': maps(message['attachments'])
                .map(wireAttachment)
                .toList(),
            'subscribe': false,
          };
    drafts.update(draftKey, {'creationRequest': fields});
    await drafts.flush();
    final response = await createWithReceipt(
      client,
      'agent',
      fields,
      recover: original.isNotEmpty,
    );
    final agent = jsonMap(response['agent']);
    if (agent['id'] == null) {
      throw StateError(
        'Creation has not returned an agent yet. Retry recovers the same creation intent.',
      );
    }
    SentUserMessages.remember(stringValue(agent['id']), message);
    if (response['promptError'] != null) {
      final createdWorkspace = stringValue(agent['workspaceId'], workspaceId);
      final createdKey = ChatDrafts.key(
        widget.target.serverId,
        createdWorkspace,
        stringValue(agent['id']),
      );
      if (stringValue(drafts.draft(createdKey)['text']).isEmpty) {
        drafts.update(createdKey, {
          'text': message['text'],
          'images': message['images'],
          'attachments': message['attachments'],
          'pending': null,
        });
      }
      drafts.update(draftKey, {'pending': null});
      await drafts.flush();
      if (mounted) {
        PaseoScope.of(context).open(
          PanelTarget(
            kind: 'agent',
            serverId: widget.target.serverId,
            workspaceId: createdWorkspace,
            cwd: stringValue(agent['cwd'], widget.target.cwd),
            id: stringValue(agent['id']),
            title: stringValue(agent['title'], 'New session'),
            data: {'submissionError': response['promptError']},
          ),
        );
      }
      return;
    }
    drafts.acknowledge(draftKey, message);
    drafts.update(draftKey, {'creationKey': null, 'creationRequest': null});
    if (!mounted) return;
    // create-flow-store markLifecycle "sent": the created agent's chat opens
    // as an optimistic running handoff.
    CreateHandoffs.instance.markSent(widget.target.serverId, stringValue(agent['id']));
    PaseoScope.of(context).open(
      PanelTarget(
        kind: 'agent',
        serverId: widget.target.serverId,
        workspaceId: stringValue(agent['workspaceId'], workspaceId),
        cwd: stringValue(agent['cwd'], widget.target.cwd),
        id: stringValue(agent['id']),
        title: stringValue(agent['title'], 'New session'),
      ),
    );
  }

  Future<void> _queueNow(JsonMap message) async {
    if (sending) return;
    setState(() => sending = true);
    final queued = maps(draft['queue']);
    final queuedIndex = queued.indexWhere(
      (q) => q['messageId'] == message['messageId'],
    );
    if (queuedIndex < 0 || queued[queuedIndex]['state'] == 'sending') {
      setState(() => sending = false);
      return;
    }
    // index.tsx handleSendQueuedNow reuses submitMessage: steer with the
    // steer preference, otherwise interrupt.
    message = queuedMessageForDelivery(
      queued[queuedIndex],
      activeTurnBehavior: behavior == 'steer' ? 'steer' : 'interrupt',
    );
    queued[queuedIndex] = {...message, 'state': 'sending'};
    drafts.update(draftKey, {'queue': queued});
    try {
      await drafts.flush();
      await _send(message);
      drafts.update(draftKey, {
        'queue': maps(draft['queue'])
          ..removeWhere((q) => q['messageId'] == message['messageId']),
      });
    } catch (e) {
      final current = maps(draft['queue']);
      final index = current.indexWhere(
        (q) => q['messageId'] == message['messageId'],
      );
      if (index >= 0) {
        current[index] = {
          ...message,
          'state': definitelyRejected(e) ? 'rejected' : 'unacknowledged',
          'error': '$e',
        };
      }
      drafts.update(draftKey, {'queue': current});
      if (mounted) setState(() => sendError = _sendErrorMessage(e));
    } finally {
      if (mounted) setState(() => sending = false);
    }
  }

  /// actions.ts editQueuedComposerMessage: the queued message leaves the
  /// queue and its text and attachments replace the composer's.
  void _editQueued(JsonMap message) {
    final queue = maps(draft['queue']);
    final index = queue.indexWhere((q) => q['messageId'] == message['messageId']);
    if (index < 0 || !const ['queued', 'blocked', 'rejected'].contains(queue[index]['state'])) {
      return;
    }
    final item = queue.removeAt(index);
    drafts.update(draftKey, {
      'queue': queue,
      'text': stringValue(item['text']),
      'attachments': List.of(jsonList(item['attachments'])),
      'images': List.of(jsonList(item['images'])),
    });
    focus.requestFocus();
  }

  Future<void> _removeQueued(JsonMap message) async {
    if (message['state'] == 'unacknowledged' &&
        !await confirmAction(
          context,
          title: 'Remove unacknowledged message?',
          message: 'The daemon may already have received it. Removing it here does not cancel the remote turn.',
        )) {
      return;
    }
    drafts.update(draftKey, {
      'queue': maps(draft['queue'])
        ..removeWhere((q) => q['messageId'] == message['messageId']),
    });
  }

  /// composer/index.tsx runClientSlashCommand: no confirmation; the draft is
  /// cleared, then /exit archives the agent and /clear also opens a fresh
  /// draft with the agent's setup (client-slash-commands buildDraftAgentSetup).
  Future<void> _clientCommand(String command) async {
    final fresh = command == '/clear' || command == '/new';
    final config = _config();
    drafts.update(draftKey, {'text': '', 'attachments': <JsonMap>[], 'images': <JsonMap>[]});
    setState(() {
      sendError = null;
      sending = true;
    });
    try {
      await controller!.action(
        'archive_agent_request',
        {},
        responseType: 'agent_archived',
      );
      if (!mounted) return;
      final store = PaseoScope.of(context);
      if (fresh) {
        store.open(
          PanelTarget(
            kind: 'draft',
            serverId: widget.target.serverId,
            workspaceId: widget.target.workspaceId,
            cwd: widget.target.cwd,
            id: const Uuid().v4(),
            title: 'New session',
            data: {'config': config},
          ),
        );
      }
      await store.closeTab(widget.target.key);
    } catch (e) {
      if (mounted) setState(() => sendError = e is ChatRequestRejected ? e.message : '$e');
    } finally {
      if (mounted) setState(() => sending = false);
    }
  }

  /// input/presentation.tsx fullscreen: the same surface1 editor filling the
  /// pane with 16px insets and 12px gaps — a 28px ghost Minimize2 (16px) top
  /// right, the editor, then the send error and Send bottom right, both on the
  /// inline send rail. Escape (or back) exits; Send exits, then sends.
  Future<void> _fullscreenEditor() async {
    _fullscreenOpen = true;
    final native = defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS;
    final glyph = native ? 20.0 : 16.0;
    // resolveGlyphRailOffset: the send arrow's ink inset minus Minimize2's.
    final railOffset = (28 - glyph) / 2 + 5 * glyph / 24 - ((28 - 16) / 2 + 3 * 16 / 24);
    fullscreenFocus.onKeyEvent = (node, event) {
      if (event is! KeyDownEvent) return KeyEventResult.ignored;
      if (event.logicalKey == LogicalKeyboardKey.escape) {
        Navigator.of(node.context!).maybePop();
        return KeyEventResult.handled;
      }
      final enter = event.logicalKey == LogicalKeyboardKey.enter ||
          event.logicalKey == LogicalKeyboardKey.numpadEnter;
      if (!enter || HardwareKeyboard.instance.isShiftPressed) return KeyEventResult.ignored;
      final ios = defaultTargetPlatform == TargetPlatform.iOS;
      if (native && !ios) return KeyEventResult.ignored;
      if (editor.text.trim().isEmpty) return KeyEventResult.handled;
      Navigator.of(node.context!).pop(true);
      return KeyEventResult.handled;
    };
    bool? send;
    try {
      send = await showDialog<bool>(
        context: context,
        builder: (dialogContext) {
          final colors = Theme.of(dialogContext).colorScheme;
          final palette = PaseoColors.of(dialogContext);
          return Dialog.fullscreen(
            backgroundColor: palette.surface1,
            child: SafeArea(
              child: Padding(
                padding: EdgeInsets.fromLTRB(
                  16, 16, 16, 16 + MediaQuery.viewInsetsOf(dialogContext).bottom,
                ),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  spacing: 12,
                  children: [
                    Align(
                      alignment: Alignment.centerRight,
                      child: Padding(
                        padding: EdgeInsets.only(right: railOffset),
                        child: Semantics(
                          button: true,
                          label: tr(dialogContext, 'composer.input.exitFullscreen'),
                          excludeSemantics: true,
                          child: Material(
                            type: MaterialType.transparency,
                            child: InkWell(
                              customBorder: const CircleBorder(),
                              hoverColor: palette.surface2,
                              onTap: () => Navigator.pop(dialogContext),
                              child: SizedBox.square(
                                dimension: 28,
                                child: Icon(LucideIcons.minimize2300, size: 16,
                                    color: colors.onSurfaceVariant),
                              ),
                            ),
                          ),
                        ),
                      ),
                    ),
                    Expanded(
                      child: TextField(
                        controller: editor,
                        focusNode: fullscreenFocus,
                        autofocus: true,
                        expands: true,
                        minLines: null,
                        maxLines: null,
                        keyboardType: TextInputType.multiline,
                        textAlignVertical: TextAlignVertical.top,
                        style: _composerTextStyle(dialogContext),
                        decoration: InputDecoration(
                          hintText: _placeholder(dialogContext),
                          hintStyle: TextStyle(color: palette.surface4),
                          filled: false,
                          isDense: true,
                          contentPadding: EdgeInsets.zero,
                          border: InputBorder.none,
                          enabledBorder: InputBorder.none,
                          focusedBorder: InputBorder.none,
                        ),
                      ),
                    ),
                    if (sendError != null) _sendErrorText(dialogContext),
                    Align(
                      alignment: Alignment.centerRight,
                      child: ListenableBuilder(
                        listenable: editor,
                        builder: (context, _) {
                          final empty = editor.text.trim().isEmpty &&
                              maps(draft['attachments']).isEmpty &&
                              maps(draft['images']).isEmpty;
                          return ComposerSendButton(
                            glyph: glyph,
                            label: tr(context, 'composer.input.sendMessage'),
                            onPressed: empty || sending || uploading
                                ? null
                                : () => Navigator.pop(dialogContext, true),
                          );
                        },
                      ),
                    ),
                  ],
                ),
              ),
            ),
          );
        },
      );
    } finally {
      _fullscreenOpen = false;
    }
    if (!mounted) return;
    focus.requestFocus();
    if (send == true) unawaited(_submit());
  }

  /// index.tsx handlePasteImage (attachment menu, native) and the desktop
  /// Ctrl/Cmd+V: clipboard images become image pills; text pastes as text.
  /// An image-only paste with no image toasts composer.errors.noClipboardImage.
  Future<void> _paste({bool imagesOnly = false}) async {
    final key = draftKey;
    try {
      final contents = await readChatClipboard(imagesOnly: imagesOnly);
      if (contents.images.isNotEmpty) {
        drafts.update(key, {
          'images': [...maps(drafts.draft(key)['images']), ...contents.images],
        });
      } else if (contents.text != null && mounted && key == draftKey) {
        _insert(contents.text!);
      } else if (imagesOnly && mounted) {
        showToast(context, tr(context, 'composer.errors.noClipboardImage'),
            variant: ToastVariant.error);
      }
    } catch (e) {
      if (mounted) {
        showToast(context, imagesOnly ? tr(context, 'composer.errors.pasteImageFailed') : '$e',
            variant: ToastVariant.error);
      }
    }
  }

  /// index.tsx handlePickImage / handlePickFile: picked images join the image
  /// tray; picked files upload (uploadSelectedFiles).
  Future<void> _pickFiles({bool images = false}) async {
    try {
      final files = await openFiles(
        acceptedTypeGroups: [
          if (images)
            XTypeGroup(
              label: 'images',
              extensions: rasterImageExtensions,
              mimeTypes: const ['image/*'],
              uniformTypeIdentifiers: const ['public.image'],
            ),
        ],
      );
      if (!mounted || files.isEmpty) return;
      await _attachFiles(files, images: images);
    } catch (e) {
      _attachFailed(e);
    }
  }

  /// [images]: every file is an image attachment. Otherwise a drop
  /// (useFileDrop) sends raster images to the image tray and uploads the
  /// rest, and a file pick uploads everything.
  Future<void> _attachFiles(List<XFile> files, {bool images = false, bool drop = false}) async {
    final key = draftKey;
    try {
      final imageFiles = [
        for (final file in files)
          if (images || (drop && rasterImageMimeType(file.name) != null)) file,
      ];
      if (imageFiles.isNotEmpty) {
        final added = [
          for (final file in imageFiles)
            {
              'data': base64Encode(await file.readAsBytes()),
              'mimeType': rasterImageMimeType(file.name) ?? file.mimeType ?? 'image/png',
            },
        ];
        drafts.update(key, {'images': [...maps(drafts.draft(key)['images']), ...added]});
      }
      final uploads = files.where((file) => !imageFiles.contains(file)).toList();
      if (uploads.isNotEmpty) await _uploadFiles(key, uploads);
    } catch (e) {
      _attachFailed(e);
    }
  }

  /// actions.ts uploadFileAttachments: every file is size-checked first, then
  /// each uploads; the uploaded files attach together once all succeed.
  Future<void> _uploadFiles(String key, List<XFile> files) async {
    checkUploadSizes([for (final file in files) (name: file.name, size: await file.length())]);
    if (!mounted) return;
    setState(() {
      uploading = true;
      uploadName = files.first.name;
    });
    try {
      final uploaded = <JsonMap>[];
      for (final file in files) {
        final bytes = await file.readAsBytes();
        if (mounted) {
          setState(() {
            uploadName = file.name;
            uploadSent = 0;
            uploadTotal = bytes.length;
          });
        }
        uploaded.add(await RemoteFileService(client).upload(
          bytes: bytes,
          fileName: file.name,
          mimeType: file.mimeType ?? mimeTypeForPath(file.name),
          onProgress: (sent, total) {
            if (mounted) {
              setState(() {
                uploadSent = sent;
                uploadTotal = total;
              });
            }
          },
        ));
      }
      drafts.update(key, {'attachments': [...maps(drafts.draft(key)['attachments']), ...uploaded]});
    } finally {
      if (mounted) setState(() => uploading = false);
    }
  }

  /// index.tsx: the error's message (fileTooLarge for an oversized file),
  /// else composer.errors.uploadFailed, as an error toast.
  void _attachFailed(Object e) {
    if (!mounted) return;
    final message = e is FileTooLargeError
        ? tr(context, 'composer.errors.fileTooLarge', args: {'size': '50MB', 'fileName': e.fileName})
        : errorMessage(e);
    showToast(
      context,
      message.trim().isEmpty ? tr(context, 'composer.errors.uploadFailed') : message,
      variant: ToastVariant.error,
    );
  }

  /// The attach button's last rect, anchoring the desktop forge picker.
  Rect? _attachAnchor;

  /// The checkout's forge (getForgePresentation): GitHub until the daemon
  /// reports otherwise. Loaded when the attachment menu or picker opens, or
  /// when forge attachments are shown (useCheckoutPrStatusQuery's enablement).
  String _forge = 'github';
  Future<void>? _forgeLoad;

  void _ensureForge() {
    if (_forgeLoad != null || widget.target.cwd.trim().isEmpty ||
        client.status != ConnectionStatus.connected) {
      return;
    }
    _forgeLoad = loadCheckoutForge(client, widget.target.cwd).then((forge) {
      if (mounted && forge != _forge) setState(() => _forge = forge);
    }).catchError((Object _) {
      _forgeLoad = null;
    });
  }

  /// index.tsx handleToggleGithubItem: choosing an already attached item
  /// removes it, otherwise it is attached; the picker closes either way.
  Future<void> _pickForge() async {
    final attachments = maps(draft['attachments']);
    final keys = {
      for (final attachment in attachments)
        if (AttachmentPill.forgeLabels(attachment) != null) forgeItemKey(attachment),
    };
    final item = await showForgePicker(
      context,
      client: client,
      cwd: widget.target.cwd,
      attachedKeys: keys,
      anchor: _attachAnchor,
      mergeRequests: _forge == 'gitlab',
    );
    if (item == null || !mounted) return;
    final key = forgeItemKey(item);
    final current = maps(draft['attachments']);
    final index = current.indexWhere(
      (a) => AttachmentPill.forgeLabels(a) != null && forgeItemKey(a) == key,
    );
    if (index >= 0) {
      // toggleForgeAttachmentFromPicker: a picker removal is never re-attached.
      _forgeAutoAttach.markRemoved(current.removeAt(index));
    } else {
      current.add(forgeAttachment(item, legacy: usesLegacyForge(client)));
    }
    drafts.update(draftKey, {'attachments': current});
    focus.requestFocus();
  }

  /// actions.ts openComposerAttachment: a forge pill opens its page in the
  /// browser; uploaded and workspace files have no open action.
  Future<void> _openForgeAttachment(JsonMap attachment) async {
    try {
      final uri = Uri.parse(stringValue(attachment['url']));
      if (!['https', 'http'].contains(uri.scheme) ||
          !await launchUrl(uri, mode: LaunchMode.externalApplication)) {
        throw StateError('Could not open the attachment URL.');
      }
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  Future<void> _imagePreview(JsonMap image) => showDialog<void>(
    context: context,
    builder: (context) => Dialog.fullscreen(
      child: Stack(
        children: [
          Positioned.fill(
            child: InteractiveViewer(
              minScale: .2,
              maxScale: 8,
              child: Image.memory(
                base64Decode(stringValue(image['data'])),
                fit: BoxFit.contain,
              ),
            ),
          ),
          Positioned(
            top: 12,
            right: 12,
            child: IconButton(
              onPressed: () => Navigator.pop(context),
              icon: const Icon(LucideIcons.x300),
            ),
          ),
        ],
      ),
    ),
  );

  Future<void> _lifecycle(String action) async {
    try {
      if (action == 'rename') {
        final name = await promptText(
          context,
          title: 'Rename session',
          initialValue: stringValue(controller!.agent['title']),
        );
        if (name == null) return;
        if (name.trim().isEmpty || name.trim().length > 200) {
          throw StateError('Session title must contain 1–200 characters.');
        }
        await controller!.action('update_agent_request', {
          'name': name.trim(),
        }, responseType: 'update_agent_response');
      } else if (action == 'refresh') {
        await controller!.action(
          'refresh_agent_request',
          {},
          responseType: 'status',
        );
      } else if (action == 'fork') {
        await _fork();
        return;
      } else if (action == 'archive' || action == 'delete') {
        if (!await confirmAction(
          context,
          title: action == 'archive' ? 'Archive session?' : 'Delete session?',
          message: action == 'archive'
              ? 'Archive this session and stop active work?'
              : 'Permanently delete this session?',
          destructive: true,
        )) {
          return;
        }
        // agent-panel.tsx isArchivingCurrentAgent: the archiving overlay
        // covers the panel until the archive lands and the tab closes.
        if (action == 'archive') _refresh(() => _chrome.archiving = true);
        try {
          await controller!.action(
            action == 'archive'
                ? 'archive_agent_request'
                : 'delete_agent_request',
            {},
            responseType: action == 'archive'
                ? 'agent_archived'
                : 'agent_deleted',
          );
        } finally {
          _refresh(() => _chrome.archiving = false);
        }
        if (mounted) await PaseoScope.of(context).closeTab(widget.target.key);
        return;
      } else if (action == 'detach') {
        await controller!.action(
          'agent.detach.request',
          {},
          responseType: 'agent.detach.response',
        );
      } else if (action == 'cancel') {
        await controller!.action(
          'cancel_agent_request',
          {},
          responseType: 'cancel_agent_response',
        );
      } else if (action == 'usage') {
        await _usage();
        return;
      }
      await controller!.reload();
    } catch (e) {
      _showFailure(e);
    }
  }

  /// serverInfo.features.agentForkContextCursor (agent-stream/view.tsx).
  bool get _forkCursorSupported =>
      jsonMap(client.serverInfo['features'])['agentForkContextCursor'] == true;

  /// hooks/use-fork-agent.ts: build the fork context (pinned to [entry], or the
  /// whole timeline so far for the in-flight turn) and open it as a draft —
  /// in this workspace (`tab`) or in a new workspace from the same checkout
  /// (`workspace`).
  Future<void> _fork([JsonMap? entry, String target = 'tab']) async {
    try {
      if (jsonMap(client.serverInfo['features'])['agentForkContext'] != true) {
        throw StateError(tr(context, 'message.actions.forkUnavailable'));
      }
      if (target == 'tab' && widget.target.workspaceId.isEmpty) {
        throw StateError(tr(context, 'message.actions.forkMissingWorkspace'));
      }
      // turn-boundary.ts resolveAssistantTurnForkBoundary: the timeline
      // position only on hosts with agentForkContextCursor, else the message id.
      final cursor = entry != null && _forkCursorSupported && controller!.epoch != null
          ? {'epoch': controller!.epoch, 'seq': entry['seqEnd']}
          : null;
      final result = await controller!.action('agent.fork_context.request', {
        'boundaryCursor': ?cursor,
        if (entry != null && jsonMap(entry['item'])['messageId'] != null)
          'boundaryMessageId': jsonMap(entry['item'])['messageId'],
      }, responseType: 'agent.fork_context.response');
      if (result['attachment'] == null) {
        if (!mounted) return;
        throw StateError(tr(context, 'message.actions.forkFailed'));
      }
      if (!mounted) return;
      PaseoScope.of(context).open(
        PanelTarget(
          kind: 'draft',
          serverId: widget.target.serverId,
          // An empty workspace makes the draft create its own workspace at
          // this checkout on send.
          workspaceId: target == 'workspace' ? '' : widget.target.workspaceId,
          cwd: widget.target.cwd,
          id: const Uuid().v4(),
          title: 'Fork session',
          data: {
            'config': _config(),
            'attachments': [result['attachment']],
            'attachmentIntentId': const Uuid().v4(),
          },
        ),
      );
    } catch (e) {
      _showFailure(e);
    }
  }

  /// rewind/use-rewind-agent-mutation.ts: rewind to the prompt with the mode
  /// picked from the RewindMenu. A conversation rewind (`conversation` /
  /// `both`) then fetches the latest tail, restores the prompt into an empty
  /// composer (composer-restore.tsx restoreComposerTextIfEmpty) and scrolls
  /// to the bottom (agent-panel.tsx handleRewindComplete). Failures toast and
  /// rethrow so the menu clears its pending state.
  Future<void> _rewind(JsonMap entry, String mode) async {
    try {
      await controller!.action('agent.rewind.request', {
        'messageId': jsonMap(entry['item'])['messageId'],
        'mode': mode,
      }, responseType: 'agent.rewind.response');
      if (mode == 'files') return;
      await controller!.refreshTail();
      if (!mounted) return;
      if (stringValue(draft['text']).isEmpty) {
        drafts.update(draftKey, {'text': stringValue(jsonMap(entry['item'])['text'])});
      }
      _sticky = true;
      _bottom();
    } catch (e) {
      _showFailure(e);
      rethrow;
    }
  }

  Future<void> _usage() async {
    final a = controller!.agent,
        usage = jsonMap(controller!.agent['lastUsage']);
    // components/context-window-sheet.tsx: details in the adaptive sheet.
    await showAdaptiveSheet<void>(
      context: context,
      builder: (context) => SheetContents(
        title: 'Session activity and usage',
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          spacing: 8,
          children: [
            for (final pair in {
              'Provider': a['provider'],
              'Model': a['model'],
              'Status': a['status'],
              'Created': a['createdAt'],
              'Updated': a['updatedAt'],
              'Last prompt': a['lastUserMessageAt'],
              'Input tokens': usage['inputTokens'],
              'Cached input': usage['cachedInputTokens'],
              'Output tokens': usage['outputTokens'],
              'Context used': usage['contextWindowUsedTokens'],
              'Context maximum': usage['contextWindowMaxTokens'],
              'Cost (USD)': usage['totalCostUsd'],
            }.entries)
              if (pair.value != null)
                Row(
                  children: [
                    Expanded(
                      child: Text(pair.key, style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                        color: Theme.of(context).colorScheme.onSurfaceVariant,
                      )),
                    ),
                    SelectableText('${pair.value}', style: Theme.of(context).textTheme.bodyMedium),
                  ],
                ),
            if (a['lastError'] != null)
              SelectableText(stringValue(a['lastError']),
                  style: TextStyle(color: Theme.of(context).colorScheme.error)),
            ExpansionTile(
              tilePadding: EdgeInsets.zero,
              shape: const Border(),
              collapsedShape: const Border(),
              title: const Text('Runtime details'),
              children: [JsonDetails(value: a['runtimeInfo'])],
            ),
          ],
        ),
      ),
    );
  }

  /// agent-panel.tsx ChatAgentReadyContent: the dock (stream with its
  /// floating tracks, the history sync error callout, then the composer or
  /// the archived callout) with the sync overlay, the panel's toast viewport
  /// and the archiving overlay on top. Until the agent resolves, the whole
  /// panel is one of the non-ready views instead.
  @override
  Widget build(BuildContext context) {
    // A hidden retained tab keeps its last frame; showing it again changes
    // PanelActivity, which rebuilds it from current state.
    final cached = _lastBuild;
    if (cached != null && !PanelActivity.of(context).visible) return cached;
    return _lastBuild = _buildPanel(context);
  }

  Widget _buildPanel(BuildContext context) {
    final c = controller;
    final agent = c?.agent ?? <String, dynamic>{};
    final nonReady = _nonReadyView();
    final archived = agent['archivedAt'] != null;
    // use-agent-screen-state-machine.ts resolveAgentScreenSync: archived is
    // idle, offline is reconnecting (the toast), then a sync error, then a
    // catch-up (overlay before any history landed, unless that first attempt
    // already failed; the "Updating messages" toast after).
    final status = c?.syncStatus;
    final syncError = c != null &&
        !isDraft &&
        _connected &&
        !archived &&
        (status == TimelineSyncStatus.error || status == TimelineSyncStatus.retrying);
    final historyOverlay = c != null &&
        !isDraft &&
        nonReady == null &&
        _connected &&
        !archived &&
        status == TimelineSyncStatus.pending &&
        !_chrome.hydrated &&
        !_chrome.initialSyncFailed &&
        // resolveCatchingUpUi: a just-created agent catches up silently.
        !c.createHandoff;
    final toast = _panelToast();
    return Column(
      children: [
        Expanded(
          child: ColoredBox(
            color: PaseoColors.of(context).surface0,
            child: Stack(
              children: [
                if (nonReady != null)
                  Positioned.fill(child: nonReady)
                else
                  Positioned.fill(
                    child: Column(
                      children: [
                        // SheetAgentFacts: shown without its composer, the
                        // agent's facts line heads the stream.
                        if (isSheet && !isSubagent && agent.isNotEmpty)
                          ?_sheetFacts(agent),
                        Expanded(child: isDraft ? _draftSetup() : _timeline()),
                        if (syncError)
                          TimelineSyncErrorCallout(
                            retrying: status == TimelineSyncStatus.retrying,
                            onRetry: c.retrySync,
                          ),
                        if (!isSubagent && !isSheet)
                          if (archived)
                            ArchivedAgentCallout(
                              connected: _connected,
                              onUnarchive: _unarchive,
                            )
                          else if (!_chrome.archiving)
                            _composer(),
                      ],
                    ),
                  ),
                if (historyOverlay) const HistorySyncOverlay(),
                PanelToastViewport(
                  toast: toast,
                  onDismiss: () => _dismissPanelToast(toast?.id),
                ),
                if (_chrome.archiving) const ArchivingOverlay(),
              ],
            ),
          ),
        ),
      ],
    );
  }

  /// composer/draft/workspace-tab.tsx: a draft has no transcript yet, so the
  /// area above its composer stays empty.
  Widget _draftSetup() => const SizedBox.expand();

  /// agent-stream/view.tsx AgentStreamView on the native strategy: the
  /// history-start slot, the stream rows, the live tail (turn footer, pending
  /// permissions in the contentMaxWidth wrapper 8px apart, then the inset
  /// that keeps the tail clear of the floating track bar), with chat find,
  /// the outline rail, the track bar and the jump-to-latest button over it.
  Widget _timeline() {
    final c = controller!;
    final compact = isCompactLayout(context);
    final archived = c.agent['archivedAt'] != null;
    final tasks = isSubagent ? const <JsonMap>[] : _latestTasks();
    final subagents = _subagentRows();
    // A provider-subagent panel shows only its children's track (no tasks/diff).
    final diffStat = isSubagent ? <String, dynamic>{} : _workspaceDiffStat();
    final tracksVisible =
        !archived &&
        !_chrome.archiving &&
        AgentTracks.visible(
          tasks: tasks,
          rows: subagents,
          archiveStatus: _chrome.archiveFinished,
          hasDiff: diffStat.integer('additions') > 0 || diffStat.integer('deletions') > 0,
        );
    final permissions = isSubagent ? const <JsonMap>[] : c.permissions.values.toList();
    final (models: models, items: streamItems) = _streamModel();
    final footerAtEnd =
        c.running || auxiliaryFooterHost(models, turnActive: c.running) != null;
    // view.tsx renderLiveAuxiliary: the track bar's tail clearance minus the
    // turn footer's own 32px bottom spacing when the footer ends the stream.
    final tailClearance = tracksVisible ? composerTrackTailClearance(compact) : 0.0;
    final existingTail = footerAtEnd && permissions.isEmpty ? 32.0 : 0.0;
    final tailInset = tailClearance > existingTail ? tailClearance - existingTail : 0.0;
    final controlClearance = tracksVisible ? composerTrackControlClearance(compact) : 0.0;
    final listInset = MediaQuery.sizeOf(context).width < 720 ? 12.0 : 16.0;
    final empty = models.isEmpty && permissions.isEmpty && !footerAtEnd;
    final find = _chrome.find;
    // strategy-native.tsx: an inverted, virtualized list. Item 0 is the
    // newest edge (tail inset, pending permissions, then the stream rows from
    // the newest), offset 0 is the bottom, and the history-start slot is the
    // far end — so a short transcript rests on the composer, streamed rows
    // keep a sticky reader at the bottom, and older pages extend the far end
    // without moving the rows on screen. Only rows near the viewport build.
    final items = <_StreamItem>[
      if (tailInset > 0)
        (key: const ValueKey('stream:tail-inset'), build: () => SizedBox(height: tailInset)),
      if (permissions.isNotEmpty)
        (
          key: const ValueKey('stream:permissions'),
          build: () => Padding(
            padding: EdgeInsets.symmetric(horizontal: listInset),
            child: Center(
              child: ConstrainedBox(
                constraints: BoxConstraints(
                  maxWidth: PaseoTypography.of(context).contentMaxWidth,
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 8),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    spacing: 8,
                    children: [
                      for (final request in permissions)
                        PermissionCard(
                          key: ValueKey(request['id']),
                          request: request,
                          onRespond: (response) => c.permission(request, response),
                        ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ...streamItems,
      (
        key: const ValueKey('stream:history-start'),
        build: () => HistoryStartSlot(loading: _chrome.loadingOlder),
      ),
    ];
    final indexes = <Key, int>{
      for (var i = 0; i < items.length; i++) items[i].key: i,
    };
    return Stack(
      children: [
        // strategy-native.tsx liveHeaderContent: with nothing to show the
        // inverted list's header is the empty state, so it rests on the
        // composer (the list's newest edge) rather than mid-pane.
        if (empty && !c.loading)
          Positioned(
            left: 0,
            right: 0,
            bottom: 0,
            child: Padding(
              padding: EdgeInsets.symmetric(horizontal: listInset),
              child: const StreamEmptyState(),
            ),
          )
        else
          Positioned.fill(
            child: NotificationListener<ScrollNotification>(
              onNotification: _onStreamScrollNotification,
              child: CustomScrollView(
                key: _chrome.viewportKey,
                controller: scroll,
                reverse: true,
                slivers: [
                  SliverList(
                    delegate: SliverChildBuilderDelegate(
                      (context, index) => KeyedSubtree(
                        key: items[index].key,
                        child: items[index].build(),
                      ),
                      childCount: items.length,
                      // Rows keep their state as newer rows shift indexes.
                      findChildIndexCallback: (key) => indexes[key],
                    ),
                  ),
                ],
              ),
            ),
          ),
        if (!isSubagent &&
            PaseoScope.of(context).preferences['chatOutlineEnabled'] != false)
          Positioned.fill(
            child: ChatOutlineRail(
              prompts: _chrome.prompts,
              activeSeq: _chrome.activePromptSeq,
              onJumpToPrompt: (seq) => unawaited(_jumpToPrompt(seq)),
            ),
          ),
        if (tracksVisible)
          Positioned(
            left: 0,
            right: 0,
            bottom: 0,
            child: _agentTracks(subagents, tasks, diffStat),
          ),
        Positioned(
          left: 0,
          right: 0,
          bottom: controlClearance > 16 ? controlClearance : 16,
          child: JumpToLatestButton(
            visible: !_atBottom && !empty,
            onPressed: _jumpToLatest,
          ),
        ),
        if (_chrome.findOpen && find != null)
          ListenableBuilder(
            listenable: find,
            builder: (context, _) => ChatFindOverlay(
              controller: find,
              onClosed: _closeFind,
            ),
          ),
      ],
    );
  }

  /// The timeline plus optimistic prompts (UserMessage isPending): the direct
  /// send held in draft['pending'] and queued messages ChatDelivery is
  /// sending, each until the daemon echoes a user_message with its
  /// clientMessageId.
  List<JsonMap> _streamEntries(List<JsonMap> entries) {
    if (isDraft || isSubagent) return entries;
    final current = draft;
    final outgoing = [
      if (jsonMap(current['pending']).isNotEmpty) jsonMap(current['pending']),
      ...maps(current['queue']).where((message) => message['state'] == 'sending'),
    ];
    if (outgoing.isEmpty) return entries;
    final echoed = {
      for (final row in entries)
        if (jsonMap(row['item'])['clientMessageId'] is String)
          jsonMap(row['item'])['clientMessageId'] as String,
    };
    final pending = <JsonMap>[];
    for (final message in outgoing) {
      final id = stringValue(message['messageId']);
      if (id.isEmpty || echoed.contains(id) || pending.any((row) => row['clientMessageId'] == id)) {
        continue;
      }
      pending.add({
        'clientMessageId': id,
        'pending': true,
        'timestamp': stringValue(message['createdAt'], DateTime.now().toUtc().toIso8601String()),
        'item': {
          'type': 'user_message',
          'text': stringValue(message['text']),
          'images': jsonList(message['images']),
          'attachments': jsonList(message['attachments']),
          'clientMessageId': id,
        },
      });
    }
    return pending.isEmpty ? entries : [...entries, ...pending];
  }

  /// [streamRowModels] and [_streamRows] for the current timeline, reused
  /// while none of their inputs changed: a notification that leaves the
  /// timeline, the outgoing prompts and the grouping alone (status,
  /// permissions, the composer) skips regrouping the whole history.
  ({List<StreamRow> models, List<_StreamItem> items}) _streamModel() {
    final c = controller!;
    final current = isDraft || isSubagent ? null : draft;
    final overview = PaseoScope.of(context).preferences['toolCallDetailLevel'] == 'overview';
    final provider = stringValue(c.agent['provider']);
    final cache = _streamCache;
    if (cache != null &&
        identical(cache.timeline, c.timeline) &&
        identical(cache.pending, current?['pending']) &&
        identical(cache.queue, current?['queue']) &&
        cache.overview == overview &&
        cache.provider == provider &&
        cache.running == c.running) {
      return (models: cache.models, items: cache.items);
    }
    final models = streamRowModels(c.timeline);
    final items = _streamRows(models).reversed.toList();
    if (_rowWidgets.length > models.length + 256) {
      final live = {for (final row in models) row.entry['seqStart']};
      _rowWidgets.removeWhere((seq, _) => !live.contains(seq));
    }
    _streamCache = (
      timeline: c.timeline,
      pending: current?['pending'],
      queue: current?['queue'],
      overview: overview,
      provider: provider,
      running: c.running,
      models: models,
      items: items,
    );
    return (models: models, items: items);
  }

  /// Stream rows (detail-level grouping, TodoWrite cards, pending prompts) for
  /// [entries]; shared by [_streamRows] and the stream chrome's tail inset.
  List<StreamRow> streamRowModels(List<JsonMap> entries) => buildStreamRows(
    _streamEntries(entries),
    overview: PaseoScope.of(context).preferences['toolCallDetailLevel'] == 'overview',
    provider: stringValue(controller?.agent['provider']),
  );

  /// agent-stream/view.tsx + layout.ts: one row per visible stream item (or
  /// overview tool-call group), a completed turn footer closing each response
  /// before the next prompt, and the live auxiliary footer at the end —
  /// running (loader, fork, elapsed) while a turn is active, otherwise the
  /// latest response's completed footer. Oldest first; each item builds its
  /// widget only when the list lays it out, keyed by its row identity
  /// (keyExtractor) so a row keeps its state across inserts.
  List<_StreamItem> _streamRows(List<StreamRow> rows) {
    final c = controller!;
    final running = c.running;
    final auxiliary = auxiliaryFooterHost(rows, turnActive: running);
    final hasAuxiliaryFooter = running || auxiliary != null;
    // layout.ts phase: rows of the active turn's live head stream; the last
    // prompt starts it.
    final liveFrom = running
        ? rows.lastIndexWhere((row) => row.kind == 'user_message') + 1
        : rows.length;
    // timeline/turn-time.ts: completed turns time their footers from the
    // stream items themselves (prompt → last item).
    final timing = deriveTurnTiming(rows, turnActive: running);
    final used = <String>{};
    String rowId(int i) {
      final row = rows[i];
      final seq = row.entry['seqStart'];
      final clientId = jsonMap(row.entry['item'])['clientMessageId'];
      final id = row.group != null
          ? 'group:${row.groupId}'
          : seq is int
          ? 'seq:$seq'
          : clientId is String
          ? 'pending:$clientId'
          : 'index:$i';
      return used.add(id) ? id : '$id#$i';
    }

    final out = <_StreamItem>[];
    final ids = <int, String>{};
    JsonMap? previousTasks;
    for (var i = 0; i < rows.length; i++) {
      final row = rows[i];
      final item = jsonMap(row.entry['item']);
      final previous = previousTasks;
      if (item['type'] == 'todo') {
        previousTasks = item;
        // stream.ts appendTodoList: an unchanged list adds no card.
        if (previous != null &&
            TimelineItem.todoActivities(item, previous).isEmpty) {
          continue;
        }
      }
      final id = ids[i] = rowId(i);
      final host = completedFooterHost(rows, i, auxiliaryHost: auxiliary);
      out.add((
        key: ValueKey('stream:$id'),
        build: () => _timelineRow(
          rows,
          i,
          previousTasks: previous,
          footerBelow: host != null || (hasAuxiliaryFooter && i == rows.length - 1),
          streaming: running && i == rows.length - 1,
          live: i >= liveFrom,
        ),
      ));
      if (host != null) {
        out.add((
          key: ValueKey('footer:${ids[host] ?? rowId(host)}'),
          build: () => _turnFooter(rows, host, timing[host]),
        ));
      }
    }
    if (running) {
      out.add((key: const ValueKey('footer:live'), build: () => _turnFooter(rows, null)));
    } else if (auxiliary != null) {
      final host = auxiliary;
      out.add((
        key: ValueKey('footer:${ids[host] ?? rowId(host)}'),
        build: () => _turnFooter(rows, host, timing[host]),
      ));
    }
    return out;
  }

  /// turn-footer.tsx TurnFooter: [host] is the response's latest assistant
  /// row (CompletedTurnFooter: copy, fork menu, "Worked for …" from its
  /// [timing]); null renders the running footer (synced loader, in-flight
  /// fork, live elapsed).
  Widget _turnFooter(List<StreamRow> rows, int? host, [TurnTiming? timing]) {
    final c = controller!;
    final entry = host == null ? null : rows[host].entry;
    final colors = Theme.of(context).colorScheme;
    final listInset = MediaQuery.sizeOf(context).width < 720 ? 12.0 : 16.0;
    final List<Widget> children;
    if (entry == null) {
      final active = jsonMap(c.agent['activeTurn']);
      final start = DateTime.tryParse(stringValue(
        active['startedAt'],
        stringValue(c.turns[stringValue(active['turnId'])]?['startedAt']),
      ));
      children = [
        Transform.translate(
          offset: const Offset(-2, 0),
          child: _WorkingDots(color: colors.onSurface),
        ),
        if (!isSubagent)
          AssistantForkMenuButton(onFork: (target) => _fork(null, target)),
        if (start != null) _ElapsedLabel(start: start, running: true),
      ];
    } else {
      // turn-boundary.ts: forking pins the turn's cursor (hosts with
      // agentForkContextCursor) or its message id; without either, no fork.
      final canFork = !isSubagent &&
          ((_forkCursorSupported && c.epoch != null && entry['seqEnd'] != null) ||
              jsonMap(entry['item'])['messageId'] != null);
      children = [
        Transform.translate(
          offset: const Offset(-4, 0),
          child: TurnCopyButton(
            getContent: () => collectAssistantResponseContent(rows, host!),
          ),
        ),
        if (canFork)
          AssistantForkMenuButton(onFork: (target) => _fork(entry, target)),
        _ElapsedLabel(start: timing?.startedAt, end: timing?.completedAt),
      ];
    }
    // TurnFooterRow: streamItemWrapper (contentMaxWidth, 8 inset) 13 below the
    // row above; the slot keeps 24px and 32 below; running content 12 apart,
    // completed 8 apart.
    return Padding(
      padding: EdgeInsets.symmetric(horizontal: listInset),
      child: Align(
        alignment: Alignment.center,
        child: ConstrainedBox(
          constraints: BoxConstraints(
            maxWidth: PaseoTypography.of(context).contentMaxWidth,
          ),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(8, 13, 8, 32),
            // turnFooterSlot: minHeight 24; a wrapped end time overflows its
            // label box (an absolute overlay) without growing the slot.
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: 24),
              child: Row(
                spacing: entry == null ? 12 : 8,
                children: children,
              ),
            ),
          ),
        ),
      ),
    );
  }

  Widget _timelineRow(
    List<StreamRow> rows,
    int i, {
    JsonMap? previousTasks,
    bool footerBelow = false,
    bool streaming = false,
    bool live = false,
  }) {
    final row = rows[i];
    final entry = row.entry;
    final below = i + 1 < rows.length ? rows[i + 1].entry : null;
    final kind = row.kind;
    final seq = entry['seqStart'];
    final c = controller!;
    final find = _chrome.find;
    final highlight = find != null && seq != null && highlighted == '$seq';
    final capabilities = jsonMap(c.agent['capabilities']);
    // ChatFindExpansion: only the message find has selected renders past the
    // desktop render cap; it is capped again once the selection moves.
    final findExpanded = seq is int && find?.open == true && find?.selectedRow == seq;
    final deps = <Object?>[
      entry,
      below,
      previousTasks,
      footerBelow,
      streaming && kind == 'reasoning',
      live,
      expandedTools,
      findExpanded,
      highlight,
      find,
      widget.target,
      c,
      ...?row.group,
    ];
    final cached = seq is int ? _rowWidgets[seq] : null;
    if (cached != null &&
        listEquals(cached.deps, deps) &&
        const DeepCollectionEquality().equals(cached.capabilities, capabilities)) {
      return cached.widget;
    }
    Widget item(ChatFindTarget? findTarget) => TimelineItem(
      entry: entry,
      target: widget.target,
      toolGroup: row.group,
      groupSource: row.group == null ? null : c,
      // Re-read outside this build (from the open group sheet): the run is
      // located by id in the overview grouping of the current timeline.
      resolveGroup: row.group == null
          ? null
          : (id) => mounted
              ? buildStreamRows(
                  _streamEntries(c.timeline),
                  overview: true,
                  provider: stringValue(c.agent['provider']),
                ).where((candidate) => candidate.groupId == id).firstOrNull?.group
              : null,
      capabilities: capabilities,
      expandedTools: expandedTools,
      previousTasks: previousTasks,
      onRewind: isSubagent ? null : _rewind,
      streaming: streaming && kind == 'reasoning',
      live: live,
      renderFullContent: findExpanded,
      find: findTarget,
      // layout.ts: no gap above a completed footer; assistant rows switch to
      // compactBottom when any footer follows directly.
      gapBelow: footerBelow && below != null ? 0 : gapBetweenStreamItems(entry, below),
      footerBelow: footerBelow,
    );
    // chat-find/index.web.tsx: the selected row highlights its current match
    // (statusWarning on surface0); stepping within the message re-renders it.
    final Widget child;
    if (highlight) {
      child = ListenableBuilder(
        listenable: find,
        builder: (context, _) => item(find.highlight),
      );
    } else {
      child = item(null);
    }
    final built = KeyedSubtree(
      key: seq is int ? rowKeys.putIfAbsent(seq, () => GlobalKey()) : null,
      child: child,
    );
    if (seq is int) {
      _rowWidgets[seq] = (deps: deps, capabilities: capabilities, widget: built);
    }
    return built;
  }

  final _autocompletePortal = OverlayPortalController()..show();
  final _composerLink = LayerLink();
  final _inputWrapperKey = GlobalKey();

  bool get _native =>
      defaultTargetPlatform == TargetPlatform.android ||
      defaultTargetPlatform == TargetPlatform.iOS;

  /// index.tsx resolveComposerButtonIconSize: 20 on native, 16 on web.
  double get _buttonIconSize => _native ? 20 : 16;

  /// index.tsx resolveMessagePlaceholder: the long desktop placeholder only
  /// on a wide non-native layout.
  String _placeholder(BuildContext context) => tr(
    context,
    !_native && !isCompactLayout(context)
        ? 'composer.placeholders.desktop'
        : 'composer.placeholders.mobile',
  );

  /// input.tsx textInput: content size, normal weight, foreground. The native
  /// app gives the text no lineHeight (the font's natural line box); web
  /// uses 1.4. `inherit: false` keeps TextField's themed height out.
  TextStyle _composerTextStyle(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final body = Theme.of(context).textTheme.bodyLarge!;
    final fontSize = PaseoTypography.of(context).contentFontSize;
    return _native
        ? TextStyle(
            inherit: false,
            fontFamily: body.fontFamily,
            fontFamilyFallback: body.fontFamilyFallback,
            fontSize: fontSize,
            fontWeight: FontWeight.normal,
            letterSpacing: body.letterSpacing,
            color: colors.onSurface,
            textBaseline: TextBaseline.alphabetic,
            decoration: TextDecoration.none,
          )
        : body.copyWith(fontSize: fontSize, height: 1.4, color: colors.onSurface);
  }

  /// index.tsx sendErrorText: red-500, base size, announced as an alert.
  Widget _sendErrorText(BuildContext context) => Semantics(
    liveRegion: true,
    child: Text(
      sendError!,
      style: Theme.of(context).textTheme.bodyMedium!.copyWith(color: const Color(0xffef4444)),
    ),
  );

  /// composer/index.tsx ComposerContent: inputAreaContainer (16px sides and
  /// bottom, centred, contentMaxWidth) holding inputAreaContent (12px gaps):
  /// the queue track, the send error, then the message input wrapper — the
  /// attachment tray, the text surface and the toolbar row 12px apart inside a
  /// surface1 card (1px borderAccent, 16px radius; 8/12/8 insets on narrow
  /// windows, 16/16/12 on wide ones; small shadow on compact layouts). The
  /// slash/@ autocomplete floats 12px above the wrapper at its width.
  Widget _composer() {
    // A dequeued (sending) message leaves the queue track; the stream shows
    // it as the pending user row until the daemon echoes it.
    final queue = maps(draft['queue']).where((q) => q['state'] != 'sending').toList();
    final attachments = maps(draft['attachments']),
        images = maps(draft['images']);
    final pending = jsonMap(draft['pending']);
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final small = Theme.of(context).textTheme.bodySmall!.copyWith(color: colors.onSurfaceVariant);
    final narrow = MediaQuery.sizeOf(context).width < 720;
    final top = narrow ? 8.0 : 16.0, bottom = narrow ? 8.0 : 12.0, side = narrow ? 12.0 : 16.0;
    const overhang = AttachmentPill.removableOverhang;
    final hasTray = attachments.isNotEmpty || images.isNotEmpty || uploading;
    final hasAgent = !isDraft && controller?.agent['status'] != null;
    final wrapper = Container(
      key: _inputWrapperKey,
      decoration: BoxDecoration(
        color: palette.surface1,
        border: Border.all(color: palette.borderAccent),
        borderRadius: BorderRadius.circular(16),
        boxShadow: isCompactLayout(context)
            ? const [BoxShadow(color: Color(0x40000000), offset: Offset(0, 2), blurRadius: 4)]
            : null,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        spacing: 12,
        children: [
          if (hasTray)
            // index.tsx attachmentTray: a wrapping row, 8px gaps. Removable
            // pills carry their 8px disc overhang, so the tray starts 8px out.
            Padding(
              padding: EdgeInsets.fromLTRB(side - overhang, top - overhang, side, 0),
              child: Wrap(
                children: [
                  for (var i = 0; i < images.length; i++)
                    AttachmentPill(
                      key: ValueKey('image:${images[i]['id'] ?? i}'),
                      disabled: sending,
                      openLabel: tr(context, 'composer.attachments.openImage'),
                      removeLabel: tr(context, 'composer.attachments.removeImage'),
                      onOpen: () => _imagePreview(images[i]),
                      onRemove: () => drafts.update(draftKey, {
                        'images': List.of(images)..removeAt(i),
                      }),
                      child: AttachmentPill.thumbnail(context, images[i]),
                    ),
                  for (var i = 0; i < attachments.length; i++)
                    _attachmentPill(attachments, i),
                  // index.tsx pendingFiles: an uploading file shows as a
                  // frame with an 18px spinner, its name and type.
                  if (uploading)
                    Padding(
                      padding: const EdgeInsets.only(top: overhang, left: overhang),
                      child: AttachmentPill(
                        child: AttachmentPill.pendingLabel(context, uploadName ?? ''),
                      ),
                    ),
                ],
              ),
            ),
          Padding(
            padding: EdgeInsets.fromLTRB(side, hasTray ? 0 : top, side, 0),
            child: _composerTextSurface(),
          ),
          // input.tsx buttonRow: bleeds 6px on both sides so the 28px glyph
          // buttons sit on the wrapper's corner inset; bottom-aligned.
          Padding(
            padding: EdgeInsets.fromLTRB(side - 6, 0, side - 6, bottom),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.end,
              children: [
                Expanded(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      _attachButton(),
                      Flexible(child: _agentControls()),
                    ],
                  ),
                ),
                Row(
                  mainAxisSize: MainAxisSize.min,
                  spacing: 4,
                  children: [
                    if (hasAgent)
                      ContextWindowMeter(
                        usage: jsonMap(controller?.agent['lastUsage']),
                        client: client,
                        agentId: stringValue(widget.target.id),
                        glyphSize: isCompactLayout(context) ? 16 : _buttonIconSize,
                      ),
                    // The primary action follows the editor itself (input.tsx
                    // hasLiveText); keystrokes rebuild only this control.
                    ListenableBuilder(
                      listenable: editor,
                      builder: (context, _) => _sendControl(),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
    return DropTarget(
      // useFileDrop is disabled while a submit or upload is in flight.
      onDragDone: (details) async {
        if (sending || uploading) return;
        await _attachFiles(details.files, drop: true);
      },
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
        child: Center(
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: PaseoTypography.of(context).contentMaxWidth),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              spacing: 12,
              children: [
                if (queue.isNotEmpty)
                  ComposerQueueTrack(
                    queue: queue,
                    sending: sending,
                    onEdit: _editQueued,
                    onSendNow: _queueNow,
                    onRemove: _removeQueued,
                  ),
                if (sendError != null) _sendErrorText(context),
                // Client-only durability states the original never reaches.
                if (jsonList(draft['unresolvedLegacyAttachments']).isNotEmpty)
                  Row(
                    spacing: 8,
                    children: [
                      Icon(LucideIcons.info300, size: 14, color: colors.onSurfaceVariant),
                      Expanded(
                        child: Text('Some original attachments need recovery before sending.',
                            style: small),
                      ),
                      TextButton(onPressed: _reviewLegacyAttachments, child: const Text('Review')),
                    ],
                  ),
                if (pending.isNotEmpty && !sending)
                  Row(
                    spacing: 8,
                    children: [
                      Expanded(
                        child: Text('Send is awaiting acknowledgement. Draft retained.',
                            style: small),
                      ),
                      TextButton(
                        onPressed: sending ? null : () => _submit(retry: pending),
                        child: const Text('Retry same message'),
                      ),
                    ],
                  ),
                OverlayPortal(
                  controller: _autocompletePortal,
                  overlayChildBuilder: _autocompleteOverlay,
                  child: CompositedTransformTarget(link: _composerLink, child: wrapper),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// components/ui/autocomplete-popover.tsx: anchored to the input wrapper's
  /// top-left, 12px above it, as wide as it, never taller than the space up
  /// to the safe area (less 24px).
  Widget _autocompleteOverlay(BuildContext overlayContext) => ListenableBuilder(
    listenable: autocomplete,
    builder: (context, _) {
      final box = _inputWrapperKey.currentContext?.findRenderObject() as RenderBox?;
      if (!autocomplete.visible || _fullscreenOpen || box == null || !box.hasSize) {
        return const SizedBox.shrink();
      }
      final top = box.localToGlobal(Offset.zero).dy;
      final room = top - MediaQuery.paddingOf(context).top - 24;
      if (room <= 0) return const SizedBox.shrink();
      return Positioned(
        left: 0,
        top: 0,
        child: CompositedTransformFollower(
          link: _composerLink,
          showWhenUnlinked: false,
          targetAnchor: Alignment.topLeft,
          followerAnchor: Alignment.bottomLeft,
          offset: const Offset(0, -12),
          child: SizedBox(
            width: box.size.width,
            child: ConstrainedBox(
              constraints: BoxConstraints(maxHeight: room),
              child: AutocompletePopover(
                autocomplete: autocomplete,
                onSelect: _applyAutocomplete,
              ),
            ),
          ),
        ),
      );
    },
  );

  /// composer/index.tsx renderComposerAttachmentPill for non-image kinds:
  /// forge items open their page; uploaded and workspace files have no open
  /// action.
  Widget _attachmentPill(List<JsonMap> attachments, int i) {
    final attachment = attachments[i];
    final forge = AttachmentPill.forgeLabels(attachment);
    final name = AttachmentPill.describe(attachment).$1;
    return AttachmentPill(
      key: ValueKey('attachment:$i:${attachment['path'] ?? attachment['number'] ?? name}'),
      disabled: sending,
      openLabel: forge != null
          ? tr(context, 'composer.attachments.openGithub',
              args: {'kind': forge.kind, 'number': forge.number})
          : name,
      removeLabel: forge != null
          ? tr(context, 'composer.attachments.removeGithub',
              args: {'kind': forge.kind, 'number': forge.number})
          : tr(context, 'composer.attachments.removeFile'),
      onOpen: forge != null ? () => _openForgeAttachment(attachment) : null,
      onRemove: () {
        // markForgeAttachmentRemoved: a removed reference never re-attaches.
        _forgeAutoAttach.markRemoved(attachment);
        drafts.update(draftKey, {
          'attachments': List.of(attachments)..removeAt(i),
        });
      },
      child: AttachmentPill.label(context, attachment),
    );
  }

  /// input.tsx AttachmentDropdown: a 28px round glyph (muted Plus at the
  /// button icon size, foreground over surface2 when hovered), disabled while
  /// disconnected; its menu opens above on desktop (220 wide) and as a titled
  /// sheet on compact layouts with composer/index.tsx attachmentMenuItems:
  /// Add image, Paste image (native only), Add issue or PR (forge mark),
  /// Upload file — 16px muted icons.
  Widget _attachButton() {
    final colors = Theme.of(context).colorScheme;
    final palette = PaseoColors.of(context);
    final label = tr(context, 'composer.input.addAttachment');
    final disabled = client.status != ConnectionStatus.connected;
    final button = Builder(
      builder: (anchor) => ComposerHoverGlyph(
        disabled: disabled,
        hoverColor: palette.surface2,
        semanticsLabel: label,
        onPressed: () async {
          _attachAnchor = paseoAnchorRect(anchor);
          _ensureForge();
          final action = await showPaseoMenu<String>(
            context,
            title: label,
            anchor: _attachAnchor,
            entries: [
              PaseoMenuEntry.item(
                value: 'image',
                label: tr(context, 'composer.attachments.addImage'),
                icon: LucideIcons.image300,
              ),
              if (_native)
                PaseoMenuEntry.item(
                  value: 'paste',
                  label: tr(context, 'composer.attachments.pasteImage'),
                  icon: LucideIcons.clipboardPaste300,
                ),
              PaseoMenuEntry.item(
                value: 'forge',
                label: tr(context, _forge == 'gitlab'
                    ? 'composer.attachments.addIssueOrPr_mr'
                    : 'composer.attachments.addIssueOrPr'),
                // renderForgeAttachmentIcon: the forge's brand mark (md, muted).
                leading: ForgeBrandIcon(forge: _forge, size: 16, color: colors.onSurfaceVariant),
              ),
              PaseoMenuEntry.item(
                value: 'file',
                label: tr(context, 'composer.attachments.addFile'),
                icon: LucideIcons.paperclip300,
              ),
            ],
          );
          if (!mounted) return;
          switch (action) {
            case 'image': _pickFiles(images: true); break;
            case 'paste': _paste(imagesOnly: true); break;
            case 'forge': _pickForge(); break;
            case 'file': _pickFiles(); break;
          }
        },
        builder: (hovered) => Icon(
          LucideIcons.plus300,
          size: _buttonIconSize,
          color: hovered ? colors.onSurface : colors.onSurfaceVariant,
        ),
      ),
    );
    return ComposerTooltip(label: label, child: button);
  }

  /// input.tsx textInput: content-size text up to 160px tall (on wide
  /// layouts up to half the window) with a surface4 placeholder; native
  /// inputs are at least MIN_INPUT_HEIGHT_MOBILE 30 tall. On compact layouts
  /// the text reserves 36px on the right (compactTextInput) for
  /// presentation.tsx's 16px Maximize2 ghost glyph, shown once the draft
  /// renders more than two lines and placed on the send button's rail (the
  /// toolbar's 6px bleed plus resolveGlyphRailOffset).
  Widget _composerTextSurface() {
    final compact = isCompactLayout(context);
    final colors = Theme.of(context).colorScheme;
    final style = _composerTextStyle(context);
    const glyphReserve = 36.0;
    final vertical = _native ? 6.0 : 4.0;
    final maxHeight = compact
        ? 160.0
        : (MediaQuery.sizeOf(context).height * .5).floorToDouble().clamp(160.0, double.infinity).toDouble();
    final field = ConstrainedBox(
      constraints: BoxConstraints(minHeight: _native ? 30 : 0, maxHeight: maxHeight),
      child: TextField(
        controller: editor,
        // workspace-tab.tsx: the draft composer is disabled while it creates.
        readOnly: isDraft && sending,
        focusNode: focus,
        minLines: compact ? 1 : 2,
        maxLines: null,
        keyboardType: TextInputType.multiline,
        style: style,
        textAlignVertical: _native ? TextAlignVertical.center : null,
        decoration: InputDecoration(
          hintText: _placeholder(context),
          hintStyle: TextStyle(color: PaseoColors.of(context).surface4),
          filled: false,
          isDense: true,
          contentPadding: EdgeInsets.fromLTRB(0, vertical, compact ? glyphReserve : 0, vertical),
          border: InputBorder.none,
          enabledBorder: InputBorder.none,
          focusedBorder: InputBorder.none,
        ),
      ),
    );
    if (!compact) {
      if (_native) return field;
      // input.tsx FocusHint: while the empty input is unfocused, a 12px muted
      // half-opacity "⌘L to focus" sits at its top right.
      return ListenableBuilder(
        listenable: Listenable.merge([focus, editor]),
        builder: (context, child) => Stack(
          children: [
            child!,
            if (!focus.hasFocus && editor.text.isEmpty)
              Positioned(
                top: 0,
                right: 0,
                child: IgnorePointer(
                  child: FutureBuilder<List<ShortcutBinding>>(
                    future: ShortcutCatalog.load(),
                    builder: (context, snapshot) {
                      final combo = snapshot.data == null
                          ? null
                          : resolveShortcutCombo(
                              snapshot.data!, 'focus-message-input', shortcutOverrides(context));
                      if (combo == null) return const SizedBox.shrink();
                      return Opacity(
                        opacity: .5,
                        child: Text(
                          tr(context, 'composer.input.focusHint',
                              args: {'shortcut': formatShortcutStep(combo.split(' ').first)}),
                          style: Theme.of(context).textTheme.bodySmall!.copyWith(
                            color: Theme.of(context).colorScheme.onSurfaceVariant,
                          ),
                        ),
                      );
                    },
                  ),
                ),
              ),
          ],
        ),
        child: field,
      );
    }
    final glyph = _buttonIconSize;
    // state.ts resolveGlyphRailOffset: the send arrow's ink inset
    // ((28 - glyph) / 2 + 5 * glyph / 24) minus Maximize2's
    // ((28 - 16) / 2 + 3 * 16 / 24).
    final railOffset = (28 - glyph) / 2 + 5 * glyph / 24 - ((28 - 16) / 2 + 3 * 16 / 24);
    // The line count follows the editor itself; keystrokes rebuild only this.
    return ListenableBuilder(
      listenable: editor,
      builder: (context, _) => LayoutBuilder(
      builder: (context, constraints) {
        final painter = TextPainter(
          text: TextSpan(text: editor.text, style: style),
          textDirection: Directionality.of(context),
          textScaler: MediaQuery.textScalerOf(context),
        )..layout(maxWidth: (constraints.maxWidth - glyphReserve).clamp(0.0, double.infinity));
        final lines = editor.text.isEmpty ? 1 : painter.computeLineMetrics().length;
        painter.dispose();
        return Stack(
          clipBehavior: Clip.none,
          children: [
            field,
            if (lines > 2)
              Positioned(
                top: 0,
                right: -6 + railOffset,
                child: Semantics(
                  button: true,
                  label: tr(context, 'composer.input.fullscreen'),
                  child: Material(
                    type: MaterialType.transparency,
                    child: InkWell(
                      customBorder: const CircleBorder(),
                      onTap: _fullscreenEditor,
                      child: SizedBox.square(
                        dimension: 28,
                        child: Icon(LucideIcons.maximize2300, size: 16, color: colors.onSurfaceVariant),
                      ),
                    ),
                  ),
                ),
              ),
          ],
        );
      },
      ),
    );
  }

  /// The conversation's actions as (value, label), offered from its tab menu.
  List<(String, String)> _conversationActions() {
    (String, String) item(String value, String key) => (value, tr(context, key));
    return [
      // Native has no chat find or outline (chat-find/index.tsx, rail.tsx).
      if (!isDraft && !isSubagent && !_nativeStream) ...[
        item('find', 'paneFind.title'),
        if (PaseoScope.of(context).preferences['chatOutlineEnabled'] != false)
          item('outline', 'settings.appearance.chatOutline.title'),
      ],
      item('tools', expandedTools ? 'agentPanel.menu.collapseTools' : 'agentPanel.menu.expandTools'),
      if (!isDraft) item('sync', 'agentPanel.menu.syncHistory'),
      if (!isDraft && !isSubagent) ...[
        item('rename', 'workspace.tabs.menu.renameAgent'),
        item('fork', 'agentPanel.menu.fork'),
        item(
          'refresh',
          controller?.agent['archivedAt'] != null
              ? 'agentPanel.archived.unarchive'
              : 'workspace.tabs.menu.reloadAgent',
        ),
        item('usage', 'sidebar.footer.usage'),
        item('detach', 'subagents.detachTooltip'),
        item('archive', 'agentList.archiveSheet.archive'),
        item('delete', 'workspace.fileActions.delete'),
      ],
    ];
  }

  void _conversationAction(String action) {
    switch (action) {
      // chat-find (pane-find widget) and the chat outline's prompt index.
      case 'find': _openFind(); break;
      case 'outline': unawaited(_showOutline()); break;
      case 'tools': setState(() => expandedTools = !expandedTools); break;
      case 'sync': controller?.reload(); break;
      default: _lifecycle(action); break;
    }
  }


  List<JsonMap> get _modes {
    if (!isDraft) return maps(controller?.agent['availableModes']);
    return maps(providers
        .where((provider) => provider['provider'] == _config()['provider'])
        .firstOrNull?['modes']);
  }

  Widget _agentControls() => AgentControls(
    providers: providers,
    config: _config(),
    modes: _modes,
    features: isDraft ? draftFeatures : maps(controller?.agent['features']),
    profiles: profiles,
    canSwitchProvider: isDraft,
    disabled: !initialized || (isDraft && sending),
    onChanged: _saveConfig,
    onApplyProfile: _applyProfile,
    loadFeatures: isDraft ? _loadDraftFeatures : null,
    live: _providerState,
    onOpen: _refetchIfLoading,
    onEditProfiles: _editProfiles,
    onCreateProfile: _profilesSupported ? (seed) => _openProfileEditor(seed: seed) : null,
    onEditProfile: _profilesSupported ? (profile) => _openProfileEditor(profile: profile) : null,
    onProviderSettings: _openProviderSettings,
  );

  Future<void> _refreshDraftFeatures() async {
    try {
      final features = await _loadDraftFeatures();
      if (mounted && stringValue(_config()['provider']).isEmpty) {
        setState(() => draftFeatures = features);
      }
    } catch (e) {
      _showFailure(e);
    }
  }

  /// use-draft-agent-features.ts: the features for the draft's effective
  /// model, mode and thinking option, showing the draft's own values over
  /// the remembered ones.
  Future<List<JsonMap>> _loadDraftFeatures([JsonMap? config]) async {
    final value = config ?? _config();
    final provider = stringValue(value['provider']);
    if (provider.isEmpty || client.status != ConnectionStatus.connected) {
      return const [];
    }
    final effective = effectiveSubmitConfig(value, providers)..remove('featureValues');
    final result = requireSuccess(await client.request(
      'list_provider_features_request',
      {'draftConfig': {...effective, 'cwd': widget.target.cwd}},
      responseType: 'list_provider_features_response',
    ));
    if (!mounted) return const [];
    final offered = maps(result['features']);
    final features = applyFeatureValues(
      offered,
      resolveFeatureValues(
        offered,
        rememberedFeatureValues(_formPreferences, provider),
        jsonMap(value['featureValues']),
      ),
    );
    if (isDraft && jsonEncode(value) == jsonEncode(_config())) {
      setState(() => draftFeatures = features);
    }
    return features;
  }

  bool _cancelling = false;

  /// actions.ts cancelComposerAgent + index.tsx handleCancelAgent: only a
  /// running, connected agent that is not already cancelling is cancelled;
  /// the input takes focus and a failure with a message toasts.
  Future<void> _cancelAgent() async {
    if (isDraft ||
        controller?.running != true ||
        _cancelling ||
        client.status != ConnectionStatus.connected) {
      return;
    }
    setState(() => _cancelling = true);
    focus.requestFocus();
    try {
      await controller!.action(
        'cancel_agent_request',
        {},
        responseType: 'cancel_agent_response',
      );
    } catch (e) {
      final message = errorMessage(e);
      if (mounted && message.trim().isNotEmpty) {
        showToast(context, message, variant: ToastVariant.error);
      }
    } finally {
      if (mounted) setState(() => _cancelling = false);
    }
  }

  /// input.tsx hasLiveText (updateLiveTextPresence from handleInputChange):
  /// every edit the native input reports, composing text included — RN's
  /// Android TextWatcher publishes composing spans through onChangeText — so
  /// any typed text turns a running agent's Stop into Send/steer.
  bool get _hasLiveText => editor.text.trim().isNotEmpty;

  /// input.tsx PrimaryAction (resolvePrimaryActionKind): Send with sendable
  /// content, the cancel button while the agent runs, Send (spinner) while a
  /// submit loads, else nothing. While loading during a run the spinner
  /// button interrupts the agent. Labels follow input/labels.ts
  /// resolveSubmitAccessibilityLabel and resolveSendTooltipLabel.
  Widget _sendControl() {
    final running = controller?.running == true;
    final hasMessage = _hasLiveText ||
        maps(draft['attachments']).isNotEmpty || maps(draft['images']).isNotEmpty;
    final loading = sending || uploading;
    final glyph = _buttonIconSize;
    if (!hasMessage && running) {
      return Padding(
        padding: const EdgeInsets.only(left: 4),
        child: ComposerCancelButton(
          glyph: glyph,
          cancelling: _cancelling,
          label: tr(context, _cancelling ? 'composer.cancel.cancelingAgent' : 'composer.cancel.stopAgent'),
          tooltip: tr(context, 'composer.cancel.interrupt'),
          onPressed: client.status == ConnectionStatus.connected ? _cancelAgent : null,
        ),
      );
    }
    if (!hasMessage && !loading) return const SizedBox.shrink();
    final canPressLoading = loading && running;
    final queues = behavior == 'queue' && running;
    final disabled = !canPressLoading &&
        (loading || !initialized || jsonList(draft['unresolvedLegacyAttachments']).isNotEmpty);
    return Padding(
      padding: const EdgeInsets.only(left: 4),
      child: ComposerSendButton(
        glyph: glyph,
        loading: loading,
        label: tr(
          context,
          canPressLoading
              ? 'composer.input.interruptAgent'
              : queues
              ? 'composer.input.queueMessage'
              : running
              ? (behavior == 'steer' ? 'composer.input.sendAndSteer' : 'composer.input.sendAndInterrupt')
              : 'composer.input.sendMessage',
        ),
        tooltip: tr(context, queues ? 'composer.input.queue' : 'composer.input.send'),
        onPressed: disabled ? null : canPressLoading ? _cancelAgent : () => _submit(),
      ),
    );
  }

  Future<void> _reviewLegacyAttachments() async {
    final key = draftKey;
    var recovering = false;
    await showDialog<void>(
      context: context,
      builder: (dialogContext) => StatefulBuilder(
        builder: (context, setDialogState) => AlertDialog(
          title: const Text('Recover original attachments'),
          content: SizedBox(
            width: 600,
            height: 360,
            child: AnimatedBuilder(
              animation: drafts,
              builder: (context, _) => ListView(
                children: [
                  const Text('The original draft and attachment references are retained. Local files can be retried. Browser-store images or excluded plugin resources can be exported from the original app and attached manually. No source files are deleted.'),
                  const SizedBox(height: 12),
                  for (final value in jsonList(drafts.draft(key)['unresolvedLegacyAttachments']))
                    ExpansionTile(
                      title: Text(stringValue(jsonMap(value)['reason'])),
                      children: [JsonDetails(value: jsonMap(value)['source'])],
                    ),
                ],
              ),
            ),
          ),
          actions: [
            TextButton(
              onPressed: recovering ? null : () async {
                setDialogState(() => recovering = true);
                try {
                  await drafts.recoverLegacyAttachments(
                    key,
                    legacyForge: jsonMap(client.serverInfo['features'])['forgeAttachments'] == false,
                  );
                } catch (e) {
                  _showFailure(e);
                } finally {
                  if (dialogContext.mounted) setDialogState(() => recovering = false);
                }
              },
              child: Text(recovering ? 'Recovering…' : 'Retry local files'),
            ),
            TextButton(
              onPressed: recovering ? null : () async {
                if (await confirmAction(
                  context,
                  title: 'Remove unresolved attachments from this draft?',
                  message: 'Only the unresolved references in this composer will be removed. The original imported draft and source files remain preserved.',
                )) {
                  drafts.update(key, {'unresolvedLegacyAttachments': []});
                  if (dialogContext.mounted) Navigator.pop(dialogContext);
                }
              },
              child: const Text('Remove from this draft'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(dialogContext),
              child: const Text('Close'),
            ),
          ],
        ),
      ),
    );
  }

  @override
  void dispose() {
    // agent-panel.tsx unmount: leaving the focused agent clears its attention.
    if (_attentionFocused) _attention?.clearOnAgentBlur();
    _removeCloseGuard?.call();
    ConversationMenus.unregister(widget.target.key, this);
    _appLifecycle?.dispose();
    _removeCommands?.call();
    autocomplete.dispose();
    _forgeAutoAttach.dispose();
    drafts.removeListener(_draftChanged);
    editor.removeListener(_editorChanged);
    focus.removeListener(_inputFocusChanged);
    fullscreenFocus.removeListener(_inputFocusChanged);
    editor.dispose();
    focus.dispose();
    fullscreenFocus.dispose();
    scroll.dispose();
    _chrome.dispose();
    _providerUpdates?.cancel();
    _providerState.dispose();
    controller?.removeListener(_changed);
    controller?.dispose();
    super.dispose();
  }
}

/// message.tsx LiveElapsed (running) and AssistantTurnFooter label
/// (completed): "Worked for 2m 12s", swapping to the end timestamp on hover
/// or for 3s after a tap.
class _ElapsedLabel extends StatefulWidget {
  const _ElapsedLabel({this.start, this.end, this.running = false});
  final DateTime? start, end;
  final bool running;
  @override
  State<_ElapsedLabel> createState() => _ElapsedLabelState();
}

class _ElapsedLabelState extends State<_ElapsedLabel>
    with IndicatorClockState<_ElapsedLabel> {
  Timer? _reveal;
  bool _hovered = false, _revealed = false;
  late String _shown = _runningText();

  @override
  bool get indicatorActive => widget.running;

  @override
  void didUpdateWidget(covariant _ElapsedLabel oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.running != widget.running) syncIndicatorClock();
  }

  String _runningText() {
    final start = widget.start;
    return start == null ? '' : formatDuration(DateTime.now().difference(start));
  }

  /// Rides the shared indicator clock, so the seconds change in the same frame
  /// as the working dots instead of on a frame of their own.
  @override
  void onIndicatorTick() {
    final next = _runningText();
    if (next != _shown) setState(() => _shown = next);
  }

  static String formatDuration(Duration duration) {
    final totalSeconds = duration.isNegative ? 0 : duration.inSeconds;
    if (totalSeconds < 60) return '${totalSeconds}s';
    final minutes = totalSeconds ~/ 60;
    if (minutes < 60) {
      final seconds = totalSeconds % 60;
      return seconds == 0 ? '${minutes}m' : '${minutes}m ${seconds}s';
    }
    final hours = minutes ~/ 60, rest = minutes % 60;
    return rest == 0 ? '${hours}h' : '${hours}h ${rest}m';
  }

  @override
  Widget build(BuildContext context) {
    final style = TextStyle(
      fontSize: 13,
      color: Theme.of(context).colorScheme.onSurfaceVariant,
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    final start = widget.start, end = widget.end;
    if (widget.running) {
      _shown = _runningText();
      return start == null ? const SizedBox.shrink() : Text(_shown, style: style);
    }
    final duration = start != null && end != null
        ? tr(context, 'message.turnFooter.workedFor', args: {
            'duration': formatDuration(end.difference(start)),
          })
        : '';
    final timestamp = end == null ? '' : formatMessageTimestamp(context, end);
    final primary = duration.isNotEmpty ? duration : timestamp;
    if (primary.isEmpty) return const SizedBox.shrink();
    final canSwap = duration.isNotEmpty && timestamp.isNotEmpty;
    final showTimestamp = canSwap && (_hovered || _revealed);
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: GestureDetector(
        onTap: canSwap
            ? () {
                _reveal?.cancel();
                setState(() => _revealed = !_revealed);
                _reveal = Timer(const Duration(seconds: 3), () {
                  if (mounted) setState(() => _revealed = false);
                });
              }
            : null,
        // container: its own node sized to the label; a plain Semantics
        // merges into the enclosing node and reports the whole row.
        child: Semantics(
          container: true,
          label: canSwap ? '$duration, ended $timestamp' : primary,
          excludeSemantics: true,
          // message.tsx AssistantTurnFooter: an invisible sizer holds
          // whichever label has more characters (String length, as JS
          // compares) and the shown label overlays it at that width — so a
          // "Worked for 1h 5m" box wraps the wider end time, while a
          // "Worked for 5s" box is sized by the end time and keeps one line.
          child: Stack(
            clipBehavior: Clip.none,
            children: [
              Opacity(
                opacity: 0,
                child: Text(
                  primary.length >= timestamp.length ? primary : timestamp,
                  style: style,
                ),
              ),
              Positioned(
                left: 0,
                top: 0,
                right: 0,
                child: Text(showTimestamp ? timestamp : primary, style: style),
              ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  void dispose() {
    _reveal?.cancel();
    super.dispose();
  }
}

/// components/synced-loader.tsx: a 2x3 dot grid stepping through six opacity
/// states every 950ms, synchronized to wall-clock time across loaders and
/// sampled on [IndicatorClock].
class _WorkingDots extends StatefulWidget {
  const _WorkingDots({required this.color});
  final Color color;
  @override
  State<_WorkingDots> createState() => _WorkingDotsState();
}

class _WorkingDotsState extends State<_WorkingDots>
    with IndicatorClockState<_WorkingDots> {
  static const _durationMs = 950;
  static const _states = [
    [1, 0, .78, 0, .56, .34],
    [.78, 1, .56, 0, .34, 0],
    [.56, .78, .34, 1, 0, 0],
    [.34, .56, 0, .78, 0, 1],
    [0, .34, 0, .56, 1, .78],
    [0, 0, 1, .34, .78, .56],
  ];
  late int step = _step();

  static int _step() =>
      (DateTime.now().millisecondsSinceEpoch % _durationMs) * 6 ~/ _durationMs;

  @override
  bool get pausesForReducedMotion => true;

  @override
  void onIndicatorTick() {
    final next = _step();
    if (next != step) setState(() => step = next);
  }

  @override
  Widget build(BuildContext context) {
    const size = 14.0;
    final gap = (size * .12).roundToDouble().clamp(1.0, size);
    final dot = ((size - gap * 2) / 3).clamp(2.0, size);
    return SizedBox.square(
      dimension: size,
      child: Center(
        child: SizedBox(
          width: dot * 2 + gap,
          height: dot * 3 + gap * 2,
          child: Stack(
            children: [
              for (var index = 0; index < 6; index++)
                Positioned(
                  left: index % 2 * (dot + gap),
                  top: index ~/ 2 * (dot + gap),
                  child: Container(
                    width: dot,
                    height: dot,
                    decoration: BoxDecoration(
                      shape: BoxShape.circle,
                      color: widget.color.withValues(
                        alpha: _states[step][index].toDouble(),
                      ),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
