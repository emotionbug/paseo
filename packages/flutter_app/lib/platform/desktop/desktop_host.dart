import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:ui';

import 'package:app_links/app_links.dart';
import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:nativeapi/nativeapi.dart' as native show Size;
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';
import 'package:screen_retriever/screen_retriever.dart';
import 'package:tray_manager/tray_manager.dart' as tray;
import 'package:window_manager/window_manager.dart';

import '../../core/app_store.dart';
import '../../core/json.dart';
import '../../core/legacy_state.dart' show originalDesktopUserDataDir;
import 'daemon_service.dart';
import 'desktop_integrations.dart';
import 'desktop_settings.dart';
import 'desktop_updater.dart';

/// Native desktop process coordinator. Only the first engine owns daemon/tray.
/// Its initial engine is a hidden coordinator, never a user-facing window.
class DesktopHost with WindowListener, WidgetsBindingObserver {
  DesktopHost._(this.store, this.controller, this.initialArguments);

  static DesktopHost? instance;
  static bool get supported =>
      Platform.isLinux || Platform.isMacOS || Platform.isWindows;
  static const _nativeEvents = MethodChannel('paseo/desktop_host_events');
  final AppStore store;
  final WindowController controller;
  final JsonMap initialArguments;
  final integrations = DesktopIntegrations.instance;
  final updater = DesktopUpdater();
  DesktopDaemonService? _daemon;
  final _settings = DesktopSettingsStore();
  final _errors = StreamController<Object>.broadcast();
  final _pendingErrors = <Object>[];
  final _startupErrors = <String>[];
  final _subscriptions = <StreamSubscription<dynamic>>[];
  final _pendingRoutes = <JsonMap>[];
  final _readyWindows = <String>{};
  final _notificationKeys = <String>{};
  Future<void> _notificationOperations = Future.value();
  tray.TrayIcon? _trayIcon;
  tray.Image? _trayImage;
  tray.Menu? _trayMenu;
  tray.ListenerId? _trayListener;
  final _trayItems = <(tray.MenuItem, tray.ListenerId)>[];
  late Directory _support;
  File? _logFile;
  File? _geometryFile;
  Timer? _geometryTimer;
  Future<void> _operations = Future.value();
  Future<void> _logs = Future.value();
  bool _logicalOpen = true;
  bool _quitting = false;
  bool _quitPrepared = false;
  bool _disposed = false;
  bool _trayCreated = false;
  bool _lifecycleReady = false;
  bool _routing = false;
  bool _initialized = false;
  bool _createdUserWindow = false;
  bool _localDaemonListening = false;
  String? _lastWindow;
  Object? _daemonFailure;

  String get windowId => controller.windowId;
  String get primaryWindowId =>
      initialArguments.text('primaryWindowId', windowId);
  bool get isPrimary => primaryWindowId == windowId;
  bool get isCoordinator => isPrimary;
  Stream<Object> get errors => Stream.multi((sink) {
    final subscription = _errors.stream.listen(sink.add, onDone: sink.close);
    for (final error in _pendingErrors) {
      sink.add(error);
    }
    _pendingErrors.clear();
    sink.onCancel = subscription.cancel;
  }, isBroadcast: true);
  DesktopDaemonService get daemon =>
      _daemon ??
      (throw StateError(
        'Daemon service belongs to the primary engine; use invoke(). ${_daemonFailure ?? ''}',
      ));

  static Future<JsonMap> windowArguments() async {
    final current = await WindowController.fromCurrentEngine();
    final args = current.arguments.isEmpty
        ? <String, dynamic>{}
        : jsonMap(jsonDecode(current.arguments));
    return {
      ...args,
      'windowId':
          args['stateWindowId'] ??
          (args.containsKey('primaryWindowId')
              ? current.windowId
              : 'coordinator'),
      'nativeWindowId': current.windowId,
      'isCoordinator':
          args.text('primaryWindowId', current.windowId) == current.windowId,
    };
  }

  static Future<DesktopHost> initialize(
    AppStore store, {
    List<String> arguments = const [],
  }) async {
    if (!supported) {
      throw UnsupportedError('DesktopHost requires a desktop platform.');
    }
    if (instance != null) return instance!;
    final controller = await WindowController.fromCurrentEngine();
    final args = controller.arguments.isEmpty
        ? <String, dynamic>{}
        : jsonMap(jsonDecode(controller.arguments));
    final host = DesktopHost._(store, controller, args);
    instance = host;
    await host._initialize(arguments);
    return host;
  }

