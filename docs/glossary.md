# Glossary

Use product terms consistently across the Rust daemon, native Flutter client, CLI and external SDK. The current source map is in [architecture](architecture.md); retired TypeScript server/React client symbols are not native ownership references.

## Hosts and navigation

- **Daemon** — The Rust process that owns agent execution, storage, files, Git, scripts and integrations. Its stable identity is `serverId`; its home defaults to `~/.paseo`.
- **Host** — A saved client connection profile for a daemon, possibly with more than one connection method. A daemon-side **connected host** is a separate saved delegation relationship; see [connected hosts](connected-hosts.md).
- **Connection** — A transport/endpoint for a host, not the host's identity. Reconnecting must not create another host or session.
- **Directory** — A path on one host. `(serverId, cwd)` is the relevant identity; equal paths on two hosts remain distinct.
- **Session tree** — The primary sidebar hierarchy **host → directory → session**. Empty directories and a single host still have rows. Search temporarily expands matching ancestors without replacing saved collapse state.
- **Project** — A host-local registered root with stable membership/identity. Repository metadata does not silently rekey or rehome it.
- **Workspace** — A durable orchestration entity for a directory/local checkout/worktree, associated sessions, scripts and state. Workspaces remain available through Command Center and APIs, but are not sidebar grouping rows.
- **Workspace kind** — The persisted Git-derived kind (`directory`, `local_checkout`, `worktree`), not the user's transient create choice.
- **Isolation** — The create-time choice to reuse a checkout or create a separate worktree.
- **Worktree** — A Git worktree used for isolated work; not a synonym for project, workspace or host.
- **Placement** — A workspace's stable project relationship plus its checkout snapshot.
- **Explorer** — The supporting native Files/Changes navigation surface. It is distinct from the session tree.
- **Pane / tab / sheet** — Presentation containers, not agent or daemon lifetimes. Closing a tab does not archive a session; closing a log view does not stop a service.

## Agents and configuration

- **Agent session** — One managed provider conversation with stable Paseo identity, working directory, configuration and history. “Session” in user copy normally means this; a protocol connection session is an internal transport concept.
- **Provider** — The integration for an external coding-agent tool. Built-in integrations run in Rust; the provider executable has its own installation/authentication requirements.
- **Provider-native ID** — The provider's opaque conversation/session identifier, not a Paseo agent ID. Preserve it for import/resume.
- **Model** — A provider-advertised model choice.
- **Mode / thinking option / feature** — Provider-owned runtime controls. They must match actual advertised capabilities.
- **Agent profile** — A host-owned saved launch configuration. A profile can be selected as the **CLI default**; explicit CLI options override it.
- **Composer** — The native prompt surface, including text, attachments, provider controls and submit/stop actions. A draft is user state, not a submitted provider turn.
- **Permission request** — A provider/daemon authorization decision with explicit identity and response; hiding the requesting view does not grant or deny it.
- **Managed subagent** — A full Paseo agent related to a parent.
- **Provider subagent** — A provider-owned child execution observed through its own descriptor/timeline, not an independent managed Paseo agent.
- **Archive** — A daemon-owned durable lifecycle mutation, distinct from closing a view. See [agent lifecycle](agent-lifecycle.md).

## Files, scripts and automation

- **Markdown preview** — Native rendering of source Markdown; no embedded browser or executable HTML/React client runtime.
- **Source editor** — Native editing of a host-side file, with draft/revision/save ownership. A remote file is not a local-client path.
- **Summary-first diff** — Load file-tree/statistics first, then individual patch bodies on demand.
- **PTY** — Daemon-owned pseudo-terminal used by setup and script processes. The backend remains supported; the app has no interactive terminal emulator.
- **Script logs** — Read-only output/replay of script or setup execution, with real running/completed/failed state.
- **Workspace script** — A named `paseo.json` command. A **service** is the long-running kind; not every script is a service.
- **Schedule** — A durable trigger/cadence for sending work or creating an agent. **Heartbeat** is a recurring prompt associated with an existing agent's lifecycle.
- **MCP** — Model Context Protocol client/server integration and tools; not embedded browser automation.
- **Forge** — A Git hosting service. **Change request** is the neutral term; display PR or MR according to the actual service.
- **Usage source / report** — Account-scoped provider usage data. Missing data is not a fabricated zero balance.

## Runtime and distribution

- **Native client** — Flutter on Linux, macOS, Windows, Android and iOS. Android/iOS connect remotely and do not ship a daemon.
- **Native CLI** — The Rust `paseo` executable. Bare `paseo` opens chat; `paseo onboard` is explicit setup; `paseo daemon start` starts a managed instance.
- **Backend extension** — Optional trusted JavaScript code run by an external-Node worker. Built-ins do not need Node; executable React UI plugins are excluded.
- **SDK** — The retained external TypeScript client library, `@getpaseo/client`. Using it does not put Node into the default installed product runtime.
- **Build tooling** — Node/npm, Rust/Cargo, Flutter and target platform toolchains used to produce artifacts. These are distinct from the installed runtime.
- **Release identity** — Validated source/version/channel/repository provenance, not merely an optimized compilation.
- **Attributions** — Dependency notices generated by packaging. They are not a compliance certificate or a corresponding-source offer; see [release](release.md#distribution-requirements).

There is no product web client, Electron/Chromium, embedded browser or browser automation. Website tooling and external agent programs remain separate from that product boundary.
