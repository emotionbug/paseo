import 'dart:convert';
import 'dart:io';

import 'json.dart';

class HostConfig {
  const HostConfig({
    required this.id,
    required this.label,
    required this.endpoint,
    this.password,
    this.localCredential,
    this.localCredentialPath,
    this.expectedServerId,
    this.color,
  });
  final String id, label, endpoint;
  final String? password,
      localCredential,
      localCredentialPath,
      expectedServerId,
      color;

  factory HostConfig.fromJson(JsonMap value) => HostConfig(
    id: value['id'] as String,
    label: value['label'] as String,
    endpoint: value['endpoint'] as String,
    password: value['password'] as String?,
    localCredential: value['localCredential'] as String?,
    localCredentialPath: value['localCredentialPath'] as String?,
    expectedServerId: value['expectedServerId'] as String?,
    color: value['color'] as String?,
  );
  JsonMap toJson() => {
    'id': id,
    'label': label,
    'endpoint': endpoint,
    if (password != null) 'password': password,
    if (localCredential != null) 'localCredential': localCredential,
    if (localCredentialPath != null) 'localCredentialPath': localCredentialPath,
    if (expectedServerId != null) 'expectedServerId': expectedServerId,
    if (color != null) 'color': color,
  };

  ResolvedHost resolve() {
    try {
      return _resolve();
    } catch (_) {
      // URI parsers include their source in errors, which can contain a password.
      throw const FormatException(
        'Invalid host connection. Use host:port, ws(s)://, tcp://, unix://, pipe://, ssh:// or a pairing URL.',
      );
    }
  }

