import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/widgets.dart';
import 'package:window_manager/window_manager.dart';

import 'package:paseo_sessions/main.dart' as production;
import 'package:paseo_sessions/platform/desktop/desktop_host.dart';
import 'package:paseo_sessions/features/files/vim_engine.dart';
import 'package:paseo_sessions/ui/shell.dart';

final proof = Directory(Platform.environment['PASEO_DESKTOP_SMOKE_PROOF']!);
void require(bool value, String message) {
  if (!value) throw StateError(message);
}

Future<void> save(String name, Object value) async {
  final file = File('${proof.path}/$name');
  final temporary = File('${file.path}.$pid.tmp');
  await temporary.writeAsString(jsonEncode(value));
  await temporary.rename(file.path);
}

void record(String phase, [Object? value]) {
  final row = jsonEncode({'phase': phase, 'value': value});
  File('${proof.path}/events.jsonl')
      .writeAsStringSync('$row\n', mode: FileMode.append);
  stdout.writeln(row);
}

Future<void> until(String label, Future<bool> Function() predicate) async {
  final deadline = DateTime.now().add(const Duration(seconds: 35));
  while (DateTime.now().isBefore(deadline)) {
    if (await predicate()) return;
    await Future<void>.delayed(const Duration(milliseconds: 100));
  }
  throw StateError('Timed out: $label');
}

Future<Map<String, dynamic>> childState(String id) async {
  final file = File('${proof.path}/window-$id.json');
  return await file.exists()
      ? jsonDecode(await file.readAsString()) as Map<String, dynamic>
      : {};
}

Future<void> command(String id, String action) => save('command-$id.json', {
  'id': DateTime.now().microsecondsSinceEpoch,
  'action': action,
});
Future<void> childLoop() async {
  await until('child host', () async => DesktopHost.instance != null);
  final host = DesktopHost.instance!;
  host.errors.listen(
    (error) =>
        record('child-error', {'window': host.windowId, 'error': '$error'}),
  );
  await until('user session shell', () async {
    var ready = false;
    void visit(Element element) {
      if (element.widget is PaseoShell) ready = true;
      element.visitChildren(visit);
    }

    WidgetsBinding.instance.rootElement?.visitChildren(visit);
    return ready;
  });
  await WidgetsBinding.instance.endOfFrame;
  Object? lastCommand;
  while (true) {
    if (host.integrations.lifecycleReady) {
      await save('window-${host.windowId}.json', {
        'id': host.windowId,
        'visible': await windowManager.isVisible(),
        'lifecycleReady': true,
      });
      final file = File('${proof.path}/command-${host.windowId}.json');
      if (await file.exists()) {
        final request =
            jsonDecode(await file.readAsString()) as Map<String, dynamic>;
        if (request['id'] != lastCommand) {
          lastCommand = request['id'];
          if (request['action'] == 'close') {
            record('child-native-close-request', host.windowId);
            await windowManager.close();
            record('child-native-close-returned', host.windowId);
          }
        }
      }
    }
    await Future<void>.delayed(const Duration(milliseconds: 100));
  }
}

Future<void> vimScenario() async {
  late VimEngine engine;
  var observed = '';
  var saved = false;
  engine = await VimEngine.start(
    text: 'alpha\nbeta',
    onText: (text) => observed = text,
    onSave: () async {
      await File('${proof.path}/vim-saved.txt').writeAsString(engine.text);
      saved = true;
      return true;
    },
    onClose: () async {},
  );
  try {
    require(
      engine.ready && engine.error == null,
      'Native Vim engine did not initialize',
    );
    await engine.input('dd');
    await until('native Vim delete line', () async => engine.text == 'beta');
    await engine.input('u');
    await until('native Vim undo', () async => engine.text == 'alpha\nbeta');
    await engine.input('A');
    await until('native Vim insert mode', () async => engine.acceptsText);
    await engine.applyFlutterValue(
      const TextEditingValue(
        text: 'alpha 한글\nbeta',
        selection: TextSelection.collapsed(offset: 8),
      ),
    );
    await engine.input('<Esc>');
    await until(
      'native Vim unicode insertion',
      () async => engine.text == 'alpha 한글\nbeta' && observed == engine.text,
    );
    await engine.input(':w<CR>');
    await until('native Vim save bridge', () async => saved);
    require(
      await File('${proof.path}/vim-saved.txt').readAsString() ==
          'alpha 한글\nbeta',
      'Native Vim save lost unicode or line state',
    );
    require(engine.error == null, 'Native Vim runtime failed: ${engine.error}');
    record('native-vim-delete-undo-unicode-save', {
      'text': engine.text,
      'mode': engine.mode,
    });
  } finally {
    await engine.close();
  }
}

