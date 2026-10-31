import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:path/path.dart' as p;

class DesktopEditorTarget {
  const DesktopEditorTarget(
    this.id,
    this.label,
    this.commands, {
    this.kind = 'editor',
    this.macApplication,
    this.macLauncher,
    this.windowsDirectory,
    this.windowsLauncher,
    this.style = 'goto',
  });
  final String id;
  final String label;
  final String kind;
  final List<String> commands;
  final String? macApplication;
  final String? macLauncher;
  final String? windowsDirectory;
  final String? windowsLauncher;
  final String style;
}

class DesktopIntegrations {
  DesktopIntegrations();
  static final instance = DesktopIntegrations();
  static const _native = MethodChannel('paseo/desktop_integrations');
  final _notifications = FlutterLocalNotificationsPlugin();
  final _clicks = StreamController<Map<String, dynamic>>.broadcast();
  final _pendingClicks = <Map<String, dynamic>>[];
  final _sessionEnding = StreamController<void>.broadcast();
  final _sessionEnded = StreamController<void>.broadcast();
  final _sessionEndingCancelled = StreamController<void>.broadcast();
  Stream<void> get sessionEnded => _sessionEnded.stream;
  Stream<void> get sessionEndingCancelled => _sessionEndingCancelled.stream;
  final _activated = StreamController<void>.broadcast();
  Stream<void> get sessionEnding => _sessionEnding.stream;
  Stream<void> get activated => _activated.stream;
  Future<void>? _initialization;
  Future<void>? _notificationInitialization;
  bool _disposed = false;
  bool _lifecycleReady = false;
  bool get lifecycleReady => _lifecycleReady;

  // Pending cold-start clicks are retained until the facade subscribes.
  Stream<Map<String, dynamic>> get notificationClicks => Stream.multi((sink) {
    final subscription = _clicks.stream.listen(
      sink.add,
      onError: sink.addError,
      onDone: sink.close,
    );
    for (final click in _pendingClicks) {
      sink.add(click);
    }
    _pendingClicks.clear();
    sink.onCancel = subscription.cancel;
  }, isBroadcast: true);

  Future<void> initialize() {
    if (_disposed) throw StateError('Desktop integrations have been disposed');
    return _initialization ??= _initialize();
  }

  Future<void> _initialize() async {
    if (!Platform.isLinux && !Platform.isMacOS && !Platform.isWindows) {
      throw UnsupportedError(
        'Desktop integrations require Linux, macOS or Windows',
      );
    }
    _native.setMethodCallHandler((call) async {
      if (_disposed) return;
      switch (call.method) {
        case 'sessionEnding':
          _sessionEnding.add(null);
        case 'sessionEnded':
          _sessionEnded.add(null);
        case 'sessionEndingCancelled':
          _sessionEndingCancelled.add(null);
        case 'lifecycleError':
          _lifecycleReady = false;
          _sessionEnding.addError(
            PlatformException(
              code: 'lifecycle_failed',
              message: call.arguments as String?,
            ),
          );
        case 'activated':
          _activated.add(null);
        default:
          throw MissingPluginException('Unknown desktop event: ${call.method}');
      }
    });
    await _native.invokeMethod<void>('initializeLifecycle');
    _lifecycleReady = true;
    await _ensureNotifications();
  }

  Future<void> _ensureNotifications() {
    if (_disposed) throw StateError('Desktop integrations have been disposed');
    return _notificationInitialization ??= _initializeNotifications();
  }

  Future<void> _initializeNotifications() async {
    final initialized = await _notifications.initialize(
      settings: const InitializationSettings(
        macOS: DarwinInitializationSettings(
          requestAlertPermission: false,
          requestSoundPermission: false,
          requestBadgePermission: false,
        ),
        linux: LinuxInitializationSettings(defaultActionName: 'Open Paseo'),
        windows: WindowsInitializationSettings(
          appName: 'Paseo Sessions',
          appUserModelId: 'sh.paseo.sessions.desktop',
          guid: '72054dd0-fc41-4c29-8f0e-d407afcc7265',
        ),
      ),
      onDidReceiveNotificationResponse: _notificationResponse,
    );
    if (initialized == false) {
      throw StateError(
        'The operating system could not initialize notifications',
      );
    }
    // Linux delivers actions through its live D-Bus callback; its plugin does
    // not implement a cold-launch notification query.
    if (Platform.isMacOS || Platform.isWindows) {
      final launch = await _notifications.getNotificationAppLaunchDetails();
      final response = launch?.notificationResponse;
      if (launch?.didNotificationLaunchApp == true && response != null) {
        _notificationResponse(response);
      }
    }
  }

