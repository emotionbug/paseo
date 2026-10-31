import 'package:flutter/widgets.dart';

import 'app.dart';
import 'core/app_store.dart';
import 'core/json.dart';
import 'platform/desktop/desktop_host.dart';

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();
  final window = DesktopHost.supported
      ? await DesktopHost.windowArguments()
      : <String, dynamic>{};
  final coordinator = window.flag('isCoordinator');
  final store = AppStore(
    windowId: window.text('windowId', 'main'),
    connectHosts: !coordinator,
  );
  runApp(
    PaseoBootstrap(
      store: store,
      arguments: arguments,
      coordinator: coordinator,
    ),
  );
}
