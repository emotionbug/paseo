import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:paseo_sessions/core/app_store.dart';
import 'package:paseo_sessions/core/json.dart';
import 'package:paseo_sessions/core/legacy_state.dart';

class _SupportDirectory extends PathProviderPlatform {
  _SupportDirectory(this.path);
  final String path;
  @override
  Future<String?> getApplicationSupportPath() async => path;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late Directory directory;
  late PathProviderPlatform original;
  setUp(() async {
    directory = await Directory.systemTemp.createTemp('paseo-state-writes-test-');
    original = PathProviderPlatform.instance;
    PathProviderPlatform.instance = _SupportDirectory(directory.path);
  });
  tearDown(() async {
    PathProviderPlatform.instance = original;
    await directory.delete(recursive: true);
  });

  test('a save with nothing changed leaves state.json alone; a change still lands', () async {
    final store = AppStore(
      connectHosts: false,
      discoverLocalHosts: false,
      legacyStateReader: () async => const LegacyStateSnapshot(values: {}, sources: []),
    );
    await store.initialize();
    store.preferences.locale = 'ja';
    await store.savePreferences();
    final file = File('${directory.path}/paseo-native/state.json');
    // Any write would replace this distinctive formatting.
    final pretty = const JsonEncoder.withIndent('  ').convert(jsonDecode(await file.readAsString()));
    await file.writeAsString(pretty);

    await store.savePreferences();
    expect(await file.readAsString(), pretty);

    store.preferences.locale = 'ko';
    await store.savePreferences();
    final saved = await file.readAsString();
    expect(saved, isNot(pretty));
    expect(jsonMap(jsonDecode(saved)).object('preferences')['language'], 'ko');
    await store.disposeAsync();
    // Disposing an unchanged store writes nothing either.
    expect(await file.readAsString(), saved);
  });
}