  Future<void> _initialize(List<String> arguments) async {
    _support = await getApplicationSupportDirectory();
    await _support.create(recursive: true);
    _logFile = File(p.join(_support.path, 'desktop-$windowId.log'));
    final geometryId = initialArguments.text('stateWindowId', windowId);
    _geometryFile = File(p.join(_support.path, 'window-$geometryId.json'));
    await windowManager.ensureInitialized();
    if (isPrimary) {
      _logicalOpen = false;
      await windowManager.hide();
    }
    await controller.setWindowMethodHandler(_windowCall);
    windowManager.addListener(this);
    WidgetsBinding.instance.addObserver(this);
    await windowManager.setPreventClose(true);
    if (!isPrimary) await _restoreGeometry();
    try {
      await integrations.initialize();
      _lifecycleReady = true;
    } catch (error) {
      // Missing desktop notification/session services must not prevent access
      // to existing sessions. With no lifecycle bridge, disable close-to-tray.
      // A desktop without a session manager is an environment capability,
      // not a failure the user can act on: record it without a banner.
      if (error is PlatformException && error.code == 'logout_unavailable') {
        _log(error);
      } else {
        _report(error);
      }
    }
    _subscriptions.add(
      integrations.sessionEnding.listen(
        (_) {
          // A cancellable OS query is intent, not consent to stop ongoing sessions.
          _quitting = true;
        },
        onError: (Object error) {
          _lifecycleReady = false;
          _report(error);
          if (isPrimary) _run(_showPreferredWindow);
        },
      ),
    );
    _subscriptions.add(
      integrations.sessionEndingCancelled.listen((_) {
        if (!_quitPrepared) _quitting = false;
      }),
    );
    _subscriptions.add(
      integrations.sessionEnded.listen((_) {
        _quitting = true;
        if (isPrimary) _run(() => quit(reason: 'shutdown', force: true));
      }),
    );
    _subscriptions.add(
      integrations.notificationClicks.listen((data) {
        _run(() async {
          final id = data['windowId'];
          if (id is String && id != windowId) {
            final window = await _findWindow(id);
            if (window != null) {
              await window.invokeMethod('route', data);
              return;
            }
          }
          await show();
          _queueRoute(data);
        });
      }),
    );
    if (isPrimary) {
      _nativeEvents.setMethodCallHandler((call) async {
        switch (call.method) {
          case 'quit':
            await quit();
            return _quitPrepared;
          case 'activate':
            await _activate(jsonMap(call.arguments));
            return true;
          default:
            throw MissingPluginException('Unknown host event ${call.method}');
        }
      });
      final activations = await _nativeEvents.invokeListMethod<dynamic>(
        'ready',
      );
      for (final activation in activations ?? const []) {
        await _activate(jsonMap(activation));
      }
      await _settings.initialize();
      await _initializeTray();
      _subscriptions.add(
        integrations.activated.listen((_) {
          if (!_quitting) _run(_showPreferredWindow);
        }),
      );
      final appLinks = AppLinks();
      final initialLink = await appLinks.getInitialLink();
      if (initialLink != null) _queueUri(initialLink);
      _subscriptions.add(
        appLinks.uriLinkStream.listen(_queueUri, onError: _report),
      );
      try {
        _daemon = await _createDaemon();
        await daemon.initialize();
        final status = await daemon.status();
        // Waiting for a still-starting daemon must not hold back the window.
        _run(() => _registerLocalHost(status));
      } catch (error) {
        _daemonFailure = error;
        _report(error);
      }
    }
    store.addListener(_drainRoutes);
    if (initialArguments.containsKey('projectPath') ||
        initialArguments.containsKey('agentId')) {
      _queueRoute(initialArguments);
    }
    for (final route in jsonList(initialArguments['routes'])) {
      _queueRoute(jsonMap(route));
    }
    // Original main.ts: an agent deep link in argv wins (parseAgentDeepLinkFromArgv,
    // first valid link only); otherwise open-project-routing.ts picks one path:
    // the first positional existing absolute directory, else the --open-project value.
    final launchUri = arguments
        .map(Uri.tryParse)
        .whereType<Uri>()
        .where((uri) => parseAgentDeepLink(uri) != null)
        .firstOrNull;
    if (launchUri != null) {
      _queueUri(launchUri);
    } else {
      final projectPath = await openProjectPathFromArgv(arguments);
      if (projectPath != null) _queueRoute({'projectPath': projectPath});
    }
    _initialized = true;
    await show();
    if (!isPrimary) {
      await WindowController.fromWindowId(primaryWindowId)
          .invokeMethod('readyWindow', {'windowId': windowId});
    }
    _drainRoutes();
  }

  Future<DesktopDaemonService> _createDaemon() async {
    final override = Platform.environment['PASEO_DESKTOP_RUNTIME'];
    final directory = p.dirname(Platform.resolvedExecutable);
    final manifest = File(
      override ??
          (Platform.isMacOS
              ? p.normalize(
                  p.join(
                    directory,
                    '..',
                    'Resources',
                    'runtime',
                    'desktop-runtime.json',
                  ),
                )
              : p.join(directory, 'runtime', 'desktop-runtime.json')),
    );
    if (!await manifest.exists()) {
      throw FileSystemException(
        'Bundled native runtime manifest is missing. Package the native Paseo runtime.',
        manifest.path,
      );
    }
    final data = jsonMap(jsonDecode(await manifest.readAsString()));

    return DesktopDaemonService(
      runtime: DesktopDaemonRuntime.fromJson(
        data,
        directory: manifest.parent.path,
      ),
      settings: () => DesktopDaemonSettings(
        managed: _settings.managed,
        keepRunningAfterQuit: _settings.keepRunning,
      ),
      appVersion: const String.fromEnvironment('PASEO_VERSION'),
    );
  }

