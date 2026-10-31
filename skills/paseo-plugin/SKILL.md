---
name: paseo-plugin
description: Build and manage trusted Paseo backend plugins. Use when the user asks to create, edit, install, reload, enable, disable, remove, update, publish, or troubleshoot a Paseo plugin; add plugin RPCs; add lifecycle hooks or before hooks; transform agent configuration, environment, MCP servers, or workspace creation; automate permissions or turn follow-ups; add a coding-agent provider or ACP adapter; add a usage source; persist plugin settings; append plugin rows to an agent timeline; or asks for a plugin screen, panel, sidebar item, composer pill, slash command, timeline renderer, or theme (the native client does not run those).
---

# Paseo plugins

Build or manage the requested plugin directly.

**User's request:** $ARGUMENTS

A Paseo plugin is a **backend extension**. The daemon runs its `index.server.ts` in a separate
worker process using Node.js 20+ installed on the daemon host. Backend code can register RPC
handlers, lifecycle hooks, persisted settings, coding-agent providers, and usage sources, and it
can append rows to agent timelines.

The native Paseo client (Linux, macOS, Windows, Android, iOS) does not execute plugin client code.
Screens, workspace and file panels, sidebar items, Command Center items, header buttons, composer
pills and attachment sources, client slash commands, timeline transformers and renderers, settings
screens, and themes are not run. The SDK still exports their historical types; that does not make
them work. If the user asks for one, say so plainly, offer the backend part that does work, and
point to the [migration guide](https://paseo.sh/docs/plugins/migration.md). Do not write
`index.client.tsx`, React, or React Native code for a native target.

## Check current documentation

In the Paseo repository, these files describe the checkout's API, including unreleased changes:

- `public-docs/plugins/index.md`: quickstart.
- `public-docs/plugins/reference.md`: entries, hooks, RPC, settings, providers, usage, sources, CLI.
- `public-docs/plugins/providers.md`: direct and ACP providers.
- `public-docs/plugins/publishing.md`: listing, npm, Git, registry.
- `public-docs/plugins/migration.md`: removing client contributions.
- `docs/plugins.md`: maintainer guidance.

The sources of truth are `packages/plugin/src/server` (author SDK),
`packages/daemon/plugin-worker/src` (what the worker loads and calls), and
`packages/daemon/src/plugins*` (what the daemon accepts).

Outside the repository, fetch [https://paseo.sh/llms.txt](https://paseo.sh/llms.txt) and the plugin
pages it lists. A page that tells you to register screens, panels, renderers, or themes describes
the former JavaScript client; follow this skill instead. Do not send the user away to read docs
instead of doing the work.

## What a plugin can contribute

| Contribution     | Registration                                           | Use it when                                                                                                     |
| ---------------- | ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| Plugin RPC       | `defineRpc` + `server.handle`                          | Daemon-side work that is not a normal Paseo operation: vendor APIs, credentials, local files                    |
| Lifecycle events | `server.on`                                            | Observe agents and workspaces, inspect ended turns, answer permission requests, send follow-ups                 |
| Before hooks     | `server.before`                                        | Change agent configuration, provider options, MCP servers, launch environment, or workspace creation beforehand |
| Settings         | `defineSettings` + `server.registerSettings`           | Versioned, host-scoped values the plugin reads and watches                                                      |
| Timeline row     | `paseo.agents.ref(id).timeline.append(...)`            | Push a plugin-owned row into an agent timeline and update it later                                              |
| Provider         | `server.registerProvider` (direct or `runAcpProvider`) | Add a coding agent to Paseo                                                                                     |
| Usage source     | `server.registerUsageSource`                           | Report plan and quota usage for a provider account                                                              |
| Paseo SDK        | `{ paseo }` in handlers and hooks                      | Normal Paseo operations: workspaces, agents, providers, config                                                  |

| Task                                                                | Example                                                                                                            |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Log every lifecycle event and before hook                           | [lifecycle-logger](https://github.com/getpaseo/paseo/tree/main/plugin-examples/lifecycle-logger)                   |
| Follow-ups, permissions, environment, provider switching, worktrees | [lifecycle-actions](https://github.com/getpaseo/paseo/tree/main/plugin-examples/lifecycle-actions)                 |
| Inject MCP servers and change Codex sandbox/approval options        | [agent-configuration](https://github.com/getpaseo/paseo/tree/main/plugin-examples/agent-configuration)             |
| Direct provider                                                     | [provider-direct](https://github.com/getpaseo/paseo/tree/main/plugin-examples/provider-direct) (server entry only) |
| ACP provider with a vendor transformer                              | [provider-acp-transformer](https://github.com/getpaseo/paseo/tree/main/plugin-examples/provider-acp-transformer)   |

Other directories in `plugin-examples/` are client UI examples that only exercise the SDK's client
types. Do not copy them into a native-targeted plugin.

## Create the project

Use an absolute path on the daemon machine. `init` writes files into an empty directory and does
not install packages.

```bash
paseo plugin init /absolute/path/to/my-plugin
cd /absolute/path/to/my-plugin
npm install
```

The generated project contains:

```text
my-plugin/
  paseo-plugin.json
  OVERVIEW.md
  package.json
  tsconfig.json
  index.server.ts
  server/greeting.ts
  shared/greeting.ts
```

`package.json` has a `typecheck` script and lists `@getpaseo/plugin`, `typescript`, and `zod` as
dev dependencies. The worker supplies the plugin SDK and Zod at runtime.

### Manifest

`paseo-plugin.json` supplies the default install ID and supported Paseo versions:

```json
{ "id": "my-plugin", "requirements": { "paseo": ">=0.13.0" } }
```

The ID matches `[a-z][a-z0-9._-]*`. Keep `requirements.paseo` correct whenever you create or edit a
plugin. `init` writes `>=` followed by the CLI version. Use an npm semver range; raise the minimum
when you adopt newer APIs and add an upper bound when a later release is incompatible. A missing
range means `<0.8.0` and current daemons reject it. `requirements` accepts only the `paseo` key.

Other manifest fields: `name`, `description`, `icon` (a PNG for the listing), `media` (listing
images and videos), and `build` (preparation commands, see [Typecheck and manage](#typecheck-and-manage)).

### Entry and module layout

| Path                            | Runtime                                    |
| ------------------------------- | ------------------------------------------ |
| `index.server.ts` and `server/` | Backend worker                             |
| `shared/`                       | Contracts and plain values used by backend |

The daemon requires a real `index.server.ts` or `index.server.tsx` file. A plugin without one is
rejected. Do not put other code modules in the plugin root; the worker's compiler fails on a module
outside `server/` or `shared/`. Shared modules contain Zod contracts and plain values; they cannot
import Node built-ins, `@getpaseo/plugin/server*`, React, or `@getpaseo/plugin/client*`. Backend
code cannot import `client/`, React, React Native, or `@getpaseo/plugin/client*`.

Imports:

| Need                                                    | Module                             |
| ------------------------------------------------------- | ---------------------------------- |
| `defineRpc`, `defineSettings`, `RpcInput`, `RpcOutput`  | `@getpaseo/plugin`                 |
| `PluginServerContext`, lifecycle types, process helpers | `@getpaseo/plugin/server`          |
| `ProviderRegistration`, `negotiateProviderCapabilities` | `@getpaseo/plugin/server/provider` |
| `runAcpProvider`, `AcpTransformer`                      | `@getpaseo/plugin/server/acp`      |
| Usage types and helpers                                 | `@getpaseo/plugin/server/usage`    |

Keep host-provided and type-only packages in `devDependencies`. Declare other runtime libraries in
`dependencies` (and install them, or use a `build` command) or bundle them under `server/`.

### Return cleanup

Default-export one function from `index.server.ts` and **always return a cleanup function**. The
worker rejects an entry that returns anything else with
`Plugin contribution must return a cleanup function`. Return `() => {}` when there is nothing to
release.

```ts
// index.server.ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  // Register handlers, hooks, settings, providers, and usage sources here.
  return () => {};
}
```

Cleanup can be async. Use it for timers, watchers, sockets, subscriptions, and child processes your
code creates. Reload, disable, removal, and daemon shutdown run cleanup, close provider connections,
and stop the worker.

## Complete backend entry

One file that registers settings, an RPC that appends a timeline row, a lifecycle event, and a
before hook. Move contracts into `shared/` and helpers into `server/` as the plugin grows.

```ts
// index.server.ts
import { defineRpc, defineSettings } from "@getpaseo/plugin";
import type { PluginServerContext } from "@getpaseo/plugin/server";
import { z } from "zod";

const preferences = defineSettings({
  id: "review",
  scope: "host",
  version: 1,
  schema: z.object({ enabled: z.boolean().default(true) }),
});

const publishReview = defineRpc({
  name: "review.publish",
  input: z.object({ agentId: z.string(), verdict: z.enum(["ship", "hold"]) }),
  output: z.object({ seq: z.number() }),
});

export default function contribute(server: PluginServerContext) {
  const settings = server.registerSettings(preferences);

  server.handle(publishReview, async ({ agentId, verdict }, { paseo }) => {
    const { seq } = await paseo.agents.ref(agentId).timeline.append({
      type: "plugin",
      id: "review",
      kind: "review-result",
      version: 1,
      data: { verdict },
    });
    return { seq };
  });

  server.on("agent.turn_ended", async (event, { paseo, signal }) => {
    const state = await settings.read();
    if (signal.aborted || state.status !== "ready" || !state.values.enabled) return;
    if (event.outcome.kind !== "completed") return;
    const agent = paseo.agents.ref(event.agent.id);
    const page = await agent.timeline.refetch({ direction: "tail", limit: 20 });
    console.log("Turn completed", event.agent.id, page.entries.length);
  });

  server.before("agent.session_open", ({ request }) => ({
    ...request,
    env: { ...request.env, REVIEW_PLUGIN: "1" },
  }));

  const stopWatching = settings.subscribe((state) => {
    console.log("Review settings changed", state.status);
  });
  return stopWatching;
}
```

## Add an RPC

Use an RPC for daemon-side work that is not a normal Paseo operation. Define the contract in
`shared/` and register the handler in `index.server.ts`:

```ts
// shared/greeting.ts
import { defineRpc } from "@getpaseo/plugin";
import { z } from "zod";

export const greetingRpc = defineRpc({
  name: "greeting.create",
  input: z.object({ name: z.string() }),
  output: z.object({ message: z.string() }),
});
```

```ts
// server/greeting.ts
import type { RpcInput } from "@getpaseo/plugin";
import { greetingRpc } from "../shared/greeting";

export function createGreeting({ name }: RpcInput<typeof greetingRpc>) {
  return { message: `Hello, ${name}!` };
}
```

```ts
// index.server.ts
import type { PluginServerContext } from "@getpaseo/plugin/server";
import { createGreeting } from "./server/greeting";
import { greetingRpc } from "./shared/greeting";

export default function contribute(server: PluginServerContext) {
  server.handle(greetingRpc, createGreeting);
  return () => {};
}
```

- Method names match `[a-z][a-z0-9._-]*` and must be unique in the plugin. `registerSettings`
  reserves `settings.<id>.read`, `settings.<id>.write`, and `settings.<id>.reset`.
- The worker validates the input before the handler runs and the output before it answers. A
  schema failure or a thrown error rejects the call with the error message.
- The handler's second argument is `{ paseo }`.
- Callers use the daemon's `plugin.rpc.invoke.request` message with `pluginId`, `method`, and
  `input`. Registering an RPC does not add a button or command to the native client, and the
  native client does not call plugin RPCs on its own. Do not add `useRpc` or a client entry.

Keep credentials, filesystem access, shell commands, and vendor API calls in backend code. Use
normal Paseo SDK calls instead of an RPC when one already exists.

## Lifecycle hooks

| Registration                    | Callback                           | Result                                                    |
| ------------------------------- | ---------------------------------- | --------------------------------------------------------- |
| `server.on(name, callback)`     | `(event, { paseo, signal })`       | `void` or a promise                                       |
| `server.before(name, callback)` | `({ request }, { paseo, signal })` | Modified request, or `undefined` to keep it; may be async |

Both return a function that removes the registration.

Events: `agent.created`, `agent.closed`, `agent.turn_started`, `agent.turn_ended`,
`agent.permission_requested`, `agent.permission_resolved`, `agent.archived`, `workspace.created`,
and `workspace.archived`.

- `agent.turn_ended` carries `agent`, `turnId`, and `outcome` (`{ kind: "completed" }`,
  `{ kind: "failed", error }`, or `{ kind: "canceled", reason }`). Its `timeline` field is
  deprecated and always empty; fetch history with
  `paseo.agents.ref(event.agent.id).timeline.refetch({ direction: "tail", limit: 20 })`.
- Answer a permission with
  `paseo.agents.ref(id).respondToPermission({ requestId, response: { behavior: "allow" } })` or
  `{ behavior: "deny", message }`. Send a follow-up with `paseo.agents.ref(id).send(text)`.
- An event callback that throws is logged to the plugin log and does not affect the operation.
- Hooks run while the plugin is enabled, including when no app is connected. Archive notification
  does not mean runtime or worktree cleanup has finished.

Before hooks:

| Before hook          | Editable request                                                       |
| -------------------- | ---------------------------------------------------------------------- |
| `agent.create`       | `config` (agent configuration except `cwd`) and launch-override `env`  |
| `agent.session_open` | Only `env`; `reason` and `purpose` tell you why the session is opening |
| `workspace.create`   | The workspace creation request                                         |

- Each callback receives a copy of the request after earlier callbacks changed it. Return the whole
  request with your change, or `undefined` to keep it.
- The returned request is validated. Changing `cwd` in `agent.create`, or anything but `env` in
  `agent.session_open`, fails.
- A before hook that throws fails the operation with `Plugin <id> before <name> failed: ...`.
- `env` holds launch overrides, not the daemon's inherited environment. Session opening covers
  create, resume, refresh, and import, for interactive or history work.

Respect `signal`; the daemon aborts it when the hook is canceled or the plugin stops.

## Persist settings

```ts
// shared/preferences.ts
import { defineSettings } from "@getpaseo/plugin";
import { z } from "zod";

export const preferences = defineSettings({
  id: "review",
  scope: "host",
  version: 1,
  schema: z.object({ enabled: z.boolean().default(true) }),
});
```

`server.registerSettings(preferences)` returns a handle with `read()` and `subscribe(listener)`;
`subscribe` returns its cleanup. `read()` resolves to `{ status: "ready", revision, values }` or
`{ status: "invalid", revision, error }`. Invalid saved data is kept, not replaced.

- `id` matches `[a-z][a-z0-9_-]*`. `scope` is always `host`: values are stored on that daemon and
  are not synced across hosts. `version` is a positive integer.
- Give every field a default so `{}` parses to a complete document. The plugin must work before
  anyone writes a value.
- Optional `migrate(values, fromVersion)` converts older stored data; it may be async and must
  return data valid under the current schema.
- Registration adds the `settings.<id>.read`, `settings.<id>.write` (`{ revision, values }`), and
  `settings.<id>.reset` (`{ revision }`) RPCs. A stale `revision` returns a conflict. The native
  client has no plugin settings screen; values change only through these RPCs.

## Append a timeline row

Any backend code with `paseo` can push a plugin-owned row into an agent timeline:

```ts
await paseo.agents.ref(agentId).timeline.append({
  type: "plugin",
  id: "review",
  kind: "review-result",
  version: 1,
  data: { verdict: "ship" },
});
```

- The daemon accepts appends only from plugin sessions and stamps `pluginId` itself.
- `data` must be JSON and at most 64 KiB serialized.
- The call resolves to `{ seq, epoch }`. Rows are saved with the agent's timeline.
- Appending again with the same `id` updates the row the client shows.
- The native client shows a plugin row as its formatted JSON. There is no plugin renderer, so keep
  `data` readable. For a provider's essential output, emit standard timeline items instead.

## Add a provider

Register a direct provider with `server.registerProvider(registration)` using `ProviderRegistration`
from `@getpaseo/plugin/server/provider`, or adapt an ACP agent:

```ts
// index.server.ts
import type { PluginServerContext } from "@getpaseo/plugin/server";
import { runAcpProvider } from "@getpaseo/plugin/server/acp";

export default function contribute(server: PluginServerContext) {
  server.registerProvider(
    runAcpProvider({
      id: "vendor-agent",
      label: "Vendor agent",
      icon: "icon.svg",
      command: ["vendor-agent", "acp"],
    }),
  );
  return () => {};
}
```

- A registration has `id` (`[a-z][a-z0-9._-]*`, unique), `label`, `connect(request)`, and optional
  `description`, `icon`, `command`, `status(request)`, `getCatalogCacheKey(options)`, and
  `requiresConfiguration`.
- A connection has `version`, `capabilities`, `send(input)`, `onEvent(listener)`, and `close()`.
  `send()` reports acceptance, not turn completion; results arrive as events. Events must match
  `ProviderEventSchema`; an invalid event closes the connection.
- Negotiate with `negotiateProviderCapabilities` and advertise only what you implement.
- Use `spawnProcess`, `execCommand`, and `terminateProcess` from `@getpaseo/plugin/server` to run the
  agent CLI with the daemon-supplied `request.launch` (`command`, `args`, `env`). They handle
  Windows `.cmd` and `.bat` launchers.
- Emit standard timeline items (user and assistant messages, reasoning, tools, todos, errors,
  notifications, compaction). Toggle and select controls in `session.config` are data the native
  client renders; they are not plugin UI.
- `icon` is a path to an SVG file inside the plugin directory, at most 64 KiB, with no scripts,
  `foreignObject`, style elements, event handlers, JavaScript URLs, or external `href`.

Follow `public-docs/plugins/providers.md` (or
[the provider guide](https://paseo.sh/docs/plugins/providers.md)) for the full session, prompt,
permission, persistence, and replay protocol.

## Add a usage source

Register with `server.registerUsageSource(source)`. Import types and helpers from
`@getpaseo/plugin/server/usage`.

- A source has `id`, `label`, optional `icon` (same SVG rules as providers), an `input` Zod schema,
  `discover(scope)`, and `fetch(input)`.
- `discover` returns accounts with a stable `key` (1–128 characters from `[A-Za-z0-9._-]`), optional
  `label` and `harness`, and JSON `input` that locates a credential store. Never put credentials or
  a raw email in `key` or `input`; use `hashAccountKey()` for sensitive identities.
- Scope is `{ kind: "global" }` or `{ kind: "session", provider, model?, env }`. For a session,
  return only that session's account, or `[]` for unrelated providers and absent logins.
- The worker validates `input` with your schema before `fetch`. `fetch` returns
  `{ status: "available", windows, balances?, details? }`, `unavailable(problem)` (`expired`,
  `rejected`, or `no_quota`), or `{ status: "error", error }`.
- Re-read the store on every fetch. Do not redeem refresh tokens or rewrite another CLI's
  credential files.
- Build windows with `windowFromUsedPct()` for named periods or `windowFromReportedDuration()` for
  reported durations. Keep window IDs stable.

Reports appear wherever Paseo lists usage reports, including `paseo usage`. A usage source does not
add a custom panel.

## Use the Paseo SDK from backend code

RPC handlers receive `{ paseo }`; lifecycle callbacks receive `{ paseo, signal }`. This is the
`@getpaseo/client` API on the plugin's own daemon session: `paseo.workspaces`, `paseo.agents`,
`paseo.projects`, `paseo.providers`, `paseo.config`, `paseo.terminals`, and `paseo.hosts`. Use it
for normal agent, workspace, provider, and configuration work instead of defining duplicate RPCs.
Release subscriptions you open in cleanup.

```ts
const workspace = await paseo.workspaces.create({
  title: "Review PR 42",
  source: {
    kind: "worktree",
    cwd: "/absolute/path/to/repository",
    action: "checkout",
    checkoutSource: { kind: "change_request", forge: "github", number: 42 },
  },
});
await workspace.agents.create({
  config: { provider: "codex/gpt-5.5" },
  prompt: "Review PR #42.",
});
```

See the [SDK reference](https://paseo.sh/docs/sdk/reference.md) for method details.

## Debug backend output

Use `console.log()` and `console.error()`. The worker captures stdout and stderr without
interfering with its protocol.

```bash
paseo plugin logs my-plugin
paseo plugin logs my-plugin --json
paseo --host <url> plugin logs my-plugin
```

The log is a recent in-memory snapshot (about the last 500 lines or 256 KiB; lines over 16 KiB are
cut) with timestamp, stream, and message. It includes load and compile failures
(`[paseo] Plugin failed to load: ...`) and lifecycle hook errors. Removing the plugin or restarting
the daemon clears it. Plugin output is not copied to `daemon.log`. Never log credentials or tokens.

## Hosts and trust

Plugins are installed per daemon and are trusted, unsandboxed code. Backend code and `build`
commands run as the daemon user with access to files, processes, credentials, and the network. The
CLI prints this warning on install. Do not install a plugin the user has not authorized or source
you have not inspected.

### Check the global switch before installing

Two switches gate execution: the daemon's root `pluginsEnabled` config and each installation's
enabled flag. On the daemon machine, read the root value:

```bash
paseo daemon config get pluginsEnabled
```

Plugins are on only when `value` is `true`; `set: false` means the field is missing and plugins are
off. Do not infer the global value from a plugin's `disabled` status; an individual installation can
also be disabled.

If `pluginsEnabled` is already `true`, continue without asking.

If it is false or unset, stop and ask the user for explicit permission before changing anything.
Include this warning:

> Plugins are trusted, unsandboxed code. Plugin code runs on your daemon machine with access to your
> files, processes, credentials, and network services. May I enable plugins on this daemon?

Do not continue unless the user agrees. After permission, on the daemon machine:

1. Run `paseo daemon config set pluginsEnabled true`. It edits that home's `config.json` and reloads
   a running daemon; require `pluginsEnabled` in the reported `appliedPaths`.
2. If the daemon was not running, start it, or run `paseo daemon reload` once it is.
3. Run `paseo plugin ls` and verify the intended plugin reaches `running` after installation.

To turn plugins off, run `paseo daemon config set pluginsEnabled false` and verify configured
plugins report `disabled`. Use `--home <path>` when the daemon does not use the default home.

For a remote daemon, run the config commands on that machine. `paseo --host <url> daemon reload`
reloads the remote daemon's own config file; it does not edit it. The native client has no switch
for plugins.

The worker needs Node.js 20+ on the daemon host. `PASEO_PLUGIN_NODE` in the daemon environment
selects the executable; otherwise it uses `node`. `PASEO_PLUGIN_WORKER` selects the worker asset
when it is not beside the daemon executable. Mobile clients never run plugins.

## Typecheck and manage

Typecheck before every install or reload. Loading compiles the plugin but does not typecheck it.

```bash
npm run typecheck
paseo plugin install /absolute/path/to/plugin
paseo plugin install /absolute/path/to/plugin --id another-runtime-id
paseo plugin install owner/slug
paseo plugin install npm:@acme/paseo-review
paseo plugin install npm:@acme/paseo-review@1.2.0
paseo plugin install github:owner/repository
paseo plugin install github:owner/repository --ref main
paseo plugin install github:owner/monorepo:plugins/review
paseo plugin ls
paseo plugin ls my-plugin
paseo plugin reload my-plugin
paseo plugin logs my-plugin
paseo plugin update my-plugin --check
paseo plugin update my-plugin --yes
paseo plugin update --all --check
paseo plugin disable my-plugin
paseo plugin enable my-plugin
paseo plugin remove my-plugin
```

- `install` and `add` are aliases. Bare `owner/slug` is a registry ID; prefix relative directories
  with `./` or `../`. With `--host`, give an absolute path that exists on that host; files are not
  uploaded.
- npm and Git acquisition run on the daemon host with that user's npm, registry, and Git
  configuration. npm is needed to acquire npm sources, not to load plugins.
- `update` shows a proposal and applies it only with `--yes`. `--check` never applies.
  `--version` (npm) and `--ref` (Git) pick an explicit target for one plugin. Directory installs are
  not updated; edit and reload them.
- A Git or npm source that must install or generate something declares `build` in
  `paseo-plugin.json` as a list of argv arrays, such as
  `[["npm", "ci", "--omit=dev", "--ignore-scripts"]]`. Paseo runs them without a shell on install
  and update, and keeps the old version if one fails. Ordinary TypeScript needs no build step.
- `ls` reports `running`, `disabled`, or `failed` with the load error.
- `remove` keeps local source directories and deletes managed Git and npm installations.
- Put `--host <url>` before the command to manage a daemon other than the CLI default.

Source edits need `paseo plugin reload <id>`. A failed reload stays failed: read `paseo plugin ls`
and `paseo plugin logs <id>`, fix the source, typecheck, and reload again. Do not restart the
daemon to load source changes; restarting it can kill the agent doing the work.

For an old plugin with a client entry or a mixed `index.ts`, follow the
[migration guide](https://paseo.sh/docs/plugins/migration.md): keep the backend behavior in
`index.server.ts` and delete the client code.

## Publishing a listing

Publish backend-only plugins. State in the README and listing that the plugin needs Node.js 20+
on the daemon host, and describe only behavior you have exercised.

`OVERVIEW.md` beside `paseo-plugin.json` is the plugin's page inside Paseo. It is optional: without
it, the registry reviewer writes a page from your README and verified behavior. If you ship one, it
replaces the reviewer's page. Include it in the npm package; the scaffold's `files` list already
includes `OVERVIEW.md` and `OVERVIEW.*.md` (translations such as `OVERVIEW.zh-CN.md`).

Write the overview in this order, using headings only when they help:

1. What the plugin does.
2. How it works, when that is not obvious.
3. Setup: settings, accounts, tokens, external tools, supported operating systems, and Paseo
   versions.
4. Useful capabilities, settings, data access, permissions, and limits. Say where data goes,
   including prompts and tool results sent to providers. If the plugin downloads or installs
   software while running, say what and from where, and require the user's confirmation first.

Use plain, factual language and sentence case. Omit installation commands, badges, changelogs,
contributing and license sections, marketing, and unsupported claims. The publication validator
rejects `paseo plugin add`, `npm install`, and `npm i` anywhere in an overview; link to the README
for those. Do not show screenshots of former plugin UI as current behavior.

Also give a one-sentence `description` (in `package.json` for npm, or the manifest for Git).

Follow `public-docs/plugins/publishing.md` (or
[the publishing guide](https://paseo.sh/docs/plugins/publishing.md)) for listing images, npm
package contents and dependency locks, Git sharing, private registries, and registry submission.

## Verify the outcome

After a change:

1. Run `npm run typecheck`.
2. Install or reload the exact runtime ID.
3. Run `paseo plugin ls <id>` and require `running` with no error. `running` only proves the
   backend loaded.
4. Exercise each changed contribution on the intended daemon:
   - Hooks: create an agent or workspace, or complete a turn, then read `paseo plugin logs <id>`.
   - Before hooks: confirm the created agent, workspace, or session has the change.
   - RPCs: call the method through `plugin.rpc.invoke.request`, including an input the schema
     rejects and a handler error.
   - Timeline rows: open the agent in the native client and find the JSON row.
   - Providers: create an agent with the provider, complete a real prompt, and exercise
     permissions, cancellation, and persistence you support.
   - Usage sources: check `paseo usage` for the account.
5. Reload or remove the plugin while work is active and confirm cleanup ran.

Common failures:

- `Plugin contribution must return a cleanup function`: the default export returned nothing.
- `Native Paseo requires index.server.ts or index.server.tsx`: the plugin has no backend entry, or
  the entry path is a directory.
- `client-only module cannot be imported into the plugin server bundle`: backend or shared code
  imports React, React Native, or `@getpaseo/plugin/client*`. Remove the dependency.
- `Plugin modules belong in client/, server/, or shared/`: move the root module.
- `Plugin "<id>" requires Paseo ...`: fix `requirements.paseo` or update the daemon.
- Worker or Node unavailable: install Node.js 20+ on the daemon host or set `PASEO_PLUGIN_NODE`.
- RPC rejection: input or output failed its Zod schema, the method name is wrong, or the handler
  threw. Check `paseo plugin logs <id>`.
- Agent or workspace creation fails with `Plugin <id> before <name> failed`: a before hook threw or
  returned an invalid request.
- Append rejected: `data` exceeds 64 KiB, or the call did not come from plugin code.
- Changes not active: the source was edited without `paseo plugin reload <id>`.
- Plugin shows `disabled`: the installation is disabled or `pluginsEnabled` is not `true`.
