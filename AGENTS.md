# AGENTS.md

Paseo is an environment for running, monitoring and controlling your local AI coding agents from anywhere. A Rust daemon runs on your desktop or server next to your code; native Flutter clients on Linux, macOS, Windows, Android and iOS connect to it directly or through the end-to-end encrypted relay. Your code stays on your machine.

**Supported agents:** Claude Code, Codex, GitHub Copilot, OpenCode, Pi, Antigravity, and Muse Code.

## Repository map

Rust and Flutter own the product. An npm workspace holds the TypeScript SDKs, relay, extension worker, tooling and website.

- `packages/daemon` — Rust daemon and `paseo` CLI: agent lifecycle, WebSocket API, MCP server, providers, desktop host IPC
- `packages/daemon/plugin-worker` — External-Node worker for optional JavaScript backend extensions
- `packages/flutter_app` — Native client for Linux, macOS, Windows, Android and iOS
- `packages/flutter_audio` — Native audio plugin for the Flutter client
- `packages/flutter_desktop_host` — Desktop packaging, desktop integrations plugin, updater, vendored `desktop_multi_window`
- `packages/protocol` — Wire schemas and types (TypeScript)
- `packages/client` — External TypeScript SDK (`@getpaseo/client`)
- `packages/plugin` — Backend extension author SDK (`@getpaseo/plugin`)
- `packages/relay` — E2E encrypted relay for remote access
- `packages/highlight` — Syntax highlighting package (`@getpaseo/highlight`)
- `packages/website` — Marketing site (paseo.sh)
- `plugins/` — First-party backend extensions (usage sources, providers); `plugin-examples/` — SDK examples

The native client is a port of the original React Native/Expo and Electron app at commit `c3bac24ccc2df8dcc67d807c805db0efa0817dff`. Behavior, copy and geometry follow that app; Dart doc comments name the original file a widget ports. Read it with `git show c3bac24:packages/app/src/<path>`.

## Docs

`docs/` is the source of truth for system-level and process-level knowledge. **"The docs", "check the docs", or "check the X docs" always mean this directory — not the web.** Look here before fetching anything online; the docs capture gotchas and conventions you cannot derive from the code or external sources.

At the start of non-trivial work, list `docs/` and skim anything relevant to the task.

