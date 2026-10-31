# Mobile testing

The mobile client is Flutter on Android and iOS. It connects to a separate Rust daemon; it never owns a local daemon process. Test the actual native application, not a web rendering or an old Expo/Metro dev client.

## Build and connect

Use `npm run dev:android -- --device <id>` or, on macOS/Xcode, `npm run dev:ios -- --device <id>`. The wrapper selects an attached Flutter device and installs/runs a debug build. Artifact builds use `npm run build:apk`, `npm run build:apk:arm64`, or `npm run build:ios`; signing limitations are documented in [Android](android.md) and [release](release.md).

Start a disposable Rust daemon with `npm run dev:daemon` or an explicitly isolated home/listen address. A standard Android AVD reaches the host through `10.0.2.2`, not its own loopback. Physical devices require reachable LAN/VPN/relay addresses and matching credentials. Do not expose a passwordless daemon for convenience.

## Behavioral matrix

Exercise compact and wide layouts, keyboard visible/hidden, rotation/resize, foreground/background, disconnect/reconnect, and persisted restart. Record the exact OS/device/build and whether it is a simulator/emulator or hardware.

- Host → directory → session navigation, empty directories, collapse persistence, filtering and same-path isolation between hosts.
- Back from file explorer to session, session to sidebar, and leaving the app without stopping remote work.
- Add Project through the selected daemon's remote folder browser.
- Chat streaming, draft retention, permissions, session rename/import/history and closing a tab without archiving.
- Markdown selection/copy/links, file editing/save failure, and large summary-first diff navigation.
- Setup/service scripts and read-only log output; no interactive terminal emulator.
- Schedules, profiles, connected hosts, and server errors.
- Microphone/audio interruptions and recovery, notification permissions/taps, and QR pairing on the actual mobile platform.

Use current native semantic controls and fresh screenshots/observations. Former React test IDs, Maestro flows, Agent Device scripts and native-module patches from the retired client are not executable acceptance instructions for Flutter. A source-only change cannot demonstrate an OS permission sheet, app lifecycle callback or device audio path.

## Evidence boundaries

Dart/widget tests prove their in-process contract. They do not prove platform plugins, signed install/upgrade, push delivery or hardware microphone behavior. Native Linux speech inference proves server-side VAD/STT/TTS, not mobile capture.

The recorded Linux-hosted Android ARM64 release-mode build passed with debug signing; no APK installation or Android-device/iOS execution is claimed. CI definitions are not execution evidence. See [testing](testing.md) for current observations rather than carrying forward old mobile pass counts.

Capture screenshots/logs without unrelated personal data or credentials. Preserve failing output, use bounded waits for observable conditions, and clean up only test-owned app instances, homes and processes. Installation or device changes require the appropriate authorization.
