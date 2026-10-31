import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:collection/collection.dart' show DeepCollectionEquality;
import 'package:flutter/widgets.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:uuid/uuid.dart';

import 'client.dart';
import 'host_config.dart';
import 'json.dart';
import 'legacy_state.dart';
import 'preferences.dart';
import 'target.dart';
import 'transport/connection_selection.dart';
export 'preferences.dart';

enum CompactDestination { agent, sidebar, explorer }

sealed class LayoutNode {
  const LayoutNode();
  JsonMap toJson();
  Iterable<PanelPane> get panes;
}

class PanelPane extends LayoutNode {
  PanelPane({required this.id, List<String>? tabKeys, this.selectedKey})
    : tabKeys = tabKeys ?? [];
  final String id;
  final List<String> tabKeys;
  String? selectedKey;
  @override
  Iterable<PanelPane> get panes => [this];
  @override
  JsonMap toJson() => {
    'kind': 'pane',
    'id': id,
    'tabs': tabKeys,
    'selected': selectedKey,
  };
}

class PanelSplit extends LayoutNode {
  PanelSplit({
    required this.id,
    required this.axis,
    required this.first,
    required this.second,
    this.ratio = 0.5,
  });
  final String id;
  final Axis axis;
  LayoutNode first;
  LayoutNode second;
  double ratio;
  @override
  Iterable<PanelPane> get panes => [...first.panes, ...second.panes];
  @override
  JsonMap toJson() => {
    'kind': 'split',
    'id': id,
    'axis': axis.name,
    'ratio': ratio,
    'first': first.toJson(),
    'second': second.toJson(),
  };
}

class HostState {
  HostState({
    required this.config,
    required this.client,
    String? serverId,
    this.originServerId,
  }) : _serverId = serverId ?? config.expectedServerId ?? config.id {
    connections.add(config);
  }
  HostConfig config;
  PaseoClient client;
  String _serverId;
  String get serverId => _serverId;
  String get label => config.label.isNotEmpty ? config.label : serverId;
  final String? originServerId;
  final List<HostConfig> connections = [];
  List<JsonMap> projects = [];
  List<JsonMap> workspaces = [];
  List<JsonMap> agents = [];
  bool loading = false;
  Object? error;
  DateTime? cachedAt;
  bool get live =>
      client.status == ConnectionStatus.connected && !loading && error == null;
  bool get cached =>
      !live &&
      (projects.isNotEmpty || workspaces.isNotEmpty || agents.isNotEmpty);
  final List<OwnedObservation> _observations = [];
  StreamSubscription<JsonMap>? _events;
  VoidCallback? _statusListener;
  ConnectionStatus? _lastStatus;
  Future<void>? _refresh;
  Timer? _resync;
  bool _hydrating = false;
  final List<JsonMap> _queuedEvents = [];
  final Map<String, String> _generations = {};
  final Map<String, int> _sequences = {};
  /// Original per-host `HostRuntimeController` probe cycle; absent for routed hosts.
  HostConnectionSelector? _selector;
}

class AppStore extends ChangeNotifier {
  AppStore({
    this.windowId = '0',
    this.connectHosts = true,
    this.discoverLocalHosts = true,
    this.legacyStateReader = readLegacyState,
  });
  final LegacyStateReader legacyStateReader;
  final String windowId;
  final bool connectHosts;
  final bool discoverLocalHosts;
  PaseoClient _newClient(HostConfig config) => PaseoClient(
    host: config,
    capabilities: const {
      'all_providers': true,
      'provider_subagents': true,
      'projected_subagent_timeline': true,
      'timeline_replacement_invalidation': true,
      'reasoning_merge_enum': true,
      'timeline_notifications': true,
      'workspace_setup_observation': true,
      'terminal_reflowable_snapshot': true,
    },
  );
  JsonMap _savedPreferences = {};
  JsonMap _savedHosts = {};
  final List<Future<void> Function(HostState)> _hostRemovalGuards = [];
  final List<HostState> hosts = [];
  AppPreferences preferences = AppPreferences();
  final List<PanelTarget> tabs = [];
  LayoutNode layout = PanelPane(id: 'main');
  String focusedPaneId = 'main';
  String? maximizedPaneId;
  final List<String> explorerTabKeys = [];
  String? selectedExplorerKey;
  /// Original per-workspace Explorer pane `focusedTabId`: the view each workspace's
  /// Explorer last showed, so returning to a workspace reopens that view.
  final Map<String, String> _explorerFocus = {};
  bool explorerVisible = false;
  double explorerWidth = 340;

  /// Original panel-store DEFAULT_SIDEBAR_WIDTH (320) within MIN/MAX 200–600.
  double sidebarWidth = 320;
  bool sidebarVisible = true;
  CompactDestination compactDestination = CompactDestination.agent;
  Object? storageError;
  bool initialized = false;
  bool _disposed = false;
  bool _storageWritable = true;
  Directory? _directory;
  JsonMap _retained = {};
  Future<void>? _initializing;
  Future<void> _writeTail = Future.value();

  /// Saves are content-addressed: any caller may request one, and only files
  /// whose contents differ from what this store last wrote or loaded are
  /// written. [_writtenFiles] holds per-path contents (host replicas, window
  /// layouts); [_writtenState] the encoded state this store last merged into
  /// state.json, which other windows also merge into.
  final Map<String, String> _writtenFiles = {};
  String? _writtenState;
  Timer? _saveTimer;
  final Map<String, Future<bool> Function()> _closeGuards = {};
  final Set<String> _routing = {};
  final Set<String> _suppressedRoutes = {};

  PanelPane get focusedPane => layout.panes.firstWhere(
    (pane) => pane.id == focusedPaneId,
    orElse: () => layout.panes.first,
  );
  PanelTarget? get selected => targetFor(focusedPane.selectedKey);
  PanelTarget? get selectedExplorer => targetFor(selectedExplorerKey);
  List<PanelTarget> get explorerTabs =>
      explorerTabKeys.map(targetFor).whereType<PanelTarget>().toList();

  /// The view [workspace]'s Explorer last focused, while it is still open.
  String? explorerFocusFor(PanelTarget workspace) {
    final key = _explorerFocus[_explorerWorkspace(workspace)];
    return key != null && explorerTabKeys.contains(key) ? key : null;
  }

  static String _explorerWorkspace(PanelTarget target) =>
      jsonEncode([target.serverId, target.workspaceId]);

  void _focusExplorer(String key) {
    selectedExplorerKey = key;
    final target = targetFor(key);
    if (target != null) _explorerFocus[_explorerWorkspace(target)] = key;
  }
  PanelTarget? targetFor(String? key) {
    if (key == null) return null;
    for (final target in tabs) {
      if (target.key == key) return target;
    }
    return null;
  }

  HostState? hostFor(String serverId) {
    for (final host in hosts) {
      if (host.serverId == serverId ||
          host.config.id == serverId ||
          host.connections.any((c) => c.id == serverId)) {
        return host;
      }
    }
    return null;
  }

  PaseoClient clientFor(String serverId) {
    final host = hostFor(serverId);
    if (host == null) throw StateError('Host is not saved: $serverId');
    return host.client;
  }

  VoidCallback registerHostRemovalGuard(
    Future<void> Function(HostState) guard,
  ) {
    _hostRemovalGuards.add(guard);
    return () => _hostRemovalGuards.remove(guard);
  }

  final List<Future<void> Function(PaseoClient)> _clientCloseHooks = [];
  VoidCallback registerClientCloseHook(
    Future<void> Function(PaseoClient) hook,
  ) {
    _clientCloseHooks.add(hook);
    return () => _clientCloseHooks.remove(hook);
  }

  final List<void Function(PaseoClient, String)> _clientOpenHooks = [];
  final Set<PaseoClient> _openedClients = {};
  VoidCallback registerClientOpenHook(void Function(PaseoClient, String) hook) {
    _clientOpenHooks.add(hook);
    for (final host in hosts) {
      if (_openedClients.contains(host.client)) {
        hook(host.client, host.serverId);
      }
    }
    return () => _clientOpenHooks.remove(hook);
  }

  Future<void> openAgent(String serverId, String agentId) async {
    final host = hostFor(serverId);
    if (host == null) throw StateError('The notification host is not saved.');
    if (host.client.status != ConnectionStatus.connected) {
      await refreshHost(serverId);
    }
    final payload = await host.client.request('fetch_agent_request', {
      'agentId': agentId,
    }, responseType: 'fetch_agent_response');
    final agent = jsonMap(payload['agent']);
    if (agent.isEmpty) {
      throw StateError(
        stringValue(payload['error'], 'Session no longer exists.'),
      );
    }
    final workspaceId = stringValue(agent['workspaceId']);
    final workspace = host.workspaces
        .where((w) => w['id'] == workspaceId)
        .firstOrNull;
    open(
      PanelTarget(
        kind: 'agent',
        serverId: host.serverId,
        workspaceId: workspaceId,
        cwd: stringValue(
          agent['cwd'],
          stringValue(workspace?['workspaceDirectory']),
        ),
        id: stringValue(agent['id'], agentId),
        title: stringValue(agent['title']),
        data: {'agent': agent},
      ),
    );
  }