| Doc                                                                  | What's in it                                                                                                      |
| -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| [docs/product.md](docs/product.md)                                   | What Paseo is, who it's for, where it's going                                                                     |
| [docs/architecture.md](docs/architecture.md)                         | Rust daemon and Flutter client design, package layering, WebSocket protocol, data flow                            |
| [docs/agent-lifecycle.md](docs/agent-lifecycle.md)                   | Agent states, parent/child relationships, archive semantics, tabs vs archive, subagents track                     |
| [docs/timeline-sync.md](docs/timeline-sync.md)                       | Live stream vs authoritative history, gap recovery, resume, projected pages vs live presentation                  |
| [docs/data-model.md](docs/data-model.md)                             | File-based JSON persistence: agents, config, schedules, project/workspace registries, Flutter client stores       |
| [docs/glossary.md](docs/glossary.md)                                 | Authoritative terminology — UI label wins, no synonyms                                                            |
| [docs/coding-standards.md](docs/coding-standards.md)                 | Commit to a shape, types, errors, density, module structure, native runtime and website rules                     |
| [docs/design.md](docs/design.md)                                     | Native design — theme ownership, reuse, hierarchy, density, states, what's forbidden                              |
| [docs/forms.md](docs/forms.md)                                       | Native forms — settings, schedule, host and creation widgets, state and load gating                               |
| [docs/hover.md](docs/hover.md)                                       | Hover and input affordances — shared hit regions for rows and their actions                                       |
| [docs/unistyles.md](docs/unistyles.md)                               | Native styling — Flutter theme ownership; the filename is kept for links                                          |
| [docs/floating-panels.md](docs/floating-panels.md)                   | Anchored menus, suggestions, tooltips and dialogs as native Flutter surfaces                                      |
| [docs/menus.md](docs/menus.md)                                       | Native menus — trigger ownership, context menu and long-press alignment                                           |
| [docs/expo-router.md](docs/expo-router.md)                           | Native navigation — shell, app store and startup restore ownership; the filename is kept for links                |
| [docs/mobile-panels.md](docs/mobile-panels.md)                       | Compact panels — sidebar → session → file view flow and back behavior                                             |
| [docs/explorer-sidebar.md](docs/explorer-sidebar.md)                 | Explorer and supporting panes — file browsing, source/Markdown views and Changes next to sessions                 |
| [docs/notifications.md](docs/notifications.md)                       | Notification identity and delivery, mobile and desktop integration                                                |
| [docs/i18n.md](docs/i18n.md)                                         | Client localization — catalogs, locale list, loader behavior                                                      |
| [docs/file-icons.md](docs/file-icons.md)                             | File and project icons — Flutter presentation, daemon-side project icon discovery                                 |
| [docs/markdown-renderer.md](docs/markdown-renderer.md)               | Native Markdown rendering for chat and file preview                                                               |
| [docs/native-text-selection.md](docs/native-text-selection.md)       | Selectable Markdown and text, selection while scrolling and resizing                                              |
| [docs/providers.md](docs/providers.md)                               | Adding a native agent provider end-to-end                                                                         |
| [docs/forge-providers.md](docs/forge-providers.md)                   | Adding a git forge: native daemon boundary, Flutter Git presentation                                              |
| [docs/custom-providers.md](docs/custom-providers.md)                 | Custom provider config: Z.AI, Alibaba/Qwen, ACP agents, profiles, custom binaries                                 |
| [docs/plugins.md](docs/plugins.md)                                   | Backend extensions — Rust built-ins, the optional JavaScript worker, manifests and RPCs                           |
| [docs/hub.md](docs/hub.md)                                           | Paseo Hub — opt-in connection, authority, session grants, revocation                                              |
| [docs/website-i18n.md](docs/website-i18n.md)                         | Website languages: locale URLs, translated Markdown pairs, `{#id}` anchors, Chinese style                         |
| [docs/service-proxy.md](docs/service-proxy.md)                       | Service proxy: exposing workspace scripts at public URLs, DNS setup, reverse proxy config                         |
| [docs/relay-endpoint-security.md](docs/relay-endpoint-security.md)   | Relay E2EE modes: Noise NK mapping, record framing, negotiation, the `requireProtectedSecurity` flag, close codes |
| [docs/development.md](docs/development.md)                           | Toolchains, dev commands, isolated state and endpoints, native builds, source ownership                           |
| [docs/rpc-namespacing.md](docs/rpc-namespacing.md)                   | WebSocket RPC naming convention — dotted namespaces and `.request`/`.response` pairs                              |
| [docs/protocol-compatibility.md](docs/protocol-compatibility.md)     | Why app/daemon versions drift, protocol vs feature contract, capability gating, COMPAT tagging                    |
| [docs/protocol-validation.md](docs/protocol-validation.md)           | TypeScript SDK inbound validation (zod-aot), schema-purity rules, native validation boundary                      |
| [docs/connected-hosts.md](docs/connected-hosts.md)                   | Daemon-owned host registrations, routed SDK scopes, identity and authority boundaries                             |
| [docs/permissions.md](docs/permissions.md)                           | Semantic daemon permissions, principals, credentials, pairing invitations, and Hub authority                      |
| [docs/terminal-activity.md](docs/terminal-activity.md)               | Workspace setup and service scripts as daemon-owned PTYs with read-only client logs                               |
| [docs/terminal-performance.md](docs/terminal-performance.md)         | Script and setup log pipeline performance, coalescing/backpressure invariants                                     |
| [docs/agent-stream-performance.md](docs/agent-stream-performance.md) | Assistant text pipeline from the Rust agent owner to the Flutter timeline, paced reveal                           |
| [docs/file-observation.md](docs/file-observation.md)                 | Daemon file and checkout observation, Linux constraints, teardown invariants                                      |
| [docs/testing.md](docs/testing.md)                                   | Rust, Flutter and SDK checks, required native smoke boundaries, Linux desktop regression                          |
| [docs/qa.md](docs/qa.md)                                             | QA evidence bar for pull requests — platform matrix, version drift, performance, UI proof                         |
| [docs/mobile-testing.md](docs/mobile-testing.md)                     | Flutter Android/iOS testing against a separate Rust daemon                                                        |
| [docs/ad-hoc-daemon-testing.md](docs/ad-hoc-daemon-testing.md)       | Running the real Rust daemon against a disposable home and endpoint                                               |
| [docs/browser-capture-harness.md](docs/browser-capture-harness.md)   | Native capture boundary — capturing the real Flutter window; the filename is kept for links                       |
| [docs/android.md](docs/android.md)                                   | The Flutter Android client: build and launch, signing, versions, F-Droid source builds and store metadata         |
| [docs/docker.md](docs/docker.md)                                     | Running the native daemon in Docker, volumes, agent images, security                                              |
| [docs/release.md](docs/release.md)                                   | Release playbook, distribution requirements, draft releases, completion checklist                                 |
| [SECURITY.md](SECURITY.md)                                           | Relay threat model, E2E encryption, DNS rebinding, agent auth                                                     |
| [public-docs/hub/security.md](public-docs/hub/security.md)           | Public Hub guide — trust boundaries, untrusted triggers, provider controls, and output authority                  |

