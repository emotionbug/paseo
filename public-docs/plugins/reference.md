---
title: Plugin reference
description: Backend extension entries, SDK contracts, sources, preparation, and native CLI commands.
nav: Reference
order: 48
category: Plugins
---

# Plugin reference

Start with the [quickstart](/docs/plugins) or [migration guide](/docs/plugins/migration). This reference describes the **unpublished native cutover in the 0.13.0 checkout**.

## Runtime boundary

The daemon, CLI, and built-ins are Rust. Optional third-party JavaScript extensions run in a private backend worker with externally installed **Node.js 20+**. Set `PASEO_PLUGIN_NODE` in the daemon environment to select the executable; otherwise it uses `node`. The optional generated worker asset must be available; `PASEO_PLUGIN_WORKER` can select its location. npm is separately needed for npm acquisition or preparation commands. Reloading installed files needs Node, not npm.

Flutter owns the Linux/macOS/Windows/Android/iOS interface. It does not execute React or React Native plugin code, client bundles, custom screens, panels, timeline renderers, themes, or composer callbacks. Mobile is remote-only. Managed candidate validation and activation require an actual `index.server.ts` or `index.server.tsx` file; directories with those names and client-only plugins are rejected, not reported as empty ready plugins. Historical SDK exports do not imply native UI compatibility.

| Owner                               | Source                                     |
| ----------------------------------- | ------------------------------------------ |
| Rust management, CLI, worker bridge | `packages/daemon/src`                      |
| Private optional backend worker     | `packages/daemon/plugin-worker/src`        |
| Public extension SDK                | `packages/plugin` (`@getpaseo/plugin`)     |
| TypeScript client SDK               | `packages/client` (`@getpaseo/client`)     |
| Shared protocol                     | `packages/protocol` (`@getpaseo/protocol`) |

Repository builds use Node.js 24, `npm ci`, and `npm run build:daemon`; the watch command is `npm run dev:daemon`. This build-tool requirement does not bundle Node into the product or turn the daemon into JavaScript.

## Project files

`paseo plugin init /absolute/path/to/plugin` creates a backend-only project with `paseo-plugin.json`, `package.json`, `tsconfig.json`, `OVERVIEW.md`, `index.server.ts`, `server/greeting.ts`, and `shared/greeting.ts`. It does not install dependencies. Run `npm install` in that project for authoring tools.

### Requirements

Declare a manifest ID and an explicit compatible Paseo semver range:

```json
{
  "id": "review-tools",
  "requirements": { "paseo": ">=0.13.0" }
}
```

The scaffold uses the checkout's version. A version range checks runtime compatibility; it is not evidence that the native migration has shipped or that former UI APIs are supported. An omitted `requirements.paseo` historically means `<0.8.0` and is rejected by current runtimes. Empty or invalid ranges and unknown requirement keys are rejected. Do not broaden the range beyond versions you support.

### Runtime entries

`index.server.ts` (also accepted as `.tsx`) default-exports a contribution function receiving `PluginServerContext`. Supported backend contributions are `handle`, `registerSettings`, `registerProvider`, `registerUsageSource`, `on`, and `before`. Keep Node operations, credentials, process transports, and machine-local resources under `server/`; keep runtime-neutral contracts under `shared/`. Do not import `client/` into backend code.

### Runtime modules

Use the SDK root for `defineRpc`, `defineSettings`, schemas, and plain types. Import server context and lifecycle types from `@getpaseo/plugin/server`, provider contracts from `/server/provider`, ACP adapters from `/server/acp`, and usage contracts/helpers from `/server/usage`. The worker supplies these SDK modules and Zod. Keep host-provided and type-only dependencies in `devDependencies`; declare other runtime libraries as dependencies or bundle them. Backend imports of React and client SDK entry points are not supported. Shared modules cannot import runtime-specific modules or types.

