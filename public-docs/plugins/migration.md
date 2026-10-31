---
title: Migrate a plugin to the native backend runtime
description: Retain backend extension behavior and remove executable client contributions for the native cutover.
nav: Migration
order: 47
category: Plugins
---

# Migrate a plugin to the native backend runtime

This guide targets the **unpublished native migration in the 0.13.0 checkout**. It is not a claim that a new native release is available. The old 0.8 migration separated client and server entries; that historical split does not make the client half compatible with Flutter.

Built-ins now belong to Rust. Optional JavaScript backend plugins remain supported through a private worker with externally installed Node.js 20+. Flutter clients do not evaluate plugin JavaScript, React components, or executable UI contributions. Mobile connects to a desktop/server daemon and never hosts the worker.

## 1. Classify the plugin

| Existing behavior                                                                    | Native migration                                                                                          |
| ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------- |
| RPC handlers and daemon-local vendor calls                                           | Retain in the backend worker.                                                                             |
| Lifecycle hooks and SDK automation                                                   | Retain server registration and cleanup.                                                                   |
| Persisted settings schemas and migrations                                            | Retain backend registration; remove executable settings screens.                                          |
| Direct or ACP provider                                                               | Retain backend protocol implementation; emit standard timeline/configuration data.                        |
| Usage discovery and fetching                                                         | Retain backend usage-source registration.                                                                 |
| Screens, sidebar/header items, panels, file renderers                                | Remove executable client contributions; no native plugin equivalent is promised.                          |
| React timeline renderers or transformers                                             | Remove; represent provider output with standard protocol item types.                                      |
| Client commands, composer pills, attachment pickers, themes, browser/audio callbacks | Remove client registrations and imports. Backend data or operations may remain when independently useful. |
| Client-only plugin                                                                   | Not a supported native extension; do not label an empty backend entry a migration.                        |

Do not move a React component into `server/`, bundle a browser, or load a client bundle from the worker. Native Markdown/source editing and other built-in interfaces are product features, not executable extension slots.

## 2. Retain an explicit backend entry

Use this layout:

```text
paseo-plugin.json
package.json
tsconfig.json
index.server.ts
server/
shared/
```

`index.server.ts` default-exports a function receiving `PluginServerContext`. Keep host operations and credentials in `server/`; keep plain values and Zod contracts in `shared/`. Remove the old mixed `index.ts` after moving its supported behavior. Remove `index.client.tsx`, client modules, and UI-only dependencies from the native-targeted package rather than leaving an unsupported entry alongside it.

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log(event.agent.id, event.outcome);
  });
  return () => {};
}
```

For older mixed APIs, replace daemon handler registration with `server.handle(contract, handler)`. Register hooks with `server.on`/`server.before`, settings with `server.registerSettings`, providers with `server.registerProvider`, and usage sources with `server.registerUsageSource`. There is no replacement registration for an executable client surface.

Keep real cleanup for timers, subscriptions, sockets, watchers, and child processes. The worker may stop when the plugin is reloaded, disabled, removed, or the daemon shuts down. Agent hooks operate even when no app is connected.

## 3. Correct imports and SDK ownership

| Need                                                   | Import/source                                      |
| ------------------------------------------------------ | -------------------------------------------------- |
| `defineRpc`, `defineSettings`, plain types and schemas | `@getpaseo/plugin`                                 |
| Server context, lifecycle and process helpers          | `@getpaseo/plugin/server`                          |
| Provider contracts                                     | `@getpaseo/plugin/server/provider`                 |
| ACP adapter                                            | `@getpaseo/plugin/server/acp`                      |
| Usage contracts/helpers                                | `@getpaseo/plugin/server/usage`                    |
| TypeScript daemon API                                  | `@getpaseo/client`, owned by `packages/client`     |
| Shared protocol types                                  | `@getpaseo/protocol`, owned by `packages/protocol` |

The extension SDK is `packages/plugin`. The private worker is `packages/daemon/plugin-worker/src`; native management and CLI code are in `packages/daemon/src`. Do not import from removed server/app/desktop/CLI trees or copy the private worker into a public plugin.

Remove `/client`, `/client/ui`, React, React Native, and UI hook imports from the migrated runtime. Historical compatibility aliases or dates in old SDK documentation do not extend support to Flutter. Keep genuine Node libraries and type-only dependencies as needed; this is a backend-only cutover, not a ban on JavaScript.

## 4. Preserve data contracts, not custom rendering

Keep RPC input/output Zod schemas and server handlers. RPC registration does not generate a native button or panel; consumers use the daemon's plugin RPC protocol. Use the SDK supplied to handlers and lifecycle hooks for ordinary Paseo operations.

Keep `defineSettings` schemas, defaults, versioned migrations, and `registerSettings` read/subscription behavior. Removing the former React settings screen does not require deleting stored values or inventing a new persistence format.

For providers, preserve capability negotiation, prompt receipts, permissions, persistence, cancellation, and replay. Emit standard user/assistant text, reasoning, tool, todo, error, notification, and compaction items. Do not require a custom React renderer for an essential result. Provider toggle/select configuration is protocol data rendered by the native client, not executable plugin UI. See [provider migration details](/docs/plugins/providers).

## 5. Prepare the actual daemon host

Install Node.js 20+ separately and expose `node` to the daemon or configure `PASEO_PLUGIN_NODE`. Supply the optional worker asset, using `PASEO_PLUGIN_WORKER` when necessary. npm is required separately for npm acquisition or declared npm preparation; Git acquisition needs Git. These tools and permissions belong to the daemon host, including when the user connects remotely.

For a source checkout, use Node.js 24, `npm ci`, and `npm run build:daemon` to create the Rust debug executable and adjacent assets. `npm run dev:daemon` is the watch command; `dev:server` is removed. `build:daemon:release` produces optimized local assets, not release provenance.

## 6. Update package and publication metadata

Update `package.json`'s `files` list to include the manifest, server entry, `server/`, `shared/`, imported assets, and listing files. Remove client entries and client-only assets. Keep host-provided libraries and type-only dependencies in `devDependencies`; lock installed runtime dependency trees as described in [publishing](/docs/plugins/publishing).

Declare an explicit `requirements.paseo` range for versions you support. The native checkout currently reports `0.13.0`; changing the range alone cannot distinguish or migrate an old UI plugin. Describe backend-only support and external Node requirements in your README and listing, and do not present the unpublished cutover as a released version. Preserve historical release descriptions as historical rather than silently rewriting them.

## 7. Exercise the migrated behavior

On a suitable test daemon, after the source changes:

```bash
npm run typecheck
paseo plugin reload <plugin-id>
paseo plugin ls <plugin-id>
paseo plugin logs <plugin-id>
```

For a new installation, use `paseo plugin install /absolute/path/to/plugin` first. The daemon-wide `pluginsEnabled` and installation's enabled state must both permit loading. Exercise every retained RPC, hook, provider, settings migration, and usage source; confirm cleanup while work is active. `running` alone does not establish behavior or native UI compatibility. Test on the host platforms you claim to support; a CI definition or historical JavaScript-client test is not evidence of an actual native run.

No UI compatibility shim is part of this migration. If the only useful behavior was a client surface, the plugin cannot currently provide that feature to Flutter; state that limitation rather than publishing a no-op backend.
