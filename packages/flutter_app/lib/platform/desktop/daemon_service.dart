import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as p;

/// Paths refer to unpacked, platform-native release assets, never Flutter assets
/// inside an archive. Both executable paths identify the native Paseo binary.
class DesktopDaemonRuntime {
  const DesktopDaemonRuntime({
    required this.executable,
    required this.cliShimPath,
    required this.workingDirectory,
    this.environment = const {},
  });

  factory DesktopDaemonRuntime.fromJson(
    Map<String, dynamic> data, {
    required String directory,
  }) {
    String resolve(String key) {
      final value = data[key];
      if (value is! String || value.isEmpty) {
        throw FormatException('Runtime manifest is missing $key');
      }
      return p.normalize(p.join(p.absolute(directory), value));
    }

    final environment = data['environment'];
    return DesktopDaemonRuntime(
      executable: resolve('executable'),
      cliShimPath: resolve('cliShimPath'),
      workingDirectory: resolve('workingDirectory'),
      environment: environment is Map
          ? environment.map(
              (key, value) => MapEntry(key as String, value.toString()),
            )
          : const {},
    );
  }

  final String executable;
  final String cliShimPath;
  final String workingDirectory;
  final Map<String, String> environment;

  Map<String, dynamic> toJson() => {
    'executable': executable,
    'cliShimPath': cliShimPath,
    'workingDirectory': workingDirectory,
  };
}

/// These preferences are independent. Disabling management must not rewrite the
/// user's keep-running preference; quit still stops only our captured launch.
class DesktopDaemonSettings {
  const DesktopDaemonSettings({
    required this.managed,
    required this.keepRunningAfterQuit,
  });

  final bool managed;
  final bool keepRunningAfterQuit;

  Map<String, dynamic> toJson() => {
    'managed': managed,
    'keepRunningAfterQuit': keepRunningAfterQuit,
  };
}

class DesktopDaemonException implements Exception {
  const DesktopDaemonException(this.code, this.message);
  final String code;
  final String message;
  @override
  String toString() => '$message ($code)';
}

/// One helper per app session holds daemon ownership in memory. Ownership cannot
/// be recovered by trusting the desktopManaged bit after a helper/app crash.
class DesktopDaemonService {
  DesktopDaemonService({
    required this.runtime,
    required this.settings,
    this.home,
    this.appVersion,
  });

  final DesktopDaemonRuntime runtime;
  final String? home;
  final String? appVersion;
  final FutureOr<DesktopDaemonSettings> Function() settings;
  Process? _process;
  Future<void>? _opening;
  Future<void>? _closing;
  bool _closed = false;
  bool _rejectNewRequests = false;
  int _nextId = 0;
  final Map<int, Completer<Object?>> _pending = {};
  StreamSubscription<String>? _stdout;
  StreamSubscription<String>? _stderr;
  String _diagnostics = '';
  Future<void> initialize() async {
    await _ensureOpen();
    if ((await settings()).managed) await start();
  }

  Future<void> _ensureOpen() {
    if (_closed || _rejectNewRequests) {
      return Future.error(StateError('Desktop daemon service is closed.'));
    }
    return _opening ??= _open();
  }

  Future<void> _open() async {
    if (!Platform.isLinux && !Platform.isMacOS && !Platform.isWindows) {
      throw UnsupportedError(
        'The built-in daemon requires a desktop operating system.',
      );
    }
    if (!await File(runtime.executable).exists()) {
      throw FileSystemException(
        'Bundled native Paseo executable is missing.',
        runtime.executable,
      );
    }
    final process = await Process.start(
      runtime.executable,
      ['desktop-host'],
      workingDirectory: runtime.workingDirectory,
      environment: runtime.environment,
      includeParentEnvironment: true,
      runInShell: false,
    );
    _process = process;
    unawaited(
      process.stdin.done.catchError((Object error, StackTrace trace) {
        _failPending(error, trace);
      }),
    );
    _stdout = process.stdout
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(
          _receive,
          onError: (Object error, StackTrace trace) =>
              _failPending(error, trace),
        );
    _stderr = process.stderr
        .transform(const Utf8Decoder(allowMalformed: true))
        .listen((text) {
          _diagnostics += text;
          if (_diagnostics.length > 8192) {
            _diagnostics = _diagnostics.substring(_diagnostics.length - 8192);
          }
        });
    unawaited(
      process.exitCode.then((code) {
        if (identical(_process, process)) {
          _process = null;
          _failPending(
            DesktopDaemonException(
              'HELPER_EXITED',
              'Desktop daemon helper exited ($code).${_diagnostics.isEmpty ? '' : '\n$_diagnostics'}',
            ),
          );
        }
      }),
    );
    try {
      await _send('initialize', {
        'runtime': runtime.toJson(),
        if (home != null) 'home': home,
        if (appVersion != null) 'appVersion': appVersion,
        'settings': (await settings()).toJson(),
      });
    } catch (_) {
      // No start request has been sent, so initialization failure owns no daemon.
      try {
        await process.stdin.close();
      } catch (_) {
        // Preserve the initialization failure if the child already exited.
      }
      await process.exitCode;
      rethrow;
    }
  }

