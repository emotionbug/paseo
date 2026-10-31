# Release

The checkout version is **0.13.0**. The Rust daemon/CLI and five-platform Flutter client cutover has **not been published**. Build automation and generated notices do not authorize publication or establish that a release is legally distributable.

## Distribution requirements

**Unresolved release-distribution requirement:** the official **Sherpa 1.12.28 TTS-enabled prebuilts statically include GPL espeak-ng**. Paseo packaging now generates Cargo, optional-worker, and speech attributions using `packages/flutter_desktop_host/packaging/third-party-notices.mjs`. Including these notices does **not** establish:

- complete corresponding source or a valid source offer;
- the applicable GPL combined-work scope and license compatibility;
- satisfaction of MPL/LGPL conditions;
- rights or required notices for proprietary CRT redistribution;
- complete Rust-toolchain runtime/dependency obligations.

Paseo's source license has not changed and local TTS has not been removed. This document is neither a compliance certification nor an offer to provide source. Distribution review remains required before publishing binary assets or container images.

Investigation evidence is recorded locally in `/tmp/paseo-source-closure-feasibility.json`. It identifies a technically feasible **separate source artifact** alongside binary assets, without installing source into the application or changing its runtime. No such complete artifact has been assembled, rebuilt, published, or offered by this migration.

The unresolved technical requirement is a complete mapping from **each shipped binary digest and architecture** to exact source trees, transitive sources/submodules/FetchContent inputs, patches, generated-source recipes, and build/packaging modifications. Top-level repository links or source tarballs alone do not establish that closure. Upstream release tags and temporally adjacent build runs do not prove that their inputs produced the selected binary bytes. The evidence records distinct Eigen inputs in Sherpa and ONNX Runtime, modified/generated Sherpa version information, ONNX workflow modifications, and missing authoritative per-binary source binding.

A controlled source build using the same pinned source archive and recorded recipes, or authoritative matching upstream binary/source records, is needed to establish that mapping. Qualified release review must separately resolve licensing, combined-work, installation-information, CRT and toolchain conditions; source delivery alone does not decide those questions. An eventual source location must actually be downloadable and tied to the release before it can be represented as available. This is a release gate, not a promise of future compliance.

Native packages carry speech libraries, **not speech model weights**. Models are downloaded on demand and must not be described as bundled. Model distribution/licensing is a separate consideration from the native libraries.

## Release contents

| Target                        | Native artifact scope                                                                         |
| ----------------------------- | --------------------------------------------------------------------------------------------- |
| Linux, macOS, Windows desktop | Flutter application, Rust daemon/CLI, native desktop integration and runtime assets           |
| Android                       | Flutter remote client APK; no daemon                                                          |
| iOS                           | Flutter remote client; no daemon; unsigned app archive unless provisioning/export is supplied |
| Standalone daemon             | Rust executable with libraries, skills, optional-worker assets, notices and provenance        |
| Container                     | Native daemon and runtime assets; no browser UI or Node executable in the default image       |
| npm                           | Retained SDK/protocol/extension libraries, not installation of a Node product daemon or CLI   |

No Electron, Chromium, web client, embedded automation, or interactive terminal UI is shipped. Script/setup PTY support stays in the daemon and the client presents read-only logs. Optional JavaScript backend extensions require external Node; their shipped compiler/SDK closure is not a Node runtime.

## Local build versus distribution identity

Use Node 24 for packaging, Rust/Cargo, Flutter, and each target's native toolchain. Build desktop artifacts on the matching OS; build iOS on macOS with Xcode.

```sh
npm ci
npm run build:daemon           # debug binary and runtime assets
npm run build:daemon:release   # optimized local Rust build, not publication
npm run build:flutter -- linux # matching host only; optimized desktop runtime
npm run package:flutter:desktop
npm run build:apk:arm64
npm run build:ios
```

The default Flutter release build uses optimized Rust; a debug development build uses the debug profile. Optimization is independent of publication: `--release` alone must not turn a local binary into an updater-enabled official build.

Published provenance is explicitly selected by `PASEO_NATIVE_RELEASE=1` together with the optimized build and validated `PASEO_UPDATE_REPOSITORY`, `PASEO_RELEASE_CHANNEL`, and `PASEO_BUILD_SOURCE_SHA`. The source SHA must identify the checked-out source. Standalone `npm run package:daemon` requires that release identity; it is not a way to relabel an arbitrary local executable.

Client artifacts and build descriptors live in `artifacts/flutter/`. Standalone daemon archives are `Paseo-Daemon-<version>-<platform>-<arch>.tar.gz` (ZIP on Windows), with a channel/platform/architecture manifest. Native runtime metadata includes `paseo-runtime.json`, `bundle-provenance.json`, and desktop-specific `desktop-runtime.json` when applicable. Preserve assets together; copying only the executable omits runtime libraries and skills.

