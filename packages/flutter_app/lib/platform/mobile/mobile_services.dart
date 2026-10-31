import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:app_badge_plus/app_badge_plus.dart';
import 'package:app_links/app_links.dart';
import 'package:firebase_core/firebase_core.dart';
import 'package:firebase_messaging/firebase_messaging.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../../core/app_store.dart';
import '../../core/client.dart';
import '../../core/json.dart';
import '../../core/target.dart';
import 'expo_push_client.dart';
import 'mobile_notification_delivery.dart';

class MobileServices extends ChangeNotifier with WidgetsBindingObserver {
  MobileServices._();
  static final instance = MobileServices._();
  static bool get supported =>
      !kIsWeb && (Platform.isAndroid || Platform.isIOS);
  static Future<void> initialize(AppStore store) => instance._initialize(store);

  static const _nativePush = MethodChannel('sh.paseo/mobile_push_environment');
  final pendingConnectionLink = ValueNotifier<String?>(null);
  final _links = <String>[];
  final _handledNotificationIds = <String>{};
  final _subscriptions = <StreamSubscription<dynamic>>[];
  final _clients = <PaseoClient, VoidCallback>{};
  final _registeredTokens = <PaseoClient, String>{};
  AppStore? _store;
  SharedPreferences? _preferences;
  VoidCallback? _removeGuard;
  String? _token;
  String? _deviceToken;
  bool _firebaseReady = false;
  // Builds without Firebase credentials cannot issue push tokens. That is a
  // packaging fact, not a runtime failure for the user to dismiss.
  bool _pushUnavailable = false;
  bool _busy = false;
  bool _disposed = false;
  Object? _error;
  AuthorizationStatus? _permission;
  Future<void>? _initializing;

  Object? get error => _error;

  Future<void> _initialize(AppStore store) async {
    if (!supported) return;
    if (_initializing != null) return _initializing;
    _store = store;
    _initializing = _start();
    return _initializing;
  }

  Future<void> _start() async {
    WidgetsBinding.instance.addObserver(this);
    _store!.addListener(_syncHosts);
    _removeGuard = _store!.registerHostRemovalGuard(revokeHost);
    // OS links do not depend on Firebase provisioning being available.
    final links = AppLinks();
    _subscriptions.add(
      links.uriLinkStream.listen(_receiveLink, onError: _report),
    );
    try {
      final initial = await links.getInitialLink();
      if (initial != null) _receiveLink(initial);
    } catch (error) {
      _report(error);
    }
    if (Platform.isIOS) {
      _nativePush.setMethodCallHandler((call) async {
        if (call.method == 'notificationTap') _nativeTap(call.arguments);
      });
      try {
        final pending = await _nativePush.invokeListMethod<dynamic>(
          'startObservation',
        );
        for (final tap in pending ?? const []) {
          _nativeTap(tap);
        }
      } catch (error) {
        _report(error);
      }
    }
    _syncHosts();
    await requestNotifications();
  }

  void _report(Object error) {
    if (_disposed) return;
    _error = error;
    notifyListeners();
  }

  void _nativeTap(Object? value) {
    if (value is! Map) return;
    final tap = Map<String, dynamic>.from(value);
    final rawData = tap['data'];
    if (rawData is! Map) return;
    _receiveTap(tap.text('id'), Map<String, dynamic>.from(rawData));
  }

  Future<void> _configurePush() async {
    if (_firebaseReady) return;
    _preferences ??= await SharedPreferences.getInstance();
    if (_pushUnavailable) throw const _PushNotProvisioned();
    try {
      await Firebase.initializeApp();
    } catch (_) {
      _pushUnavailable = true;
      throw const _PushNotProvisioned();
    }
    FirebaseMessaging.onBackgroundMessage(mobilePushBackgroundHandler);
    await FlutterLocalNotificationsPlugin().initialize(
      settings: mobileNotificationSettings,
      onDidReceiveNotificationResponse: _localTap,
    );
    await createDefaultNotificationChannel();
    await FirebaseMessaging.instance
        .setForegroundNotificationPresentationOptions(
          alert: false,
          badge: false,
          sound: false,
        );
    _subscriptions.add(
      FirebaseMessaging.onMessage.listen((message) {
        unawaited(
          presentMobileNotification(
            message,
            foreground: true,
          ).catchError(_report),
        );
      }, onError: _report),
    );
    _subscriptions.add(
      FirebaseMessaging.onMessageOpenedApp.listen((message) {
        _receiveTap(
          mobileNotificationId(message),
          mobileNotificationData(message),
        );
      }, onError: _report),
    );
    // Expo requires the FCM registration-token API provided by Messaging 16.x.
    // Messaging 17's onRegistered supplies a Firebase Installation ID instead.
    // Do not send that different identifier to Expo's device-token exchange.
    _subscriptions.add(
      FirebaseMessaging.instance.onTokenRefresh.listen((_) {
        // iOS must exchange APNs, not Firebase's refreshed FCM registration token.
        unawaited(requestNotifications(requestPermission: false));
      }, onError: _report),
    );
    _firebaseReady = true;
    final remoteLaunch = await FirebaseMessaging.instance.getInitialMessage();
    if (remoteLaunch != null) {
      _receiveTap(
        mobileNotificationId(remoteLaunch),
        mobileNotificationData(remoteLaunch),
      );
    }
    final localLaunch = await FlutterLocalNotificationsPlugin()
        .getNotificationAppLaunchDetails();
    if (localLaunch?.didNotificationLaunchApp == true &&
        localLaunch?.notificationResponse != null) {
      _localTap(localLaunch!.notificationResponse!);
    }
  }