  Future<void> openExecutionLog(
    String serverId,
    String workspaceId,
    String terminalId,
  ) async {
    final host = hostFor(serverId);
    if (host == null) throw StateError('The terminal host is not saved.');
    if (host.workspaces.every((w) => w['id'] != workspaceId)) {
      await refreshHost(serverId);
    }
    final workspace = host.workspaces
        .where((w) => w['id'] == workspaceId)
        .firstOrNull;
    if (workspace == null) {
      throw StateError('The terminal workspace is unavailable.');
    }
    open(
      PanelTarget(
        kind: 'execution_log',
        serverId: host.serverId,
        workspaceId: workspaceId,
        cwd: stringValue(
          workspace['workspaceDirectory'],
          stringValue(workspace['projectRootPath']),
        ),
        id: terminalId,
      ),
    );
  }

  /// Picks a host for an open-project launch; the shell installs its chooser
  /// (original `useHostChooser`: local host first, a single host chosen silently).
  Future<HostState?> Function()? hostChooser;

  /// Original `OpenProjectListener` + `openProjectDirectly`: a launched project path
  /// is registered as a project on the chosen host (`project.add`), which puts it in
  /// the sidebar; nothing opens. Like the original, an unreachable or older host and a
  /// rejected path end the request without a message.
  Future<void> openProjectPath(String path, {String? serverId}) async {
    final trimmed = path.trim();
    if (trimmed.isEmpty) return;
    final host = serverId == null ? await hostChooser?.call() : hostFor(serverId);
    if (host == null || host.client.status != ConnectionStatus.connected) return;
    final features = jsonMap(host.client.serverInfo['features']);
    if (features['projectAdd'] != true || features['stableProjectIdentity'] != true) {
      return;
    }
    final response = await host.client.request(
      'project.add.request',
      {'cwd': trimmed},
      responseType: 'project.add.response',
    );
    if (response['error'] != null || response['project'] is! Map) return;
    _upsert(host.projects, jsonMap(response['project']), 'projectId');
    _scheduleSave();
    _notify();
  }

  Future<void> initialize() => _initializing ??= _initialize();
  Future<void> _initialize() async {
    try {
      _directory = Directory(
        p.join((await getApplicationSupportDirectory()).path, 'paseo-native'),
      );
      await _directory!.create(recursive: true);
      await _restrict(_directory!.path, directory: true);
      final file = File(p.join(_directory!.path, 'state.json'));
      try {
        await _importLegacyState(file);
      } catch (error) {
        // A failed import must not let first-launch defaults replace recoverable
        // data. Existing Flutter state remains usable, but writes wait for retry.
        storageError = error;
        _storageWritable = false;
      }
      if (_retained.isNotEmpty) {
        preferences = AppPreferences(jsonMap(_retained['preferences']));
        _savedPreferences = jsonMap(jsonDecode(jsonEncode(preferences.values)));
        _savedHosts = _hostRecords(jsonList(_retained['hosts']));
        for (final raw in jsonList(_retained['hosts'])) {
          final row = jsonMap(raw);
          final configs = jsonList(row['connections']);
          final config = HostConfig.fromJson(
            row['config'] is Map ? jsonMap(row['config']) : row,
          );
          final host = HostState(
            config: config,
            client: _newClient(config),
            serverId: stringValue(
              row['serverId'],
              config.expectedServerId ?? config.id,
            ),
            originServerId: row['originServerId'] as String?,
          );
          for (final item in configs) {
            final candidate = HostConfig.fromJson(jsonMap(item));
            if (!host.connections.any((c) => c.id == candidate.id)) {
              host.connections.add(candidate);
            }
          }
          hosts.add(host);
          await _loadReplica(host);
          _attach(host);
        }
        final windowFile = File(
          p.join(
            _directory!.path,
            'layout-${Uri.encodeComponent(windowId)}.json',
          ),
        );
        final windowLayout = windowId != '0' && await windowFile.exists()
            ? jsonMap(jsonDecode(await windowFile.readAsString()))
            : jsonMap(_retained['layout']);
        _restoreLayout(windowLayout);
        _suppressedRoutes.addAll(
          jsonList(_retained['suppressedRoutes']).whereType<String>(),
        );
      }
    } catch (error) {
      storageError = error;
      // Preserve corrupt/unreadable state for recovery instead of replacing it.
      _storageWritable = false;
    }
    try {
      if (discoverLocalHosts) await _discoverLocalDaemon();
    } catch (error) {
      storageError ??= error;
    }
    initialized = true;
    _notify();
    await refreshHosts();
  }

  Future<JsonMap> _readStateFile(File file) async {
    if (!await file.exists()) return {};
    final decoded = jsonDecode(await file.readAsString());
    if (decoded is! Map ||
        decoded.containsKey('preferences') && decoded['preferences'] is! Map ||
        decoded.containsKey('hosts') && decoded['hosts'] is! List ||
        decoded.containsKey('layout') && decoded['layout'] is! Map) {
      throw const FormatException('App state has an invalid structure');
    }
    return jsonMap(decoded);
  }

  /// The original app's snapshot (legacy_state.dart's `legacyImport` marker,
  /// every original string as the re-sync base) is large and changes only on
  /// import/re-sync. It lives beside state.json so routine saves, which
  /// re-read and rewrite state.json, never carry it.
  File _legacyImportFile() => File(p.join(_directory!.path, 'legacy-import.json'));

  Future<void> _importLegacyState(File file) async {
    final lockFile = File(p.join(_directory!.path, 'state.lock'));
    final lock = await lockFile.open(mode: FileMode.append);
    var locked = false;
    try {
      await _restrict(lockFile.path);
      await lock.lock(FileLock.blockingExclusive);
      locked = true;
      final stored = await _readStateFile(file);
      if (intValue(stored['version']) > 1) {
        throw StateError('App state belongs to a newer Paseo version; it will not be overwritten.');
      }
      // Earlier builds kept the marker inside state.json; it moves out once.
      final embedded = stored.remove('legacyImport');
      final markerFile = _legacyImportFile();
      final marker = embedded ??
          (await markerFile.exists() ? jsonDecode(await markerFile.readAsString()) : null);
      _retained = stored;
      final current = <String, dynamic>{...stored, 'legacyImport': ?marker};
      final resync = jsonMap(marker)['version'] == legacyImportVersion;
      var next = current;
      try {
        next = mergeLegacyState(current, await legacyStateReader());
      } catch (error) {
        if (!resync) rethrow;
        // Flutter state is already complete and stays writable; an unreadable
        // original only defers the re-sync. Neither state nor base changes.
        storageError = error;
      }
      // Unchanged original (same digest) and nothing to move: no write.
      if (identical(next, current) && embedded == null) return;
      final nextMarker = next['legacyImport'];
      final nextState = <String, dynamic>{...next}..remove('legacyImport');
      // State first, then base. A lost base write re-syncs against the older
      // base next launch, where the values merged here already win as ours.
      // Nothing is observable on a parse/read failure; next launch retries.
      await _atomicWrite(file.path, jsonEncode(nextState));
      if (nextMarker != null) await _atomicWrite(markerFile.path, jsonEncode(nextMarker));
      _retained = nextState;
    } finally {
      if (locked) await lock.unlock();
      await lock.close();
    }
  }

  Future<void> discoverLocalDaemon() async {
    await _discoverLocalDaemon();
    await refreshHosts();
  }

