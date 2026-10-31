import 'dart:async';
import 'dart:convert';
import 'dart:ffi' show Abi;
import 'dart:io';
import 'dart:math';

import 'package:crypto/crypto.dart';
import 'package:flutter/services.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

/// Native Flutter releases deliberately never consume Electron updater assets.
/// Trust is GitHub HTTPS + SHA512, with installed platform signing identity retained.
class DesktopUpdater {
  static const _version = String.fromEnvironment('PASEO_VERSION');
  static const _defaultChannel = String.fromEnvironment(
    'PASEO_RELEASE_CHANNEL',
    defaultValue: 'stable',
  );
  static const _repository = String.fromEnvironment(
    'PASEO_UPDATE_REPOSITORY',
    defaultValue: 'emotionbug/paseo',
  );
  static const _appId = 'sh.paseo.sessions.desktop';
  static const _helperAsset = '../flutter_desktop_host/updater/replace.sh';

  Future<void> _queue = Future<void>.value();
  _Release? _admitted;
  _Prepared? _prepared;
  String? _lastError;
  String? _installLog;

  String get _platform => Platform.isMacOS ? 'macos' : Platform.operatingSystem;
  String get _arch {
    final abi = Abi.current().toString();
    if (abi.endsWith('_x64')) return 'x64';
    if (abi.endsWith('_arm64')) return 'arm64';
    throw UnsupportedError('No Flutter updater package for $abi.');
  }

  Future<T> _exclusive<T>(Future<T> Function() action) {
    final result = _queue.then((_) => action());
    _queue = result.then<void>((_) {}, onError: (Object _, StackTrace _) {});
    return result;
  }

  Future<Map<String, dynamic>> diagnostics() async => {
    'currentVersion': _version,
    'releaseChannel': _defaultChannel,
    'repository': _repository,
    'platform': _platform,
    'abi': Abi.current().toString(),
    'configured': _version.isNotEmpty,
    'manifestPattern': 'flutter-<channel>-$_platform-<arch>.json',
    'lastError': _lastError,
    'preparedVersion': _prepared?.release.version,
    'installLog': _installLog,
  };

  Future<Map<String, dynamic>> check({
    String? releaseChannel,
    bool manual = false,
  }) => _exclusive(() => _check(releaseChannel ?? _defaultChannel, manual));

  Future<Map<String, dynamic>> _check(String channel, bool manual) async {
    _Release? release;
    try {
      _validateConfiguration(channel);
      release = await _discover(channel);
      final newer =
          _Version.parse(release.version).compareTo(_Version.parse(_version)) >
          0;
      if (!newer ||
          (!manual &&
              !await _admit(release) &&
              _admitted?.key != release.key)) {
        _admitted = null;
        await _discardPrepared();
        _lastError = null;
        return _checkResult(null);
      }
      _admitted = release;
      await _prepare(release);
      _lastError = null;
      return _checkResult(release);
    } catch (error) {
      _lastError = error.toString();
      return _checkResult(
        _admitted?.key == release?.key ? release : null,
        error: _lastError,
      );
    }
  }

  Map<String, dynamic> _checkResult(_Release? release, {String? error}) => {
    'hasUpdate': release != null,
    'readyToInstall': release != null && _prepared?.release.key == release.key,
    'currentVersion': _version,
    'latestVersion': release?.version ?? _version,
    'body': release?.notes,
    'date': release?.date,
    'errorMessage': error,
  };

