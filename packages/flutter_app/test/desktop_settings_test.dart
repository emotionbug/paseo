import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path_provider_platform_interface/path_provider_platform_interface.dart';
import 'package:paseo_sessions/platform/desktop/desktop_settings.dart';

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
}