  Future<void> _discoverLocalDaemon() async {
    if (!(Platform.isLinux || Platform.isMacOS || Platform.isWindows)) return;
    final home =
        Platform.environment['HOME'] ?? Platform.environment['USERPROFILE'];
    final explicitHome = Platform.environment['PASEO_HOME'];
    final daemonHome = explicitHome?.isNotEmpty == true
        ? explicitHome!
        : home == null
        ? null
        : p.join(home, '.paseo');
    if (daemonHome == null) return;
    final lock = File(p.join(daemonHome, 'paseo.pid'));
    if (!await lock.exists()) return;
    final state = jsonMap(jsonDecode(await lock.readAsString()));
    final listen = stringValue(state['listen']);
    if (listen.isEmpty || intValue(state['pid']) <= 0) return;
    final credential = File(p.join(daemonHome, 'local-credential'));
    String? credentialPath;
    if (await credential.exists()) {
      final token = (await credential.readAsString()).trim();
      if (RegExp(r'^[A-Za-z0-9_-]{43}$').hasMatch(token)) {
        credentialPath = credential.path;
      }
    }
    // Same socket rule the client applies before sending a local credential.
    String? identity(String endpoint) {
      try {
        final target = HostConfig(id: '', label: '', endpoint: endpoint).resolve();
        if (target.isRelay || target.uri.scheme == 'ssh') return null;
        final uri = target.uri;
        if (uri.scheme == 'unix' || uri.scheme == 'pipe') return '${uri.scheme}:${uri.path}';
        const loopback = {'localhost', '127.0.0.1', '0.0.0.0', '::1', '::'};
        return '${loopback.contains(uri.host) ? 'localhost' : uri.host}:${uri.port}';
      } on FormatException {
        return null;
      }
    }
    final listening = identity(listen);
    bool local(HostConfig c) =>
        c.endpoint == listen || listening != null && identity(c.endpoint) == listening;
    final serverId = stringValue(state['serverId']);
    // Imported original hosts already name this daemon (the old desktop app
    // supplied its local credential implicitly); never add a duplicate host.
    final existing = hosts.where((host) =>
        host.connections.any(local) ||
        serverId.isNotEmpty &&
            (host.serverId == serverId ||
                host.connections.any((c) => c.expectedServerId == serverId))).firstOrNull;
    if (existing != null) {
      var changed = false;
      if (!existing.connections.any(local)) {
        existing.connections.add(HostConfig(
          id: const Uuid().v4(),
          label: existing.config.label,
          endpoint: listen,
          localCredentialPath: credentialPath,
          expectedServerId: serverId.isEmpty ? null : serverId,
          color: existing.config.color,
        ));
        changed = true;
      } else if (credentialPath != null) {
        for (var i = 0; i < existing.connections.length; i++) {
          final c = existing.connections[i];
          if (c.localCredentialPath != null || !local(c)) continue;
          existing.connections[i] = HostConfig.fromJson({
            ...c.toJson(),
            'localCredentialPath': credentialPath,
          });
          changed = true;
        }
      }
      if (!changed) return;
      final selected = existing.connections
          .where((c) => c.id == existing.config.id)
          .firstOrNull;
      if (selected != null && !identical(selected, existing.config)) {
        await _detach(existing);
        existing.config = selected;
        existing.client = _newClient(selected);
        _attach(existing);
      }
      await savePreferences();
      return;
    }
    // Original `upsertConnectionFromListen`: the desktop daemon's host is
    // labelled with the hostname it advertises. Without one, the endpoint
    // label counts as unnamed and the connected daemon's hostname is shown.
    final hostname = stringValue(state['hostname']).trim();
    final config = HostConfig(
      id: const Uuid().v4(),
      label: hostname.isEmpty ? listen : hostname,
      endpoint: listen,
      localCredentialPath: credentialPath,
    );
    final host = HostState(config: config, client: _newClient(config));
    hosts.add(host);
    _attach(host);
    await savePreferences();
  }

  Future<void> addHost(HostConfig config) async {
    final existing = hosts
        .where((h) => h.connections.any((c) => c.id == config.id))
        .firstOrNull;
    if (existing != null) {
      await updateHost(config);
      return;
    }
    final host = HostState(config: config, client: _newClient(config));
    hosts.add(host);
    _attach(host);
    await savePreferences();
    await refreshHost(host.serverId);
    if (host.error != null) throw host.error!;
  }

  Future<void> updateHost(HostConfig config) async {
    final host = hostFor(config.id);
    if (host == null) {
      await addHost(config);
      return;
    }
    await _detach(host);
    final index = host.connections.indexWhere((c) => c.id == config.id);
    // Original `HostRuntimeController.updateHost`: a changed password forgets
    // credential rejections and reprobes every connection.
    final credentialsChanged =
        index >= 0 && host.connections[index].password != config.password;
    if (index >= 0) {
      host.connections[index] = config;
    } else {
      host.connections.add(config);
    }
    host.config = config;
    host.client = _newClient(config);
    _attach(host);
    unawaited(
      host._selector?.connectionsChanged(credentialsChanged: credentialsChanged),
    );
    await savePreferences();
    await refreshHost(host.serverId);
    if (host.error != null) throw host.error!;
  }

  Future<void> removeHost(String id) async {
    final host = hostFor(id);
    if (host == null) return;
    final keys = tabs
        .where(
          (t) => t.serverId == host.serverId || t.serverId == host.config.id,
        )
        .map((t) => t.key)
        .toList();
    if (!await _canClose(keys)) return;
    for (final guard in List.of(_hostRemovalGuards)) {
      await guard(host);
    }
    for (final key in keys) {
      _removeTab(key);
    }
    if (host.originServerId != null) {
      _suppressedRoutes.add('${host.originServerId}/${host.serverId}');
    }
    hosts.remove(host);
    _disposeSelector(host);
    await _detach(host);
    await savePreferences();
    _notify();
  }

  Future<void> removeConnection(String serverId, String connectionId) async {
    final host = hostFor(serverId);
    if (host == null) return;
    if (host.connections.length == 1) {
      await removeHost(serverId);
      return;
    }
    host.connections.removeWhere((c) => c.id == connectionId);
    if (host.config.id == connectionId) {
      await updateHost(host.connections.first);
    } else {
      unawaited(host._selector?.connectionsChanged());
    }
    await savePreferences();
  }

  void _attach(HostState host) {
    if (!connectHosts) return;
    host._lastStatus = host.client.status;
    host._events = host.client.events.listen(
      (event) => _event(host, event),
      onError: (Object error) {
        host.error = error;
        _notify();
      },
    );
    host._statusListener = () {
      final prior = host._lastStatus;
      host._lastStatus = host.client.status;
      if (host.client.status == ConnectionStatus.error) {
        host.error = host.client.error;
        // Original `failConnection`: a credential rejection of the active
        // connection excludes it and probes the others at once.
        if (host.client.error case final error?) {
          host._selector?.reportActiveFailure(host.config.id, error);
        }
      }
      if (host.client.status == ConnectionStatus.connected &&
          prior != ConnectionStatus.connected &&
          host._refresh == null) {
        unawaited(refreshHost(host.serverId));
      }
      _notify();
    };
    host.client.addListener(host._statusListener!);
    _startSelector(host);
  }

  /// Original `HostRuntimeStore.syncHosts`: every saved host gets one runtime
  /// controller that probes all of its connections, keeps the saved active
  /// connection (`initialConnection`), and switches to the best reachable one when
  /// it fails or another is consistently faster. Routed hosts have a single path
  /// through their origin daemon and are not probed.
  void _startSelector(HostState host) {
    if (host._selector != null || host.originServerId != null || _disposed) return;
    final selector = HostConnectionSelector(
      connections: () => host.connections,
      activeConnectionId: () => host.config.id,
      activeClient: () => host.client,
      expectedServerId: () => host.config.expectedServerId,
      createProbeClient: (connection) =>
          _newClient(connection)..setReconnectEnabled(false),
      activate: (connection, existing) => _activateConnection(host, connection, existing),
      onAuthFailure: (reason) {
        host.error = HelloRejectedException(reason, const []);
        _notify();
      },
    );
    host._selector = selector;
    unawaited(selector.start());
  }

  void _disposeSelector(HostState host) {
    host._selector?.dispose();
    host._selector = null;
  }

  /// Original `switchToConnection`: the old client closes, [connection] becomes the
  /// active one on [existing] (an admitted probe client, listeners attached once
  /// here) or a fresh client, and the host rehydrates on it.
  Future<void> _activateConnection(
    HostState host,
    HostConfig connection,
    PaseoClient? existing,
  ) async {
    if (_disposed || !hosts.contains(host) || host._selector == null) {
      existing?.dispose();
      return;
    }
    await _detach(host);
    host.config = connection;
    host.client = existing ?? _newClient(connection);
    _attach(host);
    // A refresh still running against the closed client settles first, so the
    // new client is hydrated rather than sharing that failed attempt.
    await host._refresh?.catchError((Object _) {});
    await refreshHost(host.serverId);
  }

  Future<void> _detach(HostState host) async {
    host._resync?.cancel();
    if (host._statusListener != null) {
      host.client.removeListener(host._statusListener!);
    }
    await host._events?.cancel();
    await _releaseObservations(host);
    for (final hook in List.of(_clientCloseHooks)) {
      await hook(host.client);
    }
    _openedClients.remove(host.client);
    await host.client.close();
  }