  Future<Map<String, dynamic>> install({
    String? releaseChannel,
    required Future<void> Function() beforeQuit,
  }) => _exclusive(() async {
    try {
      final channel = releaseChannel ?? _defaultChannel;
      final checked = await _check(channel, true);
      if (checked['errorMessage'] != null || checked['hasUpdate'] != true) {
        return _installResult(
          false,
          false,
          _version,
          checked['errorMessage'] as String? ?? 'No update available.',
        );
      }
      final prepared = _prepared!;
      // A newer publication or withdrawn package must invalidate an older download.
      final current = await _discover(channel);
      if (current.key != prepared.release.key) {
        await _discardPrepared();
        throw StateError(
          'The release changed during download. Check for updates again.',
        );
      }
      await _validateFile(prepared.file, prepared.release);
      await _validateNative(prepared);
      final replacement = await _stageReplacement(prepared);
      File? helper;
      File? log;
      try {
        if (replacement != null) {
          helper = File(p.join(prepared.directory.path, 'replace.sh'));
          await helper.writeAsString(
            await rootBundle.loadString(_helperAsset),
            flush: true,
          );
          log = File(p.join(prepared.directory.path, 'install.log'));
          await log.writeAsString(
            'Replacing ${prepared.release.version}\n',
            flush: true,
          );
          _installLog = log.path;
        }
        await beforeQuit();
      } catch (_) {
        if (replacement != null) {
          await replacement.directory.delete(recursive: true);
        }
        rethrow;
      }
      if (replacement != null) {
        try {
          await Process.start('/bin/sh', [
            helper!.path,
            '$pid',
            replacement.target,
            replacement.staged,
            replacement.backup,
            _platform,
            log!.path,
          ], mode: ProcessStartMode.detached);
        } catch (_) {
          await replacement.directory.delete(recursive: true);
          rethrow;
        }
        return _installResult(
          false,
          true,
          prepared.release.version,
          'Update verified. The application will close, replace itself, and restart.',
        );
      }
      if (Platform.isWindows) {
        await _powershell(
          "\$ErrorActionPreference = 'Stop'; Start-Process -FilePath ${_powershellQuote(prepared.file.path)} -PassThru | Select-Object -ExpandProperty Id",
        );
        return _installResult(
          false,
          true,
          prepared.release.version,
          'The verified installer has started. Complete its installation prompts.',
        );
      }
      final command = prepared.release.format == 'deb'
          ? '/usr/bin/dpkg'
          : '/usr/bin/rpm';
      final args = prepared.release.format == 'deb'
          ? [command, '-i', prepared.file.path]
          : [command, '-U', prepared.file.path];
      await _run('/usr/bin/pkexec', args);
      _lastError = null;
      return _installResult(
        true,
        true,
        prepared.release.version,
        'The native package manager installed the update. Restart the application.',
      );
    } catch (error) {
      _lastError = error.toString();
      return _installResult(false, false, _version, 'Update failed: $error');
    }
  });

  Map<String, dynamic> _installResult(
    bool installed,
    bool launched,
    String version,
    String message,
  ) => {
    'installed': installed,
    'installerLaunched': launched,
    'version': version,
    'message': message,
  };

  void _validateConfiguration(String channel) {
    if (!(Platform.isMacOS || Platform.isWindows || Platform.isLinux)) {
      throw UnsupportedError('Native application updates are desktop-only.');
    }
    if (_version.isEmpty) {
      throw StateError(
        'This build has no PASEO_VERSION release identity. Install a packaged Flutter release.',
      );
    }
    _Version.parse(_version);
    if (channel != 'stable' && channel != 'beta') {
      throw ArgumentError('Unknown release channel: $channel');
    }
    if (!RegExp(r'^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$').hasMatch(_repository)) {
      throw StateError(
        'PASEO_UPDATE_REPOSITORY must be a GitHub owner/repository.',
      );
    }
  }

