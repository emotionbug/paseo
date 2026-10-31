import 'package:flutter/widgets.dart';

/// What a retained workspace panel knows about its own presentation
/// (retained-panel context `useRetainedPanelActive` plus the pane's
/// `isPaneFocused`): [visible] while it is the shown tab of a shown pane,
/// [focused] while it is additionally the focused pane's tab, and
/// [appActive] while the application is in the foreground
/// (`getIsAppActivelyVisible`).
class PanelActivity extends InheritedWidget {
  const PanelActivity({
    super.key,
    required this.visible,
    required this.focused,
    required this.appActive,
    required super.child,
  });

  final bool visible, focused, appActive;

  /// Panels mounted outside the workspace shell (tests, standalone routes)
  /// are always shown, focused and foreground.
  static const _standalone = (visible: true, focused: true, appActive: true);

  static ({bool visible, bool focused, bool appActive}) of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<PanelActivity>();
    if (scope == null) return _standalone;
    return (visible: scope.visible, focused: scope.focused, appActive: scope.appActive);
  }

  @override
  bool updateShouldNotify(PanelActivity oldWidget) =>
      visible != oldWidget.visible ||
      focused != oldWidget.focused ||
      appActive != oldWidget.appActive;
}
