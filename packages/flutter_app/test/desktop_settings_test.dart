import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:paseo_sessions/platform/desktop/desktop_settings.dart';
import 'package:paseo_sessions/platform/desktop/desktop_host.dart';

class _SupportDirectory extends PathProviderPlatform {
  _SupportDirectory(this.directory);
  final String directory;

  @override
  Future<String?> getApplicationSupportPath() async => directory;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test(
    'missing migration metadata applies the legacy quit policy only once',
    () async {
      final directory = await Directory.systemTemp.createTemp(
        'paseo-settings-test-',
      );
      final original = PathProviderPlatform.instance;
      PathProviderPlatform.instance = _SupportDirectory(directory.path);
      try {
        // An existing document avoids consulting any real user's legacy directory.
        await File('${directory.path}/desktop-settings.json').writeAsString(
          jsonEncode({
            'version': 1,
            'settings': {
              'releaseChannel': 'beta',
              'daemon': {
                'manageBuiltInDaemon': false,
                'keepRunningAfterQuit': true,
              },
            },
          }),
        );
        final settings = DesktopSettingsStore();
        await settings.initialize();
        expect(settings.managed, isFalse);
        expect(settings.releaseChannel, 'beta');
        expect(settings.keepRunning, isFalse);

        await settings.patch({
          'daemon': {'keepRunningAfterQuit': true},
        });
        final reopened = DesktopSettingsStore();
        await reopened.initialize();
        expect(reopened.keepRunning, isTrue);
        expect(reopened.managed, isFalse);
        expect(reopened.releaseChannel, 'beta');
      } finally {
        PathProviderPlatform.instance = original;
        await directory.delete(recursive: true);
      }
    },
  );

  test('first launch takes the original window bounds with its minimums', () async {
    final directory = await Directory.systemTemp.createTemp('paseo-window-state-');
    try {
      final env = {'PASEO_ELECTRON_USER_DATA_DIR': directory.path};
      final file = File('${directory.path}/window-state.json');
      expect(await DesktopHost.originalWindowState(env), isNull);
      await file.writeAsString(jsonEncode({
        'version': 1,
        'state': {'x': 16, 'y': 10.4, 'width': 1200, 'height': 800, 'isMaximized': true},
      }));
      expect(await DesktopHost.originalWindowState(env),
          {'x': 16, 'y': 10, 'width': 1200, 'height': 800, 'maximized': true});
      // A half-known position is dropped; a tiny window grows to 400×300.
      await file.writeAsString(jsonEncode({
        'version': 1,
        'state': {'x': 16, 'width': 120, 'height': 90, 'isMaximized': false},
      }));
      expect(await DesktopHost.originalWindowState(env),
          {'width': 400, 'height': 300, 'maximized': false});
      await file.writeAsString('{broken');
      expect(await DesktopHost.originalWindowState(env), isNull);
    } finally {
      await directory.delete(recursive: true);
    }
  });
}