  Future<_Release> _discover(String channel) async {
    final format = await _installedFormat();
    final name = 'flutter-$channel-$_platform-$_arch.json';
    _Release? newest;
    // Pagination is bounded; missing a published Flutter channel is an explicit error.
    for (var page = 1; page <= 10; page++) {
      final data = await _json(
        Uri.https('api.github.com', '/repos/$_repository/releases', {
          'per_page': '100',
          'page': '$page',
        }),
      );
      if (data is! List) {
        throw const FormatException('GitHub did not return a release list.');
      }
      for (final value in data) {
        final release = _map(value);
        if (release['draft'] == true ||
            (channel == 'stable' && release['prerelease'] == true)) {
          continue;
        }
        final assets = (release['assets'] as List? ?? const [])
            .map(_map)
            .toList();
        final manifests = assets
            .where((asset) => asset['name'] == name)
            .toList();
        if (manifests.isEmpty) continue;
        if (manifests.length != 1) {
          throw StateError('Ambiguous Flutter release manifest $name.');
        }
        final manifest = _map(await _json(_assetUrl(manifests.single)));
        if (manifest['schemaVersion'] != 1 ||
            manifest['runtime'] != 'flutter' ||
            manifest['appId'] != _appId ||
            manifest['channel'] != channel ||
            manifest['platform'] != _platform ||
            manifest['arch'] != _arch) {
          throw StateError('Flutter release identity mismatch in $name.');
        }
        final version = manifest['version'] as String;
        final parsed = _Version.parse(version);
        if (channel == 'stable' && parsed.prerelease.isNotEmpty) {
          throw StateError(
            'Stable update manifest contains prerelease $version.',
          );
        }
        final entries = manifest['artifacts'] is List
            ? (manifest['artifacts'] as List).map(_map).toList()
            : [_map(manifest['artifact'])];
        final candidates = entries
            .where((entry) => entry['format'] == format)
            .toList();
        if (candidates.length != 1) {
          throw StateError(
            'Release $version has no unique $format Flutter artifact.',
          );
        }
        final artifact = candidates.single;
        final artifactName = artifact['name'] as String;
        if (p.basename(artifactName) != artifactName ||
            artifactName.contains('\\') ||
            artifactName == '.' ||
            artifactName == '..') {
          throw const FormatException('Invalid update asset filename.');
        }
        final matches = assets
            .where((asset) => asset['name'] == artifactName)
            .toList();
        if (matches.length != 1) {
          throw StateError('Release artifact $artifactName is unavailable.');
        }
        final size = artifact['size'];
        final digest = artifact['sha512'];
        if (size is! int ||
            size <= 0 ||
            size > 8 * 1024 * 1024 * 1024 ||
            digest is! String ||
            base64Decode(digest).length != 64 ||
            matches.single['size'] != size) {
          throw const FormatException(
            'Invalid release artifact size or SHA512.',
          );
        }
        final candidate = _Release(
          version: version,
          channel: channel,
          name: artifactName,
          format: format,
          size: size,
          digest: digest,
          url: _assetUrl(matches.single),
          notes:
              manifest['releaseNotes'] as String? ?? release['body'] as String?,
          date:
              manifest['releaseDate'] as String? ??
              release['published_at'] as String?,
          rollout: double.tryParse('${manifest['rolloutHours']}'),
        );
        if (newest == null ||
            parsed.compareTo(_Version.parse(newest.version)) > 0) {
          newest = candidate;
        }
      }
      if (data.length < 100) break;
    }
    if (newest == null) {
      throw StateError(
        'No $channel Flutter release is published for $_platform/$_arch in $_repository.',
      );
    }
    return newest;
  }

  Uri _assetUrl(Map<String, dynamic> asset) {
    final uri = Uri.parse(asset['browser_download_url'] as String);
    if (uri.scheme != 'https' ||
        uri.host != 'github.com' ||
        !uri.path.startsWith('/$_repository/releases/download/') ||
        uri.userInfo.isNotEmpty) {
      throw const FormatException(
        'Release asset is not hosted by the configured GitHub repository.',
      );
    }
    return uri;
  }