  Future<void> _registerLocalHost(JsonMap status) async {
    // Original `upsertDesktopDaemonConnection`: a starting daemon is added as
    // soon as it reports its listen address; the client reconnects until it
    // is up. Only the address can be missing early, so wait for that.
    var current = status;
    final deadline = DateTime.now().add(const Duration(seconds: 60));
    while (current['status'] == 'starting' &&
        stringValue(current['listen']).isEmpty &&
        !_quitting &&
        !_disposed &&
        DateTime.now().isBefore(deadline)) {
      await Future<void>.delayed(const Duration(seconds: 1));
      current = await daemon.status();
    }
    if (!{'running', 'starting'}.contains(current['status']) ||
        stringValue(current['listen']).isEmpty) {
      return;
    }
    _localDaemonListening = true;
    // The coordinator owns processes, not daemon clients or UI observations.
    // Windows that become ready later are refreshed in `readyWindow`.
    for (final id in List<String>.of(_readyWindows)) {
      await WindowController.fromWindowId(id).invokeMethod('refreshLocalHost');
    }
  }

  Future<dynamic> _windowCall(MethodCall call) async {
    final args = jsonMap(call.arguments);
    switch (call.method) {
      case 'state':
        return {
          'logicalOpen': _logicalOpen,
          'quitting': _quitting,
          'serverId': store.selected?.serverId,
          'agentId': store.selected?.kind == 'agent'
              ? store.selected?.id
              : null,
        };
      case 'show':
        await show();
        return true;
      case 'route':
        await show();
        _queueRoute(args);
        return true;
      case 'refreshLocalHost':
        await store.discoverLocalDaemon();
        await store.refreshHosts();
        return true;
      case 'canClose':
        // Dirty-document prompts must not be trapped in a retained tray window.
        await show();
        return store.canCloseWindow();
      case 'finishClose':
        await _finishWindowClose();
        return true;
      case 'beginQuit':
        _quitting = true;
        return true;
      case 'cancelQuit':
        _quitting = false;
        return true;
      case 'focused':
        _lastWindow = args.text('windowId');
        return true;
      case 'readyWindow':
        _readyWindows.add(args.text('windowId'));
        _lastWindow = args.text('windowId');
        for (final message in _startupErrors) {
          await WindowController.fromWindowId(_lastWindow!)
              .invokeMethod('hostError', {'message': message});
        }
        _startupErrors.clear();
        // Its own startup discovery may have run before the daemon published
        // its listen address.
        if (_localDaemonListening) {
          await WindowController.fromWindowId(_lastWindow!)
              .invokeMethod('refreshLocalHost');
        }
        _drainRoutes();
        return true;
      case 'hostError':
        _report(StateError(args.text('message')));
        return true;
      case 'disposeForQuit':
        await _disposeWindowResources();
        return true;
      case 'closeRequest':
        if (!isPrimary) {
          throw StateError('Close coordination requires primary window.');
        }
        return _serialize(() => _closeWindow(args.text('windowId')));
      case 'quitRequest':
        await quit(
          reason: args.text('reason', 'quit'),
          force: args.flag('force'),
        );
        return true;
      case 'notify':
        if (!isCoordinator) {
          throw StateError('Notifications require the coordinator.');
        }
        await _notifyPrimary(
          id: args.integer('id'),
          title: args.text('title'),
          body: args['body'] as String?,
          data: jsonMap(args['data']),
          eventKey: args['eventKey'] as String?,
          originWindowId: args.text('windowId'),
        );
        return true;
      case 'invoke':
        if (!isPrimary) {
          throw StateError('Desktop command requires primary window.');
        }
        return invoke(args.text('command'), jsonMap(args['args']));
      default:
        throw MissingPluginException(
          'Unknown desktop window operation ${call.method}',
        );
    }
  }

  Future<T> _serialize<T>(Future<T> Function() action) {
    final future = _operations.catchError((Object _) {}).then((_) => action());
    _operations = future.then<void>(
      (_) {},
      onError: (Object _, StackTrace _) {},
    );
    return future;
  }

  Future<WindowController?> _findWindow(String id) async {
    for (final window in await WindowController.getAll()) {
      if (window.windowId == id) return window;
    }
    return null;
  }

  Future<List<WindowController>> _openWindows() async {
    final windows = <WindowController>[];
    for (final window in await WindowController.getAll()) {
      if (window.windowId == windowId) {
        if (_logicalOpen) windows.add(window);
      } else {
        try {
          final state = jsonMap(await window.invokeMethod('state'));
          if (state['logicalOpen'] != false) windows.add(window);
        } on WindowChannelException catch (error) {
          if (error.code != 'CHANNEL_UNREGISTERED' &&
              error.code != 'NO_HANDLER') {
            rethrow;
          }
          // A newly created engine has not installed its handler yet; it is open.
          windows.add(window);
        }
      }
    }
    return windows;
  }