  Future<void> _releaseObservations(HostState host) async {
    final observations = List<OwnedObservation>.of(host._observations);
    host._observations.clear();
    for (final observation in observations) {
      try {
        await observation.close();
      } catch (_) {
        /* Disconnected subscriptions are already gone. */
      }
    }
  }

  /// Original `HostRuntimeStore.setAppVisible`: foregrounding bypasses every host's
  /// reconnect backoff and verifies healthy-looking sockets
  /// (`ensureConnectedAll({verify: true})`); hiding leaves reconnects running.
  void setAppVisible(bool visible) {
    if (!visible || !connectHosts) return;
    for (final host in hosts) {
      host.client.ensureConnected(verify: true);
    }
  }

  Future<void> refreshHosts() async {
    await _reloadSharedHosts();
    await Future.wait(
      List<HostState>.of(hosts).map((h) => refreshHost(h.serverId)),
    );
  }

  Future<void> _reloadSharedHosts() async {
    if (_directory == null || !_storageWritable) return;
    final file = File(p.join(_directory!.path, 'state.json'));
    if (!await file.exists()) return;
    try {
      final latest = jsonMap(jsonDecode(await file.readAsString()));
      for (final raw in jsonList(latest['hosts'])) {
        final row = jsonMap(raw);
        final config = HostConfig.fromJson(jsonMap(row['config']));
        final serverId = stringValue(
          row['serverId'],
          config.expectedServerId ?? config.id,
        );
        final existing = hostFor(config.id) ?? hostFor(serverId);
        if (existing != null) continue;
        final host = HostState(
          config: config,
          client: _newClient(config),
          serverId: serverId,
          originServerId: row['originServerId'] as String?,
        );
        for (final rawConnection in jsonList(row['connections'])) {
          final connection = HostConfig.fromJson(jsonMap(rawConnection));
          if (!host.connections.any((c) => c.id == connection.id)) {
            host.connections.add(connection);
          }
        }
        hosts.add(host);
        _savedHosts[config.id] = row;
        await _loadReplica(host);
        _attach(host);
      }
    } catch (error) {
      storageError = error;
      _notify();
    }
  }

  Future<void> refreshHost(String serverId) {
    final host = hostFor(serverId);
    if (host == null || _disposed || !connectHosts) return Future.value();
    return host._refresh ??= _refreshHost(host).whenComplete(() {
      host._refresh = null;
    });
  }

  Future<void> _refreshHost(HostState host) async {
    host.loading = true;
    host.error = null;
    _notify();
    try {
      if (host.originServerId != null &&
          host.client.status != ConnectionStatus.connected) {
        final origin = hostFor(host.originServerId!);
        if (origin == null) {
          throw StateError(
            'The host connection used for this route is unavailable.',
          );
        }
        if (origin.client.status != ConnectionStatus.connected) {
          await refreshHost(origin.serverId);
        }
        final routed = await origin.client.connectHost(
          host.serverId,
          label: host.label,
        );
        await _detach(host);
        host.client = routed;
        _attach(host);
      }
      if (host.client.status != ConnectionStatus.connected) {
        // Only the active connection connects here. Replacing an unreachable one
        // is the probe cycle's job (`HostConnectionSelector`), never an
        // in-order walk of the saved connections.
        try {
          await host.client.connect();
        } catch (error) {
          host._selector?.reportActiveFailure(host.config.id, error);
          rethrow;
        }
      }
      final actual = stringValue(host.client.serverInfo['serverId']);
      if (actual.isEmpty) {
        throw StateError('Connected host did not supply a server identity.');
      }
      if (host.config.expectedServerId != null &&
          host.config.expectedServerId != actual) {
        throw StateError(
          'Host identity changed. Confirm the new host before replacing this connection.',
        );
      }
      if (host.config.expectedServerId == null) {
        host.config = HostConfig.fromJson({
          ...host.config.toJson(),
          'expectedServerId': actual,
        });
        final index = host.connections.indexWhere(
          (c) => c.id == host.config.id,
        );
        if (index >= 0) host.connections[index] = host.config;
      }
      final oldId = host.serverId;
      host._serverId = actual;
      if (oldId != actual) _remapTargets(oldId, actual);
      final duplicate = hosts
          .where((h) => !identical(h, host) && h.serverId == actual)
          .firstOrNull;
      if (duplicate != null) {
        for (final config in host.connections) {
          if (!duplicate.connections.any((c) => c.id == config.id)) {
            duplicate.connections.add(config);
          }
        }
        hosts.remove(host);
        _disposeSelector(host);
        await _detach(host);
        unawaited(duplicate._selector?.connectionsChanged());
        await savePreferences();
        if (duplicate._refresh == null) await refreshHost(duplicate.serverId);
        return;
      }
      if (_openedClients.add(host.client)) {
        for (final hook in List.of(_clientOpenHooks)) {
          hook(host.client, host.serverId);
        }
      }
      await _releaseObservations(host);
      host._hydrating = true;
      host._queuedEvents.clear();
      // Explicit-event clients receive these broadcasts only while an owned
      // feed is installed. Establish it before baselines so updates during
      // pagination are buffered by _event instead of falling into a gap.
      final eventFeed = await host.client.observe(
        type: 'session.events.set_subscription.request',
        fields: {
          'events': [
            'project.update',
            'providers_snapshot_update',
            'agent_attention_required',
            'terminal_attention_required',
            'agent_permission_request',
            'agent_permission_resolved',
            'agent.provider_subagents.update',
            'checkout_status_update',
            'script_status_update',
            'status.server_info',
            'workspace_setup_progress',
            'activity_log',
            'status.daemon_config_changed',
          ],
          'notifications': true,
        },
        responseType: 'session.events.set_subscription.response',
        onEvent: (event) => _observationLifecycle(host, event),
      );
      host._observations.add(eventFeed);
      // Install project baseline first. Agent rows retain their own placement overlays.
      final projectSnapshot = await host.client.request(
        'project.list.request',
        {},
        responseType: 'project.list.response',
      );
      host.projects = jsonList(projectSnapshot['projects'])
          .map(jsonMap)
          .toList();
      _cursor(host, 'project', projectSnapshot);
      final workspaceObservation = await host.client.observe(
        type: 'fetch_workspaces_request',
        fields: {
          'subscribe': {},
          'page': {'limit': 200},
          if (_backgroundSupported(host)) 'includeBackground': true,
        },
        responseType: 'fetch_workspaces_response',
        onEvent: (event) => _observationLifecycle(host, event),
      );
      host._observations.add(workspaceObservation);
      final workspaceSnapshot = workspaceObservation.snapshot;
      host.workspaces = await _allPages(
        host,
        'fetch_workspaces_request',
        'fetch_workspaces_response',
        workspaceSnapshot,
      );
      _cursor(host, 'workspace', workspaceSnapshot);
      final agentObservation = await host.client.observe(
        type: 'fetch_agents_request',
        fields: {
          'subscribe': {},
          'page': {'limit': 200},
          if (_backgroundSupported(host)) 'includeBackground': true,
        },
        responseType: 'fetch_agents_response',
        onEvent: (event) => _observationLifecycle(host, event),
      );
      host._observations.add(agentObservation);
      final agentSnapshot = agentObservation.snapshot;
      host.agents = (await _allPages(
        host,
        'fetch_agents_request',
        'fetch_agents_response',
        agentSnapshot,
      )).map(_agentRow).toList();
      _cursor(host, 'agent', agentSnapshot);
      host._hydrating = false;
      for (final event in List<JsonMap>.of(host._queuedEvents)) {
        _event(host, event);
      }
      host._queuedEvents.clear();
      host.cachedAt = DateTime.now();
      _resolveImportedTargetDirectories(host);
      await _saveReplica(host);
      for (final target in tabs.where((t) => t.serverId == host.serverId)) {
        unawaited(_markChildTabOpen(target));
      }
      if (host.originServerId == null && _hostsSupported(host)) {
        final observation = await host.client.observe(
          type: 'hosts.list.request',
          fields: {'observe': true},
          responseType: 'hosts.list.response',
          onEvent: (event) => _observationLifecycle(host, event),
        );
        host._observations.add(observation);
        unawaited(
          _discoverRoutes(host, jsonMap(observation.snapshot['directory'])),
        );
      }
      await savePreferences();
    } catch (error) {
      host.error = error;
    } finally {
      host._hydrating = false;
      host.loading = false;
      _notify();
    }
  }

  void _observationLifecycle(HostState host, JsonMap event) {
    if (_disposed || !hosts.contains(host)) return;
    // Actual wire updates are processed once through client.events. Restored
    // feeds have no replay history: refresh projects/workspaces/agents (and
    // their script state) rather than treating restored membership as a sync.
    if (event['type'] == 'subscription.restored') {
      if (host._refresh == null) _scheduleResync(host);
    } else if (event['type'] == 'subscription.error') {
      host.error =
          jsonMap(event['payload'])['error'] ?? 'Host observation failed.';
      _notify();
    }
  }