  Future<void> requestNotifications({bool requestPermission = true}) async {
    if (!supported || _busy || _disposed) return;
    _busy = true;
    _error = null;
    notifyListeners();
    try {
      await _configurePush();
      final messaging = FirebaseMessaging.instance;
      final settings = requestPermission
          ? await messaging.requestPermission(
              alert: true,
              badge: true,
              sound: true,
            )
          : await messaging.getNotificationSettings();
      _permission = settings.authorizationStatus;
      final allowed =
          _permission == AuthorizationStatus.authorized ||
          _permission == AuthorizationStatus.provisional;
      if (!allowed) {
        _token = null;
        _deviceToken = null;
        for (final host in _store!.hosts) {
          await revokeHost(host);
        }
        return;
      }
      final deviceToken = Platform.isIOS
          ? await messaging.getAPNSToken()
          : await messaging.getToken();
      if (deviceToken == null || deviceToken.isEmpty) {
        throw StateError(
          'The operating system has not issued a push token. Check notification signing and Firebase configuration, then retry.',
        );
      }
      if (deviceToken != _deviceToken || _token == null) {
        final nextToken = await ExpoPushClient().issue(deviceToken);
        if (_disposed) return;
        // Retire each previous backend subscription before replacing its cached identity.
        for (final host in _store!.hosts) {
          final old = _preferences!.getString(_tokenKey(host));
          if (old != null && old != nextToken) await revokeHost(host);
        }
        _token = nextToken;
        _deviceToken = deviceToken;
      }
      _syncHosts();
      await _readBackgroundError();
    } on _PushNotProvisioned {
      // Local notifications, links and hosts keep working without push.
    } catch (error) {
      _report(error);
    } finally {
      _busy = false;
      if (!_disposed) notifyListeners();
    }
  }

  String _tokenKey(HostState host) => 'mobile.expo.host.${host.config.id}';

  void _syncHosts() {
    if (_disposed || _store == null) return;
    final current = _store!.hosts.map((host) => host.client).toSet();
    for (final client in _clients.keys.toList()) {
      if (!current.contains(client)) {
        client.removeListener(_clients.remove(client)!);
        _registeredTokens.remove(client);
      }
    }
    for (final host in _store!.hosts) {
      if (!_clients.containsKey(host.client)) {
        void changed() => _register(host);
        _clients[host.client] = changed;
        host.client.addListener(changed);
      }
      _register(host);
    }
  }

  void _register(HostState host) {
    if (host.client.status != ConnectionStatus.connected) {
      _registeredTokens.remove(host.client);
      return;
    }
    final token = _token;
    if (token == null || _registeredTokens[host.client] == token) return;
    // Mark synchronously so repeated store notifications do not duplicate writes.
    _registeredTokens[host.client] = token;
    unawaited(
      _saveAndRegister(host, token).catchError((Object error) {
        _registeredTokens.remove(host.client);
        _report(error);
      }),
    );
  }

  Future<void> _saveAndRegister(HostState host, String token) async {
    if (!await _preferences!.setString(_tokenKey(host), token)) {
      throw StateError(
        'Could not save the Expo token for later host revocation.',
      );
    }
    if (_disposed ||
        !_store!.hosts.contains(host) ||
        host.client.status != ConnectionStatus.connected ||
        _token != token) {
      return;
    }
    // This protocol message has no acknowledgement. Never report server acceptance.
    host.client.send('register_push_token', {'token': token});
  }

