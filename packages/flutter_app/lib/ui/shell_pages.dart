import 'package:flutter/material.dart';

import 'common.dart' show isCompactLayout;

/// The desktop shell's content area for whole-screen pages.
abstract class ShellPageHost {
  void showPage(Widget page);
}

/// Original `/sessions` and `/schedules` routes render inside the app layout, beside
/// the sidebar, on desktop; compact layouts push the page over the shell. Each
/// desktop window is its own engine, so one host per isolate is that window's shell.
class ShellPages {
  ShellPages._();
  static ShellPageHost? _host;

  static void attach(ShellPageHost host) => _host = host;

  static void detach(ShellPageHost host) {
    if (identical(_host, host)) _host = null;
  }

  static Future<void> open(BuildContext context, Widget page) async {
    final host = _host;
    if (host != null && !isCompactLayout(context)) {
      host.showPage(page);
      return;
    }
    await Navigator.of(context).push(MaterialPageRoute<void>(builder: (_) => page));
  }
}

/// Marks a page shown beside the sidebar: its menu toggle shows or hides the
/// sidebar (original `SidebarMenuToggle`) instead of leaving the page.
class EmbeddedShellPage extends InheritedWidget {
  const EmbeddedShellPage({super.key, required super.child});

  static bool of(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<EmbeddedShellPage>() != null;

  @override
  bool updateShouldNotify(EmbeddedShellPage oldWidget) => false;
}

/// Reports when the embedded page's route is popped back to the empty base route.
class ShellPageObserver extends NavigatorObserver {
  ShellPageObserver(this.onClosed);
  final VoidCallback onClosed;

  @override
  void didPop(Route<dynamic> route, Route<dynamic>? previousRoute) {
    if (previousRoute?.isFirst ?? false) onClosed();
  }
}