  bool _backgroundSupported(HostState host) {
    final features = jsonMap(host.client.serverInfo['features']);
    return features['backgroundWorkspaces'] == true;
  }

  bool _hostsSupported(HostState host) {
    return jsonMap(host.client.serverInfo['features'])['connectedHosts'] ==
        true;
  }

  Future<List<JsonMap>> _allPages(
    HostState host,
    String request,
    String response,
    JsonMap snapshot,
  ) async {
    final rows = jsonList(snapshot['entries']).map(jsonMap).toList();
    var page = jsonMap(snapshot['pageInfo']);
    final seen = <String>{};
    while (page['hasMore'] == true) {
      final cursor = stringValue(page['nextCursor']);
      if (cursor.isEmpty || !seen.add(cursor)) {
        throw StateError('Host returned an invalid directory cursor.');
      }
      final next = await host.client.request(request, {
        'page': {'limit': 200, 'cursor': cursor},
        if (_backgroundSupported(host)) 'includeBackground': true,
      }, responseType: response);
      rows.addAll(jsonList(next['entries']).map(jsonMap));
      page = jsonMap(next['pageInfo']);
    }
    return rows;
  }

  JsonMap _agentRow(JsonMap entry) => entry['agent'] is Map
      ? {
          ...jsonMap(entry['agent']),
          'project': entry['project'],
          if (entry.containsKey('syncSeq')) 'syncSeq': entry['syncSeq'],
        }
      : entry;
  void _cursor(HostState host, String kind, JsonMap payload) {
    final sync = jsonMap(payload['sync']);
    if (sync['generation'] is String) {
      host._generations[kind] = sync['generation'] as String;
    }
    if (sync['headSeq'] is num) {
      host._sequences[kind] = intValue(sync['headSeq']);
    }
  }

  void _event(HostState host, JsonMap event) {
    if (_disposed || !hosts.contains(host)) return;
    final type = stringValue(event['type']);
    final payload = jsonMap(event['payload']);
    if (type == 'hosts.changed') {
      unawaited(_discoverRoutes(host, jsonMap(payload['directory'])));
      return;
    }
    if (!const {
      'agent_update',
      'workspace_update',
      'project.update',
      'script_status_update',
      'agent_status',
    }.contains(type)) {
      return;
    }
    if (host._hydrating) {
      host._queuedEvents.add(event);
      return;
    }
    final kind = type.startsWith('agent')
        ? 'agent'
        : type.startsWith('project')
        ? 'project'
        : 'workspace';
    final generation = payload['generation'];
    final seq = intValue(payload['seq']);
    if (generation is String &&
        host._generations[kind] != null &&
        generation != host._generations[kind]) {
      _scheduleResync(host);
      return;
    }
    if (seq > 0 && seq <= (host._sequences[kind] ?? 0)) return;
    if (generation is String) host._generations[kind] = generation;
    if (seq > 0) host._sequences[kind] = seq;
    // An update that leaves every row as it was (a repeated snapshot) saves
    // nothing and rebuilds nothing.
    var changed = false;
    switch (type) {
      case 'agent_update':
        if (payload['kind'] == 'remove') {
          changed = _remove(host.agents, (a) => a['id'] == payload['agentId']);
        } else {
          final row = _agentRow(payload);
          final old = host.agents
              .where((a) => a['id'] == row['id'])
              .firstOrNull;
          if (row['project'] == null && old != null) {
            row['project'] = old['project'];
          }
          changed = _upsert(host.agents, row, 'id');
        }
      case 'agent_status':
        final row = jsonMap(payload['info']);
        final old = host.agents.where((a) => a['id'] == row['id']).firstOrNull;
        if (old != null) changed = _upsert(host.agents, {...old, ...row}, 'id');
      case 'workspace_update':
        if (payload['kind'] == 'remove') {
          changed = _remove(host.workspaces, (w) => w['id'] == payload['id']);
          if (payload['emptyProject'] is Map) {
            changed = _upsert(
                  host.projects,
                  jsonMap(payload['emptyProject']),
                  'projectId',
                ) ||
                changed;
          }
          if (payload['removedProjectId'] != null) {
            changed = _remove(
                  host.projects,
                  (p) => p['projectId'] == payload['removedProjectId'],
                ) ||
                changed;
          }
        } else {
          changed = _upsert(host.workspaces, jsonMap(payload['workspace']), 'id');
        }
      case 'project.update':
        if (payload['kind'] == 'remove') {
          changed = _remove(
            host.projects,
            (p) => p['projectId'] == payload['projectId'],
          );
        } else {
          changed = _upsert(host.projects, jsonMap(payload['project']), 'projectId');
        }
      case 'script_status_update':
        for (final workspace in host.workspaces) {
          if (workspace['id'] == payload['workspaceId'] &&
              !_equality.equals(workspace['scripts'], payload['scripts'])) {
            workspace['scripts'] = payload['scripts'];
            changed = true;
          }
        }
    }
    if (!changed) return;
    host.cachedAt = DateTime.now();
    _scheduleSave();
    _notify();
  }

  static const _equality = DeepCollectionEquality();

  bool _remove(List<JsonMap> rows, bool Function(JsonMap row) test) {
    final before = rows.length;
    rows.removeWhere(test);
    return rows.length != before;
  }

  /// Inserts or replaces [row]; false when an equal row is already there.
  bool _upsert(List<JsonMap> rows, JsonMap row, String key) {
    if (row[key] == null) return false;
    final index = rows.indexWhere((item) => item[key] == row[key]);
    if (index < 0) {
      rows.add(row);
    } else if (_equality.equals(rows[index], row)) {
      return false;
    } else {
      rows[index] = row;
    }
    return true;
  }

  void _scheduleResync(HostState host) {
    host._resync?.cancel();
    host._resync = Timer(
      const Duration(milliseconds: 150),
      () => unawaited(refreshHost(host.serverId)),
    );
  }

  Future<void> _discoverRoutes(HostState origin, JsonMap directory) async {
    for (final raw in jsonList(directory['hosts'])) {
      final row = jsonMap(raw);
      final id = stringValue(row['serverId']);
      final route = '${origin.serverId}/$id';
      if (id.isEmpty ||
          hostFor(id) != null ||
          row['enabled'] != true ||
          row['status'] != 'online' ||
          _suppressedRoutes.contains(route) ||
          !_routing.add(route)) {
        continue;
      }
      try {
        final client = await origin.client.connectHost(
          id,
          label: stringValue(row['label'], id),
        );
        if (_disposed) {
          await client.close();
          return;
        }
        final host = HostState(
          config: client.host,
          client: client,
          serverId: id,
          originServerId: origin.serverId,
        );
        hosts.add(host);
        await _loadReplica(host);
        _attach(host);
        await refreshHost(id);
      } catch (error) {
        origin.error = error;
        _notify();
      } finally {
        _routing.remove(route);
      }
    }
  }

  final Set<String> _markingAgents = {};

  /// Marks an open child session's tab for this client
  /// (`paseo.open-agent-tab.<clientId>`). Opening never clears attention:
  /// that belongs to the focused agent panel only
  /// (hooks/use-agent-attention-clear.ts). The original clears a whole
  /// workspace only from its sidebar row (use-workspace-read-state.ts),
  /// never on open.
  Future<void> _markChildTabOpen(PanelTarget target) async {
    if (target.kind != 'agent' ||
        target.id == null ||
        !_markingAgents.add(target.key)) {
      return;
    }
    final host = hostFor(target.serverId);
    try {
      if (host == null || host.client.status != ConnectionStatus.connected) {
        return;
      }
      final agent = host.agents.where((a) => a['id'] == target.id).firstOrNull;
      if (agent == null) return;
      final labels = jsonMap(agent['labels']);
      if (agent['parentAgentId'] == null &&
          labels['paseo.parent-agent-id'] == null) {
        return;
      }
      final label = 'paseo.open-agent-tab.${await host.client.getClientId()}';
      if (labels[label] == 'true') return;
      final response = await host.client.request('update_agent_request', {
        'agentId': target.id,
        'labels': {label: 'true'},
      }, responseType: 'update_agent_response');
      if (response['accepted'] == false) {
        throw StateError(
          stringValue(response['error'], 'Could not mark child session open.'),
        );
      }
    } catch (error) {
      host?.error = error;
      _notify();
    } finally {
      _markingAgents.remove(target.key);
    }
  }

  /// Bumped whenever a tab is opened or selected, so a page shown in place of
  /// the conversation knows the user moved on.
  int navigationSerial = 0;