  void _notificationResponse(NotificationResponse response) {
    if (_disposed) return;
    try {
      final payload = response.payload;
      final data = payload == null || payload.isEmpty
          ? <String, dynamic>{}
          : Map<String, dynamic>.from(jsonDecode(payload) as Map);
      final event = <String, dynamic>{
        ...data,
        'notificationId': response.id,
        'actionId': response.actionId,
        if (response.input != null) 'input': response.input,
      };
      if (_clicks.hasListener) {
        _clicks.add(event);
      } else {
        _pendingClicks.add(event);
      }
    } catch (error, stack) {
      _clicks.addError(error, stack);
    }
  }

  Future<bool> requestNotificationPermission() async {
    await _ensureNotifications();
    if (Platform.isMacOS) {
      return await _notifications
              .resolvePlatformSpecificImplementation<
                MacOSFlutterLocalNotificationsPlugin
              >()!
              .requestPermissions(alert: true, badge: true, sound: true) ??
          false;
    }
    // Linux and Windows manage notification permission in system settings,
    // rather than exposing a runtime permission prompt.
    return true;
  }

  Future<void> showNotification({
    required int id,
    required String title,
    String? body,
    Map<String, dynamic> data = const {},
    bool sound = true,
    int? badgeCount,
  }) async {
    await _ensureNotifications();
    await _notifications.show(
      id: id,
      title: title,
      body: body,
      payload: jsonEncode(data),
      notificationDetails: NotificationDetails(
        macOS: DarwinNotificationDetails(
          presentAlert: true,
          presentBanner: true,
          presentList: true,
          presentSound: sound,
          presentBadge: badgeCount != null,
          badgeNumber: badgeCount,
        ),
        linux: LinuxNotificationDetails(
          defaultActionName: 'Open Paseo',
          suppressSound: !sound,
          sound: sound ? ThemeLinuxSound('message-new-instant') : null,
        ),
        windows: WindowsNotificationDetails(
          audio: sound
              ? WindowsNotificationAudio.preset(
                  sound: WindowsNotificationSound.defaultSound,
                )
              : WindowsNotificationAudio.silent(),
        ),
      ),
    );
    if (badgeCount != null) await setBadgeCount(badgeCount);
  }

  Future<void> setBadgeCount(int count) async {
    if (count < 0) {
      throw ArgumentError.value(count, 'count', 'Must be nonnegative');
    }
    await _native.invokeMethod<void>('setBadgeCount', {'count': count});
  }

  Future<int> idleMilliseconds() async {
    final value = await _native.invokeMethod<int>('idleMilliseconds');
    if (value == null) throw StateError('System idle time was not returned');
    return value;
  }

  Future<Map<String, dynamic>> runtimeInfo() async => {
    'platform': Platform.operatingSystem,
    'operatingSystemVersion': Platform.operatingSystemVersion,
    'dartVersion': Platform.version,
    'executable': Platform.resolvedExecutable,
    'pid': pid,
    'processors': Platform.numberOfProcessors,
    'locale': Platform.localeName,
    'residentMemoryBytes': ProcessInfo.currentRss,
    'peakResidentMemoryBytes': ProcessInfo.maxRss,
    'sessionType': Platform.environment['XDG_SESSION_TYPE'],
    'desktop': Platform.environment['XDG_CURRENT_DESKTOP'],
    'notificationInitializationStarted': _notificationInitialization != null,
    'lifecycleReady': _lifecycleReady,
  };

