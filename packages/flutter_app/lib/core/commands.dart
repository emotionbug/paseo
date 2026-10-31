import 'dart:async';

import 'package:flutter/foundation.dart';

import 'json.dart';

typedef PaseoCommandHandler = FutureOr<void> Function(JsonMap arguments);

/// Each retained panel owns handlers; dispatch targets only the focused panel.
class PaseoCommands {
  PaseoCommands._();
  static final instance = PaseoCommands._();
  final Map<String, Map<String, PaseoCommandHandler>> _owners = {};

  VoidCallback register(
    String owner,
    Map<String, PaseoCommandHandler> handlers,
  ) {
    _owners[owner] = handlers;
    return () {
      if (identical(_owners[owner], handlers)) _owners.remove(owner);
    };
  }

  Future<bool> invoke(
    String owner,
    String action, [
    JsonMap arguments = const {},
  ]) async {
    final handler = _owners[owner]?[action];
    if (handler == null) return false;
    await handler(arguments);
    return true;
  }
}