  /// Original `revokeSubscription`: revocation is best effort (2s,
  /// PUSH_TOKEN_REVOCATION_TIMEOUT_MS) and the cached token is always forgotten,
  /// so an offline or slow host never blocks removing it.
  Future<void> revokeHost(HostState host) async {
    if (!supported) return;
    final preferences = _preferences ??= await SharedPreferences.getInstance();
    final token = preferences.getString(_tokenKey(host));
    if (token == null) return;
    if (host.client.status == ConnectionStatus.connected &&
        jsonMap(host.client.serverInfo)
            .object('features')
            .flag('pushTokenRevocation')) {
      try {
        await host.client.request(
          'push.unregister.request',
          {'token': token},
          responseType: 'push.unregister.response',
          timeout: const Duration(seconds: 2),
        );
      } catch (error) {
        debugPrint('[PushNotifications] Failed to revoke push token: $error');
      }
    }
    await preferences.remove(_tokenKey(host));
    _registeredTokens.remove(host.client);
  }

  void _localTap(NotificationResponse response) {
    try {
      if (response.payload == null) return;
      final payload = jsonMap(jsonDecode(response.payload!));
      _receiveTap(payload.text('id'), payload.object('data'));
    } catch (error) {
      _report(error);
    }
  }

  void _receiveTap(String id, Map<String, dynamic> data) {
    if (_disposed || id.isEmpty || !_handledNotificationIds.add(id)) return;
    unawaited(_navigateNotification(data).catchError(_report));
  }

  Future<void> _navigateNotification(Map<String, dynamic> data) async {
    final store = _store!;
    final serverId = stringValue(data['serverId']).trim();
    if (serverId.isEmpty) return;
    final workspaceId = stringValue(data['workspaceId']).trim();
    final terminalId = stringValue(data['terminalId']).trim();
    final agentId = stringValue(data['agentId']).trim();
    if (workspaceId.isNotEmpty && terminalId.isNotEmpty) {
      await store.openExecutionLog(serverId, workspaceId, terminalId);
    } else if (agentId.isNotEmpty) {
      await store.openAgent(serverId, agentId);
    } else {
      if (store.hostFor(serverId) == null) {
        throw StateError(
          'This notification refers to a host that is no longer paired.',
        );
      }
      store.open(
        PanelTarget(
          kind: 'new_tab',
          serverId: serverId,
          workspaceId: '',
          cwd: '',
          title: store.hostFor(serverId)!.label,
        ),
      );
      store.showSidebar(true);
    }
    await setBadgeCount(0);
  }

  void _receiveLink(Uri uri) {
    if (_disposed) return;
    // Parsing and approval belong to the existing host connection dialog.
    final value = uri.toString();
    if (_links.contains(value) || pendingConnectionLink.value == value) return;
    _links.add(value);
    pendingConnectionLink.value ??= _links.removeAt(0);
  }

  String? takeConnectionLink() {
    final link = pendingConnectionLink.value;
    pendingConnectionLink.value = _links.isEmpty ? null : _links.removeAt(0);
    return link;
  }

  Future<void> setBadgeCount(int count) async {
    if (!supported) return;
    if (count < 0) throw ArgumentError.value(count, 'count');
    // Some Android launchers do not support numeric badges.
    if (await AppBadgePlus.isSupported()) await AppBadgePlus.updateBadge(count);
  }

  Future<void> _readBackgroundError() async {
    await _preferences!.reload();
    final error = _preferences!.getString('mobile.push.backgroundError');
    if (error != null) {
      await _preferences!.remove('mobile.push.backgroundError');
      _report(StateError(error));
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(requestNotifications(requestPermission: false));
      unawaited(setBadgeCount(0).catchError(_report));
    }
  }

  Future<void> disposeAsync() async {
    if (_disposed) return;
    if (supported && Platform.isIOS) {
      try {
        await _nativePush.invokeMethod<void>('stopObservation');
      } catch (error) {
        _report(error);
      }
      _nativePush.setMethodCallHandler(null);
    }
    _disposed = true;
    WidgetsBinding.instance.removeObserver(this);
    _store?.removeListener(_syncHosts);
    _removeGuard?.call();
    for (final entry in _clients.entries) {
      entry.key.removeListener(entry.value);
    }
    _clients.clear();
    for (final subscription in _subscriptions) {
      await subscription.cancel();
    }
    _subscriptions.clear();
    pendingConnectionLink.dispose();
    super.dispose();
  }
}

class _PushNotProvisioned implements Exception {
  const _PushNotProvisioned();
}