## Entry point and cleanup

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log(event.agent.id, event.outcome);
  });
  return () => {};
}
```

The entry must return a synchronous or asynchronous cleanup function; return `() => {}` when there is nothing to release. The worker rejects an entry that returns anything else. Release timers, watchers, sockets, SDK subscriptions, and child processes you create. Reload, disable, removal, and daemon shutdown tear down the worker and its registrations. Hooks do not require an app connection.

## Lifecycle hooks

| Registration                    | Callback                           | Result                                                    |
| ------------------------------- | ---------------------------------- | --------------------------------------------------------- |
| `server.on(name, callback)`     | `(event, { paseo, signal })`       | `void` or promise                                         |
| `server.before(name, callback)` | `({ request }, { paseo, signal })` | Modified request, or `undefined` to keep it; may be async |

Events include `agent.created`, `agent.closed`, `agent.turn_started`, `agent.turn_ended`, `agent.permission_requested`, `agent.permission_resolved`, `agent.archived`, `workspace.created`, and `workspace.archived`. Public background agents use the same lifecycle; private disposable helpers are excluded. Archive notification is not proof that runtime/worktree cleanup is finished.

`agent.turn_ended` carries `agent`, `turnId`, and `outcome` (`completed`, `failed`, or `canceled`). Its deprecated `timeline` is always empty. Fetch a bounded tail instead:

```ts
server.on("agent.turn_ended", async (event, { paseo, signal }) => {
  if (signal.aborted) return;
  const agent = paseo.agents.ref(event.agent.id);
  const history = await agent.timeline.refetch({ direction: "tail", limit: 20 });
  console.log("Fetched turn history", event.agent.id, history);
});
```

Do not log private history in a production plugin unless it is explicitly intended and disclosed. Respect the hook abort signal and release subscriptions during cleanup.

| Before hook          | Editable request                                                    |
| -------------------- | ------------------------------------------------------------------- |
| `agent.create`       | Public agent configuration except `cwd`, and launch overrides `env` |
| `agent.session_open` | Only `env`                                                          |
| `workspace.create`   | Explicit workspace creation request                                 |

For example, inject a launch override on every session opening:

```ts
server.before("agent.session_open", ({ request }) => ({
  ...request,
  env: { ...request.env, COMPANY_ENV: "development" },
}));
```

The environment map contains launch overrides, not the daemon's inherited environment. Session opening includes create, resume, refresh, and import, and can serve interactive or history work. Use ordinary SDK permission methods to answer requests; do not treat every permission request as a shell command or a completed turn.

## Connected hosts and the Paseo SDK {#connected-hosts}

Handlers receive `{ paseo }`; lifecycle callbacks receive `{ paseo, signal }`. These use the `@getpaseo/client` API, not a separate legacy server SDK. Use it for normal agent, workspace, provider, and configuration operations rather than defining duplicate plugin RPCs.

`context.paseo.hosts` describes the selected daemon's saved host connections. Host identity belongs to the daemon connection; paths and workspace IDs must not be treated as globally interchangeable across hosts. See the [SDK reference](/docs/sdk/reference). This backend API does not provide the former app's React hooks or a way to inject native screens.

## Add plugin-specific backend behavior

Use RPC for vendor APIs, daemon-local resources, or credentials that should stay on the daemon. In `shared/greeting.ts`:

```ts
import { defineRpc } from "@getpaseo/plugin";
import { z } from "zod";

export const greeting = defineRpc({
  name: "greeting.create",
  input: z.object({ name: z.string() }),
  output: z.object({ message: z.string() }),
});
```

In `index.server.ts`:

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";
import { greeting } from "./shared/greeting";

export default function contribute(server: PluginServerContext) {
  server.handle(greeting, ({ name }) => ({ message: `Hello, ${name}` }));
  return () => {};
}
```

Input and output are validated by the contract; `RpcInput` and `RpcOutput` from the SDK root derive its types. Registration creates backend behavior, not a UI or automatic native button. A consumer must use the daemon's plugin RPC protocol. Do not add `useRpc` or a React client entry as a native integration recipe.

## Persisted settings {#settings-screens}

The backend settings API remains supported; executable settings screens do not. Define a document in `shared/`:

```ts
import { defineSettings } from "@getpaseo/plugin";
import { z } from "zod";

export const preferences = defineSettings({
  id: "review",
  scope: "host",
  version: 1,
  schema: z.object({ enabled: z.boolean().default(true) }),
});
```

Register it with `server.registerSettings(preferences)`. The handle exposes `read()` and `subscribe(listener)`; the latter returns cleanup. A read returns `ready` with `values` and opaque `revision`, or `invalid` with `error` and `revision`. Invalid saved data is preserved rather than silently replaced.

`id` matches `[a-z][a-z0-9_-]*` and is unique within the installation. `scope` is `host`, shared by authorized clients of that daemon, not synchronized across hosts. `version` is a positive schema version, distinct from the write revision. Supply defaults so `{}` parses to a complete document. Optional `migrate(values, fromVersion)` may be async and must produce data valid under the current schema. Invalid writes and revision conflicts do not notify subscribers; listener errors do not undo committed writes.

## Providers

