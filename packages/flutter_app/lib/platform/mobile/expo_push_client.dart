import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:uuid/uuid.dart';

/// Implements the same token exchange as expo-notifications/getExpoPushTokenAsync.
/// The daemon accepts Expo tokens, never the device's FCM or APNs token.
class ExpoPushClient {
  static const projectId = '0e7f65ce-0367-46c8-a238-2b65963d235a';
  static const _environment = MethodChannel('sh.paseo/mobile_push_environment');

  Future<String> issue(String deviceToken) async {
    final preferences = await SharedPreferences.getInstance();
    var installationId = preferences.getString('mobile.expo.installationId');
    if (installationId == null) {
      installationId = const Uuid().v4().toLowerCase();
      if (!await preferences.setString(
        'mobile.expo.installationId',
        installationId,
      )) {
        throw StateError(
          'Could not save the push notification installation identity.',
        );
      }
    }
    final application = await PackageInfo.fromPlatform();
    final environment = Platform.isIOS
        ? await _environment.invokeMethod<String>('apnsEnvironment')
        : 'production';
    if (environment != 'development' && environment != 'production') {
      throw StateError(
        'Cannot determine the signing profile APNs environment.',
      );
    }
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 20);
    try {
      final request = await client.postUrl(
        Uri.parse('https://exp.host/--/api/v2/push/getExpoPushToken'),
      );
      request.headers.contentType = ContentType.json;
      request.write(
        jsonEncode({
          'type': Platform.isIOS ? 'apns' : 'fcm',
          'deviceId': installationId,
          'development': environment == 'development',
          'appId': application.packageName,
          'deviceToken': deviceToken,
          'projectId': projectId,
        }),
      );
      final response = await request.close().timeout(
        const Duration(seconds: 30),
      );
      final body = await utf8.decoder
          .bind(response)
          .join()
          .timeout(const Duration(seconds: 30));
      if (response.statusCode != HttpStatus.ok) {
        // Do not expose the response body: the upstream can echo device tokens.
        throw HttpException(
          'Expo push token issuance failed (HTTP ${response.statusCode}). Check the EAS project push credentials.',
        );
      }
      final decoded = jsonDecode(body);
      final token = decoded is Map && decoded['data'] is Map
          ? decoded['data']['expoPushToken']
          : null;
      if (token is! String ||
          !RegExp(r'^(ExponentPushToken|ExpoPushToken)\[[^\]]+\]$')
              .hasMatch(token)) {
        throw const FormatException('Expo returned an invalid push token.');
      }
      return token;
    } finally {
      client.close(force: true);
    }
  }
}