  Future<String> _installedFormat() async {
    if (Platform.isWindows) return 'exe';
    if (Platform.isMacOS) {
      _installedApp();
      return 'zip';
    }
    final image = Platform.environment['APPIMAGE'];
    if (image != null && image.isNotEmpty && await File(image).exists()) {
      return 'appimage';
    }
    final executable = await File(Platform.resolvedExecutable)
        .resolveSymbolicLinks();
    if (await File('/usr/bin/dpkg-query').exists()) {
      final result = await Process.run('/usr/bin/dpkg-query', [
        '-S',
        executable,
      ]);
      if (result.exitCode == 0 &&
          RegExp(
            r'^paseo-sessions(?::[^: ]+)?: ',
            multiLine: true,
          ).hasMatch('${result.stdout}')) {
        return 'deb';
      }
    }
    if (await File('/usr/bin/rpm').exists()) {
      final result = await Process.run('/usr/bin/rpm', [
        '-qf',
        '--qf',
        '%{NAME}',
        executable,
      ]);
      if (result.exitCode == 0 &&
          '${result.stdout}'.trim() == 'paseo-sessions') {
        return 'rpm';
      }
    }
    throw StateError(
      'This Linux installation is not an AppImage or an installed paseo-sessions deb/rpm. Use a packaged Flutter release.',
    );
  }

  String _installedApp() {
    final executable = Platform.resolvedExecutable;
    final marker = executable.lastIndexOf('.app/Contents/MacOS/');
    if (marker < 0) {
      throw StateError(
        'The running executable is not inside a macOS application bundle.',
      );
    }
    return executable.substring(0, marker + 4);
  }

  Future<bool> _admit(_Release release) async {
    final hours = release.rollout;
    if (release.channel != 'stable' ||
        hours == null ||
        !hours.isFinite ||
        hours <= 0) {
      return true;
    }
    final date = DateTime.tryParse(release.date ?? '');
    if (date == null) return true;
    final age =
        DateTime.now().toUtc().difference(date.toUtc()).inMilliseconds /
        3600000;
    if (age < 0) return false;
    final support = await getApplicationSupportDirectory();
    await support.create(recursive: true);
    final file = File(p.join(support.path, '.updaterId'));
    String id = await file.exists() ? (await file.readAsString()).trim() : '';
    if (!RegExp(
      r'^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$',
    ).hasMatch(id)) {
      final random = Random.secure();
      final bytes = List<int>.generate(16, (_) => random.nextInt(256));
      bytes[6] = (bytes[6] & 15) | 64;
      bytes[8] = (bytes[8] & 63) | 128;
      final hex = bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
      id =
          '${hex.substring(0, 8)}-${hex.substring(8, 12)}-${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20)}';
      await file.writeAsString(id, flush: true);
    }
    final bucket =
        int.parse(id.replaceAll('-', '').substring(24), radix: 16) /
        0x100000000;
    return bucket < min(1, age / hours);
  }

  Future<void> _prepare(_Release release) async {
    if (_prepared?.release.key == release.key) return;
    await _discardPrepared();
    final directory = await Directory.systemTemp.createTemp(
      'paseo-flutter-update-',
    );
    final file = File(p.join(directory.path, release.name));
    try {
      await _download(release.url, file, release.size);
      await _validateFile(file, release);
      final prepared = _Prepared(release, directory, file);
      await _validateNative(prepared);
      _prepared = prepared;
    } catch (_) {
      await directory.delete(recursive: true);
      rethrow;
    }
  }

  Future<void> _discardPrepared() async {
    final old = _prepared;
    _prepared = null;
    if (old != null && await old.directory.exists()) {
      await old.directory.delete(recursive: true);
    }
  }

  Future<void> _validateFile(File file, _Release release) async {
    if (await file.length() != release.size ||
        base64Encode((await sha512.bind(file.openRead()).first).bytes) !=
            release.digest) {
      throw StateError(
        'The downloaded update failed its SHA512/size verification.',
      );
    }
  }