  void open(PanelTarget target, {bool beside = false}) {
    navigationSerial++;
    unawaited(_markChildTabOpen(target));
    final current = tabs.indexWhere((tab) => tab.key == target.key);
    if (current < 0) {
      tabs.add(target);
    } else {
      tabs[current] = target;
    }
    for (final pane in layout.panes) {
      if (pane.tabKeys.contains(target.key)) {
        pane.selectedKey = target.key;
        focusedPaneId = pane.id;
        compactDestination = CompactDestination.agent;
        _changed();
        return;
      }
    }
    explorerTabKeys.remove(target.key);
    if (beside) splitPane(focusedPane.id, Axis.horizontal);
    focusedPane.tabKeys.add(target.key);
    focusedPane.selectedKey = target.key;
    compactDestination = CompactDestination.agent;
    _changed();
  }

  void openExplorer(PanelTarget target, {bool focus = true}) {
    final existing = tabs.indexWhere((t) => t.key == target.key);
    if (existing < 0) {
      tabs.add(target);
    } else {
      tabs[existing] = target;
    }
    for (final pane in layout.panes) {
      _removeFromPane(pane, target.key);
    }
    if (!explorerTabKeys.contains(target.key)) explorerTabKeys.add(target.key);
    _focusExplorer(target.key);
    explorerVisible = true;
    if (focus) compactDestination = CompactDestination.explorer;
    _changed();
  }

  void selectExplorerTab(String key) {
    if (!explorerTabKeys.contains(key)) return;
    _focusExplorer(key);
    explorerVisible = true;
    compactDestination = CompactDestination.explorer;
    _changed();
  }

  void showExplorer(bool visible) {
    explorerVisible = visible;
    compactDestination = visible
        ? CompactDestination.explorer
        : CompactDestination.agent;
    _changed();
  }

  /// Original compact explorer overlay (`showMobileFileExplorer` / `showMobileAgent`): only
  /// the compact destination moves; the wide explorer column's visibility is untouched.
  void showCompactExplorer(bool visible) {
    compactDestination = visible
        ? CompactDestination.explorer
        : CompactDestination.agent;
    _notify();
  }

  void setExplorerWidth(double width) {
    explorerWidth = width.clamp(180, 1000);
    _changed();
  }

  void setSidebarWidth(double width) {
    sidebarWidth = width.clamp(200, 600);
    _changed();
  }

  void showSidebar(bool visible) {
    sidebarVisible = visible;
    compactDestination = visible
        ? CompactDestination.sidebar
        : CompactDestination.agent;
    _changed();
  }

  bool handleBack({required bool compact}) {
    if (!compact) return false;
    if (compactDestination == CompactDestination.explorer) {
      compactDestination = CompactDestination.agent;
      _notify();
      return true;
    }
    if (compactDestination == CompactDestination.agent) {
      compactDestination = CompactDestination.sidebar;
      _notify();
      return true;
    }
    return false;
  }

  void selectTab(String key) {
    navigationSerial++;
    for (final pane in layout.panes) {
      if (pane.tabKeys.contains(key)) {
        pane.selectedKey = key;
        focusedPaneId = pane.id;
        final target = targetFor(key);
        if (target != null) unawaited(_markChildTabOpen(target));
        compactDestination = CompactDestination.agent;
        _changed();
        return;
      }
    }
    if (explorerTabKeys.contains(key)) selectExplorerTab(key);
  }

  void focusPane(String id) {
    if (focusedPaneId == id || !layout.panes.any((p) => p.id == id)) return;
    focusedPaneId = id;
    _changed();
  }

  String? adjacentPane(AxisDirection direction) {
    final rectangles = <String, Rect>{};
    void locate(LayoutNode node, Rect rect) {
      if (node is PanelPane) {
        rectangles[node.id] = rect;
        return;
      }
      final split = node as PanelSplit;
      if (split.axis == Axis.horizontal) {
        final boundary = rect.left + rect.width * split.ratio;
        locate(
          split.first,
          Rect.fromLTRB(rect.left, rect.top, boundary, rect.bottom),
        );
        locate(
          split.second,
          Rect.fromLTRB(boundary, rect.top, rect.right, rect.bottom),
        );
      } else {
        final boundary = rect.top + rect.height * split.ratio;
        locate(
          split.first,
          Rect.fromLTRB(rect.left, rect.top, rect.right, boundary),
        );
        locate(
          split.second,
          Rect.fromLTRB(rect.left, boundary, rect.right, rect.bottom),
        );
      }
    }

    locate(layout, const Rect.fromLTWH(0, 0, 1, 1));
    final current = rectangles[focusedPane.id]!;
    String? nearest;
    var distance = double.infinity;
    for (final entry in rectangles.entries) {
      if (entry.key == focusedPane.id) continue;
      final delta = entry.value.center - current.center;
      final eligible = switch (direction) {
        AxisDirection.left => entry.value.right <= current.left + 0.0001,
        AxisDirection.right => entry.value.left >= current.right - 0.0001,
        AxisDirection.up => entry.value.bottom <= current.top + 0.0001,
        AxisDirection.down => entry.value.top >= current.bottom - 0.0001,
      };
      if (eligible && delta.distanceSquared < distance) {
        nearest = entry.key;
        distance = delta.distanceSquared;
      }
    }
    return nearest;
  }

  void focusAdjacentPane(AxisDirection direction) {
    final id = adjacentPane(direction);
    if (id != null) focusPane(id);
  }

  void moveFocusedTab(AxisDirection direction) {
    final key = focusedPane.selectedKey;
    final id = adjacentPane(direction);
    if (key != null && id != null) moveTab(key, id);
  }

  String splitPane(String paneId, Axis axis) {
    final pane = layout.panes.where((p) => p.id == paneId).firstOrNull;
    if (pane == null) throw StateError('Pane no longer exists');
    final next = PanelPane(id: const Uuid().v4());
    final split = PanelSplit(
      id: const Uuid().v4(),
      axis: axis,
      first: pane,
      second: next,
    );
    layout = _replaceNode(layout, paneId, split);
    focusedPaneId = next.id;
    maximizedPaneId = null;
    _changed();
    return next.id;
  }

  LayoutNode _replaceNode(LayoutNode node, String id, LayoutNode replacement) {
    if (node is PanelPane) return node.id == id ? replacement : node;
    final split = node as PanelSplit;
    if (split.id == id) return replacement;
    split.first = _replaceNode(split.first, id, replacement);
    split.second = _replaceNode(split.second, id, replacement);
    return split;
  }

  void resizeSplit(String id, double ratio) {
    void visit(LayoutNode node) {
      if (node is PanelSplit) {
        if (node.id == id) node.ratio = ratio.clamp(0.1, 0.9);
        visit(node.first);
        visit(node.second);
      }
    }

    visit(layout);
    _changed();
  }

  void maximizePane(String? id) {
    maximizedPaneId = id;
    _changed();
  }

  /// Original `moveTabToPaneInLayout`: a source pane the move leaves empty is removed
  /// (`detachTabFromTree` without a preserved pane).
  void moveTab(String key, String paneId, {int? index}) {
    if (targetFor(key) == null) return;
    final target = layout.panes.where((p) => p.id == paneId).firstOrNull;
    if (target == null) return;
    final sources = [
      for (final pane in layout.panes)
        if (pane.id != paneId && pane.tabKeys.contains(key)) pane.id,
    ];
    for (final pane in layout.panes) {
      _removeFromPane(pane, key);
    }
    explorerTabKeys.remove(key);
    target.tabKeys.insert(
      (index ?? target.tabKeys.length).clamp(0, target.tabKeys.length),
      key,
    );
    target.selectedKey = key;
    for (final source in sources) {
      if (layout.panes.any((pane) => pane.id == source && pane.tabKeys.isEmpty)) {
        _removePane(source);
      }
    }
    focusedPaneId = target.id;
    _changed();
  }

  void reorderTab(String paneId, int oldIndex, int newIndex) {
    final pane = layout.panes.where((p) => p.id == paneId).firstOrNull;
    if (pane == null || oldIndex < 0 || oldIndex >= pane.tabKeys.length) return;
    final key = pane.tabKeys.removeAt(oldIndex);
    pane.tabKeys.insert(newIndex.clamp(0, pane.tabKeys.length), key);
    _changed();
  }

  VoidCallback registerCloseGuard(String key, Future<bool> Function() guard) {
    _closeGuards[key] = guard;
    return () {
      if (identical(_closeGuards[key], guard)) _closeGuards.remove(key);
    };
  }

  Future<bool> _canClose(Iterable<String> keys) async {
    for (final key in keys.toSet()) {
      final guard = _closeGuards[key];
      if (guard != null && !await guard()) return false;
    }
    return true;
  }

