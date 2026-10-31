import 'dart:convert';
import 'dart:io';

import 'package:firebase_core/firebase_core.dart';
import 'package:firebase_messaging/firebase_messaging.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:shared_preferences/shared_preferences.dart';

const mobileNotificationSettings = InitializationSettings(
  android: AndroidInitializationSettings('@drawable/paseo_notification_icon'),
  iOS: DarwinInitializationSettings(
    requestAlertPermission: false,
    requestBadgePermission: false,
    requestSoundPermission: false,
  ),
);

Future<void> createDefaultNotificationChannel() async {
  await FlutterLocalNotificationsPlugin()
      .resolvePlatformSpecificImplementation<
        AndroidFlutterLocalNotificationsPlugin
      >()
      ?.createNotificationChannel(
        const AndroidNotificationChannel(
          'default',
          'default',
          importance: Importance.defaultImportance,
        ),
      );
}

Map<String, dynamic> mobileNotificationData(RemoteMessage message) {
  // Expo's FCM transport stores application data in the JSON-encoded `body`.
  final body = message.data['body'];
  if (body is String) {
    try {
      final decoded = jsonDecode(body);
      if (decoded is Map) return Map<String, dynamic>.from(decoded);
    } on FormatException {
      // Native APNs messages expose application data directly instead.
    }
  } else if (body is Map) {
    return Map<String, dynamic>.from(body);
  }
  return message.data;
}

String mobileNotificationId(RemoteMessage message) =>
    message.messageId ??
    '${message.sentTime?.millisecondsSinceEpoch}:${jsonEncode(message.data)}';

Future<void> presentMobileNotification(
  RemoteMessage message, {
  required bool foreground,
}) async {
  // APNs and FCM notification messages already have an OS-rendered background alert.
  if (!foreground && (Platform.isIOS || message.notification != null)) return;
  final title =
      message.notification?.title ?? message.data['title']?.toString();
  final body =
      message.notification?.body ?? message.data['message']?.toString();
  if (title == null && body == null) return;
  final identity = mobileNotificationId(message);
  var id = 0;
  for (final unit in identity.codeUnits) {
    id = (id * 31 + unit) & 0x7fffffff;
  }
  await FlutterLocalNotificationsPlugin().show(
    id: id,
    title: title,
    body: body,
    notificationDetails: NotificationDetails(
      android: AndroidNotificationDetails(
        'default',
        'default',
        importance: Importance.defaultImportance,
        priority: Priority.defaultPriority,
        silent: foreground,
        playSound: !foreground,
        enableVibration: !foreground,
        channelAction: AndroidNotificationChannelAction.update,
        number: int.tryParse(message.data['badge']?.toString() ?? ''),
      ),
      iOS: const DarwinNotificationDetails(
        presentAlert: false,
        presentBanner: false,
        presentList: true,
        presentSound: false,
        presentBadge: false,
      ),
    ),
    payload: jsonEncode({
      'id': identity,
      'data': mobileNotificationData(message),
    }),
  );
}

@pragma('vm:entry-point')
Future<void> mobilePushBackgroundHandler(RemoteMessage message) async {
  try {
    await Firebase.initializeApp();
    if (!Platform.isAndroid) return;
    await FlutterLocalNotificationsPlugin().initialize(
      settings: mobileNotificationSettings,
    );
    await createDefaultNotificationChannel();
    await presentMobileNotification(message, foreground: false);
  } catch (error) {
    // Surface background-isolate failures when the UI resumes instead of losing them.
    final preferences = await SharedPreferences.getInstance();
    await preferences.setString(
      'mobile.push.backgroundError',
      error.toString(),
    );
  }
}