  Future<void> close() async {
    if (_quitting) return;
    if (isPrimary) {
      await quit();
    } else {
      await WindowController.fromWindowId(primaryWindowId)
          .invokeMethod('closeRequest', {'windowId': windowId});
    }
  }

  Future<void> _closeWindow(String id) async {
    if (_quitting) return;
    final window = await _findWindow(id);
    if (window == null) return;
    var open = await _openWindows();
    final hasTray = open.length <= 1 && await _usableTray();
    if (_quitting) return;
    open = await _openWindows();
    if (open.length <= 1 && hasTray) {
      // Hide before dirty-tab guards: this is not closing the document/session.
      _lastWindow = id;
      await window.hide();
      return;
    }
    if (open.length <= 1) {
      await _quitPrimary(reason: 'quit', force: false);
      return;
    }
    final allowed = await window.invokeMethod<bool>('canClose') == true;
    if (!allowed) return;
    _readyWindows.remove(id);
    await window.invokeMethod('finishClose');
  }

  Future<void> _finishWindowClose() async {
    _logicalOpen = false;
    await _disposeWindowResources();
    await windowManager.setPreventClose(false);
    // destroy() terminates the entire process on Windows/macOS; never use for a child.
    // Reply to the caller before destroying the engine carrying this channel.
    Timer.run(() => unawaited(windowManager.close()));
  }

  Future<void> show() async {
    if (_quitting) return;
    if (isPrimary) {
      await _showPreferredWindow();
      return;
    }
    _logicalOpen = true;
    if (await windowManager.isMinimized()) await windowManager.restore();
    await controller.show();
    await windowManager.focus();
  }

  Future<void> _showPreferredWindow() async {
    if (_quitting || !_initialized) return;
    final windows = (await WindowController.getAll())
        .where((window) => window.windowId != windowId)
        .toList();
    WindowController? preferred;
    for (final window in windows) {
      if (_readyWindows.contains(window.windowId)) {
        preferred ??= window;
        if (window.windowId == _lastWindow) {
          preferred = window;
          break;
        }
      }
    }
    if (preferred != null) {
      await preferred.invokeMethod('show');
    } else if (windows.isEmpty) {
      await openNew();
    }
  }

  Future<WindowController> openNew({
    String? pendingOpenProjectPath,
    String? serverId,
    String? agentId,
  }) async {
    if (_quitting) throw StateError('Application is quitting.');
    final first = isPrimary && !_createdUserWindow;
    final routes = first ? List<JsonMap>.of(_pendingRoutes) : const <JsonMap>[];
    final window = await WindowController.create(
      WindowConfiguration(
        hiddenAtLaunch: true,
        arguments: jsonEncode({
          'primaryWindowId': primaryWindowId,
          if (first) 'stateWindowId': 'main',
          if (routes.isNotEmpty) 'routes': routes,
          'projectPath': ?pendingOpenProjectPath,
          'serverId': ?serverId,
          'agentId': ?agentId,
        }),
      ),
    );
    if (first) {
      _createdUserWindow = true;
      _pendingRoutes.removeRange(0, routes.length);
    }
    return window;
  }

  Future<void> quit({String reason = 'quit', bool force = false}) async {
    if (!isPrimary) {
      await WindowController.fromWindowId(primaryWindowId)
          .invokeMethod('quitRequest', {'reason': reason, 'force': force});
      return;
    }
    await _serialize(() => _quitPrimary(reason: reason, force: force));
  }

  Future<void> _prepareQuit({
    required String reason,
    required bool force,
  }) async {
    if (_quitPrepared) return;
    final windows = await WindowController.getAll();
    if (!force) {
      for (final window in windows) {
        if (!_readyWindows.contains(window.windowId)) continue;
        final allowed = await window.invokeMethod<bool>('canClose') == true;
        if (!allowed) throw const DesktopQuitCancelled();
      }
    }
    _quitting = true;
    for (final window in windows) {
      if (_readyWindows.contains(window.windowId)) {
        await window.invokeMethod('beginQuit');
      }
    }
    if (_daemon != null) {
      // Keep-running is independent of management enablement, including quit.
      // Update is the existing exception: stop the owned old-version daemon.
      // Original quit-lifecycle.ts: a failed daemon stop is logged (onStopError)
      // and the quit still completes; it never keeps the app open.
      try {
        await daemon.close(
          keepRunning: reason == 'app_update' ? false : _settings.keepRunning,
        );
      } catch (error, stack) {
        _log(error, stack);
      }
    }
    _quitPrepared = true;
  }

  Future<void> _cancelQuit() async {
    _quitting = false;
    _quitPrepared = false;
    for (final window in await WindowController.getAll()) {
      if (_readyWindows.contains(window.windowId)) {
        await window.invokeMethod('cancelQuit');
      }
    }
  }