  void _receive(String line) {
    try {
      final message = jsonDecode(line) as Map<String, dynamic>;
      final id = message['id'];
      final completer = id is int ? _pending.remove(id) : null;
      if (completer == null) return;
      final error = message['error'];
      if (error is Map) {
        completer.completeError(
          DesktopDaemonException(
            error['code']?.toString() ?? 'DAEMON_HELPER_ERROR',
            error['message']?.toString() ?? 'Desktop daemon operation failed.',
          ),
        );
      } else {
        completer.complete(message['result']);
      }
    } catch (error, trace) {
      _failPending(
        DesktopDaemonException('INVALID_HELPER_RESPONSE', '$error'),
        trace,
      );
    }
  }

  void _failPending(Object error, [StackTrace? trace]) {
    final requests = _pending.values.toList(growable: false);
    _pending.clear();
    for (final request in requests) {
      request.completeError(error, trace);
    }
  }

  Future<Object?> _send(String command, Map<String, dynamic> args) async {
    final process = _process;
    if (process == null) {
      throw StateError('Desktop daemon helper is not running.');
    }
    final id = ++_nextId;
    final completer = Completer<Object?>();
    _pending[id] = completer;
    // Register the response before writing so even immediate replies are safe.
    try {
      process.stdin.writeln(
        jsonEncode({'id': id, 'command': command, 'args': args}),
      );
    } catch (_) {
      _pending.remove(id);
      rethrow;
    }
    return completer.future;
  }

  Future<Object?> _request(
    String command, [
    Map<String, dynamic> args = const {},
  ]) async {
    await _ensureOpen();
    final preferences = await settings();
    if (_rejectNewRequests) {
      throw StateError('Desktop daemon service is closing.');
    }
    return _send(command, {...args, 'settings': preferences.toJson()});
  }

  Future<Map<String, dynamic>> _map(
    String command, [
    Map<String, dynamic> args = const {},
  ]) async => Map<String, dynamic>.from(await _request(command, args) as Map);

  Future<Map<String, dynamic>> status() => _map('status');
  Future<Map<String, dynamic>> start() => _map('start');
  Future<Map<String, dynamic>> stop({
    String reason = 'manual_ipc',
    int? pid,
    String? startedAt,
  }) => _map('stop', {'reason': reason, 'pid': ?pid, 'startedAt': ?startedAt});
  Future<Map<String, dynamic>> restart({int? pid, String? startedAt}) =>
      _map('restart', {'pid': ?pid, 'startedAt': ?startedAt});
  Future<Map<String, dynamic>> logs() => _map('logs');

  /// Retains the desktop command names used by existing settings/host workflows.
  Future<Object?> invoke(
    String command, [
    Map<String, dynamic> args = const {},
  ]) async {
    switch (command) {
      case 'desktop_daemon_status':
        return status();
      case 'start_desktop_daemon':
        return start();
      case 'stop_desktop_daemon':
        return stop(
          reason: args['reason'] as String? ?? 'manual_ipc',
          pid: args['pid'] as int?,
          startedAt: args['startedAt'] as String?,
        );
      case 'restart_desktop_daemon':
        return restart(
          pid: args['pid'] as int?,
          startedAt: args['startedAt'] as String?,
        );
      case 'desktop_daemon_logs':
        return logs();
      default:
        throw UnsupportedError('Unknown desktop daemon command: $command');
    }
  }

  /// Cancels pending startup before stopping only the exact launch we acquired.
  /// A shutdown failure is surfaced and the service remains available for retry.
  Future<void> close({bool? keepRunning}) {
    if (_closed) return Future.value();
    return _closing ??= _close(keepRunning).whenComplete(() {
      _closing = null;
    });
  }

  Future<void> _close(bool? keepRunning) async {
    _rejectNewRequests = true;
    try {
      final opening = _opening;
      if (opening != null) {
        try {
          await opening;
        } catch (_) {
          // Initialization already reported its error and launched no daemon.
        }
      }
      final process = _process;
      if (process != null) {
        await _send('close', {
          'keepRunning': keepRunning ?? (await settings()).keepRunningAfterQuit,
        });
        await process.stdin.close();
        await process.exitCode;
      }
      _closed = true;
      await _stdout?.cancel();
      await _stderr?.cancel();
    } catch (_) {
      _rejectNewRequests = false;
      rethrow;
    }
  }
}
