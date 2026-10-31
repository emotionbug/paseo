# Notifications

Notifications are native platform integration. Flutter mobile ownership is under `packages/flutter_app/lib/platform/mobile`: service lifecycle, notification delivery/settings and push-token transport. Desktop integration is under `lib/platform/desktop`. Navigation into a notification target is owned by the shared native app state.

## Identity and delivery

A notification target identifies the host and session. Resolve it against saved hosts and current daemon data before opening; do not select an equal session/path on another host or create a replacement session when the target is unavailable. Wait for application initialization and host readiness, deduplicate delivery, and expose stale/unreachable-target failures.

The native mobile implementation uses Firebase messaging and native local notifications. It retains compatibility with the existing push service's payload/token contract, including Expo-formatted FCM application data. That service compatibility is not an Expo/Metro application runtime or a web client.

OS-rendered background notifications and locally presented foreground notifications have different ownership. Avoid duplicate alerts. Background handler errors must remain available to the foreground UI rather than being silently discarded. Notification permission is a user choice, not a daemon requirement.

## Configuration and privacy

Android/iOS credentials are optional and must match the Sessions application identity. Source builds do not include production push credentials or store provisioning. Do not claim push delivery works solely because a client compiles or a token is stored. Tokens and notification payloads may expose private host/session data; keep diagnostics scoped and redact credentials.

## Verification

Exercise permission denial/grant, foreground delivery, background delivery, terminated-app launch, duplicate taps, reconnect, stale targets and Back on the actual OS/device. Linux native client and server-side speech tests do not establish Android/iOS notification behavior. See [mobile testing](mobile-testing.md), [Android](android.md), and [testing](testing.md).