  Future<JsonMap> _installUpdate(JsonMap args) async {
    try {
      final result = await updater.install(
        releaseChannel:
            args['releaseChannel'] as String? ?? _settings.releaseChannel,
        beforeQuit: () => _prepareQuit(reason: 'app_update', force: false),
      );
      if (result['installerLaunched'] == true || result['installed'] == true) {
        Timer.run(
          () => _run(() => _quitPrimary(reason: 'app_update', force: true)),
        );
      } else if (_quitPrepared) {
        await _recoverFailedUpdate();
      }
      return result;
    } catch (_) {
      if (_quitPrepared) await _recoverFailedUpdate();
      rethrow;
    }
  }

  Future<void> _recoverFailedUpdate() async {
    await _cancelQuit();
    _daemon = await _createDaemon();
    await daemon.initialize();
    await _registerLocalHost(await daemon.status());
  }

  Future<void> _activate(JsonMap payload) async {
    if (payload['url'] is String) {
      final uri = Uri.tryParse(payload['url'] as String);
      if (uri != null) _queueUri(uri);
    } else if (payload['projectPath'] is String) {
      if (payload['initial'] == true) {
        _queueRoute({'projectPath': payload['projectPath']});
      } else {
        await openNew(pendingOpenProjectPath: payload['projectPath'] as String);
      }
    } else {
      await _showPreferredWindow();
    }
  }

  Future<void> _quitPrimary({
    required String reason,
    required bool force,
  }) async {
    try {
      await _prepareQuit(reason: reason, force: force);
    } on DesktopQuitCancelled {
      return;
    }
    for (final window in await WindowController.getAll()) {
      if (_readyWindows.contains(window.windowId)) {
        await window.invokeMethod('disposeForQuit');
        await window.invokeMethod('finishClose');
      }
    }
    await _disposeWindowResources();
    _destroyTray();
    await _nativeEvents.invokeMethod<void>('allowTermination');
    await windowManager.setPreventClose(false);
    await windowManager.destroy();
  }

  Future<dynamic> invoke(String command, [JsonMap args = const {}]) async {
    if (!isPrimary) {
      return WindowController.fromWindowId(primaryWindowId)
          .invokeMethod('invoke', {'command': command, 'args': args});
    }
    switch (command) {
      case 'desktop_settings_get':
        return _settings.value;
      case 'desktop_settings_update':
        final wasManaged = _settings.managed;
        final result = await _settings.patch(jsonMap(args['patch']));
        if (wasManaged && !_settings.managed && _daemon != null) {
          await daemon.stop(reason: 'settings');
        }
        if (!wasManaged && _settings.managed) {
          _daemon ??= await _createDaemon();
          await _registerLocalHost(await daemon.start());
        }
        return result;
      case 'desktop_get_runtime_info':
        return {
          ...await integrations.runtimeInfo(),
          'appVersion': const String.fromEnvironment('PASEO_VERSION'),
          'windowId': windowId,
          'primaryWindowId': primaryWindowId,
        };
      case 'desktop_app_logs':
        await _logs;
        return {'logPath': _logFile!.path, 'contents': await _tail(_logFile!)};
      case 'desktop_update_diagnostics':
        return updater.diagnostics();
      case 'check_app_update':
        return updater.check(
          releaseChannel:
              args['releaseChannel'] as String? ?? _settings.releaseChannel,
          manual: args['intent'] == 'manual',
        );
      case 'install_app_update':
        return _serialize(() => _installUpdate(args));
      default:
        _daemon ??= await _createDaemon();
        final result = await daemon.invoke(command, args);
        if (command == 'start_desktop_daemon' ||
            command == 'restart_desktop_daemon') {
          await _registerLocalHost(jsonMap(result));
        }
        return result;
    }
  }

  Future<JsonMap> updateSettings(JsonMap patch) async =>
      jsonMap(await invoke('desktop_settings_update', {'patch': patch}));

  Future<void> notify({
    required int id,
    required String title,
    String? body,
    JsonMap data = const {},
    String? eventKey,
  }) async {
    if (!isCoordinator) {
      await WindowController.fromWindowId(primaryWindowId)
          .invokeMethod('notify', {
            'id': id,
            'title': title,
            'body': body,
            'data': data,
            'windowId': windowId,
            'eventKey': ?eventKey,
          });
      return;
    }
    await _notifyPrimary(
      id: id,
      title: title,
      body: body,
      data: data,
      eventKey: eventKey,
      originWindowId: windowId,
    );
  }

  Future<void> _notifyPrimary({
    required int id,
    required String title,
    required String? body,
    required JsonMap data,
    required String? eventKey,
    required String originWindowId,
  }) {
    // Serialize the check and OS delivery so simultaneous engines cannot both
    // emit an event. Remember only successful deliveries, bounded to 512 keys.
    final operation = _notificationOperations.then((_) async {
      if (eventKey != null && _notificationKeys.contains(eventKey)) return;
      if (_disposed || _quitting) {
        throw StateError('The application is shutting down.');
      }
      await integrations.showNotification(
        id: id,
        title: title,
        body: body,
        sound: _settings.playSound,
        data: {...data, 'windowId': originWindowId},
      );
      if (eventKey != null) {
        _notificationKeys.add(eventKey);
        if (_notificationKeys.length > 512) {
          _notificationKeys.remove(_notificationKeys.first);
        }
      }
    });
    _notificationOperations = operation.catchError((Object _) {});
    return operation;
  }

