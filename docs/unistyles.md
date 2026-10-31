# Native styling

The client uses Flutter themes and widgets, not Unistyles, React Native StyleSheet, CSS or a JavaScript renderer. This existing filename is retained for documentation links; the former Unistyles rules are not instructions for the native app.

`packages/flutter_app/lib/ui/theme.dart` owns the native theme. Shared UI behavior belongs in `ui/common.dart` and the existing feature widgets. Reuse those boundaries instead of introducing a second token system or copying retired TSX primitives.

Theme changes must preserve host/session identity, editor drafts, scroll position and active subscriptions. Rebuilding presentation is not permission to restart provider work or reset navigation. Use semantic colors and native text styles, respect text scaling and contrast, and keep loading/error/disabled states readable in both themes.

Compact layouts must account for safe areas, keyboard insets and touch targets. Hover-only affordances need a visible or long-press alternative. Desktop and touch behavior are input/layout concerns, not evidence of a browser target.

Measure the actual Flutter frame/layout behavior on the affected platform. React/Chromium profiling and old native-module crash workarounds do not validate the Flutter rendering pipeline. See [design](design.md), [development](development.md), and [testing](testing.md).