Future<void> rootScenario() async {
  await until('coordinator host', () async => DesktopHost.instance != null);
  final host = DesktopHost.instance!;
  final failures = <Object>[];
  host.errors.listen((error) {
    failures.add(error);
    record('desktop-error', '$error');
  });
  await until('first actual visible user engine', () async {
    final windows = (await WindowController.getAll())
        .where((value) => value.windowId != host.windowId)
        .toList();
    return windows.length == 1 &&
        (await childState(windows.single.windowId))['visible'] == true;
  });
  require(failures.isEmpty, 'Desktop startup failed: $failures');
  require(
    host.isCoordinator && !await windowManager.isVisible(),
    'Coordinator became a user-facing window',
  );
  final first = (await WindowController.getAll()).singleWhere(
    (value) => value.windowId != host.windowId,
  );
  final started = await host.daemon.status();
  require(
    started['status'] == 'running',
    'Desktop coordinator failed to start native daemon',
  );
  await save('started.json', started);
  record('native-coordinator-and-user-engine', {
    'coordinator': host.windowId,
    'user': first.windowId,
    'daemon': started,
  });
  await vimScenario();
  await until(
    'capturing initial rendered window',
    () =>
        File('${proof.path}/captured-native-vim-delete-undo-unicode-save')
            .exists(),
  );
  final second = await host.openNew();
  await until(
    'second actual visible user engine',
    () async => (await childState(second.windowId))['visible'] == true,
  );
  await command(first.windowId, 'close');
  await until(
    'first user engine destroyed without quitting app',
    () async => !(await WindowController.getAll()).any(
      (value) => value.windowId == first.windowId,
    ),
  );
  require(
    (await host.daemon.status())['pid'] == started['pid'],
    'Closing one window stopped or replaced the daemon',
  );
  record('multiwindow-close-retains-daemon', {'retained': second.windowId});
  await until(
    'capturing the retained window before hiding',
    () =>
        File('${proof.path}/captured-multiwindow-close-retains-daemon')
            .exists(),
  );
  if (Platform.environment['PASEO_SMOKE_PANEL'] == '0') {
    record('expect-no-panel-quit');
    await command(second.windowId, 'close');
    return;
  }
  await command(second.windowId, 'close');
  await until(
    'last user engine hidden to tray',
    () async => (await childState(second.windowId))['visible'] == false,
  );
  require(
    (await WindowController.getAll()).any(
      (value) => value.windowId == second.windowId,
    ),
    'Tray close destroyed the retained engine',
  );
  require(
    (await host.daemon.status())['pid'] == started['pid'],
    'Tray close stopped or replaced the daemon',
  );
  record('last-window-hidden-to-tray');
  await until(
    'capturing the hidden window',
    () => File('${proof.path}/captured-last-window-hidden-to-tray').exists(),
  );
  await save('panel-command.json', {'action': 'activate'});
  await until(
    'real tray activation restores user engine',
    () async => (await childState(second.windowId))['visible'] == true,
  );
  require(failures.isEmpty, 'Desktop lifecycle failed: $failures');
  record('tray-activation-restores-same-engine', second.windowId);
  await until(
    'restoring the same rendered window',
    () =>
        File('${proof.path}/captured-tray-activation-restores-same-engine')
            .exists(),
  );
  if (Platform.environment['PASEO_SMOKE_LOGOUT'] == '1') {
    await command(second.windowId, 'close');
    await until(
      'hidden before logout',
      () async => (await childState(second.windowId))['visible'] == false,
    );
    record('expect-session-logout-quit');
    await save('panel-command.json', {'action': 'logout'});
  } else {
    record('expect-real-tray-menu-quit');
    await save('panel-command.json', {'action': 'quit'});
  }
}

Future<void> main(List<String> arguments) async {
  await runZonedGuarded(
    () async {
      await production.main(arguments);
      if (arguments.firstOrNull == 'multi_window') {
        await childLoop();
      } else {
        await rootScenario();
      }
    },
    (Object error, StackTrace stack) {
      record('FAIL', {'error': '$error', 'stack': '$stack'});
      exit(1);
    },
  );
}