  Future<void> _initializeTray() async {
    try {
      if (!_lifecycleReady || !tray.TrayManager.instance.isSupported()) return;
      final iconPath = Platform.isMacOS
          ? 'assets/desktop/trayTemplate.png'
          : 'assets/desktop/tray.png';
      final image = tray.ImageAsset.fromAsset(iconPath);
      if (image == null) throw StateError('Unable to load tray icon $iconPath');
      _trayImage = image;
      final icon = tray.TrayIcon.createWithIdentifier(
        'sh.paseo.sessions.desktop',
      );
      if (icon == null) {
        throw StateError('Unable to create the system tray icon.');
      }
      _trayIcon = icon;
      final size = Platform.isMacOS ? 18.0 : 32.0;
      icon
        ..icon = image
        ..isIconTemplate = Platform.isMacOS
        ..iconSize = native.Size(width: size, height: size)
        ..setTooltip('Paseo');
      final menu = tray.Menu.create();
      if (menu == null) throw StateError('Unable to create the tray menu.');
      _trayMenu = menu;
      _addTrayItem(menu, 'Open Paseo', _showPreferredWindow);
      menu.addSeparator();
      _addTrayItem(menu, 'Quit Paseo', () => quit());
      icon
        ..setContextMenu(menu)
        ..setContextMenuTrigger(tray.ContextMenuTrigger.rightClicked);
      _trayListener = icon.addListener((event) {
        if (event is tray.TrayIconClickedEvent ||
            event is tray.TrayIconDoubleClickedEvent) {
          Timer.run(() {
            if (!_quitting && !_disposed) _run(_showPreferredWindow);
          });
        }
      });
      if (!icon.setVisible(true)) {
        throw StateError('The system could not display the tray icon.');
      }
      _trayCreated = true;
    } catch (error) {
      _destroyTray();
      _report(error);
    }
  }

  void _addTrayItem(
    tray.Menu menu,
    String label,
    Future<void> Function() action,
  ) {
    final item = tray.MenuItem.createWithLabelAndType(
      label,
      tray.MenuItemType.normal,
    );
    if (item == null) {
      throw StateError('Unable to create tray menu item $label');
    }
    late final tray.ListenerId listener;
    try {
      listener = item.addListener((event) {
        if (event is! tray.MenuItemClickedEvent) return;
        // Let native menu dispatch finish before Quit can release its handles.
        Timer.run(() {
          if (!_quitting && !_disposed) _run(action);
        });
      });
    } catch (_) {
      item.dispose();
      rethrow;
    }
    _trayItems.add((item, listener));
    menu.addItem(item);
  }

  void _destroyTray() {
    _trayCreated = false;
    final icon = _trayIcon;
    final listener = _trayListener;
    if (icon != null) {
      if (listener != null) icon.removeListener(listener);
      icon.setVisible(false);
      icon.setContextMenu(null);
    }
    _trayListener = null;
    for (final (item, itemListener) in _trayItems) {
      item.removeListener(itemListener);
      item.dispose();
    }
    _trayItems.clear();
    _trayMenu?.dispose();
    _trayMenu = null;
    icon?.dispose();
    _trayIcon = null;
    _trayImage?.dispose();
    _trayImage = null;
  }

  Future<bool> _usableTray() async {
    if (!_trayCreated ||
        !_lifecycleReady ||
        _quitting ||
        _trayIcon?.isVisible() != true) {
      return false;
    }
    if (!Platform.isLinux) return true;
    // A watcher without a registered panel host is not a usable tray.
    try {
      final result = await Process.run('dbus-send', [
        '--session',
        '--print-reply',
        '--reply-timeout=1000',
        '--dest=org.kde.StatusNotifierWatcher',
        '/StatusNotifierWatcher',
        'org.freedesktop.DBus.Properties.Get',
        'string:org.kde.StatusNotifierWatcher',
        'string:IsStatusNotifierHostRegistered',
      ]).timeout(const Duration(milliseconds: 1500));
      return result.exitCode == 0 &&
          RegExp(r'\bvariant\s+boolean true\s*$')
              .hasMatch(result.stdout.toString());
    } catch (_) {
      return false;
    }
  }

  void _queueUri(Uri uri) {
    final target = parseAgentDeepLink(uri);
    if (target == null) return;
    final (:serverId, :agentId) = target;
    _run(() async {
      for (final window in await WindowController.getAll()) {
        if (window.windowId == windowId ||
            !_readyWindows.contains(window.windowId)) {
          continue;
        }
        final state = jsonMap(await window.invokeMethod('state'));
        if (state['serverId'] == serverId && state['agentId'] == agentId) {
          await window.invokeMethod('show');
          return;
        }
      }
      _queueRoute({'serverId': serverId, 'agentId': agentId});
      await show();
    });
  }

