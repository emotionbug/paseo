# Android

The Android application is the Flutter **Paseo Sessions** client, package `sh.paseo.sessions`. It connects to a Rust daemon on another machine; it does not run a local daemon. The same Flutter client also targets iOS, Linux, macOS and Windows. There is no Expo/Metro, EAS, embedded browser, web client, or interactive terminal UI in the native application.

## Build and launch

Use Node 24/npm for the build wrapper, Flutter, JDK 17-compatible tooling and the Android SDK. The checked-in project is `packages/flutter_app/android`, not a generated Expo project. Minimum SDK is 24. The current dependency set requires compile SDK **37.0** (API 37, minor 0), **Android Gradle Plugin 9.1.1**, **NDK 28.2.13676358**, and JNI **CMake 3.22.1**; retain the Flutter-selected target SDK. AGP 9.1.0 with integer compile SDK 37 does not resolve the required `android-37.0` platform.

From the repository root:

```sh
npm ci
npm run build:apk             # universal APK
npm run build:apk:arm64       # arm64 APK
npm run dev:android -- --device <device-id>
```

The development wrapper uses an attached Android device/emulator selected through Flutter. If more than one supported device is attached, select its exact ID with `--device`. It runs `flutter pub get --enforce-lockfile` and uses the checked-in Dart dependency lock.

Set consistent `ANDROID_HOME` and `ANDROID_SDK_ROOT` for every Flutter build, including desktop builds from the same checkout. Flutter can rewrite Android `local.properties`; concurrent builds with different SDK environments can redirect Gradle to an unintended SDK. Avoid overlapping builds that mutate the same checkout's generated configuration.

APK artifacts are copied into `artifacts/flutter/` and named with the version and architecture. Builds without a release key are explicitly marked `-debug-signed.apk`. The final Linux-hosted ARM64 release-mode wrapper build passed with Flutter 3.47.5. ZIP enumeration verified that every native library is under `arm64-v8a`, with both `libapp.so` and `libflutter.so` present; the Gradle target-platform filter covers all native dependencies while leaving split-per-ABI builds alone.

The verified local artifact is `artifacts/flutter/Paseo-Sessions-Flutter-0.13.0-android-arm64-debug-signed.apk`: **37,410,467 bytes** (37.4 MB decimal), SHA-256 `11a5c8ddca90e1dfa46e57474086dcf81099a10b2995a1864dc068df2f3d47b9`. `apksigner verify` passed with Android Debug certificate SHA-256 `fac61745dc0903786fb9ede62a962b399f7348f0bb6f899b8332667591033b9c`. No APK was installed or published, no phone/emulator was exercised, and no production signing credentials were supplied. Compilation and local signature verification are not device execution or store readiness; see [testing](testing.md).

## Signing and identity

The project uses the maintained Sessions debug keystore at `packages/flutter_app/android/app/debug.keystore` for local debug compatibility. This is not a production signing credential.

For release signing supply all four values together:

- `ANDROID_KEYSTORE_PATH`
- `ANDROID_KEYSTORE_PASSWORD`
- `ANDROID_KEY_ALIAS`
- `ANDROID_KEY_PASSWORD`

Alternatively use the corresponding `storeFile`, `storePassword`, `keyAlias`, and `keyPassword` properties in `packages/flutter_app/android/key.properties`. A partial configuration is an error. `PASEO_ANDROID_REQUIRE_RELEASE_SIGNING=1` rejects a release build without production credentials instead of creating debug-signed output.

The app ID stays `sh.paseo.sessions`, separate from the upstream app. Optional Firebase configuration is supplied with `GOOGLE_SERVICES_FILE_SESSIONS`; the wrapper checks that it belongs to this package. Play Store credentials, submission and signing authorization are not included in this source fork.

## Version codes

`packages/flutter_desktop_host/packaging/common.mjs` resolves the build number from `PASEO_BUILD_NUMBER`, then `ANDROID_VERSION_CODE`, then `13000`. Android clamps it to at least `13000`, as does the Gradle project. Set the next code against actual installed/store history before a distribution. Do not reuse the former Expo version-code formula or EAS remote counters.

The repository's `fastlane/metadata/android` store text and changelog tooling are metadata, not evidence of a Flutter F-Droid/Play release. The old Expo source-only/F-Droid flags are not an Android build mode for this project. A source-distribution/store build needs its own completed dependency, signing and policy review; no such submission is claimed here.

## Connect to a development daemon

Start an isolated daemon on the development machine:

```sh
PASEO_LISTEN=0.0.0.0:6768 npm run dev:daemon
```

Add its host address in the client. A standard Android AVD can reach the machine at `10.0.2.2:6768`; `localhost` inside the emulator means the emulator itself. A physical phone needs a reachable LAN/VPN address or encrypted relay. Use a password for any network-reachable daemon and avoid exposing the port beyond the intended network.

An explicitly configured ADB reverse tunnel can instead forward the daemon port to the device's loopback. There is no Metro port to forward and no `EXPO_PUBLIC_*` endpoint baked into a JavaScript bundle. Select the host connection in the client.

## Navigation and retained features

The compact sidebar is host → directory → session. Back returns from an open file explorer to the session and from the session to the sidebar; leaving the app must not stop remote sessions. Directory collapse is persisted per host/path. Add Project browses folders on the selected host, not Android's local filesystem.

Markdown, source editing, large summary-first diffs, schedules, settings, speech controls and read-only script/setup logs are native client surfaces. Microphone/notification permissions and behavior require actual Android testing; Linux speech inference does not prove Android capture or push delivery.

## Release boundary

`build:ios` is a separate macOS/Xcode path; without export/provisioning configuration it produces an unsigned app archive. Native mobile targets do not inherit desktop daemon ownership. Review [release](release.md) for exact signing/artifact rules and unresolved distribution requirements before publishing anything.