  Future<bool> canCloseWindow() => _canClose(_closeGuards.keys);
  Future<bool> closeTab(String key) async {
    if (!await _canClose([key])) return false;
    _removeTab(key);
    _changed();
    return true;
  }

  Future<bool> closePane(String paneId) async {
    final pane = layout.panes.where((p) => p.id == paneId).firstOrNull;
    if (pane == null) return true;
    if (!await _canClose(pane.tabKeys)) return false;
    for (final key in List<String>.of(pane.tabKeys)) {
      _removeTab(key);
    }
    _removePane(paneId);
    _changed();
    return true;
  }

  /// Original `closePaneInLayout`/`removePaneByPath`: drops [paneId] from the split tree
  /// unless it is the last pane. Focus that was on it moves to the nearest sibling
  /// (`findNearestSiblingPaneId`: the preceding sibling's last pane, else the following
  /// sibling's first pane).
  void _removePane(String paneId) {
    if (layout.panes.length <= 1 || !layout.panes.any((p) => p.id == paneId)) return;
    String? fallback;
    LayoutNode remove(LayoutNode node) {
      if (node is! PanelSplit) return node;
      if (node.first is PanelPane && (node.first as PanelPane).id == paneId) {
        fallback = node.second.panes.first.id;
        return node.second;
      }
      if (node.second is PanelPane && (node.second as PanelPane).id == paneId) {
        fallback = node.first.panes.last.id;
        return node.first;
      }
      node.first = remove(node.first);
      node.second = remove(node.second);
      return node;
    }

    layout = remove(layout);
    if (focusedPaneId == paneId) focusedPaneId = fallback ?? layout.panes.first.id;
    if (maximizedPaneId == paneId) maximizedPaneId = null;
  }

  void _removeFromPane(PanelPane pane, String key) {
    final index = pane.tabKeys.indexOf(key);
    if (index < 0) return;
    pane.tabKeys.removeAt(index);
    if (pane.selectedKey == key) {
      pane.selectedKey = pane.tabKeys.isEmpty
          ? null
          : pane.tabKeys[index.clamp(0, pane.tabKeys.length - 1)];
    }
  }

  /// Original `closeTabInLayout`: the closed tab's pane goes away once empty, except the
  /// default `main` pane and the last pane, which stay (`preserveEmptyPaneId`).
  void _removeTab(String key) {
    for (final pane in List<PanelPane>.of(layout.panes)) {
      if (!pane.tabKeys.contains(key)) continue;
      _removeFromPane(pane, key);
      if (pane.tabKeys.isEmpty && pane.id != 'main') _removePane(pane.id);
    }
    explorerTabKeys.remove(key);
    if (selectedExplorerKey == key) {
      selectedExplorerKey = explorerTabKeys.lastOrNull;
    }
    tabs.removeWhere((t) => t.key == key);
    _closeGuards.remove(key);
  }

  JsonMap _targetJson(PanelTarget t) => {
    'kind': t.kind,
    'serverId': t.serverId,
    'workspaceId': t.workspaceId,
    'cwd': t.cwd,
    'id': t.id,
    'title': t.title,
    'data': t.data,
  };

  void _resolveImportedTargetDirectories(HostState host) {
    for (var index = 0; index < tabs.length; index++) {
      final target = tabs[index];
      if (target.serverId != host.serverId || target.cwd.isNotEmpty ||
          target.data['legacyImported'] != true) {
        continue;
      }
      final workspace = host.workspaces
          .where((row) => row['id'] == target.workspaceId).firstOrNull;
      final cwd = stringValue(workspace?['workspaceDirectory'],
          stringValue(workspace?['projectRootPath']));
      if (cwd.isNotEmpty) tabs[index] = target.copyWith(cwd: cwd);
    }
  }
  PanelTarget _targetFromJson(JsonMap t) => PanelTarget(
    kind: stringValue(t['kind']),
    serverId: stringValue(t['serverId']),
    workspaceId: stringValue(t['workspaceId']),
    cwd: stringValue(t['cwd']),
    id: t['id'] as String?,
    title: t['title'] as String?,
    data: jsonMap(t['data']),
  );
  void _restoreLayout(JsonMap saved) {
    final migratedKeys = <String, String>{};
    for (final raw in jsonList(saved['tabs'])) {
      final old = _targetFromJson(jsonMap(raw));
      if (old.kind == 'browser' || old.kind == 'plugin') continue;
      if (old.kind == 'terminal' && old.id == null) continue;
      final target = old.kind == 'terminal'
          ? old.copyWith(kind: 'execution_log')
          : old;
      migratedKeys[old.key] = target.key;
      if (!tabs.any((t) => t.key == target.key)) tabs.add(target);
    }
    final seen = <String>{};
    LayoutNode restore(JsonMap node) {
      if (node['kind'] == 'split') {
        final ratio = node['ratio'];
        return PanelSplit(
          id: stringValue(node['id'], const Uuid().v4()),
          axis: node['axis'] == 'vertical' ? Axis.vertical : Axis.horizontal,
          ratio: ratio is num && ratio.isFinite
              ? ratio.toDouble().clamp(0.1, 0.9)
              : 0.5,
          first: restore(jsonMap(node['first'])),
          second: restore(jsonMap(node['second'])),
        );
      }
      final keys = jsonList(node['tabs'])
          .whereType<String>()
          .map((key) => migratedKeys[key] ?? key)
          .where((key) => targetFor(key) != null && seen.add(key))
          .toList();
      final oldSelection = stringValue(node['selected']);
      final selected = migratedKeys[oldSelection] ?? oldSelection;
      return PanelPane(
        id: stringValue(node['id'], 'main'),
        tabKeys: keys,
        selectedKey: keys.contains(selected) ? selected : keys.firstOrNull,
      );
    }

    layout = restore(jsonMap(saved['tree']));
    focusedPaneId = stringValue(saved['focusedPane'], layout.panes.first.id);
    explorerTabKeys.addAll(
      jsonList(saved['explorerTabs'])
          .whereType<String>()
          .map((key) => migratedKeys[key] ?? key)
          .where((key) => targetFor(key) != null && seen.add(key)),
    );
    final oldExplorerKey = saved['selectedExplorer'] as String?;
    selectedExplorerKey = migratedKeys[oldExplorerKey] ?? oldExplorerKey;
    if (!explorerTabKeys.contains(selectedExplorerKey)) {
      selectedExplorerKey = explorerTabKeys.firstOrNull;
    }
    // Layouts saved before version 2 opened a workspace's Explorer with the Files view
    // alone. The original Explorer starts with Files + Changes, Changes focused
    // (workspace-layout-actions.ts createDefaultExplorerSidebarTabs), so those
    // Flutter-created Explorers gain their missing Changes view once. Imported
    // original layouts stay authoritative.
    if (intValue(saved['version']) < 2) {
      for (final key in List.of(explorerTabKeys)) {
        final files = targetFor(key);
        if (files == null ||
            files.kind != 'files' ||
            files.data['legacyImported'] == true) {
          continue;
        }
        final changes = PanelTarget(
          kind: 'changes_tree',
          serverId: files.serverId,
          workspaceId: files.workspaceId,
          cwd: files.cwd,
        );
        if (!seen.add(changes.key)) continue;
        if (targetFor(changes.key) == null) tabs.add(changes);
        explorerTabKeys.insert(explorerTabKeys.indexOf(key) + 1, changes.key);
        if (selectedExplorerKey == key) selectedExplorerKey = changes.key;
      }
    }
    if (selectedExplorerKey case final key?) _focusExplorer(key);
    for (final target in tabs) {
      if (seen.add(target.key)) focusedPane.tabKeys.add(target.key);
    }
    focusedPane.selectedKey ??= focusedPane.tabKeys.firstOrNull;
    explorerVisible = saved['explorerVisible'] == true;
    if (saved['explorerWidth'] is num) {
      explorerWidth = (saved['explorerWidth'] as num).toDouble().clamp(
        180,
        1000,
      );
    }
    // A saved Flutter width wins; otherwise the original panel-state width
    // imported into preferences.
    final sidebar = saved['sidebarWidth'] ?? preferences['sidebarWidth'];
    if (sidebar is num) sidebarWidth = sidebar.toDouble().clamp(200, 600);
    sidebarVisible = saved['sidebarVisible'] != false;
    // Compact starts at the active agent, never a persisted side destination.
    compactDestination = CompactDestination.agent;
  }