  Future<void> _validateNative(_Prepared prepared) async {
    final release = prepared.release;
    if (Platform.isWindows) {
      final script =
          '''
\$ErrorActionPreference = 'Stop'
\$old = Get-AuthenticodeSignature -LiteralPath ${_powershellQuote(Platform.resolvedExecutable)}
\$new = Get-AuthenticodeSignature -LiteralPath ${_powershellQuote(prepared.file.path)}
if (\$new.Status -ne 'Valid' -and \$new.Status -ne 'NotSigned') { throw 'Installer Authenticode signature is invalid.' }
if (\$old.Status -eq 'Valid') {
  if (\$new.Status -ne 'Valid' -or \$old.SignerCertificate.Subject -ne \$new.SignerCertificate.Subject) { throw 'Installer publisher does not match the installed application.' }
} elseif (\$old.Status -ne 'NotSigned') { throw 'The installed application has an invalid Authenticode signature.' }
''';
      await _powershell(script);
      return;
    }
    if (Platform.isMacOS) {
      final extracted = Directory(p.join(prepared.directory.path, 'expanded'));
      if (await extracted.exists()) await extracted.delete(recursive: true);
      await extracted.create();
      // Validate archive members before extracting to reject traversal outside staging.
      final listing = await _run('/usr/bin/unzip', ['-Z1', prepared.file.path]);
      for (final member
          in listing.split('\n').where((entry) => entry.isNotEmpty)) {
        if (p.posix.isAbsolute(member) ||
            member.split('/').contains('..') ||
            member.contains('\\')) {
          throw const FormatException('Unsafe path in macOS update archive.');
        }
      }
      await _run('/usr/bin/ditto', [
        '-x',
        '-k',
        prepared.file.path,
        extracted.path,
      ]);
      final apps = await extracted
          .list(followLinks: false)
          .where((entry) => entry is Directory && entry.path.endsWith('.app'))
          .toList();
      if (apps.length != 1) {
        throw StateError(
          'The macOS archive must contain exactly one root application bundle.',
        );
      }
      prepared.app = apps.single.path;
      final plist = p.join(prepared.app!, 'Contents', 'Info.plist');
      final id = (await _run('/usr/libexec/PlistBuddy', [
        '-c',
        'Print :CFBundleIdentifier',
        plist,
      ])).trim();
      final version = (await _run('/usr/libexec/PlistBuddy', [
        '-c',
        'Print :CFBundleShortVersionString',
        plist,
      ])).trim();
      if (id != _appId || version != release.version) {
        throw StateError('macOS package bundle identity/version mismatch.');
      }
      await _run('/usr/bin/codesign', [
        '--verify',
        '--deep',
        '--strict',
        prepared.app!,
      ]);
      final oldIdentity = await _signature(_installedApp());
      final newIdentity = await _signature(prepared.app!);
      final team = RegExp(r'^TeamIdentifier=(.+)$', multiLine: true);
      final oldTeam = team.firstMatch(oldIdentity)?.group(1);
      final newTeam = team.firstMatch(newIdentity)?.group(1);
      if (oldTeam != null && oldTeam != 'not set') {
        if (newTeam != oldTeam) {
          throw StateError(
            'The update signing team does not match the installed application.',
          );
        }
        await _run('/usr/sbin/spctl', [
          '--assess',
          '--type',
          'execute',
          prepared.app!,
        ]);
      } else if (!oldIdentity.contains('Signature=adhoc') ||
          !newIdentity.contains('Signature=adhoc')) {
        throw StateError(
          'The installed application has no verifiable macOS signing identity.',
        );
      }
      return;
    }
    if (release.format == 'appimage') {
      final header = await prepared.file.open();
      try {
        final bytes = await header.read(20);
        if (bytes.length != 20 ||
            bytes[0] != 127 ||
            bytes[1] != 69 ||
            bytes[2] != 76 ||
            bytes[3] != 70 ||
            bytes[4] != 2 ||
            bytes[5] != 1 ||
            bytes[8] != 65 ||
            bytes[9] != 73 ||
            bytes[10] != 2 ||
            (bytes[18] | (bytes[19] << 8)) != (_arch == 'x64' ? 62 : 183)) {
          throw const FormatException(
            'Update is not a type-2 AppImage for this architecture.',
          );
        }
      } finally {
        await header.close();
      }
      return;
    }
    if (!await File('/usr/bin/pkexec').exists()) {
      throw StateError(
        'Package updates require polkit pkexec, which is not installed.',
      );
    }
    final values = release.format == 'deb'
        ? await _run('/usr/bin/dpkg-deb', [
            '--show',
            '--showformat=\${Package}\n\${Version}\n\${Architecture}\n',
            prepared.file.path,
          ])
        : await _run('/usr/bin/rpm', [
            '-qp',
            '--qf',
            '%{NAME}\n%{VERSION}\n%{ARCH}\n',
            prepared.file.path,
          ]);
    final fields = values.trim().split('\n');
    final expectedArch = release.format == 'deb'
        ? (_arch == 'x64' ? 'amd64' : 'arm64')
        : (_arch == 'x64' ? 'x86_64' : 'aarch64');
    if (fields.length != 3 ||
        fields[0] != 'paseo-sessions' ||
        fields[1] != release.version ||
        fields[2] != expectedArch) {
      throw StateError(
        'The native package name/version/architecture does not match the release manifest.',
      );
    }
  }

