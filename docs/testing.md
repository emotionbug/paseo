# Testing

Test observable behavior at the boundary that owns it. Rust unit tests, Dart/widget tests, wire integration tests, actual provider runs, native app interaction, packaging, and signed distribution each prove different things. None is a substitute for all the others.

## Commands and ownership

From the repository root:

```sh
npm run test:daemon        # cargo test --workspace --locked
npm run typecheck:daemon   # cargo check --workspace --all-targets --locked
npm run test:flutter      # Flutter tests through the repository SDK wrapper
npm run typecheck:flutter # Flutter analyze through the same wrapper
npm run test:node          # retained SDK/extension/tooling workspace tests
npm run typecheck:node     # retained TypeScript workspaces
```

The root `test` and `typecheck` commands include the native daemon as well as Flutter and retained Node tooling. Node tests do not exercise a Node product daemon: the runtime is Rust. `cargo test --workspace --locked` and `cargo check --workspace --all-targets --locked` can also be run directly.

Rust tests are colocated in `packages/daemon/src` and its test modules. Flutter tests live in `packages/flutter_app/test`; actual app integration runs must use the Flutter client, not a browser rendering of the old UI. Optional worker and SDK checks belong to their retained TypeScript workspaces. Packaging/tooling tests under `scripts/` validate artifact contracts, not the actual behavior of every target OS.

Do not run all heavy suites concurrently. Select a bounded relevant check during an explicitly authorized verification pass. Preserve errors and failing output; do not suppress a real service failure or silently relabel a mock as end-to-end.

## Required native smoke boundaries

Use a disposable `PASEO_HOME`, checkout, endpoint, and model cache where applicable. Never stop, reset, install over, or reuse the user's installed daemon/application for a migration smoke.

- Launch the real Rust daemon and native Flutter client; connect, authenticate, and restore persisted host state.
- Exercise host → directory → session navigation, independent collapse, empty directories, filtering, and same-path separation across hosts.
- Browse a remote host's folders; create a session in the selected directory; rename and reopen it.
- Stream provider output, answer permissions, reconnect, restore history, and verify failures remain visible.
- Render Markdown, edit/read back source, and open a large summary-first diff without eagerly loading every patch.
- Start/stop workspace setup and service scripts; inspect read-only logs. Verify the backend PTY survives client presentation changes.
- Exercise schedules, MCP, connected-host forwarding, profiles, and native CLI operations at their real protocol boundaries.
- Exercise desktop start/idempotence, worker restart, supervisor ownership, owned shutdown, keep-running policy, and preservation of an unowned daemon. A direct Rust unit test is not a Dart-to-desktop-host IPC test.
- Exercise native tray/menu behavior on the actual desktop, and bundled Neovim editing as a real process. Compilation alone does not prove either.
- Exercise speech model installation, native library loading, VAD, synthesis, resampling, and recognition separately from microphone capture and cloud-provider tests.

The client deliberately excludes interactive terminal UI, embedded browser/automation, web, and executable React UI plugins. Do not keep obsolete Electron/Metro/browser smoke commands as the acceptance path for the native product.

## Retained Linux desktop regression

`packages/flutter_app/integration_test/desktop_lifecycle.dart` exercises the real native desktop coordinator, user engines, managed daemon and editor. `tools/native-desktop-smoke/run.py` and `bus.py` provide isolated Linux desktop/session services and collect results. Prerequisites are Linux, Xvfb, `dbus-run-session`, Python 3 with `python3-dbus` and `python3-gi`, Gdk 3 introspection, the Flutter Linux toolchain, and a complete built native runtime directory. Do not replace that runtime with a bare Cargo executable missing its assets.

From `packages/flutter_app`, with consistent `ANDROID_HOME`/`ANDROID_SDK_ROOT` if Android tooling is also configured:

```sh
flutter build linux --debug --target=integration_test/desktop_lifecycle.dart
python3 ../../tools/native-desktop-smoke/run.py --runtime /absolute/path/to/native-runtime-directory
```

The runner uses private `HOME`/`PASEO_HOME` and the default GPU path, without a Node product runtime. It waits for actual widgets before reporting case state. Its three lifecycle cases are tray hide/activate/menu Quit, last-window close with no tray panel (a real watcher without a host), and GNOME logout while the user window is hidden. Assert managed-daemon shutdown in every case. Software-renderer runs are diagnostic evidence, not a substitute for default-GPU coverage.

Screenshot phase acknowledgements keep the user window alive until capture completes, preventing a race with Quit. The visual assertion compares the same window immediately before hiding and after restoration: matching pixels establish that hide/restore preserved its rendered contents, not conformity to a style golden.

### Vendored Linux multiwindow ownership