### Writing docs

- **Integrate, don't append.** Find the doc that owns the subject and rewrite the part that is now wrong. The standard failure is finishing a task and adding a paragraph to the bottom of the closest-looking doc; ten tasks later the doc is a pile of paragraphs in discovery order. `docs/custom-providers.md` is what that looks like.
- **Don't document logic.** Prose that restates code drifts from the code and loses. Write down what the code can't tell you: why something is shaped the way it is, the gotcha that cost an afternoon, conventions nothing enforces, constraints that span packages or versions. If a reader could get it in two minutes by opening the file, cut it.
- **One fact, one doc.** Every other mention is a link. If you are about to write the same paragraph in two docs, one of them is a link.
- **Respect the layers.** `CONTRIBUTING.md` and this file name things and link out. Activity docs like `docs/qa.md` and `docs/testing.md` set the bar for a kind of work. Subject docs like `docs/timeline-sync.md` own one thing completely. A layer never re-explains the one below it.
- **One subject per doc.** If the subject doesn't fit in a sentence, split the doc. A section per provider, vendor, or platform is a table plus one worked example.
- **Delete.** Obsolete sections go. Prefer a `packages/daemon/src/thing.rs:120` reference over a pasted block.
- **New doc?** Add a row to the table above and link it from the docs that should send readers there.
- Code-level facts belong in comments next to the code, not here.

### Doc voice

Plain and short. Second person. State the rule, then the reason when the reason isn't obvious. Match the doc you're editing.

Do not:

- Write a sentence to land a point. "It's not X, it's Y", "That's not a Z, that's a W", and every other setup-and-punchline shape.
- Add a clause that only asserts importance: "and that matters", "which is what keeps it working", "this is critical".
- Use "honest", "robust", "seamless", "powerful", "simply", "just", "delightful".
- Restate something you already said, in different words, for emphasis.
- Hedge with "generally", "typically", or "you may want to" when the answer is "do this".
- Clear your throat: "It's worth noting that", "In order to", "This section covers".

## Quick start

```bash
npm run dev                          # Start the Rust dev daemon
npm run dev:app                      # Run the desktop Flutter client against the dev daemon
npm run dev:android -- --device <id> # Run the Android client on a device
npm run cli -- ls -a -g              # List all agents
npm run cli -- daemon status         # Check daemon status
npm run typecheck                    # cargo check, TypeScript workspaces, flutter analyze
npm run lint                         # oxlint (JS/TS)
npm run format                       # oxfmt (JS/TS/JSON/Markdown/YAML/CSS)
```

Repo dev commands use checkout-local state. `PASEO_HOME` resolves to `.dev/paseo-home`, the dev daemon listens on `127.0.0.1:6768`, and `npm run cli -- ...` targets that same dev home. The installed daemon and app keep using `~/.paseo` on port `6767`.

When Mo requests an APK, use `npm run build:apk:arm64` for an ARM64 build: it writes the APK to `artifacts/flutter/` (debug-signed without a release key) without starting a device; `npm run build:apk` builds a universal APK.

See [docs/development.md](docs/development.md) for toolchains, isolated state, native builds and source ownership.

## Release branches

