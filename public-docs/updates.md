---
title: Updates
description: Native build identity, update channels, and migration availability.
nav: Updates
order: 5
category: Getting started
---

# Updates

The checkout version is **0.13.0**, but the Rust/Flutter migration is **unpublished**. Existing downloads, npm CLI packages, store listings, and container `latest` tags must not be assumed to contain it.

## Native build identity

The product client is Flutter on Linux, macOS, Windows, Android, and iOS. Desktop/server hosts run the Rust daemon and Rust `paseo` CLI; mobile clients connect remotely and do not run a daemon.

For a local source build, use Node 24, Rust/Cargo, Flutter, and the target platform's native tools:

```bash
npm ci
npm run build:daemon
# Or an optimized LOCAL build, not release provenance:
npm run build:daemon:release
npm run build:flutter -- linux
```

Choose `linux`, `macos`, or `windows` on the matching desktop host. Flutter outputs are under `artifacts/flutter/`. The daemon is `target/debug/paseo` or `target/release/paseo` (`paseo.exe` on Windows), unless `CARGO_TARGET_DIR` overrides the root. Keep generated adjacent assets with the executable.

## Updating a local installation

Back up your Paseo home before replacing a runtime. The default remains `~/.paseo`, including existing configuration and history. Stop the managed daemon with `paseo daemon stop`, replace the complete runtime with a build from the intended revision, then run `paseo daemon start` and `paseo daemon status`. A worker restart is not a replacement of the supervisor executable.

Bare `paseo` opens CLI chat. Use `paseo onboard` for explicit setup, not as an update command.

## Channels and publication

Stable and beta are release identities, not proof that this migration has shipped. Optimizing a local binary does not enable official updater provenance. Native publication requires validated source identity, channel/repository metadata, complete runtime artifacts, signing where applicable, and an authorized release.

Do not rely on legacy desktop update-screen instructions for a local Flutter build. See the repository [release guide](https://github.com/getpaseo/paseo/blob/main/docs/release.md) for current update manifests, rollout controls, and packaging requirements.

## Mobile and verification boundaries

`npm run build:apk` or `npm run build:apk:arm64` builds the Android remote client. Without release signing credentials, output is explicitly debug-signed. `npm run build:ios` requires macOS/Xcode and emits an unsigned device app archive unless provisioning/export is supplied. Neither output is automatically store-ready.

Recorded runtime execution is on Linux. Android ARM64 release-mode compilation passed with debug signing, without APK installation or device execution. Windows/macOS/iOS execution is not claimed. See [testing evidence](https://github.com/getpaseo/paseo/blob/main/docs/testing.md) for current desktop follow-up results and limitations; workflow definitions are not successful runs.

Generated third-party notices do not certify distributability. The TTS-enabled Sherpa 1.12.28 prebuilts statically include GPL espeak-ng; corresponding-source/offer, combined-work, MPL/LGPL, CRT, and Rust toolchain obligations remain unresolved. The [release guide](https://github.com/getpaseo/paseo/blob/main/docs/release.md) owns these requirements. No license change or removal of local TTS is implied.
