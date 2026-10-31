import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:path/path.dart' as p;
import 'package:paseo_sessions/platform/desktop/daemon_service.dart';

void main() {
  final binary = Platform.isWindows ? 'paseo.exe' : 'paseo';
  final manifest = <String, dynamic>{
    'schemaVersion': 1,
    'executable': binary,
    'cliShimPath': binary,
    'workingDirectory': '.',
    'environment': <String, String>{},
  };

  test('native runtime paths resolve relative to the manifest directory', () {
    final directory = p.join('release', 'runtime');
    final runtime = DesktopDaemonRuntime.fromJson(
      manifest,
      directory: directory,
    );
    final root = p.normalize(p.absolute(directory));
    expect(runtime.executable, p.join(root, binary));
    expect(runtime.cliShimPath, runtime.executable);
    expect(runtime.workingDirectory, root);
  });

  test('absolute native paths and process environment are preserved', () {
    final directory = p.join(Directory.systemTemp.path, 'Paseo runtime');
    final executable = p.join(directory, binary);
    final runtime = DesktopDaemonRuntime.fromJson({
      ...manifest,
      'executable': executable,
      'cliShimPath': executable,
      'workingDirectory': directory,
      'environment': {'PASEO_HOME': p.join(directory, 'home')},
    }, directory: p.join('different', 'directory'));
    expect(runtime.executable, executable);
    expect(runtime.cliShimPath, executable);
    expect(runtime.workingDirectory, directory);
    expect(runtime.environment, {'PASEO_HOME': p.join(directory, 'home')});
    expect(runtime.toJson().containsKey('environment'), isFalse);
  });

  test('all native runtime paths are required', () {
    for (final key in ['executable', 'cliShimPath', 'workingDirectory']) {
      for (final value in [null, '', 42]) {
        expect(
          () => DesktopDaemonRuntime.fromJson(
            {...manifest, key: value},
            directory: 'runtime',
          ),
          throwsFormatException,
          reason: '$key=$value must not silently select another runtime',
        );
      }
    }
  });

}