When the user says "this goes to next", create or
retarget the PR to `next` and preserve that destination through delivery. Follow
[release branch discipline](docs/release.md#release-branch-discipline) for creating
and updating `next`, integrating it after a release, and releasing a hotfix from a tag.

## Critical rules

- **NEVER restart the main Paseo daemon on port 6767 without permission** — it manages all running agents. If you're an agent, restarting it kills your own process.
- **NEVER assume a timeout means the service needs restarting** — timeouts can be transient.
- **NEVER add auth checks to tests** — agent providers handle their own auth.
- **Before changing navigation, startup routing, remembered workspace restore, or active workspace selection, read [docs/expo-router.md](docs/expo-router.md).**
- **NEVER run the full test suites locally.** `cargo test`, `flutter test` and the Node suites are heavy and will freeze the machine, especially if multiple agents run them in parallel. Rules:
  - Run only the tests for what you changed: `cargo test --locked -p paseo-daemon --lib <test-name-filter>`, `npm run test:flutter -- test/<file>_test.dart`, or `npx vitest run <file> --bail=1` in a TypeScript workspace.
  - Never run `npm run test` or a whole suite unless explicitly asked, and never run two heavy suites at once.
  - If you must run a broad suite, pipe output to a file and read it afterward: `npm run test:flutter > /tmp/test-output.txt 2>&1` then read the file.
  - Never re-run a test suite that another agent already ran and reported green — trust the result.
  - For full suite verification, push to CI and check GitHub Actions instead.
- Add tests to existing suites and reuse their npm scripts and CI jobs instead of creating feature-specific ones.
- **Always run typecheck and lint for what you changed.** Rust: `npm run typecheck:daemon`. Dart: `npm run typecheck:flutter`. TypeScript: `npm run typecheck:node` and `npm run lint`.
- **Build workspace packages before diagnosing cross-package TypeScript type errors.** The TypeScript workspaces consume generated declarations from each other. If typecheck fails in a package that depends on another workspace, run `npm run build:client` to rebuild protocol and client declarations first. Do not patch inferred callback parameters or add local duplicate types just to silence stale declaration errors.
- **Run `npm run format` before committing.** `oxfmt` formats JS/TS/JSON/Markdown/YAML/CSS. Do not manually fix formatting — let the formatter handle it. Rust and Dart have no repository formatter; match the surrounding file.
- **Always use npm scripts for linting and formatting.** Do not run tools directly with `npx oxfmt`, `npx oxlint`, or package-local binaries. For targeted checks, pass file paths through the npm script:
  - `npm run lint -- packages/client/src/daemon-client.ts`
  - `npm run format:files -- AGENTS.md packages/client/src/daemon-client.ts`
- **The protocol stays backward-compatible. Features don't have to.** Read [docs/protocol-compatibility.md](docs/protocol-compatibility.md) before touching the wire protocol in `packages/protocol` or the daemon's message handling. The short version:
  - **Protocol contract (always):** an old client parses messages from a new daemon, and a new daemon parses messages from an old client. New fields are optional; never narrow, never remove, never require. Wire schemas stay pure — no `.transform()`, `.catch()`, or `.preprocess()`.
  - **Feature contract (per-feature):** gate the capability once on `server_info.features.*`, then run the feature or tell the user to update the host. No fallback paths, no defensive branches.
  - **Every shim is tagged.** `// COMPAT(name): added in vX, remove after <date>` at the site that has to be deleted. `rg "COMPAT\("` is the cleanup backlog; untagged back-compat is permanent by accident.
  - **New RPCs use dotted namespaces with direction suffixes.** Follow [docs/rpc-namespacing.md](docs/rpc-namespacing.md): `domain.provider.operation.request` pairs with `domain.provider.operation.response`. Existing flat RPC names will migrate over time; don't add new ones.

## Platforms

The Flutter client targets Linux, macOS and Windows desktops and Android and iOS. Desktop can manage a local daemon through the desktop host; Android and iOS are remote clients and never run a daemon. Code is cross-platform by default. Gate only when you must.

| I need to...                                     | Use                                                                                                        |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| Switch layout between phone and tablet/desktop   | `isCompactLayout(context)` from `packages/flutter_app/lib/ui/common.dart` (width below 600)                |
| Branch on the operating system                   | `defaultTargetPlatform` from `package:flutter/foundation.dart`                                             |
| Use a desktop or mobile platform service         | The integration under `packages/flutter_app/lib/platform/desktop` or `lib/platform/mobile`                 |
| Show something on hover, always visible on touch | Hover on desktop targets; keep the control visible on Android and iOS (see [docs/hover.md](docs/hover.md)) |

- **Keep the original's platform branches.** The original app's native (iOS/Android) behavior maps to Android and iOS; its web and Electron behavior maps to the desktop targets. A branch that differs from the original is a bug, not a simplification.
- **Don't use the platform as a proxy for layout.** A narrow desktop window is compact; use `isCompactLayout`, not `defaultTargetPlatform`.
- **The native client excludes** the interactive terminal UI (daemon PTYs and read-only script/setup logs stay), the web client, embedded browser and Electron, executable React UI plugins, and microphone dictation and voice input (audio output stays). Don't reintroduce them.

## Debugging

Find the complete daemon logs and traces in `$PASEO_HOME/daemon.log`.