  void _queueRoute(JsonMap route) {
    _pendingRoutes.add(route);
    _drainRoutes();
  }

  void _drainRoutes() {
    if (_routing ||
        !_initialized ||
        _disposed ||
        _pendingRoutes.isEmpty ||
        (!isPrimary && store.hosts.isEmpty)) {
      return;
    }
    _routing = true;
    _run(() async {
      try {
        while (_pendingRoutes.isNotEmpty) {
          final route = _pendingRoutes.first;
          if (isPrimary) {
            final id = _readyWindows.contains(_lastWindow)
                ? _lastWindow
                : (_readyWindows.isEmpty ? null : _readyWindows.first);
            if (id == null) return;
            await WindowController.fromWindowId(id)
                .invokeMethod('route', route);
            _pendingRoutes.removeAt(0);
            continue;
          }
          final serverId = route['serverId'] as String?;
          if (serverId != null && store.hostFor(serverId) == null) return;
          if (route['terminalId'] is String &&
              route['workspaceId'] is String &&
              serverId != null) {
            await store.openExecutionLog(
              serverId,
              route['workspaceId'] as String,
              route['terminalId'] as String,
            );
          } else if (route['agentId'] is String && serverId != null) {
            await store.openAgent(serverId, route['agentId'] as String);
          } else if (route['projectPath'] is String) {
            await store.openProjectPath(
              route['projectPath'] as String,
              serverId: serverId,
            );
          }
          _pendingRoutes.removeAt(0);
        }
      } finally {
        _routing = false;
      }
    });
  }

  Future<void> _restoreGeometry() async {
    final file = _geometryFile!;
    try {
      final data = await file.exists()
          ? jsonMap(jsonDecode(await file.readAsString()))
          : await originalWindowState(Platform.environment);
      if (data == null) {
        await windowManager.center();
        return;
      }
      final size = Size(
        (data['width'] as num).toDouble(),
        (data['height'] as num).toDouble(),
      );
      // The original window has no minimum size; narrow windows use the
      // compact (phone) layout.
      if (!size.isFinite || size.width <= 0 || size.height <= 0) return;
      final x = data['x'], y = data['y'];
      final bounds = x is num && y is num ? Offset(x.toDouble(), y.toDouble()) & size : null;
      final displays = await screenRetriever.getAllDisplays();
      final visible = bounds != null &&
          displays.any(
            (display) => ((display.visiblePosition ?? Offset.zero) & display.size)
                .overlaps(bounds),
          );
      if (visible) {
        await windowManager.setBounds(bounds);
      } else {
        await windowManager.setSize(size);
        await windowManager.center();
      }
      if (data['maximized'] == true) await windowManager.maximize();
    } catch (error) {
      _report(error);
    }
  }

  /// settings/window-state.ts coerceWindowState over the original Electron
  /// window's `window-state.json`: the last bounds (at least 400×300, a
  /// position only with both coordinates), used until this app saves its own.
  static Future<JsonMap?> originalWindowState(Map<String, String> env) async {
    final directory = originalDesktopUserDataDir(env);
    if (directory == null) return null;
    final file = File(p.join(directory, 'window-state.json'));
    try {
      if (!await file.exists()) return null;
      final state = jsonMap(jsonMap(jsonDecode(await file.readAsString()))['state']);
      final width = state['width'], height = state['height'];
      if (width is! num || height is! num || !width.isFinite || !height.isFinite) return null;
      final x = state['x'], y = state['y'];
      return {
        if (x is num && y is num && x.isFinite && y.isFinite) ...{'x': x.round(), 'y': y.round()},
        'width': width.round() < 400 ? 400 : width.round(),
        'height': height.round() < 300 ? 300 : height.round(),
        'maximized': state['isMaximized'] == true,
      };
    } on FormatException {
      return null;
    } on FileSystemException {
      return null;
    }
  }

  Future<void> _saveGeometry() async {
    if (_geometryFile == null || await windowManager.isFullScreen()) return;
    final maximized = await windowManager.isMaximized();
    final bounds = await windowManager.getBounds();
    JsonMap previous = {};
    if (maximized && await _geometryFile!.exists()) {
      previous = jsonMap(jsonDecode(await _geometryFile!.readAsString()));
    }
    final data = maximized && previous.isNotEmpty
        ? {...previous, 'maximized': true}
        : {
            'x': bounds.left,
            'y': bounds.top,
            'width': bounds.width,
            'height': bounds.height,
            'maximized': maximized,
          };
    await _geometryFile!.writeAsString(jsonEncode(data), flush: true);
  }

  void _scheduleGeometry() {
    _geometryTimer?.cancel();
    _geometryTimer = Timer(
      const Duration(milliseconds: 250),
      () => _run(_saveGeometry),
    );
  }

  @override
  void onWindowClose() => _run(close);
  @override
  void onWindowFocus() {
    if (isPrimary) {
      _lastWindow = windowId;
    } else {
      _run(() async {
        await WindowController.fromWindowId(primaryWindowId)
            .invokeMethod('focused', {'windowId': windowId});
      });
    }
  }