The project vendors `desktop_multi_window` **0.3.1+paseo.1**, retaining its license and upstream macOS/Windows implementations. The Linux patch gives each Flutter engine a private `GdkDisplay`/`EGLDisplay`, scopes GL bindings during creation, unbinds EGL before GTK's before-paint phase, and keeps the display alive until engine teardown finishes. Flutter 3.47 terminates an engine's EGL display during teardown; sharing that display across live engines invalidated another window's rendering resources. A captured failure reached GTK's next-frame `gdk_gl_context_make_current` and libGLX `BadAccess` after deferred EGL teardown. The regression must cover closing and restoring actual windows, not merely engine construction or a mocked channel response.

## Recorded migration evidence

The following is **local Linux evidence for this cutover**, not a release certification or a statement about another OS:

| Boundary                                | Observed result                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust daemon checks                      | 633 unit tests and one doctest passed (634 total)                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Retained JavaScript workspaces          | `npm run test:node` passed 1,994 tests, with four skipped; repository Node script checks passed 34 tests; final retained-workspace typecheck passed, including worker, examples and SDKs                                                                                                                                                                                                                                                                                                     |
| Native Flutter + Rust UI smoke          | Ten-flow smoke passed; four real native screenshots reviewed: host tree, Markdown, 432 KB diff, compact sidebar                                                                                                                                                                                                                                                                                                                                                                              |
| Provider and extension interoperability | Real Rust adapters/daemon passed Claude, ACP and MCP scenarios against controlled provider-protocol boundary fixtures; these were not paid live Claude/ACP LLM executions. Optional external-Node backend-extension lifecycle was exercised separately                                                                                                                                                                                                                                       |
| Desktop lifecycle IPC                   | Actual Dart `DesktopDaemonService` → Rust desktop-host passed managed start, idempotence, worker restart, supervisor ownership, owned stop, keep-running, and unowned-daemon preservation                                                                                                                                                                                                                                                                                                    |
| Native runtime assets and CLI           | Normal-entry Linux release build via `node scripts/build-flutter.mjs linux` passed with a bundled native runtime and no publication identity. Packaged executable hash matched provenance; `PATH=/nonexistent` version execution returned `paseo 0.13.0`. Reserved/Unicode CLI deep-link IDs and spaced project paths passed a separate launcher-boundary check                                                                                                                              |
| Normal-entry Linux release UI           | Actual release `paseo_sessions`, with no test Dart entry or runtime override, passed in private Xvfb/D-Bus and private homes using `/usr/bin:/bin` with Node absent. The supervisor executable resolved to bundled `runtime/paseo`; daemon status was reachable, desktop-managed and v0.13.0. Reviewed production screenshot showed the full Sessions shell and This computer Live without an error banner. Native tray Quit Paseo exited the app with status 0 and stopped its owned daemon |
| Flutter static/unit checks              | Final Flutter analyze reported no issues; the complete Flutter test run passed 15 tests                                                                                                                                                                                                                                                                                                                                                                                                      |
| Native speech                           | Production Sherpa 1.12.28 loader/FFI; Silero VAD rejected all 31 digital-silence windows and accepted 48 synthesized-speech windows; transactional Kokoro/Parakeet model download/install; TTS produced 44,404 samples at 24 kHz; production PCM resampler and Parakeet recognized exactly `Hello, this is a speech test.`                                                                                                                                                                   |
| Android compilation and payload         | Final Linux-hosted ARM64 release-mode wrapper build passed with Flutter 3.47.5, compile SDK 37.0 and AGP 9.1.1. ZIP enumeration verified only `arm64-v8a` native libraries, including `libapp.so` and `libflutter.so`; `apksigner verify` passed with Android Debug signing. The corrected APK is 37,410,467 bytes (37.4 MB decimal). No installation, device execution or publication is claimed                                                                                            |
| Native editor                           | Actual packaged Neovim delete, undo, Korean insertion and `:w` passed                                                                                                                                                                                                                                                                                                                                                                                                                        |
| Native desktop lifecycle                | Latest retained debug integration build and portable default-GPU regression passed all three cases after the GLES `eglBindAPI` correction, with actual `PaseoShell` readiness and no renderer overrides. Earlier phase-acknowledged captures matched the same window's pixels before hide and after restoration. Earlier software-renderer and Skia-GPU runs also passed                                                                                                                     |

Recorded local proof locations include:

- `/tmp/paseo-native-smoke-proof-RQuBYN`: ten-flow native client/daemon smoke.
- `/tmp/paseo-dart-lifecycle-proof-EGYKDJ`: final recorded Dart-to-desktop-host lifecycle run.
- `/tmp/paseo-native-package-proof-i1Dilu/verification.json`: relocated executable hash and native version execution without Node on `PATH`. Reserved/Unicode deep-link IDs and spaced project paths were verified separately in the recorded `desktop_cli.py` stdout (job `bg108`), not by this JSON file.
- `packages/flutter_app/build/linux/x64/release/bundle`: normal-entry local Linux release build, with packaged runtime SHA-256 `49461ebb63a8a5cebed6756e78664f36e4c557100c689e523281920382bdc256` matching provenance, native version execution with `PATH=/nonexistent`, and release identity false. These hash/version observations are build evidence; UI execution is recorded separately below.
- `/tmp/paseo-production-bundle-proof-ifgtgr7c`: normal-entry release UI, bundled-runtime executable identity, reachable desktop-managed daemon, reviewed `production.png`, native tray Quit and owned-daemon shutdown. The run used private Xvfb/D-Bus, `HOME` and `PASEO_HOME`, no `PASEO_DESKTOP_RUNTIME` override, and `/usr/bin:/bin` with Node confirmed absent. Final artifact verification JSON is retained alongside the screenshot.
- `/tmp/paseo-native-extension-smoke-JRISnz/attempt-1793297454710918880/evidence`: optional extension lifecycle, with a native-only default process tree.
- `/tmp/paseo-native-desktop-proof-cvkj2q7m/tray/events.jsonl`: actual Neovim editing.
- `/tmp/paseo-native-desktop-proof-gp71uas4`: default-GPU portable regression, three of three lifecycle cases passed; `bus.jsonl` records `restored-pixels-match-before-hide` from phase-acknowledged captures of the same window.
- `/tmp/paseo-native-desktop-proof-yrr4uyir`: latest retained integration build and all three default-GPU lifecycle scenarios passed after the GLES `eglBindAPI` correction.
- `/tmp/paseo-native-desktop-proof-3hw3cdv6/results.json`: all three software-renderer lifecycle cases, including real no-panel behavior.
- `/tmp/paseo-native-speech-proof-XQQw9g/result.json` and `native-speech.wav`: native speech inference.
- `artifacts/flutter/Paseo-Sessions-Flutter-0.13.0-android-arm64-debug-signed.apk`: verified ARM64-only native payload, SHA-256 `11a5c8ddca90e1dfa46e57474086dcf81099a10b2995a1864dc068df2f3d47b9`; verified Android Debug certificate SHA-256 `fac61745dc0903786fb9ede62a962b399f7348f0bb6f899b8332667591033b9c`.

These are temporary local investigation artifacts, not published downloadable release evidence. Each proves the recorded run, not unexecuted later edits. The speech models were temporary downloads, not packaged weights.

Residual upstream GTK 3 signal-disconnect cleanup warnings and Flutter implicit-view/messenger teardown warnings were observed. They were not suppressed; none of the three recorded lifecycle flows crashed. Passing these flows does not establish warning-free teardown.

The empty-`PATH` check covers the native CLI's version command only. A full GTK UI still needs system helpers: removing every command from `PATH` prevented the system glycin PNG loader from finding `bwrap`. For a Node-absent UI check, preserve required system paths and verify that Node is absent there; do not treat the deliberately broken system-helper environment as an application packaging failure.

At the time of this record there was **no actual Windows, macOS, Android-device, iOS or Wayland execution**. Android ARM64 compilation passed, but no production signing, installation or mobile runtime test is claimed. Provider fixtures do not establish paid live LLM execution. Microphone hardware and live cloud-speech services were not tested by the native inference run. No release was published, and the user's existing app, daemon and OMP were not installed over or restarted. Do not convert an unexecuted check into a passing claim.

## CI definitions versus execution

`.github/workflows/ci.yml` defines native-daemon Linux, Windows and macOS lanes and Flutter lanes. Native-daemon jobs build the native executable/optional worker and run `cargo test --workspace --locked`. Retired TypeScript server/integration/CLI jobs are not the product runtime acceptance path. `.github/ci-paths.yml` and CI Recovery route the retained jobs.

A matrix entry proves intended routing only. Report its actual run ID, platform, revision, result and logs before calling that platform verified. The same rule applies to release/build workflows: an installer filename or generated manifest does not establish successful execution, signing, publication, or legal distribution readiness.

## Failure and security expectations

Every fallible action exposes pending, success, and error state. A failed save must retain the draft; a failed provider turn must retain the error; a failed directory listing must allow retry without selecting a different host. Tests should assert these transitions, not just absence of crashes.

Use bounded waits tied to observable conditions rather than arbitrary sleeps. Retain process identity when testing shutdown so an unrelated process cannot be signalled. Redact credentials and tokens from shared logs. A missing real credential or unavailable service is an explicit test limitation, not permission to substitute a fake result.
