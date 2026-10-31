# Development

## Toolchains

- Rust and Cargo for `packages/daemon` (the daemon and `paseo` CLI).
- Flutter/Dart for `packages/flutter_app`; set `FLUTTER_BIN` or `FLUTTER_ROOT`, or put Flutter on `PATH`. Packaging also searches `$XDG_CACHE_HOME/paseo-flutter-sdk` (default `~/.cache/paseo-flutter-sdk`).
- Node **24** and npm for workspace dependencies, build orchestration, the optional extension compiler, SDKs, and website. Node is not the installed daemon runtime.
- Linux desktop: Flutter's Linux toolchain and native GTK/build dependencies. macOS/iOS: macOS, Xcode and the Flutter Apple toolchain. Windows: Flutter's Windows/Visual Studio C++ toolchain. Android: JDK and Android SDK as required by the checked-in Flutter Android project.

Install repository dependencies with `npm ci`. The Flutter wrapper runs `flutter pub get --enforce-lockfile`. Desktop builds require their target host; the native packaging scripts reject `CARGO_BUILD_TARGET` cross-compilation.

## Development commands

Run commands from the repository root:

```sh
npm run build:daemon       # Cargo binary plus native runtime assets
npm run dev:daemon         # rebuild/watch and run the Rust daemon
npm run dev:app            # build/run the native desktop Flutter client
npm run dev:desktop        # the same desktop client entry point
npm run dev:android -- --device <device-id>
npm run dev:ios -- --device <device-id>
npm run cli -- ls          # native CLI against checkout development home
npm run dev:website        # separate website, not a product web client
```

`npm run dev` starts the development daemon. There is no Metro server, Expo prebuild, Electron renderer, Chromium debugging port, or web client build.

For direct Rust iteration, `cargo build --locked -p paseo-daemon --bin paseo` compiles the executable. Use `npm run build:daemon` when the run also needs staged speech libraries, skills, optional-extension compiler assets, or packaging provenance. Cargo-only compilation is not a complete distributable runtime.

`npm run build:daemon:release` produces an optimized local native runtime, not a published release identity. Default Flutter release builds use optimized Rust; debug development builds use the debug profile. `PASEO_NATIVE_RELEASE=1` is a separate, validated publication-provenance choice; see [release](release.md).

## Isolated state and endpoints

The ordinary native runtime defaults to `~/.paseo` and port `6767`. Development launchers use `<checkout>/.dev/paseo-home` and `127.0.0.1:6768`; they must not disturb an installed daemon or application.

The daemon development wrapper honors:

- `PASEO_HOME`: explicit isolated home.
- `PASEO_DEV_ROOT`: root for its default `.dev/paseo-home`.
- `PASEO_LISTEN`: listening endpoint.
- `PASEO_SERVICE_DAEMON_PORT`: service port, defaulting the listen address to `0.0.0.0:<port>` when supplied.
- `PASEO_DEV_SEED_HOME`: copy durable agent/project JSON and configuration into an empty development home, not process IDs, sockets, or logs.
- `PASEO_DEV_RESET_HOME=1`: destructive reset of that development destination before seeding; use only with an explicitly disposable target.
- `PASEO_LOCAL_MODELS_DIR`: speech model cache override. The development wrapper defaults to `~/.paseo/models/local-speech`; set a temporary directory for isolated model-download tests.

The Flutter `--run` wrapper fixes its development home to this checkout's `.dev/paseo-home` and defaults its listen endpoint to `127.0.0.1:6768`. Do not assume a custom daemon `PASEO_HOME` automatically changes the app wrapper's managed home. To use another host instance, add it as an explicit client connection.

`PASEO_NODE_ENV=development` is a retained configuration name used for development-only provider behavior; it does not switch the daemon implementation to Node.

A mobile emulator's `localhost` is not the development machine. On an Android AVD use `10.0.2.2:6768` and a reachable daemon listen address, or an explicitly configured ADB reverse tunnel. Physical devices need a reachable LAN/VPN/relay endpoint and the matching password. See [Android](android.md).

## Native builds

```sh
npm run build:flutter -- linux
npm run build:flutter -- macos
npm run build:flutter -- windows
npm run build:apk
npm run build:apk:arm64
npm run build:ios
npm run package:flutter:desktop
```

Only run the desktop target matching the host OS. iOS requires macOS. `build:ios` without export/provisioning configuration emits an explicitly unsigned device app archive, not an installable IPA. Android output without a release key is marked debug-signed. Build descriptors and client artifacts live under `artifacts/flutter/`; see [release](release.md) before producing distribution artifacts.

## Source ownership

- Rust: `packages/daemon/src`; CLI commands in `cli/`, providers in `providers/`, process ownership in `daemon_control/`, desktop IPC in `desktop_host/`.
- Dart: `packages/flutter_app/lib`; protocol and state in `core/`, features under `features/`, native desktop/mobile integration under `platform/`.
- Native packaging: `packages/flutter_desktop_host/packaging` and `scripts/build-native-daemon.mjs` / `scripts/build-flutter.mjs`.
- External TypeScript SDK: `packages/client`; wire definitions: `packages/protocol`; optional extension author SDK: `packages/plugin`.

Use Flutter tooling for frame/layout profiling and Rust instrumentation for daemon work. Browser/CDP and React profilers do not profile the native client. A development mock provider can test controlled UI states but is not evidence of real provider interoperability.

## Scripts, logs, and diagnostics

Workspace setup and services remain daemon-side shell/PTY operations. The Flutter app exposes start/stop and **read-only logs**. Do not remove PTY backend support because the interactive terminal UI is excluded.

Inspect the selected home's `daemon.log` and native CLI status. Keep diagnostic captures free of credentials and unrelated user state. A script or provider process is owned by the host daemon, never the mobile client.

## Checks

See [testing](testing.md) for Rust, Flutter, SDK, and real-runtime checks. Run the appropriate checks only when authorized by the assignment. A CI job definition, compilation, or a mocked widget test is not an actual platform smoke test.