Bundle provenance records the executable digest **after relocation**, because runtime patching can change its bytes. `sourceExecutableSha256` separately identifies the input executable. Check an artifact against its final digest, not against the pre-relocation source hash.

## Signing and mobile build numbers

`packages/flutter_desktop_host/packaging/common.mjs` resolves the build number from `PASEO_BUILD_NUMBER`, then `ANDROID_VERSION_CODE`, then `13000`. Android enforces a minimum of `13000`. Confirm the next build number against the actual store history; do not infer store acceptance from a local version string.

Android release signing uses `ANDROID_KEYSTORE_PATH`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, and `ANDROID_KEY_PASSWORD`, or the checked-in project's local signing properties. Without a release key, output is explicitly named **debug-signed**, not store-ready. Credentials are not part of this fork. The Android application ID is `sh.paseo.sessions`.

iOS uses `PASEO_IOS_EXPORT_OPTIONS_PLIST` for an exported IPA. Without it, the build emits an explicitly **unsigned device `.app` archive**, not an installable IPA or App Store submission. macOS and Windows signing use the native packaging signing configuration; neither platform has been claimed executed in the local migration evidence.

Optional Firebase configuration must match `sh.paseo.sessions`: `GOOGLE_SERVICES_FILE_SESSIONS` for Android and `GOOGLE_SERVICE_INFO_PLIST_SESSIONS` for iOS. The wrapper stages these files for the build and restores previous state afterward.

## Workflow definitions and publication controls

`.github/workflows/desktop-release.yml` is the **Native Release** workflow despite its retained filename. It builds Flutter/Rust targets, preserves complete per-target artifacts, validates recovery provenance, and publishes only through its publication path. Its matrix definition is not evidence that those targets have run.

Manual inputs include `tag`, `mode` (`build` or `publish-only`), `recovery_run_id`, `platform`, `checkout_ref`, `publish`, and `rollout_hours`. **`publish` defaults to true**; explicitly select false for an authorized non-publishing build. Tag pushes also select publication. Do not push tags or dispatch a publishing workflow as a build test.

`publish-only` consumes preserved artifacts from an exact prior completed canonical workflow run. Recovery requires a complete, unique, unexpired target inventory; one-architecture macOS recovery must not silently replace the other architecture. The publication scripts and update-manifest generator own final artifact integrity and source identity. Desktop rollout defaults to 36 hours; a rollout change is a publication-side operation requiring authorization, not a local check.

`.github/workflows/docker.yml`, `desktop-packages.yml`, and `nix.yml` cover their respective native packaging/deployment definitions. They do not establish a completed release or remove the distribution requirements above.

## Authorization and verification

A release requires an explicit approved source revision, version/channel decision, publication target and authorization. Keep preparation separate from publication. Commands named `release:publish`, `release:push`, `release:patch`, `release:minor`, `release:major`, their beta variants, and promotion commands have external effects; they are not verification shortcuts.

Before a release decision, gather actual Rust/Flutter/SDK checks, target-platform execution, packaging/signing/install/update evidence, and distribution review for the exact revision and binary inventory. `npm run release:check` is a repository check command; it is not a compliance audit, proof of signing, or permission to publish.

Current actual Linux evidence and explicit missing platform checks are in [testing](testing.md). Android ARM64 release-mode compilation passed with debug signing, not production credentials; no Android device execution or Windows/macOS/iOS execution is claimed. Consult the testing record for desktop tray/editor follow-up results rather than treating a build as runtime proof. No release was published, installed, committed, pushed, or offered by the documentation update.

## Release branch discipline

While you finalize a release on `main`, use a temporary `next` branch for work intended for the following
release. This applies to both beta and stable releases.

- Create each new `next` from freshly fetched `origin/main`. Reuse it while active.
- "This goes to next" means create the PR against `next` or retarget an existing
  PR, and keep that destination through delivery.
- Keep `next` current by merging `origin/main` into it as release fixes land.
  Avoid rebasing this shared branch because agents and open PRs depend on its history.
- After the release ships, bring `next` up to date and open a `next` → `main` PR.
  Pass CI and merge without squashing away the individual PR commits needed for
  the changelog. Retarget remaining PRs based on `next` to `main` and delete the integrated
  `next`. Create it fresh when needed again.

**Setup still needed:** PR checks run for every base branch, but CI, Docker, and Nix
push runs target only `main`, and GitHub permits only squash merges. Enable
required-check protection for `next`, CI on its pushes, and merge commits for the
integration PR. Handle PR
base changes (`edited` events) so retargeting runs checks against the new base;
GitHub's default PR events do not cover this. Deployment triggers stay unchanged.

### Hotfix from a release tag

If `main` contains changes you do not want to release, branch from the affected
release tag and cherry-pick only the required fixes. Run CI on that branch, then
use the normal release flow with it as the explicit source, choosing a new patch
or beta version. Ensure the fixes and changelog also reach `main` and any active
`next`, preserving newer development and version changes there. This is a
short-lived hotfix branch, not another maintained release track.