  void _remapTargets(String oldId, String newId) {
    final keys = <String, String>{};
    for (var i = 0; i < tabs.length; i++) {
      final target = tabs[i];
      if (target.serverId != oldId) continue;
      final replacement = target.copyWith(serverId: newId);
      keys[target.key] = replacement.key;
      tabs[i] = replacement;
    }
    for (final pane in layout.panes) {
      for (var i = 0; i < pane.tabKeys.length; i++) {
        pane.tabKeys[i] = keys[pane.tabKeys[i]] ?? pane.tabKeys[i];
      }
      pane.selectedKey = keys[pane.selectedKey] ?? pane.selectedKey;
    }
    for (var i = 0; i < explorerTabKeys.length; i++) {
      explorerTabKeys[i] = keys[explorerTabKeys[i]] ?? explorerTabKeys[i];
    }
    selectedExplorerKey = keys[selectedExplorerKey] ?? selectedExplorerKey;
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  void _changed() {
    _notify();
    _scheduleSave();
  }

  void _scheduleSave() {
    _saveTimer?.cancel();
    _saveTimer = Timer(const Duration(milliseconds: 350), () {
      unawaited(
        savePreferences().catchError((Object error) {
          storageError = error;
          _notify();
        }),
      );
    });
  }

  Future<void> savePreferences() async {
    _notify();
    if (_directory == null || !_storageWritable) return;
    final data = <String, dynamic>{
      ..._retained,
      'version': 1,
      'preferences': preferences.values,
      'hosts': hosts
          .map(
            (h) => {
              'serverId': h.serverId,
              'originServerId': h.originServerId,
              'config': h.config.toJson(),
              'connections': h.connections.map((c) => c.toJson()).toList(),
            },
          )
          .toList(),
      'suppressedRoutes': _suppressedRoutes.toList(),
      'layout': {
        'version': 2,
        'tabs': tabs.map(_targetJson).toList(),
        'tree': layout.toJson(),
        'focusedPane': focusedPaneId,
        'explorerTabs': explorerTabKeys,
        'selectedExplorer': selectedExplorerKey,
        'explorerVisible': explorerVisible,
        'explorerWidth': explorerWidth,
        'sidebarWidth': sidebarWidth,
        'sidebarVisible': sidebarVisible,
      },
    };
    final encoded = jsonEncode(data);
    final snapshot = jsonMap(jsonDecode(encoded));
    final replicas = connectHosts
        ? hosts
              .map(
                (host) => MapEntry(
                  _replicaPath(host.serverId),
                  jsonEncode(_replicaJson(host)),
                ),
              )
              .toList()
        : <MapEntry<String, String>>[];
    final next = _writeTail.catchError((Object _) {}).then((_) async {
      // Compared after earlier writes settle, so queued saves see their result.
      final stateChanged = _writtenState != encoded;
      final changedReplicas = [
        for (final replica in replicas)
          if (_writtenFiles[replica.key] != replica.value) replica,
      ];
      // Host events usually change only a replica; state.json stays untouched.
      if (!stateChanged && changedReplicas.isEmpty) return;
      final lockFile = File(p.join(_directory!.path, 'state.lock'));
      final lock = await lockFile.open(mode: FileMode.append);
      try {
        await _restrict(lockFile.path);
        await lock.lock(FileLock.blockingExclusive);
        if (stateChanged) await _mergeState(snapshot);
        for (final replica in changedReplicas) {
          await _writeIfChanged(replica.key, replica.value);
        }
        if (stateChanged) _writtenState = encoded;
      } finally {
        await lock.unlock();
        await lock.close();
      }
    });
    _writeTail = next;
    await next;
  }

  /// Three-way merges this store's state into state.json under the state lock:
  /// preferences and hosts against what this store last saved, so another
  /// window's concurrent edits survive.
  Future<void> _mergeState(JsonMap snapshot) async {
    final file = File(p.join(_directory!.path, 'state.json'));
    final latest = await _readStateFile(file);
    if (intValue(latest['version']) > 1) {
      throw StateError('A newer client owns the saved state.');
    }
    final nextPreferences = jsonMap(snapshot['preferences']);
    final nextHosts = _hostRecords(jsonList(snapshot['hosts']));
    final mergedPreferences = _mergeChanges(
      jsonMap(latest['preferences']),
      _savedPreferences,
      nextPreferences,
    );
    final mergedHosts = _mergeChanges(
      _hostRecords(jsonList(latest['hosts'])),
      _savedHosts,
      nextHosts,
    );
    final merged = <String, dynamic>{
      ...latest,
      'version': 1,
      'preferences': mergedPreferences,
      'hosts': mergedHosts.values.toList(),
      'suppressedRoutes': {
        ...jsonList(latest['suppressedRoutes']).whereType<String>(),
        ..._suppressedRoutes,
      }.toList(),
      if (connectHosts && windowId == '0') 'layout': snapshot['layout'],
    };
    await _atomicWrite(file.path, jsonEncode(merged));
    if (connectHosts && windowId != '0') {
      await _writeIfChanged(
        p.join(_directory!.path, 'layout-${Uri.encodeComponent(windowId)}.json'),
        jsonEncode(snapshot['layout']),
      );
    }
    _savedPreferences = nextPreferences;
    _savedHosts = nextHosts;
  }

  Future<void> _writeIfChanged(String path, String contents) async {
    if (_writtenFiles[path] == contents) return;
    await _atomicWrite(path, contents);
    _writtenFiles[path] = contents;
  }

  JsonMap _hostRecords(List<dynamic> rows) => {
    for (final raw in rows)
      stringValue(
        jsonMap(jsonMap(raw)['config'])['id'],
        stringValue(jsonMap(raw)['id']),
      ): jsonMap(
        raw,
      ),
  };
  JsonMap _mergeChanges(JsonMap latest, JsonMap before, JsonMap next) {
    final result = Map<String, dynamic>.from(latest);
    for (final key in {...before.keys, ...next.keys}) {
      if (jsonEncode(before[key]) == jsonEncode(next[key]) &&
          before.containsKey(key) == next.containsKey(key)) {
        continue;
      }
      if (!next.containsKey(key)) {
        result.remove(key);
      } else if (next[key] is Map && before[key] is Map && result[key] is Map) {
        result[key] = _mergeChanges(
          jsonMap(result[key]),
          jsonMap(before[key]),
          jsonMap(next[key]),
        );
      } else {
        result[key] = next[key];
      }
    }
    return result;
  }

  String _replicaPath(String id) => p.join(
    _directory!.path,
    'replica-${base64Url.encode(utf8.encode(id)).replaceAll('=', '')}.json',
  );
  JsonMap _replicaJson(HostState host) => {
    'serverId': host.serverId,
    'cachedAt': host.cachedAt?.toIso8601String(),
    'projects': host.projects,
    'workspaces': host.workspaces,
    'agents': host.agents,
  };
  Future<void> _loadReplica(HostState host) async {
    if (_directory == null || !connectHosts) return;
    final file = File(_replicaPath(host.serverId));
    if (!await file.exists()) return;
    try {
      final contents = await file.readAsString();
      final data = jsonMap(jsonDecode(contents));
      if (data['serverId'] != host.serverId) return;
      host.projects = jsonList(data['projects']).map(jsonMap).toList();
      host.workspaces = jsonList(data['workspaces']).map(jsonMap).toList();
      host.agents = jsonList(data['agents']).map(jsonMap).toList();
      host.cachedAt = DateTime.tryParse(stringValue(data['cachedAt']));
      _writtenFiles[file.path] = contents;
    } catch (error) {
      host.error = error;
    }
  }

  Future<void> _saveReplica(HostState host) async {
    if (_directory == null || !_storageWritable) return;
    final path = _replicaPath(host.serverId);
    final encoded = jsonEncode(_replicaJson(host));
    final next = _writeTail
        .catchError((Object _) {})
        .then((_) => _writeIfChanged(path, encoded));
    _writeTail = next;
    await next;
  }

  Future<void> _atomicWrite(String path, String contents) async {
    final temporary = File('$path.${const Uuid().v4()}.tmp');
    try {
      await temporary.create(exclusive: true);
      await _restrict(temporary.path);
      await temporary.writeAsString(contents, flush: true);
      await temporary.rename(path);
    } finally {
      if (await temporary.exists()) await temporary.delete();
    }
  }

  Future<void> _restrict(String path, {bool directory = false}) async {
    if (!(Platform.isLinux || Platform.isMacOS)) {
      return; // Mobile sandbox / Windows profile ACL.
    }
    final result = await Process.run('chmod', [
      directory ? '700' : '600',
      path,
    ]);
    if (result.exitCode != 0) {
      throw FileSystemException('Cannot protect private app state', path);
    }
  }

  Future<void> disposeAsync() async {
    _saveTimer?.cancel();
    await savePreferences();
    _disposed = true;
    for (final host in List<HostState>.of(hosts)) {
      _disposeSelector(host);
      await _detach(host);
    }
    await _writeTail;
    super.dispose();
  }
}
