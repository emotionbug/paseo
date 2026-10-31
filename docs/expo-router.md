# Native navigation

Navigation is owned by the Flutter client; Expo Router and React Navigation are not runtime dependencies. This existing filename is retained for links, not as an instruction to recreate the retired router.

The current ownership is `packages/flutter_app/lib/ui/shell.dart`, `core/app_store.dart`, and `features/navigation`. Native platform services pass host/session targets into this application state rather than constructing routes in a separate JavaScript app.

## Identity and selection

The primary tree is **host → directory → session**. A path is only meaningful together with its host. Preserve independent host and directory collapse state, including empty directories. Search can reveal matching ancestors without overwriting the saved collapse choices.

Workspaces remain durable daemon entities and Command Center targets, but they are not sidebar grouping rows. Closing a chat tab is navigation, not archive. Reopening a session must select its actual host and current persisted identity instead of inventing a new session.

## Compact behavior

Back returns from file explorer to the session, then from session to sidebar. Leaving the mobile client does not stop a remote daemon or its sessions. Settings and modal workflows retain their own navigation state. Width changes must not change host/session identity or duplicate provider connections.

## Startup and external targets

Restore saved connections and reconcile host/session state before opening a notification or deep-link target. Handle stale or unavailable targets visibly. Do not discard a user's saved host list or drafts merely because connection startup failed.

Verify navigation through actual Flutter startup, reconnect, resize and restart. Pure state tests do not establish Android/iOS lifecycle behavior. See [mobile testing](mobile-testing.md) and [testing](testing.md).