  static const _editors = <DesktopEditorTarget>[
    DesktopEditorTarget(
      'cursor',
      'Cursor',
      ['cursor'],
      macApplication: 'Cursor',
      macLauncher: 'Contents/Resources/app/bin/cursor',
      windowsDirectory: 'Cursor',
      windowsLauncher: 'resources/app/bin/cursor.cmd',
    ),
    DesktopEditorTarget('trae', 'Trae', ['trae']),
    DesktopEditorTarget('kiro', 'Kiro', ['kiro'], style: 'kiro'),
    DesktopEditorTarget(
      'vscode',
      'VS Code',
      ['code'],
      macApplication: 'Visual Studio Code',
      macLauncher: 'Contents/Resources/app/bin/code',
      windowsDirectory: 'Microsoft VS Code',
      windowsLauncher: 'bin/code.cmd',
    ),
    DesktopEditorTarget(
      'vscode-insiders',
      'VS Code Insiders',
      ['code-insiders'],
      macApplication: 'Visual Studio Code - Insiders',
      macLauncher: 'Contents/Resources/app/bin/code',
      windowsDirectory: 'Microsoft VS Code Insiders',
      windowsLauncher: 'bin/code-insiders.cmd',
    ),
    DesktopEditorTarget(
      'vscodium',
      'VSCodium',
      ['codium'],
      macApplication: 'VSCodium',
      macLauncher: 'Contents/Resources/app/bin/codium',
      windowsDirectory: 'VSCodium',
      windowsLauncher: 'bin/codium.cmd',
    ),
    DesktopEditorTarget(
      'zed',
      'Zed',
      ['zed', 'zeditor'],
      macApplication: 'Zed',
      style: 'zed',
    ),
    DesktopEditorTarget('antigravity', 'Antigravity', ['agy', 'antigravity']),
    DesktopEditorTarget(
      'android-studio',
      'Android Studio',
      ['studio', 'studio64'],
      macApplication: 'Android Studio',
      macLauncher: 'Contents/MacOS/studio',
      style: 'studio',
    ),
    DesktopEditorTarget('intellij-idea', 'IntelliJ IDEA', [
      'idea',
      'idea64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('aqua', 'Aqua', ['aqua', 'aqua64'], style: 'jetbrains'),
    DesktopEditorTarget('clion', 'CLion', [
      'clion',
      'clion64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('datagrip', 'DataGrip', [
      'datagrip',
      'datagrip64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('dataspell', 'DataSpell', [
      'dataspell',
      'dataspell64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('goland', 'GoLand', [
      'goland',
      'goland64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('phpstorm', 'PhpStorm', [
      'phpstorm',
      'phpstorm64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('pycharm', 'PyCharm', [
      'pycharm',
      'pycharm64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('rider', 'Rider', [
      'rider',
      'rider64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('rubymine', 'RubyMine', [
      'rubymine',
      'rubymine64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('rustrover', 'RustRover', [
      'rustrover',
      'rustrover64',
    ], style: 'jetbrains'),
    DesktopEditorTarget('webstorm', 'WebStorm', [
      'webstorm',
      'webstorm64',
    ], style: 'jetbrains'),
  ];

  List<String> _macApplications(DesktopEditorTarget target) => [
    '/Applications/${target.macApplication}.app',
    if (Platform.environment['HOME'] case final home?)
      '$home/Applications/${target.macApplication}.app',
    '/System/Applications/${target.macApplication}.app',
  ];

  Future<String?> _command(DesktopEditorTarget target) async {
    final candidates = [...target.commands];
    if (Platform.isMacOS && target.macLauncher != null) {
      candidates.addAll(
        _macApplications(target).map((app) => '$app/${target.macLauncher}'),
      );
    }
    if (Platform.isWindows && target.windowsDirectory != null) {
      final local = Platform.environment['LOCALAPPDATA'];
      final programs = Platform.environment['ProgramFiles'];
      if (local != null) {
        candidates.add(
          p.join(
            local,
            'Programs',
            target.id == 'cursor' ? 'cursor' : target.windowsDirectory!,
            target.windowsLauncher!,
          ),
        );
      }
      if (programs != null) {
        candidates.add(
          p.join(programs, target.windowsDirectory!, target.windowsLauncher!),
        );
      }
      if (target.id == 'cursor') {
        if (local != null) {
          candidates.add(p.join(local, 'Programs', 'cursor', 'Cursor.exe'));
        }
        if (programs != null) {
          candidates.add(p.join(programs, 'Cursor', 'Cursor.exe'));
        }
      }
    }
    final path =
        Platform.environment['PATH'] ?? Platform.environment['Path'] ?? '';
    final directories = path
        .split(Platform.isWindows ? ';' : ':')
        .where((value) => value.isNotEmpty);
    for (final candidate in candidates) {
      final paths = p.isAbsolute(candidate)
          ? [candidate]
          : directories.map((dir) => p.join(dir, candidate));
      for (final path in paths) {
        final extensions = Platform.isWindows && p.extension(path).isEmpty
            ? ['.exe', '.cmd', '.bat', '.com', '']
            : [''];
        for (final extension in extensions) {
          final executable = '$path$extension';
          final stat = await File(executable).stat();
          if (stat.type == FileSystemEntityType.file &&
              (Platform.isWindows || (stat.mode & 0x49) != 0)) {
            return executable;
          }
        }
      }
    }
    return null;
  }

  Future<bool> _hasMacApp(DesktopEditorTarget target) async {
    if (!Platform.isMacOS ||
        target.macApplication == null ||
        target.style == 'studio') {
      return false;
    }
    for (final app in _macApplications(target)) {
      if (await Directory(app).exists()) return true;
    }
    return false;
  }

  Future<List<DesktopEditorTarget>> listEditorTargets() async {
    final installed = <DesktopEditorTarget>[];
    for (final editor in _editors) {
      if (await _command(editor) != null || await _hasMacApp(editor)) {
        installed.add(editor);
      }
    }
    installed.add(
      DesktopEditorTarget(
        Platform.isMacOS
            ? 'finder'
            : Platform.isWindows
            ? 'explorer'
            : 'file-manager',
        Platform.isMacOS
            ? 'Finder'
            : Platform.isWindows
            ? 'Explorer'
            : 'File Manager',
        const [],
        kind: 'file-manager',
      ),
    );
    return installed;
  }

  Future<void> openEditor({
    required String editorId,
    required String workspacePath,
    String? filePath,
    int? line,
    int? column,
  }) async {
    for (final path in [workspacePath, ?filePath]) {
      if (!p.isAbsolute(path)) {
        throw ArgumentError('Editor paths must be absolute local paths: $path');
      }
      if (await FileSystemEntity.type(path) == FileSystemEntityType.notFound) {
        throw FileSystemException('Path does not exist', path);
      }
    }
    if (line != null && line < 1) throw ArgumentError.value(line, 'line');
    if (column != null && column < 1) {
      throw ArgumentError.value(column, 'column');
    }
    final manager = Platform.isMacOS
        ? 'finder'
        : Platform.isWindows
        ? 'explorer'
        : 'file-manager';
    if (editorId == manager) {
      if (Platform.isMacOS) {
        await _run('/usr/bin/open', [
          if (filePath != null) '-R',
          filePath ?? workspacePath,
        ]);
      } else if (Platform.isWindows) {
        await _launch('explorer.exe', [
          if (filePath != null) '/select,$filePath' else workspacePath,
        ]);
      } else if (filePath != null) {
        // The standardized FileManager1 API reveals the selected file, not just its parent.
        await _run('gdbus', [
          'call',
          '--session',
          '--dest',
          'org.freedesktop.FileManager1',
          '--object-path',
          '/org/freedesktop/FileManager1',
          '--method',
          'org.freedesktop.FileManager1.ShowItems',
          "['${Uri.file(filePath).toString().replaceAll("'", '%27')}']",
          '',
        ]);
      } else {
        await _run('xdg-open', [workspacePath]);
      }
      return;
    }
    final matches = _editors.where((editor) => editor.id == editorId);
    if (matches.isEmpty) {
      throw ArgumentError.value(editorId, 'editorId', 'Unknown editor target');
    }
    final editor = matches.single;
    final command = await _command(editor);
    if (command == null) {
      if (await _hasMacApp(editor)) {
        if (line != null || column != null) {
          throw StateError(
            '${editor.label} is installed but its command-line launcher is required for opening a position',
          );
        }
        await _run('/usr/bin/open', [
          '-a',
          editor.macApplication!,
          workspacePath,
          ?filePath,
        ]);
        return;
      }
      throw StateError('Editor target unavailable: ${editor.label}');
    }
    final location = filePath == null
        ? null
        : '$filePath${line == null ? '' : ':$line${column == null ? '' : ':$column'}'}';
    final args = <String>[];
    if (editor.style == 'kiro') args.add('ide');
    if (editor.style == 'jetbrains') {
      if (filePath != null && line != null) args.addAll(['--line', '$line']);
      if (filePath != null && column != null) {
        args.addAll(['--column', '$column']);
      }
    }
    args.add(workspacePath);
    if (filePath != null) {
      if (editor.style == 'studio') {
        if (line != null) args.addAll(['--line', '$line']);
        if (column != null) args.addAll(['--column', '$column']);
        args.add(filePath);
      } else if (editor.style == 'jetbrains') {
        args.add(filePath);
      } else if (editor.style == 'zed') {
        args.add(location!);
      } else {
        if (line != null) args.add('--goto');
        args.add(location!);
      }
    }
    await _launch(command, args);
  }

  Map<String, String> _externalEnvironment() => Map.of(Platform.environment)
    ..remove('PASEO_NODE_ENV')
    ..remove('PASEO_DESKTOP_MANAGED')
    ..remove('PASEO_SUPERVISED')
    ..remove('ELECTRON_RUN_AS_NODE')
    ..remove('ELECTRON_NO_ATTACH_CONSOLE');

  Future<void> _run(String command, List<String> args) async {
    final result = await Process.run(
      command,
      args,
      environment: _externalEnvironment(),
      includeParentEnvironment: false,
    );
    if (result.exitCode != 0) {
      throw ProcessException(
        command,
        args,
        '${result.stderr}'.trim(),
        result.exitCode,
      );
    }
  }

  Future<void> _launch(String command, List<String> args) async {
    final environment = _externalEnvironment();
    if (Platform.isWindows &&
        ['.cmd', '.bat'].contains(p.extension(command).toLowerCase())) {
      // Variables are expanded once by cmd. No path is interpolated into shell
      // syntax, and delayed expansion is disabled (literal ! and % are preserved).
      final values = [command, ...args];
      for (var i = 0; i < values.length; i++) {
        if (values[i].contains('"') ||
            values[i].contains('\n') ||
            values[i].contains('\r')) {
          throw ArgumentError(
            'Windows command arguments must not contain quotes or newlines',
          );
        }
        environment['PASEO_EDITOR_ARG_$i'] = values[i];
      }
      await _native.invokeMethod<void>('launchCommandScript', {
        'environment': environment,
        'argumentCount': values.length,
      });
    } else {
      await Process.start(
        command,
        args,
        environment: environment,
        includeParentEnvironment: false,
        mode: ProcessStartMode.detached,
      );
    }
  }

  Future<void> dispose() async {
    _disposed = true;
    _lifecycleReady = false;
    _pendingClicks.clear();
    try {
      if (_initialization != null) {
        await _native.invokeMethod<void>('disposeLifecycle');
      }
    } finally {
      _native.setMethodCallHandler(null);
      await _sessionEnding.close();
      await _sessionEnded.close();
      await _sessionEndingCancelled.close();
      await _activated.close();
      await _clicks.close();
    }
  }
}

class DesktopDropRegion extends StatefulWidget {
  const DesktopDropRegion({
    super.key,
    required this.child,
    required this.onFiles,
    required this.onError,
    this.enabled = true,
  });
  final Widget child;
  final Future<void> Function(List<XFile>) onFiles;
  final void Function(Object, StackTrace) onError;
  final bool enabled;
  @override
  State<DesktopDropRegion> createState() => _DesktopDropRegionState();
}

class _DesktopDropRegionState extends State<DesktopDropRegion> {
  bool _hovered = false;
  Future<void> _accept(DropDoneDetails details) async {
    final scoped = <DropItem>[];
    try {
      for (final file in details.files) {
        final bookmark = file.extraAppleBookmark;
        if (Platform.isMacOS && bookmark != null && bookmark.isNotEmpty) {
          if (!await DesktopDrop.instance.startAccessingSecurityScopedResource(
            bookmark: bookmark,
          )) {
            throw FileSystemException(
              'macOS denied access to dropped file',
              file.path,
            );
          }
          scoped.add(file);
        }
      }
      // The receiver must finish reading/copying before returning; security
      // scopes are released even when upload fails or the widget unmounts.
      await widget.onFiles(details.files);
    } catch (error, stack) {
      widget.onError(error, stack);
    } finally {
      for (final file in scoped.reversed) {
        try {
          final stopped = await DesktopDrop.instance
              .stopAccessingSecurityScopedResource(
                bookmark: file.extraAppleBookmark!,
              );
          if (!stopped) throw StateError('Could not release macOS drop access');
        } catch (error, stack) {
          widget.onError(error, stack);
        }
      }
      if (mounted) setState(() => _hovered = false);
    }
  }

  @override
  Widget build(BuildContext context) => DropTarget(
    enable: widget.enabled,
    onDragEntered: (_) => setState(() => _hovered = true),
    onDragExited: (_) => setState(() => _hovered = false),
    onDragDone: _accept,
    child: DecoratedBox(
      position: DecorationPosition.foreground,
      decoration: BoxDecoration(
        border: _hovered
            ? Border.all(color: Theme.of(context).colorScheme.primary, width: 2)
            : null,
      ),
      child: widget.child,
    ),
  );
}