  Future<String> _signature(String app) async {
    final result = await Process.run('/usr/bin/codesign', [
      '-d',
      '--verbose=4',
      app,
    ]);
    if (result.exitCode != 0) {
      throw StateError(
        'Cannot read installed macOS signing identity: ${result.stderr}',
      );
    }
    return '${result.stdout}\n${result.stderr}';
  }

  Future<_Replacement?> _stageReplacement(_Prepared prepared) async {
    if (!Platform.isMacOS && prepared.release.format != 'appimage') return null;
    final target = Platform.isMacOS
        ? _installedApp()
        : await File(Platform.environment['APPIMAGE']!).resolveSymbolicLinks();
    final directory = await Directory(p.dirname(target))
        .createTemp('.paseo-update-');
    final staged = p.join(directory.path, p.basename(target));
    try {
      if (Platform.isMacOS) {
        await _run('/usr/bin/ditto', [prepared.app!, staged]);
      } else {
        await prepared.file.copy(staged);
        await _run('/bin/chmod', ['755', staged]);
      }
      return _Replacement(
        directory,
        target,
        staged,
        p.join(directory.path, 'previous'),
      );
    } catch (_) {
      await directory.delete(recursive: true);
      rethrow;
    }
  }

  String _powershellQuote(String value) => "'${value.replaceAll("'", "''")}'";

  Future<String> _powershell(String script) {
    final root = Platform.environment['SystemRoot'];
    if (root == null) throw StateError('Windows SystemRoot is unavailable.');
    final encoded = base64Encode(
      script.codeUnits.expand((value) => [value & 255, value >> 8]).toList(),
    );
    return _run(
      p.join(root, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
      ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', encoded],
    );
  }

  Future<String> _run(String executable, List<String> args) async {
    if (!await File(executable).exists()) {
      throw StateError(
        'Required native update tool is unavailable: $executable',
      );
    }
    final result = await Process.run(executable, args);
    if (result.exitCode != 0) {
      throw StateError(
        '${p.basename(executable)} failed (${result.exitCode}): ${result.stderr}\n${result.stdout}',
      );
    }
    return '${result.stdout}';
  }

  Future<Object?> _json(Uri uri) async {
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 30);
    try {
      final response = await _request(client, uri);
      final bytes = <int>[];
      await for (final chunk in response.timeout(const Duration(seconds: 30))) {
        bytes.addAll(chunk);
        if (bytes.length > 16 * 1024 * 1024) {
          throw const FormatException('Update metadata exceeds 16 MiB.');
        }
      }
      return jsonDecode(utf8.decode(bytes));
    } finally {
      client.close(force: true);
    }
  }