Call `server.registerProvider()` with `ProviderRegistration` from `@getpaseo/plugin/server/provider`, or use `runAcpProvider()` from `/server/acp`. A connection accepts input via `send()`, publishes events via `onEvent()`, and releases resources with `close()`. `send()` reports acceptance, not turn completion.

Use `session.prompt` for messages, structured commands, and steering. Repeat `clientMessageId` on the user timeline item and emit one matching `session.prompt_result`. Return model, mode, thinking, capability, configuration, permission, persistence, and timeline data through the provider protocol. Provider-created children use `parentSessionId`. Refresh closes and reopens the session with current configuration and persistence.

Toggle/select configuration descriptors and standard timeline items are data rendered by the native client; they are not executable plugin UI. Keep provider-private options in `ProviderSessionConfig.providerOptions` and validate them. Host tools arrive as MCP servers. See [provider options](/docs/sdk/provider-options) and the complete [direct/ACP provider guide](/docs/plugins/providers).

### SVG rules

`ProviderRegistration.icon` is a file path relative to the plugin directory, not inline SVG or a URL. It must resolve to a regular file inside that directory, including after symlink resolution. Usage-source icons use the same rules. SVG must begin with an `<svg>` document and be at most 64 KiB. Scripts, `foreignObject`, style elements, event-handler attributes, JavaScript URLs, and non-fragment `href` references are rejected. Listing PNG icons have a separate [publication contract](/docs/plugins/publishing#icons-and-screenshots).

## Usage sources

Register with `server.registerUsageSource()`. Import contracts and helpers from `@getpaseo/plugin/server/usage`. A source has `id`, `label`, optional icon path, an input Zod schema, `discover(scope)`, and `fetch(input)`.

- `discover` returns accounts with stable `key`, optional `label` and `harness`, and JSON `input` identifying a credential store, never containing credentials.
- Scope is global or session-specific (`provider`, optional `model`, and launch `env`). Session discovery must resolve only that session's account; return `[]` for unrelated providers or absent logins. Do not scan other agents or default stores as a fallback.
- `fetch` returns `available` with usage windows and optional balances/details, `unavailable` with a typed problem, or `error` with a message. Validate input with the source schema.
- Account keys use 1–128 characters from `[A-Za-z0-9._-]`; never use raw credentials or email. `hashAccountKey()` handles sensitive stable identities. Prefer account identity that survives token rotation.
- Read the store again when fetching. Do not redeem refresh tokens or rewrite another CLI's credential files.
- `unavailable(problem)` requires `expired` (`expiresAt`, optional `refreshedBy`), `rejected` (`status`, optional `refreshedBy`), or `no_quota` (`detail`). `refreshedBy` is a CLI name, not a user-facing instruction.
- Derive period labels from provider data, not response order or reset countdowns. `windowFromReportedDuration()` accepts reported seconds or `null` and a neutral unknown name/identity; `windowFromUsedPct()` is appropriate for provider-named periods. Keep window IDs stable across response reordering and resets.

Built-in usage sources belong to Rust, not this optional worker. The backend registration does not create a custom native usage panel.

## Plugin sources

Pass a source to `paseo plugin install` (`add` is also accepted):

| Source              | Example                                                                        |
| ------------------- | ------------------------------------------------------------------------------ |
| Registry            | `owner/slug` or `registry.example/owner/slug`                                  |
| Host directory      | `/srv/paseo/plugins/review` or `./review`                                      |
| GitHub              | `github:acme/paseo-review` or `git:acme/paseo-review`                          |
| Git URL/SCP         | `git:https://git.example/acme/review.git` or `git@git.example:acme/review.git` |
| npm                 | `npm:@acme/paseo-review@1.2.0`                                                 |
| Source subdirectory | `github:acme/monorepo:plugins/review`                                          |

Bare `owner/slug` is a registry ID, not a local directory or GitHub shorthand. Prefix local relative paths with `./` or `../`. Absolute paths, `~`, Windows drive paths, and UNC paths identify directory sources. The CLI resolves relative paths and `~` locally; with `--host`, supply an absolute path on the daemon host. Files are not uploaded. A `file://` URL selects Git acquisition, not a directory.

npm accepts an exact version, tag, or semver range after `@`, defaulting to `latest`. Quote ranges containing shell operators. Tarball URLs, npm aliases, and npm `file:` specifications are not plugin source identifiers. Use explicit `npm:` for an unscoped package with both a version and subdirectory, avoiding ambiguity with SCP syntax.

A subdirectory suffix must stay inside the acquired source: no empty, `.` or `..` segments; a lone `.` selects the root. URL ports and SCP separators remain part of the source. Prefer `/` separators across hosts. `--ref` selects an initial Git branch/tag/commit; without it, acquisition uses default HEAD. `--path` is also accepted for a source-relative plugin directory.

The default plugin registry is `https://plugins.paseo.sh`. `PASEO_PLUGIN_REGISTRY` selects another default, including a path prefix. Daemon config `pluginRegistries: { "host": { "authorization": "Bearer token" } }` supplies private registry authorization. Restart after changing startup settings. Registry credentials are not sent to artifact hosts or redirects. Existing installs retain their recorded registry and source. Registry records own the reviewed revision and plugin path.

npm runs on the daemon host using that user's registry/auth configuration. Acquisition installs a candidate and production dependencies into managed storage; the installed lock records actual resolution. npm package authors must also ship the [publication lock](/docs/plugins/publishing#2-lock-installed-dependencies) when required. Git acquisition needs Git and access on that host.

`--id` overrides the manifest's installation ID. Duplicate IDs are rejected. Removing a directory installation preserves its source; removing a managed npm/Git installation removes its managed files. Installation grants unsandboxed daemon-user access to code, dependencies, and preparation commands.

## Preparation commands

Ordinary TypeScript extensions need no separate build: the worker compiles them. Use manifest `build` only for required dependency installation or generated source/assets:

```json
{
  "id": "review-tools",
  "requirements": { "paseo": ">=0.13.0" },
  "build": [["npm", "ci", "--omit=dev", "--ignore-scripts"]]
}
```

Each command is a non-empty argv array, executed directly without an implicit shell from the staged plugin directory. Paseo does not infer commands from lockfiles. Installation and update run declared preparation before validation and activation. A failed candidate is discarded; update retains the previous installation. Declared commands remain trusted executable code even though npm lifecycle scripts are suppressed during acquisition. Keep installed dependency trees locked and disclose additional setup; see [publishing](/docs/plugins/publishing#plugins-with-a-build-step).

## CLI reference

```bash
paseo plugin init /absolute/path/to/plugin
paseo plugin install /absolute/path/to/plugin
paseo plugin install npm:@acme/paseo-review@1.2.0 --id review
paseo plugin ls review
paseo plugin logs review
paseo plugin update review --check
paseo plugin update review --yes
paseo plugin update --all --check
paseo plugin update --all --yes
paseo plugin update review --version 1.2.0
paseo plugin update review --ref v2
paseo plugin reload review
paseo plugin disable review
paseo plugin enable review
paseo plugin remove review
```

Put global `--host <url>` before the management command for a remote daemon. `ls` reports installed identity/revision without querying the remote source. Ordinary native CLI updates show the proposal and require `--yes`; there is no interactive approval prompt. `--check` always previews only. Explicit `--version` (npm) or `--ref` (Git) authorizes one plugin target and cannot be combined with `--all` or each other.

Direct npm updates consider newer `latest` versions; direct Git updates consider default HEAD regardless of the initial selector. Explicit npm targets can downgrade. Registry installs use reviewed pins. Directory installs are skipped: edit and reload them. Each bulk result is independent. A changed or unavailable proposal fails rather than silently applying different content.

The root `pluginsEnabled` config and each installation's enabled flag both gate execution. Use `paseo reload --json` after changing the root config; manage installation entries through plugin commands. Typecheck in the plugin project before installing or reloading; load-time compilation is not a typecheck.

## Debug backend output

Use normal `console.log`/`console.error`. `paseo plugin logs <id>` returns a recent snapshot with timestamps, stream, sequence, and message, not a live follower. Logs include load/compile failures and worker output. They can contain private data; never log credentials or access tokens. A status of `running` proves neither provider correctness nor a native UI contribution.

## Load failures

| Symptom                     | Action                                                                                                                                                            |
| --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Worker/Node unavailable     | Install external Node 20+, check daemon environment and `PASEO_PLUGIN_NODE`, and supply the optional worker asset.                                                |
| Legacy or missing entry     | Supply a real `index.server.ts` or `index.server.tsx` file implementing supported backend behavior; client-only packages and entry-path directories are rejected. |
| Backend imports client code | Remove the dependency, not just the compiler error; keep only backend logic.                                                                                      |
| RPC rejects                 | Check both Zod schemas and the handler error.                                                                                                                     |
| Changes not active          | Typecheck, reload, then inspect status and logs.                                                                                                                  |
| Reload/worker failure       | Fix the reported source/runtime error and reload; do not assume the old worker is still active.                                                                   |
| No former plugin screen     | Expected native boundary, not a reason to install React or a browser into the app.                                                                                |
