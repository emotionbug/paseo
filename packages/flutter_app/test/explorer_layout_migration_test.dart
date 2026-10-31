import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/legacy_state.dart';
import 'package:paseo_sessions/core/target.dart';

class _SupportDirectory extends PathProviderPlatform {
  _SupportDirectory(this.path);
  final String path;
  @override
  Future<String?> getApplicationSupportPath() async => path;
}

const _files = PanelTarget(kind: 'files', serverId: 'srv', workspaceId: 'ws', cwd: '/repo');
const _changes = PanelTarget(kind: 'changes_tree', serverId: 'srv', workspaceId: 'ws', cwd: '/repo');

Map<String, dynamic> _target(PanelTarget t, [Map<String, dynamic> data = const {}]) => {
  'kind': t.kind,
  'serverId': t.serverId,
  'workspaceId': t.workspaceId,
  'cwd': t.cwd,
  'id': t.id,
  'title': t.title,
  'data': data,
};

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late Directory directory;
  late PathProviderPlatform original;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('paseo-explorer-layout-test-');
    original = PathProviderPlatform.instance;
    PathProviderPlatform.instance = _SupportDirectory(directory.path);
  });
  tearDown(() async {
    PathProviderPlatform.instance = original;
    await directory.delete(recursive: true);
  });

  Future<AppStore> open(Map<String, dynamic> layout) async {
    final dir = Directory('${directory.path}/paseo-native');
    await dir.create(recursive: true);
    await File('${dir.path}/state.json').writeAsString(jsonEncode({
      'version': 1,
      'preferences': <String, dynamic>{},
      'hosts': <dynamic>[],
      'layout': layout,
    }));
    final store = AppStore(
      connectHosts: false,
      discoverLocalHosts: false,
      legacyStateReader: () async => const LegacyStateSnapshot(values: {}, sources: []),
    );
    await store.initialize();
    return store;
  }

  test('a pre-v2 Flutter Explorer holding only Files gains Changes, focused', () async {
    final store = await open({
      'tabs': [_target(_files)],
      'tree': {'kind': 'pane', 'id': 'main', 'tabs': <String>[]},
      'explorerTabs': [_files.key],
      'selectedExplorer': _files.key,
    });
    expect(store.explorerTabKeys, [_files.key, _changes.key]);
    expect(store.selectedExplorerKey, _changes.key);
    expect(store.explorerFocusFor(_files), _changes.key);
    await store.disposeAsync();
  });

  test('a v2 layout keeps an Explorer whose Changes view the user closed', () async {
    final store = await open({
      'version': 2,
      'tabs': [_target(_files)],
      'tree': {'kind': 'pane', 'id': 'main', 'tabs': <String>[]},
      'explorerTabs': [_files.key],
      'selectedExplorer': _files.key,
    });
    expect(store.explorerTabKeys, [_files.key]);
    expect(store.selectedExplorerKey, _files.key);
    await store.disposeAsync();
  });

  test('an imported original Explorer stays authoritative', () async {
    final store = await open({
      'tabs': [_target(_files, {'legacyImported': true})],
      'tree': {'kind': 'pane', 'id': 'main', 'tabs': <String>[]},
      'explorerTabs': [_files.key],
      'selectedExplorer': _files.key,
    });
    expect(store.explorerTabKeys, [_files.key]);
    await store.disposeAsync();
  });
}
