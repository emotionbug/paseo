# Compact native panels

The Flutter shell owns compact presentation in `packages/flutter_app/lib/ui/shell.dart`, using application state from `core/app_store.dart` and feature navigation under `features/navigation`. It does not use React/Zustand or a Reanimated gesture store.

The compact flow is sidebar → session → supporting file view. Back returns from the file explorer to the session, then to the sidebar. Leaving the app does not stop remote sessions. Settings and focused modal workflows retain their own navigation behavior.

Keep navigation intent separate from transient geometry. Resizing or rotating must not change host identity, create duplicate sessions, lose drafts or mutate persisted collapse state. Repeated open/close actions and a cancelled gesture must settle to one coherent destination rather than leaving invisible hit regions.

The sidebar remains host → directory → session on compact screens. Every host is represented, empty directories remain visible, and equal paths on different hosts stay separate. Directory creation and remote browsing retain their explicit host/path target.

Keyboard and safe-area changes must keep the composer and primary actions reachable. Menus, pickers and supporting views must not intercept input intended for a different layer. Provide visible controls as well as gestures.

Exercise the real Flutter app with rapid navigation, rotation, keyboard, reconnect, persisted restart and Back. Linux compact-window evidence is not Android/iOS device evidence; see [mobile testing](mobile-testing.md) and [testing](testing.md).