  @override
  void onWindowMoved() => _scheduleGeometry();
  @override
  void onWindowResized() => _scheduleGeometry();
  @override
  void onWindowMaximize() => _scheduleGeometry();
  @override
  void onWindowUnmaximize() => _scheduleGeometry();
  @override
  Future<AppExitResponse> didRequestAppExit() async {
    // Native Quit follows the explicit path, never the normal window-close path.
    await quit();
    return AppExitResponse.cancel;
  }

  void _run(Future<void> Function() action) =>
      unawaited(action().catchError(_report));
  void _report(Object error, [StackTrace? stack]) {
    if (isPrimary && !_disposed) {
      if (!_initialized || _readyWindows.isEmpty) {
        _startupErrors.add(error.toString());
      } else if (_lastWindow != null && _readyWindows.contains(_lastWindow)) {
        unawaited(
          WindowController.fromWindowId(_lastWindow!)
              .invokeMethod('hostError', {'message': error.toString()})
              .catchError((Object _) => null),
        );
      }
    }
    if (!_errors.isClosed) {
      if (_errors.hasListener) {
        _errors.add(error);
      } else {
        _pendingErrors.add(error);
      }
    }
    _log(error, stack);
  }

  void _log(Object error, [StackTrace? stack]) {
    final line =
        '${DateTime.now().toIso8601String()} $error${stack == null ? '' : '\n$stack'}\n';
    _logs = _logs.catchError((Object _) {}).then((_) async {
      if (_logFile != null) {
        await _logFile!.writeAsString(line, mode: FileMode.append);
      }
    });
  }

  Future<String> _tail(File file) async {
    if (!await file.exists()) return '';
    final handle = await file.open();
    try {
      final length = await handle.length();
      const maximum = 128 * 1024;
      if (length > maximum) await handle.setPosition(length - maximum);
      return utf8.decode(
        await handle.read(length > maximum ? maximum : length),
        allowMalformed: true,
      );
    } finally {
      await handle.close();
    }
  }

  Future<void> _disposeWindowResources() async {
    if (_disposed) return;
    _geometryTimer?.cancel();
    await _saveGeometry();
    _disposed = true;
    if (isCoordinator) await _notificationOperations;
    store.removeListener(_drainRoutes);
    windowManager.removeListener(this);
    WidgetsBinding.instance.removeObserver(this);
    for (final subscription in _subscriptions) {
      await subscription.cancel();
    }
    _subscriptions.clear();
    await integrations.dispose();
    await store.disposeAsync();
    await _logs;
    await _errors.close();
  }

  /// Explicit disposal means application shutdown, not closing one document/window.
  Future<void> dispose() => quit();
}

class DesktopQuitCancelled implements Exception {
  const DesktopQuitCancelled();
  @override
  String toString() => 'Quit cancelled by an open document.';
}

/// @getpaseo/protocol agent-deep-link.ts parseAgentDeepLink: exactly
/// `paseo://h/<serverId>/agent/<agentId>` with no credentials, port, query or
/// fragment; segments are percent-decoded and trimmed, and a malformed escape
/// or blank segment rejects the link.
({String serverId, String agentId})? parseAgentDeepLink(Uri uri) {
  if (uri.scheme != 'paseo' ||
      uri.host != 'h' ||
      uri.userInfo.isNotEmpty ||
      uri.hasPort ||
      uri.query.isNotEmpty ||
      uri.fragment.isNotEmpty) {
    return null;
  }
  final segments = uri.path.split('/').where((part) => part.isNotEmpty).toList();
  if (segments.length != 3 || segments[1] != 'agent') return null;
  try {
    final serverId = Uri.decodeComponent(segments[0]).trim();
    final agentId = Uri.decodeComponent(segments[2]).trim();
    return serverId.isEmpty || agentId.isEmpty
        ? null
        : (serverId: serverId, agentId: agentId);
  } on ArgumentError {
    return null; // malformed escape (JS decodeURIComponent URIError)
  } on FormatException {
    return null; // escape that is not UTF-8
  }
}

/// desktop open-project-routing.ts parseOpenProjectPathFromArgv: ignoring
/// `-psn_*` and `--no-sandbox*`, the first non-flag existing absolute
/// directory wins, else the directory following `--open-project` (the Linux
/// runner forwards it as `--paseo-project`).
Future<String?> openProjectPathFromArgv(List<String> argv) async {
  Future<bool> directory(String candidate) async =>
      p.isAbsolute(candidate) && await Directory(candidate).exists();
  final args = argv
      .where((arg) => !arg.startsWith('-psn_') && !arg.startsWith('--no-sandbox'))
      .toList();
  for (final arg in args) {
    if (!arg.startsWith('-') && await directory(arg)) return arg;
  }
  var index = args.indexOf('--open-project');
  if (index == -1) index = args.indexOf('--paseo-project');
  if (index == -1 || index + 1 >= args.length) return null;
  final flagged = args[index + 1];
  return await directory(flagged) ? flagged : null;
}
