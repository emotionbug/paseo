import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/host_config.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/legacy_state.dart';

class _SupportDirectory extends PathProviderPlatform {
  _SupportDirectory(this.path);
  final String path;
  @override
  Future<String?> getApplicationSupportPath() async => path;
}

// These are the original AsyncStorage payloads, not the Flutter destination
// schema: a plain settings object, profile array, and Zustand {state,version}.
Map<String, String> _legacyValues() => {
  '@paseo:app-settings': jsonEncode({
    'language': 'ko', 'theme': 'midnight', 'uiFontFamily': 'Inter',
    'monoFontFamily': 'JetBrains Mono', 'uiBaseFontSize': 17,
    'contentFontSize': 19, 'codeFontSize': 13, 'contentMaxWidth': 1040,
    'sendBehavior': 'queue', 'syntaxTheme': 'one', 'vimKeybindings': true,
    'sidebarRowItems': {'host': true, 'branch': false},
    'openInSidePane': {'explorerFiles': true, 'subagents': false},
    'futureSetting': {'retained': true},
  }),
  '@paseo:settings-migrations': jsonEncode({'applied': ['steer-default', 'mobile-content-16']}),
  // normalizeStoredHostProfile output as persisted by the old app: connection
  // ids are derived (direct:/relay:wss:) and preferredConnectionId names them.
  '@paseo:daemon-registry': jsonEncode([
    {
      'serverId': 'srv-home', 'label': '개발 서버', 'password': '  secret\n密碼  ',
      'appearance': {'color': 'blue', 'badgeDisplay': 'name'},
      'connections': [
        {'id': 'direct:192.168.10.20:6767', 'type': 'directTcp', 'endpoint': '192.168.10.20:6767'},
        {'id': 'relay:wss:relay.paseo.sh:443', 'type': 'relay', 'relayEndpoint': 'relay.paseo.sh:443',
          'daemonPublicKeyB64': base64Encode(List<int>.filled(32, 7)),
          'useTls': true, 'minimumSecurityMode': 'protected'},
      ],
      'preferredConnectionId': 'relay:wss:relay.paseo.sh:443', 'lifecycle': {},
      'createdAt': '2026-01-01T00:00:00.000Z', 'updatedAt': '2026-10-29T00:00:00.000Z',
    },
  ]),
  '@paseo:keyboard-shortcut-overrides': jsonEncode({
    'message-input.send': 'Mod+Enter', 'theme.cycle': null,
  }),
  'paseo:last-workspace-route-selection': jsonEncode({'serverId': 'srv-home', 'workspaceId': 'opaque:workspace'}),
  'sidebar-view': jsonEncode({'version': 8, 'state': {
    'showBackground': true, 'groupMode': 'project', 'workspaceSort': 'created',
    'hostFilters': ['srv-home'], 'projectFilters': [], 'labelFilter': {'labels': ['important']},
  }}),
  'panel-state': jsonEncode({'version': 17, 'state': {
    'desktop': {'agentListOpen': false, 'focusModeEnabled': false},
    'sidebarWidth': 286, 'treeRailWidth': 264, 'explorerShowHiddenFiles': true,
  }}),
  'workspace-layout-state': jsonEncode({'version': 2, 'state': {
    'layoutByWorkspace': {'srv-home:opaque:workspace': {
      'focusedPaneId': 'conversation',
      'root': {'kind': 'group', 'group': {'id': 'split', 'direction': 'horizontal', 'sizes': [70, 30], 'children': [
        {'kind': 'pane', 'pane': {'id': 'conversation', 'tabIds': ['agent_session-9'], 'focusedTabId': 'agent_session-9', 'tabs': [
          {'tabId': 'agent_session-9', 'createdAt': 1, 'target': {'kind': 'agent', 'agentId': 'session-9'}},
        ]}},
        {'kind': 'pane', 'pane': {'id': 'editor', 'tabIds': ['file_readme', 'browser_1'], 'focusedTabId': 'file_readme', 'tabs': [
          {'tabId': 'file_readme', 'createdAt': 2, 'target': {'kind': 'file', 'path': 'README.md', 'lineStart': 8}},
          {'tabId': 'browser_1', 'createdAt': 3, 'target': {'kind': 'browser', 'browserId': '1'}},
        ]}},
      ]}},
    }},
  }}),
  'paseo-drafts': jsonEncode({'version': 5, 'state': {'drafts': {
    'agent:srv-home:session-9': {'input': {'text': 'unsent original', 'attachments': []},
      'lifecycle': 'active', 'updatedAt': 42, 'version': 1},
  }, 'createModalDraft': null}}),
  '@paseo:expo-push-token:srv-home': 'ExponentPushToken[old-token]',
  // Unconsumed caches are retained verbatim, never parsed (this one is torn).
  '@paseo/provider-snapshot-index/v2': '{"truncated":',
  'unknown-old-store': 'uninterpreted original value',
};

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late Directory directory;
  late PathProviderPlatform original;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('paseo-legacy-state-test-');
    original = PathProviderPlatform.instance;
    PathProviderPlatform.instance = _SupportDirectory(directory.path);
  });
  tearDown(() async {
    PathProviderPlatform.instance = original;
    await directory.delete(recursive: true);
  });
  AppStore store(LegacyStateReader reader) => AppStore(connectHosts: false,
    discoverLocalHosts: false, legacyStateReader: reader);
  Future<File> stateFile() async {
    final dir = Directory('${directory.path}/paseo-native');
    await dir.create(recursive: true);
    return File('${dir.path}/state.json');
  }

  test('init restores original auth, preferences, selected session and split layout exactly once', () async {
    final values = _legacyValues();
    final source = File('${directory.path}/original-async-storage.json');
    await source.writeAsString(jsonEncode(values));
    final originalBytes = await source.readAsBytes();
    var reads = 0;
    Future<LegacyStateSnapshot> read() async {
      reads++;
      return LegacyStateSnapshot(values: Map<String, String>.from(jsonDecode(await source.readAsString()) as Map), sources: [source.path]);
    }
    final first = store(read);
    await first.initialize();
    expect(first.storageError, isNull);
    expect(first.hosts, hasLength(1));
    final host = first.hosts.single;
    expect(host.label, '개발 서버');
    expect(host.config.id, 'relay:wss:relay.paseo.sh:443');
    expect(host.connections.map((c) => c.id), unorderedEquals(['direct:192.168.10.20:6767', 'relay:wss:relay.paseo.sh:443']));
    final lan = host.connections.firstWhere((c) => c.id.startsWith('direct:'));
    expect(lan.endpoint, 'ws://192.168.10.20:6767/ws');
    expect(lan.password, '  secret\n密碼  ');
    expect(host.config.color, '#5179b0');
    expect(host.serverId, 'srv-home');
    expect(host.config.password, '  secret\n密碼  ');
    expect(host.config.resolve().requireProtected, isTrue);
    expect(host.config.resolve().daemonPublicKey, base64Encode(List<int>.filled(32, 7)));
    expect(first.preferences.locale, 'ko');
    expect(first.preferences.themeMode, ThemeMode.dark);
    expect(first.preferences['theme'], 'midnight');
    expect(first.preferences['uiBaseFontSize'], 17);
    expect(first.preferences['contentFontSize'], 19);
    expect(first.preferences['monoFontFamily'], 'JetBrains Mono');
    expect(first.preferences['codeFontSize'], 13);
    expect(first.preferences['contentMaxWidth'], 1040);
    expect(first.preferences['uiFontFamily'], 'Inter');
    expect(jsonMap(first.preferences['keyboardShortcuts'])['message-input.send'], 'Mod+Enter');
    expect(jsonMap(first.preferences['keyboardShortcuts'])['theme.cycle'], '');
    expect(jsonMap(first.preferences['navigation'])['hostFilters'], ['srv-home']);
    expect(jsonMap(first.preferences['navigation']).containsKey('sort'), isFalse);
    expect(first.preferences['sidebarWidth'], 286);
    expect(first.selected?.id, 'session-9');
    expect(first.selected?.workspaceId, 'opaque:workspace');
    expect(first.layout, isA<PanelSplit>());
    expect((first.layout as PanelSplit).ratio, 0.7);
    expect(first.sidebarVisible, isFalse);
    expect(first.tabs.map((t) => t.kind), ['agent', 'file']);
    expect(jsonMap(first.preferences['legacyDrafts']).object('drafts'), isNotEmpty);
    first.preferences.locale = 'ja';
    await first.savePreferences();
    await first.disposeAsync();

    final reopened = store(read);
    await reopened.initialize();
    // Every launch reads the original to re-sync; an unchanged one merges nothing.
    expect(reads, 2);
    expect(reopened.hosts, hasLength(1));
    expect(reopened.hosts.single.connections, hasLength(2));
    expect(reopened.preferences.locale, 'ja');
    final saved = jsonMap(jsonDecode(await (await stateFile()).readAsString()));
    expect(saved.object('legacyImport').object('values'), values);
    expect(await source.readAsBytes(), originalBytes);
    await reopened.disposeAsync();
  });

  test('already-created Flutter state wins per key while missing fields and hosts import', () {
    final existing = {
      'version': 1,
      'preferences': {'language': 'fr', 'openInSidePane': {'explorerFiles': false}, 'futureSetting': null},
      'hosts': [{'serverId': 'srv-home', 'config': {'id': 'new-lan', 'label': 'My new label',
        'endpoint': 'ws://192.168.10.20:6767/ws', 'password': '', 'expectedServerId': 'srv-home'}, 'connections': []}],
      'layout': {'tabs': [], 'tree': {'kind': 'pane', 'id': 'main', 'tabs': []}},
    };
    final imported = mergeLegacyState(existing, LegacyStateSnapshot(values: _legacyValues(), sources: const ['fixture']));
    final preferences = imported.object('preferences');
    expect(preferences['language'], 'fr');
    expect(preferences['theme'], 'midnight');
    expect(preferences['futureSetting'], isNull);
    expect(preferences.object('openInSidePane'), {'explorerFiles': false, 'subagents': false});
    final host = jsonMap(imported.array('hosts').single);
    expect(host.object('config')['password'], '');
    expect(host.object('config')['label'], 'My new label');
    expect(host.array('connections'), hasLength(2));
    expect(identical(mergeLegacyState(imported, LegacyStateSnapshot(values: _legacyValues(), sources: const ['fixture'])), imported), isTrue);
  });

  test('an unregistered default explorer pane imports as the explorer, not a main split', () {
    // explorerSidebarPaneIdByWorkspace never recorded the pane: the original resolves the
    // default "explorer" pane id instead (resolveExplorerSidebarPaneId) and shows it while
    // the pane is not hidden (selectIsExplorerSidebarVisible).
    JsonMap layoutFor({required bool hidden}) {
      final values = _legacyValues()..['workspace-layout-state'] = jsonEncode({'version': 2, 'state': {
        'layoutByWorkspace': {'srv-home:opaque:workspace': {
          'focusedPaneId': 'explorer',
          'root': {'kind': 'group', 'group': {'id': 'split', 'direction': 'horizontal', 'sizes': [70, 30], 'children': [
            {'kind': 'pane', 'pane': {'id': 'main', 'focusedTabId': 'agent_session-9', 'tabs': [
              {'tabId': 'agent_session-9', 'target': {'kind': 'agent', 'agentId': 'session-9'}},
            ]}},
            {'kind': 'pane', 'pane': {'id': 'explorer', 'hidden': hidden, 'focusedTabId': 'changes', 'tabs': [
              {'tabId': 'files', 'target': {'kind': 'files'}},
              {'tabId': 'changes', 'target': {'kind': 'changes_tree'}},
            ]}},
          ]}},
        }},
      }});
      return mergeLegacyState({}, LegacyStateSnapshot(values: values, sources: const ['fixture']))
          .object('layout');
    }
    final layout = layoutFor(hidden: false);
    final tree = layout.object('tree');
    expect(tree['kind'], 'pane');
    expect(tree['id'], 'main');
    expect(tree.array('tabs'), hasLength(1));
    expect(layout.array('explorerTabs'), hasLength(2));
    expect({for (final tab in layout.array('tabs').map(jsonMap)) tab['kind']},
      {'agent', 'files', 'changes_tree'});
    expect(layout['explorerVisible'], isTrue);
    expect(layout['focusedPane'], isNull);
    final hiddenLayout = layoutFor(hidden: true);
    expect(hiddenLayout['explorerVisible'], isFalse);
    expect(hiddenLayout.array('explorerTabs'), hasLength(2));
  });

  test('corrupt legacy JSON is not committed or marked and next startup retries', () async {
    final file = await stateFile();
    const before = '{"version":1,"preferences":{"language":"fr"},"hosts":[]}';
    await file.writeAsString(before);
    var bad = true;
    Future<LegacyStateSnapshot> read() async => LegacyStateSnapshot(values:
      bad ? {'@paseo:daemon-registry': '[broken'} : _legacyValues(), sources: const ['fixture']);
    final first = store(read);
    await first.initialize();
    expect(first.storageError, isA<FormatException>());
    expect(first.preferences.locale, 'fr');
    first.preferences.locale = 'en';
    await first.disposeAsync();
    expect(await file.readAsString(), before);
    bad = false;
    final retry = store(read);
    await retry.initialize();
    expect(retry.storageError, isNull);
    expect(retry.hosts, hasLength(1));
    expect(retry.preferences.locale, 'fr');
    await retry.disposeAsync();
  });

  test('corrupt destination and unavailable source cannot be reset by dispose', () async {
    final file = await stateFile();
    const broken = '{broken existing state';
    await file.writeAsString(broken);
    var called = false;
    final corrupt = store(() async { called = true; return LegacyStateSnapshot(values: _legacyValues(), sources: const []); });
    await corrupt.initialize();
    expect(called, isFalse);
    expect(corrupt.storageError, isA<FormatException>());
    await corrupt.disposeAsync();
    expect(await file.readAsString(), broken);
    await file.delete();
    final unavailable = store(() async => throw const FileSystemException('unreadable original storage'));
    await unavailable.initialize();
    expect(unavailable.storageError, isA<FileSystemException>());
    await unavailable.disposeAsync();
    expect(await file.exists(), isFalse);
  });

  test('pre-v0.4 settings key and v0.9 registry convert exactly as the original loader', () {
    final state = mergeLegacyState({}, LegacyStateSnapshot(values: {
      // LEGACY_SETTINGS_KEY with the COMPAT(uiFontSizeScale) and compactToolCalls fields.
      '@paseo:settings': jsonEncode({'theme': 'pureBlack', 'uiFontSize': 20,
        'compactToolCalls': true, 'sendBehavior': 'interrupt'}),
      '@paseo:daemon-registry': jsonEncode([
        {'serverId': ' server ', 'preferredConnectionId': 'direct:localhost:6767', 'connections': [
          // COMPAT(connectionPassword): ids omitted, password on the connection.
          {'type': 'directTcp', 'endpoint': ' 127.0.0.1:6767 ', 'password': ''},
          {'type': 'directTcp', 'endpoint': 'example.test:6767', 'password': ' \told auth  '},
          {'type': 'directTcp', 'endpoint': 'no-port'},
        ]},
        {'serverId': 'local:pending', 'connections': [{'type': 'directSocket', 'path': '/tmp/paseo.sock'}]},
        {'serverId': 'srv-child', 'label': '  ', 'password': 'child',
          'appearance': {'color': 'none', 'badgeDisplay': null},
          'connections': [{'id': 'route:server', 'type': 'routed', 'originServerId': 'server'}]},
      ]),
    }, sources: const ['fixture']), native: false);
    final preferences = AppPreferences(state.object('preferences'));
    expect(preferences['theme'], 'pureBlack');
    expect(preferences.themeMode, ThemeMode.dark);
    expect(preferences['uiBaseFontSize'], 18);
    expect(preferences['contentFontSize'], 18);
    expect(preferences['toolCallDetailLevel'], 'overview');
    expect(preferences['sendBehavior'], 'steer');
    expect(preferences.values.containsKey('uiFontSize'), isFalse);
    final hosts = state.array('hosts').map(jsonMap).toList();
    expect(hosts.map((h) => h['serverId']), ['server', 'srv-child']);
    final config = HostConfig.fromJson(hosts.first.object('config'));
    expect(config.id, 'direct:localhost:6767');
    expect(config.endpoint, 'ws://localhost:6767/ws');
    expect(config.label, 'server');
    expect(config.resolve().password, ' \told auth  ');
    expect(hosts.first.array('connections').map((c) => jsonMap(c)['id']),
      ['direct:localhost:6767', 'direct:example.test:6767']);
    final child = HostConfig.fromJson(hosts.last.object('config'));
    expect(hosts.last['originServerId'], 'server');
    expect(child.id, 'route:server');
    expect(child.label, 'srv-child');
    expect(child.password, 'child');
    expect(child.color, isNull);
    expect(child.expectedServerId, 'srv-child');
    expect(state.object('legacyImport').array('retainedOnly').join('\n'),
      allOf(contains('local:pending'), contains('invalid directTcp connection dropped')));
  });

  test('original schema catches, clamps and settings migrations are applied', () {
    JsonMap settings(Map<String, dynamic> stored, {bool native = false, List<String>? applied}) =>
      mergeLegacyState({}, LegacyStateSnapshot(values: {
        '@paseo:app-settings': jsonEncode(stored),
        if (applied != null) '@paseo:settings-migrations': jsonEncode({'applied': applied}),
      }, sources: const ['fixture']), native: native).object('preferences');
    final caught = settings({'theme': 'neon', 'language': 'xx', 'uiBaseFontSize': '17',
      'codeFontSize': 99, 'contentMaxWidth': 100, 'uiFontFamily': 'Inter; color: red',
      'monoFontFamily': '  "Fira Code", monospace ', 'toolCallDetailLevel': 'concise',
      'terminalScrollbackLines': -5}, native: true);
    expect(caught['theme'], 'auto');
    expect(AppPreferences(caught).themeMode, ThemeMode.system);
    expect(caught['language'], 'system');
    expect(caught['uiBaseFontSize'], 17);
    expect(caught['contentFontSize'], 17);
    expect(caught['codeFontSize'], 22);
    expect(caught['contentMaxWidth'], 600);
    expect(caught['uiFontFamily'], '');
    expect(caught['monoFontFamily'], '"Fira Code", monospace');
    expect(caught['toolCallDetailLevel'], 'overview');
    expect(caught['executionLogLines'], 0);
    expect(settings({})['contentFontSize'], 14);
    // Absent content size renders on the UI base (native 15), which the
    // unapplied mobile-content-16 migration then lifts to 16.
    expect(settings({}, native: true)['contentFontSize'], 16);
    expect(settings({}, native: true, applied: ['mobile-content-16'])['contentFontSize'], 15);
    expect(settings({'contentFontSize': 15}, native: true)['contentFontSize'], 16);
    expect(settings({'contentFontSize': 15}, native: true, applied: ['mobile-content-16'])['contentFontSize'], 15);
    expect(settings({'sendBehavior': 'interrupt'}, applied: ['steer-default'])['sendBehavior'], 'interrupt');
    for (final entry in {
      'light': ThemeMode.light, 'dark': ThemeMode.dark, 'auto': ThemeMode.system,
      'zinc': ThemeMode.dark, 'midnight': ThemeMode.dark, 'claude': ThemeMode.dark,
      'ghostty': ThemeMode.dark, 'pureBlack': ThemeMode.dark, 'plugin': ThemeMode.system,
    }.entries) {
      final imported = settings({'theme': entry.key});
      expect(imported['theme'], entry.key);
      expect(AppPreferences(imported).themeMode, entry.value, reason: entry.key);
    }
  });

  test('a discovered local daemon host absorbs its original registry entry', () {
    final state = mergeLegacyState({
      'version': 1,
      'hosts': [{'serverId': 'uuid-1', 'config': {'id': 'uuid-1', 'label': 'This computer',
        'endpoint': '127.0.0.1:6767', 'localCredentialPath': '/home/user/.paseo/local-credential'},
        'connections': []}],
    }, LegacyStateSnapshot(values: {
      '@paseo:daemon-registry': jsonEncode([{'serverId': 'srv-local', 'password': 'pw',
        'connections': [{'id': 'direct:localhost:6767', 'type': 'directTcp', 'endpoint': 'localhost:6767'}]}]),
    }, sources: const ['fixture']), native: false);
    final host = jsonMap(state.array('hosts').single);
    final config = host.object('config');
    expect(config['id'], 'uuid-1');
    expect(config['label'], 'This computer');
    expect(config['localCredentialPath'], '/home/user/.paseo/local-credential');
    expect(config['expectedServerId'], 'srv-local');
    expect(config['password'], 'pw');
    expect(host.array('connections').map((c) => jsonMap(c)['id']), ['uuid-1']);
    expect(host['serverId'], 'srv-local');
  });

  Map<String, String> withSettings(Map<String, dynamic> changes) {
    final values = _legacyValues();
    values['@paseo:app-settings'] = jsonEncode({
      ...jsonMap(jsonDecode(values['@paseo:app-settings']!)),
      ...changes,
    });
    return values;
  }

  test('re-sync adopts a setting changed only in the original after the first import', () async {
    var values = _legacyValues();
    Future<LegacyStateSnapshot> read() async =>
        LegacyStateSnapshot(values: Map.of(values), sources: const ['fixture']);
    final first = store(read);
    await first.initialize();
    expect(first.preferences['theme'], 'midnight');
    await first.disposeAsync();

    // The user rolled back to the original app and kept working there.
    values = withSettings({'theme': 'light', 'codeFontSize': 15});
    final reopened = store(read);
    await reopened.initialize();
    expect(reopened.storageError, isNull);
    expect(reopened.preferences['theme'], 'light');
    expect(reopened.preferences.themeMode, ThemeMode.light);
    expect(reopened.preferences['codeFontSize'], 15);
    expect(reopened.preferences.locale, 'ko');
    // Nothing else moved: same host, same restored session.
    expect(reopened.hosts, hasLength(1));
    expect(reopened.selected?.id, 'session-9');
    final saved = jsonMap(jsonDecode(await (await stateFile()).readAsString()));
    expect(saved.object('legacyImport').object('values'), values);
    expect(saved.object('legacyImport')['digest'], legacyDigest(values));
    await reopened.disposeAsync();
  });

  test('re-sync keeps a setting changed in Flutter even when the original changed it too', () async {
    var values = _legacyValues();
    Future<LegacyStateSnapshot> read() async =>
        LegacyStateSnapshot(values: Map.of(values), sources: const ['fixture']);
    final first = store(read);
    await first.initialize();
    first.preferences.locale = 'ja';
    first.preferences['theme'] = 'claude';
    await first.savePreferences();
    await first.disposeAsync();

    values = withSettings({'language': 'fr', 'theme': 'light', 'vimKeybindings': false});
    final reopened = store(read);
    await reopened.initialize();
    expect(reopened.storageError, isNull);
    expect(reopened.preferences.locale, 'ja');
    expect(reopened.preferences['theme'], 'claude');
    // Changed only in the original: adopted alongside the kept Flutter edits.
    expect(reopened.preferences['vimKeybindings'], isFalse);
    await reopened.disposeAsync();

    // The new base is the original's latest snapshot: a later original-only
    // change still lands, and Flutter's edits still win.
    values = withSettings({'language': 'fr', 'theme': 'light', 'vimKeybindings': true});
    final again = store(read);
    await again.initialize();
    expect(again.preferences['vimKeybindings'], isTrue);
    expect(again.preferences.locale, 'ja');
    expect(again.preferences['theme'], 'claude');
    await again.disposeAsync();
  });

  test('re-sync adds a host new in the original once, absorbing a same-socket Flutter host', () {
    JsonMap roundTrip(JsonMap state) => jsonMap(jsonDecode(jsonEncode(state)));
    final imported = roundTrip(mergeLegacyState({},
        LegacyStateSnapshot(values: _legacyValues(), sources: const ['fixture']), native: false));
    // Added by hand in Flutter: the same daemon the original registers next.
    imported['hosts'] = [...imported.array('hosts'), {
      'serverId': 'manual', 'originServerId': null,
      'config': {'id': 'manual', 'label': 'CI box', 'endpoint': 'ws://10.0.0.9:6767/ws'},
      'connections': [{'id': 'manual', 'label': 'CI box', 'endpoint': 'ws://10.0.0.9:6767/ws'}],
    }];
    Map<String, String> registry(String labLabel) => _legacyValues()
      ..['@paseo:daemon-registry'] = jsonEncode([
        ...jsonList(jsonDecode(_legacyValues()['@paseo:daemon-registry']!)),
        {'serverId': 'srv-lab', 'label': labLabel,
          'connections': [{'id': 'direct:10.0.0.5:6767', 'type': 'directTcp', 'endpoint': '10.0.0.5:6767'}]},
        {'serverId': 'srv-ci', 'label': 'ci',
          'connections': [{'id': 'direct:10.0.0.9:6767', 'type': 'directTcp', 'endpoint': '10.0.0.9:6767'}]},
      ]);
    final resynced = roundTrip(mergeLegacyState(imported,
        LegacyStateSnapshot(values: registry('Lab'), sources: const ['fixture']), native: false));
    final hosts = resynced.array('hosts').map(jsonMap).toList();
    expect(hosts.map((h) => h['serverId']), ['srv-home', 'srv-ci', 'srv-lab']);
    expect(hosts.first.array('connections'), hasLength(2));
    final ci = hosts[1];
    expect(ci.object('config')['label'], 'CI box');
    expect(ci.array('connections'), hasLength(1));
    expect(hosts[2].object('config')['endpoint'], 'ws://10.0.0.5:6767/ws');

    // A later original edit updates the untouched imported host in place.
    final renamed = mergeLegacyState(resynced,
        LegacyStateSnapshot(values: registry('Lab 2'), sources: const ['fixture']), native: false);
    final again = renamed.array('hosts').map(jsonMap).toList();
    expect(again, hasLength(3));
    expect(again[2].object('config')['label'], 'Lab 2');
    expect(again[1].object('config')['label'], 'CI box');
  });

  test('an unchanged original never writes, including a pre-digest import marker', () async {
    final values = _legacyValues();
    var reads = 0;
    Future<LegacyStateSnapshot> read() async {
      reads++;
      return LegacyStateSnapshot(values: Map.of(values), sources: const ['fixture']);
    }
    final first = store(read);
    await first.initialize();
    await first.disposeAsync();
    final file = await stateFile();
    final state = jsonMap(jsonDecode(await file.readAsString()));
    expect(identical(mergeLegacyState(state, LegacyStateSnapshot(values: Map.of(values), sources: const ['fixture'])), state), isTrue);
    // An earlier build's marker has no digest; any write would also replace
    // this distinctive formatting.
    state.object('legacyImport').remove('digest');
    final pretty = const JsonEncoder.withIndent('  ').convert(state);
    await file.writeAsString(pretty);
    final again = store(read);
    await again.initialize();
    expect(reads, 2);
    expect(again.storageError, isNull);
    expect(await file.readAsString(), pretty);
    await again.disposeAsync();
  });
}
