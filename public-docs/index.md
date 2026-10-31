---
title: Getting started
description: Build the native Paseo client and Rust daemon from this checkout.
nav: Getting started
order: 1
category: Getting started
---

# Getting started

Paseo Sessions uses Flutter on **Linux, macOS, Windows, Android and iOS**, with a **Rust daemon and CLI**. Desktop/server hosts run agents; Android and iOS connect remotely. The client includes Markdown, source editing, summary-first diffs, remote folder browsing and read-only script/setup logs, organized as host → directory → session.

**This native cutover is not a published release.** Existing upstream downloads and store listings must not be assumed to contain it. Build from the migration checkout for local use. Binary redistribution remains subject to the unresolved requirements in the repository's `docs/release.md`, including source/licensing obligations for the native speech libraries.

## Prerequisites

Install and authenticate at least one supported provider CLI on the daemon machine. Paseo manages these tools; it does not supply your provider credentials. Git and, for relevant forge operations, an authenticated forge CLI are also needed.

Source builds require Rust/Cargo and **Node 24/npm as build tooling**. Client builds also require Flutter and the target platform's native toolchain. Desktop builds require the matching OS; iOS requires macOS/Xcode. Node is not bundled in the installed default runtime. Optional JavaScript backend extensions require an external Node installation.

## Desktop app

From the repository root:

```sh
npm ci
npm run dev:desktop
```

This builds/runs the native desktop client with an isolated checkout development home, `.dev/paseo-home`, on port `6768`. It does not replace an installed app or its daemon. For an optimized native build use `npm run build:flutter -- linux`, `macos`, or `windows` on the corresponding host. Packaging outputs go under `artifacts/flutter/`; a local build is not a published signed installer.

## Server / CLI

Build the native runtime and keep its generated assets together:

```sh
npm ci
npm run build:daemon
./target/debug/paseo onboard
./target/debug/paseo daemon start
./target/debug/paseo daemon status
```

Use `paseo.exe` on Windows and the corresponding directory when `CARGO_TARGET_DIR` is set. `npm run build:daemon:release` builds an optimized local runtime under the release target directory; optimization does not confer publication provenance.

The normal daemon endpoint is `ws://127.0.0.1:6767/ws`. Configuration and durable state remain under `PASEO_HOME` (default `~/.paseo`), or the explicit CLI `--home`. `paseo onboard` performs setup; bare `paseo` opens CLI chat, not daemon installation. For an isolated development daemon use `npm run dev:daemon` instead.

If you receive a native daemon archive from a separately authorized distribution, extract the **whole** archive, keep libraries/skills/worker assets alongside the executable, and invoke that executable. Do not install the retired Node CLI with npm or assume an old npm package supplies the Rust runtime.

## Mobile clients

```sh
npm run build:apk:arm64
npm run build:ios
```

Android uses package `sh.paseo.sessions`. Android/iOS outputs are under `artifacts/flutter/`; without signing/provisioning they are explicitly debug-signed or unsigned, not store submissions. Add the remote daemon's reachable address and credentials in the client. A phone's loopback address is not the daemon machine.

## Docker

Build the native daemon-only image from this checkout:

```sh
docker build -f docker/base/Dockerfile -t paseo:local .
docker run -d --name paseo-native \
  -p 127.0.0.1:6767:6767 \
  -e PASEO_PASSWORD=change-me \
  -v "$PWD/paseo-home:/home/paseo" \
  -v "$PWD/workspace:/workspace" \
  paseo:local
```

Choose a real password. Connect a native client or CLI; there is **no bundled web UI** to open at the HTTP root. Agent CLIs must be supplied separately. Existing registry images are not evidence of a published native migration.

## Runtime scope

There is no product web client, Electron/Chromium, embedded browser/automation, interactive terminal UI or executable React UI-plugin host. Workspace/service scripts still run in daemon-owned PTYs and provide read-only logs. Backend extensions and the external TypeScript SDK remain supported.

Current Linux execution evidence and unverified platform boundaries are documented in the repository's `docs/testing.md`. Cross-platform CI entries are not successful device tests. Native packages do not include speech model weights; they are downloaded on demand.

See [Connectivity](/docs/connectivity), [CLI reference](/docs/cli), [TypeScript SDK](/docs/sdk), and the checkout's `README.md` / `docs/development.md` for connection and development details.