  Future<void> _download(Uri uri, File file, int size) async {
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 30);
    final sink = file.openWrite();
    try {
      final response = await _request(client, uri);
      var received = 0;
      await sink.addStream(
        response.timeout(const Duration(minutes: 2)).map((chunk) {
          received += chunk.length;
          if (received > size) {
            throw const FormatException(
              'Update download exceeds the published size.',
            );
          }
          return chunk;
        }),
      );
      await sink.flush();
      if (received != size) {
        throw const FormatException('Update download is incomplete.');
      }
    } finally {
      client.close(force: true);
      await sink.close();
    }
  }

  Future<HttpClientResponse> _request(HttpClient client, Uri uri) async {
    for (var redirect = 0; redirect <= 5; redirect++) {
      if (uri.scheme != 'https' || uri.userInfo.isNotEmpty) {
        throw const FormatException('Updates require HTTPS.');
      }
      final request = await client
          .getUrl(uri)
          .timeout(const Duration(seconds: 30));
      request.followRedirects = false;
      request.headers.set(HttpHeaders.userAgentHeader, 'Paseo-Flutter-Updater');
      request.headers.set(
        HttpHeaders.acceptHeader,
        'application/vnd.github+json, application/octet-stream',
      );
      final response = await request.close().timeout(
        const Duration(seconds: 30),
      );
      if ([301, 302, 303, 307, 308].contains(response.statusCode)) {
        final location = response.headers.value(HttpHeaders.locationHeader);
        if (location == null) {
          throw const HttpException('Update redirect has no location.');
        }
        uri = uri.resolve(location);
        await response.drain<void>();
        continue;
      }
      if (response.statusCode != 200) {
        throw HttpException(
          'Update source returned HTTP ${response.statusCode}; release may be unavailable or GitHub rate-limited.',
          uri: uri,
        );
      }
      return response;
    }
    throw const HttpException('Too many update redirects.');
  }
}

Map<String, dynamic> _map(Object? value) {
  if (value is! Map) {
    throw const FormatException('Expected an update metadata object.');
  }
  return Map<String, dynamic>.from(value);
}

class _Release {
  const _Release({
    required this.version,
    required this.channel,
    required this.name,
    required this.format,
    required this.size,
    required this.digest,
    required this.url,
    this.notes,
    this.date,
    this.rollout,
  });
  final String version, channel, name, format, digest;
  final int size;
  final Uri url;
  final String? notes, date;
  final double? rollout;
  String get key => '$channel:$version:$format:$digest:$size:${url.toString()}';
}

class _Prepared {
  _Prepared(this.release, this.directory, this.file);
  final _Release release;
  final Directory directory;
  final File file;
  String? app;
}

class _Replacement {
  const _Replacement(this.directory, this.target, this.staged, this.backup);
  final Directory directory;
  final String target, staged, backup;
}

class _Version implements Comparable<_Version> {
  const _Version(this.core, this.prerelease);
  final List<int> core;
  final List<String> prerelease;
  factory _Version.parse(String value) {
    final match = RegExp(
      r'^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$',
    ).firstMatch(value);
    if (match == null) throw FormatException('Invalid release version: $value');
    final pre = match.group(4)?.split('.') ?? const <String>[];
    if (pre.any(
      (part) =>
          part.isEmpty ||
          (RegExp(r'^[0-9]+$').hasMatch(part) &&
              part.length > 1 &&
              part.startsWith('0')),
    )) {
      throw FormatException('Invalid prerelease version: $value');
    }
    return _Version([
      int.parse(match.group(1)!),
      int.parse(match.group(2)!),
      int.parse(match.group(3)!),
    ], pre);
  }
  @override
  int compareTo(_Version other) {
    for (var i = 0; i < 3; i++) {
      final result = core[i].compareTo(other.core[i]);
      if (result != 0) return result;
    }
    if (prerelease.isEmpty || other.prerelease.isEmpty) {
      return prerelease.isEmpty ? (other.prerelease.isEmpty ? 0 : 1) : -1;
    }
    for (var i = 0; i < min(prerelease.length, other.prerelease.length); i++) {
      final left = int.tryParse(prerelease[i]);
      final right = int.tryParse(other.prerelease[i]);
      final result = left != null && right != null
          ? left.compareTo(right)
          : left != null
          ? -1
          : right != null
          ? 1
          : prerelease[i].compareTo(other.prerelease[i]);
      if (result != 0) return result;
    }
    return prerelease.length.compareTo(other.prerelease.length);
  }
}
