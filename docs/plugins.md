# Backend extensions

Paseo's default daemon is Rust. Built-in providers, lifecycle behavior and usage integrations execute in Rust; they do not start a Node process. **Optional JavaScript backend extensions** remain supported through a separate worker. The Flutter client does **not** execute React UI plugins.

## Runtime and source ownership

- Native extension management, persistence, hooks, settings, RPC and provider bridge: `packages/daemon/src/plugins.rs` and `plugins/`.
- Optional JavaScript worker source: `packages/daemon/plugin-worker/src`, private workspace `@getpaseo/plugin-worker`.
- Extension author SDK: `packages/plugin`, including `@getpaseo/plugin/server`, `/server/provider`, and `/server/usage`.
- External daemon client SDK: `packages/client`; protocol definitions: `packages/protocol`.
- Worker build entry: `scripts/build-plugin-worker.mjs`.

The runtime package includes the worker and its compiler/SDK assets, but **does not bundle Node**. An enabled JavaScript extension requires externally installed Node 20+ on the daemon host. `PASEO_PLUGIN_NODE` selects its executable; otherwise the worker uses `node` from its environment. `PASEO_PLUGIN_WORKER` can select a worker asset; the default is `plugin-worker/worker.mjs` next to the native executable.

Node 24 is required for repository build/packaging tooling, distinct from the optional worker's external runtime requirement. A missing Node installation is a visible extension error, not a reason to replace the Rust daemon or download a hidden runtime.

## Install and manage

The native `paseo plugin` command manages extension sources and configuration. Inspect `paseo plugin --help` for subcommands, and target the intended home/host before changing configuration.

```sh
paseo plugin add /absolute/path/to/my-plugin
paseo plugin add owner/slug
paseo plugin ls
```

Directory, registry and managed-source operations remain host-side. An extension source may need its own dependency preparation. Only install code you trust: backend extensions run with the daemon user's filesystem, credential and process privileges. They are not a sandbox. A client connecting to a daemon does not install Node or execute the backend extension on the phone.

Keep source identity, managed checkout ownership, installed configuration, settings and logs separate. Uninstalling one managed extension must not delete arbitrary directories supplied by the user. Registry availability is independent of this unpublished native cutover.

## Contributions retained

Backend registrations include RPCs, lifecycle hooks/transforms, provider connections, usage sources, and settings data. Use the server-side SDK entry points and preserve asynchronous completion/cancellation and error propagation. Provider messages cross the native worker bridge; plugin RPC is not a substitute for the provider event/permission data path.

The worker protocol carries bounded requests, explicit results/errors, logs, provider events and cancellation. The native owner tracks process identity and shuts down the worker it owns. A worker crash fails pending operations rather than silently dropping events or reporting success.

A backend registration can provide data to the native application where that feature already has a native surface. It does not permit JavaScript to define arbitrary Flutter widgets.

## Client compatibility boundary

Executable React client entries are excluded: screens, workspace/file panels, sidebar component renderers, timeline renderers, composer components, arbitrary buttons and themes implemented as React code are **not loaded by Flutter**. Existing examples or public SDK symbols for those former clients are not a claim of native compatibility. Do not advertise “write the UI once and run it on every native device.”

Native candidate validation and activation require an actual `index.server.ts` or `index.server.tsx` file. A client-only package, or a directory named like a server entry, fails explicitly rather than activating as an empty ready plugin.

Built-in Markdown, source editing, settings and read-only script logs are Flutter features, not a React plugin host. There is no browser/webview fallback to execute a client extension. Embedded browser automation and interactive terminal UI are also excluded.

## Developing and checking a backend extension

Build the native runtime assets with `npm run build:daemon`, run an isolated daemon with `npm run dev:daemon`, and install the extension into that development instance. `npm run cli -- plugin ...` selects the checkout development home. Never use the installed user daemon as a disposable extension test host.

Test actual worker initialization, registration, RPC results/errors, cancellation, restart and shutdown. For provider extensions, exercise a real turn and permission path, not just successful registration. Actual optional external-Node extension scenarios were exercised on Linux during the cutover; see [testing](testing.md) for the exact coverage boundary.

## Distribution

Worker/compiler notices are generated with the native package's Cargo and speech attributions. They do not grant additional rights to third-party extension code or certify the whole distribution. See [release requirements](release.md#distribution-requirements), particularly the unresolved speech-library source and licensing obligations.