  ResolvedHost _resolve() {
    var input = endpoint.trim();
    final connect = input.indexOf('#connect=');
    if (connect >= 0) input = Uri.decodeComponent(input.substring(connect + 9));
    final offer = input.indexOf('#offer=');
    if (offer >= 0) {
      final value = jsonDecode(
        utf8.decode(
          base64Url.decode(base64Url.normalize(input.substring(offer + 7))),
        ),
      );
      if (value is! Map ||
          value['v'] != 2 ||
          value['serverId'] is! String ||
          value['daemonPublicKeyB64'] is! String ||
          value['relay'] is! Map) {
        throw const FormatException('Invalid pairing offer');
      }
      final relay = value['relay'] as Map;
      final minimum = value['minimumSecurityMode'];
      if (minimum != null && minimum != 'protected') {
        throw const FormatException('Invalid relay security policy');
      }
      return _relay(
        relay['endpoint'] as String,
        relay['useTls'] as bool? ??
            (Uri.parse('ws://${relay['endpoint']}').port == 443),
        value['serverId'] as String,
        value['daemonPublicKeyB64'] as String,
        minimum == 'protected',
        password,
      );
    }
    if (RegExp(r'^\d+$').hasMatch(input)) input = 'ws://localhost:$input/ws';
    if (input.startsWith('unix://') || input.startsWith('pipe://')) {
      final scheme = input.startsWith('pipe://') ? 'pipe' : 'unix';
      var path = input.substring(7);
      if (path.isEmpty || path == '~') {
        throw const FormatException('Socket path is required');
      }
      if (scheme == 'unix' && path.startsWith('~/')) {
        path =
            '${Platform.environment['HOME'] ?? Platform.environment['USERPROFILE']}/${path.substring(2)}';
      }
      return ResolvedHost(
        Uri(scheme: scheme, path: path),
        password: password,
        expectedServerId: expectedServerId,
      );
    }
    if (input.startsWith(r'\\.\pipe\')) {
      return ResolvedHost(
        Uri(scheme: 'pipe', path: input),
        password: password,
        expectedServerId: expectedServerId,
      );
    }
    if (input.startsWith('/') || input.startsWith('~/')) {
      if (input.startsWith('~/')) {
        input =
            '${Platform.environment['HOME'] ?? Platform.environment['USERPROFILE']}/${input.substring(2)}';
      }
      return ResolvedHost(
        Uri(scheme: 'unix', path: input),
        password: password,
        expectedServerId: expectedServerId,
      );
    }
    if (!input.contains('://')) input = 'ws://$input/ws';
    var uri = Uri.parse(input);
    if (uri.scheme == 'ssh') {
      if ((uri.queryParametersAll['password']?.length ?? 0) > 1 ||
          (uri.queryParametersAll['daemonPort']?.length ?? 0) > 1) {
        throw const FormatException('Duplicate SSH option');
      }
      final selectedPassword = password ?? uri.queryParameters['password'];
      uri = uri.replace(
        queryParameters: Map<String, String>.from(uri.queryParameters)
          ..remove('password'),
      );
      if (uri.host.isEmpty ||
          uri.userInfo.contains(':') ||
          (uri.path.isNotEmpty && uri.path != '/') ||
          uri.hasFragment ||
          uri.queryParameters.keys.any((key) => key != 'daemonPort')) {
        throw const FormatException('Invalid SSH endpoint');
      }
      return ResolvedHost(
        uri,
        password: selectedPassword,
        expectedServerId: expectedServerId,
      );
    }
    if (uri.userInfo.isNotEmpty || uri.hasFragment) {
      throw const FormatException(
        'Endpoint userinfo and fragments are not supported',
      );
    }
    if (uri.scheme == 'relay') {
      final security = uri.queryParameters['security'];
      if (security != null && security != 'protected') {
        throw const FormatException('Unsupported relay security policy');
      }
      if (uri.host.isEmpty ||
          !uri.hasPort ||
          uri.pathSegments.length != 1 ||
          uri.queryParameters['key'] == null) {
        throw const FormatException(
          'Relay endpoint requires server identity and key',
        );
      }
      return _relay(
        uri.authority,
        uri.queryParameters['ssl'] == 'true',
        uri.pathSegments.single,
        uri.queryParameters['key']!,
        security == 'protected',
        password ?? uri.queryParameters['password'],
      );
    }
    if (!{'tcp', 'ws', 'wss'}.contains(uri.scheme) || uri.host.isEmpty) {
      throw const FormatException('Unsupported daemon endpoint');
    }
    if (uri.scheme == 'tcp' &&
        (!uri.hasPort || uri.path.isNotEmpty && uri.path != '/')) {
      throw const FormatException('Invalid TCP endpoint');
    }
    if (uri.queryParameters['role'] == 'client' ||
        uri.queryParameters.containsKey('serverId')) {
      throw const FormatException(
        'A relay pairing offer with its daemon public key is required',
      );
    }
    final selectedPassword = password ?? uri.queryParameters['password'];
    final query = Map<String, String>.from(uri.queryParameters)
      ..remove('password')
      ..remove('ssl');
    if (uri.scheme == 'tcp') {
      uri = uri.replace(
        scheme: uri.queryParameters['ssl'] == 'true' ? 'wss' : 'ws',
        path: '/ws',
      );
    }
    uri = uri.replace(
      path: uri.path.isEmpty || uri.path == '/' ? '/ws' : uri.path,
      queryParameters: query,
    );
    if (uri.port < 1 || uri.port > 65535) {
      throw const FormatException('Invalid daemon port');
    }
    return ResolvedHost(
      uri,
      password: selectedPassword,
      expectedServerId: expectedServerId,
    );
  }

  ResolvedHost _relay(
    String address,
    bool tls,
    String serverId,
    String rawKey,
    bool protected,
    String? selectedPassword,
  ) {
    // Original relayConnectionFromOffer stores `daemonPublicKeyB64.trim()`.
    final key = rawKey.trim();
    final bytes = base64.decode(key);
    if (bytes.length != 32 || base64.encode(bytes) != key) {
      throw const FormatException('Noncanonical daemon public key');
    }
    final uri = Uri.parse('${tls ? 'wss' : 'ws'}://$address/ws').replace(
      queryParameters: {'serverId': serverId, 'role': 'client', 'v': '2'},
    );
    if (uri.host.isEmpty ||
        uri.userInfo.isNotEmpty ||
        uri.port < 1 ||
        uri.port > 65535 ||
        serverId.isEmpty ||
        serverId.contains('/')) {
      throw const FormatException('Invalid relay endpoint');
    }
    return ResolvedHost(
      uri,
      password: selectedPassword,
      expectedServerId: expectedServerId ?? serverId,
      daemonPublicKey: key,
      requireProtected: protected,
    );
  }
}

class ResolvedHost {
  const ResolvedHost(
    this.uri, {
    this.password,
    this.expectedServerId,
    this.daemonPublicKey,
    this.requireProtected = false,
  });
  final Uri uri;
  final String? password, expectedServerId, daemonPublicKey;
  final bool requireProtected;
  bool get isRelay => daemonPublicKey != null;
}
