---
title: Plugin quickstart
description: Build, install, share, and update a trusted backend extension.
nav: Quickstart
order: 44
category: Plugins
---

# Plugin quickstart

An optional JavaScript plugin extends one Paseo daemon with RPC handlers, lifecycle hooks, settings persistence, usage sources, or a coding-agent provider. Built-in behavior is implemented in Rust. Third-party JavaScript runs in a separate backend worker using **externally installed Node.js 20+** on the daemon host, not in the Flutter app.

These instructions describe the native cutover in the **0.13.0 checkout, not a published native release**. The Flutter clients for Linux, macOS, Windows, Android, and iOS do not execute React plugins. Screens, panels, themes, sidebar components, composer callbacks, and custom timeline renderers from the former JavaScript client are not supported native extension points. Mobile clients connect remotely; they do not run the daemon or plugin worker.

## Prepare the daemon host

Install Node.js 20+ separately if you need JavaScript extensions. Make `node` available to the daemon or set `PASEO_PLUGIN_NODE` to its executable in the daemon environment. npm is also needed to acquire npm sources or run npm preparation commands; selecting `PASEO_PLUGIN_NODE` does not install npm. The optional worker asset must be present beside the native runtime's generated assets, or selected with `PASEO_PLUGIN_WORKER`.

For a repository build, use the repository's **Node.js 24** build environment, `npm ci`, and `npm run build:daemon`. This builds the Rust debug executable and adjacent assets. `npm run dev:daemon` is the watch command. Do not use the removed `dev:server` command or install a legacy JavaScript daemon to obtain plugins.

## Create a plugin

On the daemon machine:

```bash
paseo plugin init /absolute/path/to/workspace-plugin
cd /absolute/path/to/workspace-plugin
npm install
```

The Rust CLI creates a backend-only TypeScript project without running the package manager:

```text
workspace-plugin/
  paseo-plugin.json
  package.json
  tsconfig.json
  OVERVIEW.md
  index.server.ts
  server/greeting.ts
  shared/greeting.ts
```

The scaffold registers a greeting RPC. `npm install` installs authoring dependencies; the worker supplies the plugin SDK and Zod at runtime. Keep daemon-local operations in `server/` and plain data and contracts in `shared/`. Do not add a client entry to make a native interface.

For a simple observable backend contribution, replace `index.server.ts` with:

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log("Turn ended", event.agent.id, event.outcome);
  });
  return () => {};
}
```

This hook runs while the plugin is enabled, including when no app is connected. See the [reference](/docs/plugins/reference) for RPC, settings, lifecycle, usage, and SDK contracts, or [build a provider](/docs/plugins/providers) to connect an agent directly or through ACP.

## Install and try it

Plugins are trusted, unsandboxed code. Backend code and preparation commands run with the daemon user's access to files, processes, credentials, and the network. Trust the source, dependencies, and updates before installing.

```bash
npm run typecheck
paseo plugin install /absolute/path/to/workspace-plugin
paseo plugin ls workspace-plugin
paseo plugin logs workspace-plugin
```

The daemon-wide `pluginsEnabled` setting and the installation's own enabled state must both allow execution. After changing the root setting in `config.json`, use `paseo reload --json`; use plugin commands to change installations. `paseo plugin enable workspace-plugin` enables that installation, not the root switch.

Complete an agent turn on the selected daemon to exercise the hook, then inspect its logs. No plugin screen or sidebar item is expected. `running` is backend activation status, not evidence that an old client contribution works.

With a remote daemon, put `--host <url>` before the command and use an absolute path that exists **on that host**. The CLI does not upload local plugin files.

## Edit and reload

```bash
npm run typecheck
paseo plugin reload workspace-plugin
paseo plugin ls workspace-plugin
paseo plugin logs workspace-plugin
```

Loading compiles the backend but does not replace author-side TypeScript checking. Release timers, watchers, sockets, subscriptions, and child processes from your entry's returned cleanup function. Reload restarts the worker; inspect errors before assuming the new code is running.

## Install and update shared plugins

```bash
paseo plugin install npm:@acme/paseo-review@1.2.0
paseo plugin install github:acme/paseo-review
paseo plugin install github:acme/monorepo:plugins/review
paseo plugin update paseo-review --check
paseo plugin update paseo-review --yes
```

Use the plugin's installed ID, which need not equal its npm name. The native CLI displays a proposal and requires `--yes` for an ordinary update; it does not offer an interactive approval prompt. `--check` never applies the update. Explicit `--version` or `--ref` selects and authorizes a target for one plugin. Directory sources are edited in place and reloaded, not updated from a remote.

`npm` is needed for acquisition, not subsequent loading; **Node remains required for backend execution**. Source selectors, registry pins, preparation commands, and update rules are documented in the [reference](/docs/plugins/reference#plugin-sources). Use [publishing](/docs/plugins/publishing) to share a plugin and [migration](/docs/plugins/migration) to remove obsolete client contributions.

## Source ownership

- `packages/daemon/src`: Rust daemon, CLI, plugin management, and worker bridge.
- `packages/daemon/plugin-worker/src`: extracted private JavaScript backend worker.
- `packages/plugin`: public `@getpaseo/plugin` author SDK.
- `packages/client`: `@getpaseo/client` TypeScript SDK used by backend handlers.
- `packages/protocol`: shared protocol contracts.

The private worker is not an application SDK or an executable UI host. Existing SDK client exports and historical examples do not imply native UI support.
