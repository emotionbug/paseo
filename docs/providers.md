# Providers

The product daemon and built-in provider integrations are **Rust**. Provider executables remain external tools installed and authenticated on the daemon machine. Their own runtime requirements do not imply that Paseo bundles Node.

Optional JavaScript backend provider extensions remain available through `@getpaseo/plugin/server/provider` and an external-Node worker. See [plugins](plugins.md). A built-in native provider is not implemented by adding a TypeScript server class or a React component.

## Native ownership

`packages/daemon/src/providers` owns the native provider boundary:

- `api.rs`: `AgentProvider`, `AgentSession`, `ProviderSpec`, `LaunchContext`, event sinks and tool execution authority.
- `registry.rs` and `registry/`: provider registration/configuration, catalog snapshots, client ownership, options, discovery and shutdown.
- `options.rs` and the options schema: validation of provider-specific configuration.
- `create_config.rs`: default creation policy, resolved mode and unattended behavior.
- Native adapter modules: provider transport, prompt shaping, streaming, permissions, persistence, import/resume and model/mode support.

Agent orchestration, canonical timeline persistence and client publication belong to `packages/daemon/src/agent` rather than to a UI-specific provider implementation. Usage integrations live under `packages/daemon/src/usage`.

Shared TypeScript wire metadata remains under `packages/protocol`, for the retained external SDK and optional worker. Keep that contract aligned with Rust and Dart when adding a provider capability or changing serialization.

## Provider contract

A provider owns its native session identifier, opaque persistence handle, native options and actual capabilities. Preserve existing persisted shapes and IDs when changing the implementation. Client disconnect or closing a chat tab must not close the provider session or archive it.

The session API includes turn start/steering, history, runtime info, modes/model/thinking options, permissions, interruption, commands, rewind and close. An unsupported operation must agree with the advertised capabilities and return an explicit error; successful registration alone does not demonstrate that a provider is usable.

Event sinks are bounded and cancellation-aware. The agent owner persists events before outward publication. Never drop an event because a socket or extension consumer is slow, and never publish durable success before the corresponding mutation is persisted.

`LaunchContext` binds tools to the agent's authority and environment. Provider options are not an alternate channel for replacing that authority. Exact MCP preapproval must only be advertised when supported; broad “allow all” behavior is not a substitute for an exact tool policy.

## Catalog and configuration behavior

Keep provider models, modes and configuration scoped to the effective environment and working directory. Native catalogs may be discovered asynchronously; distinguish pending discovery, unsupported discovery, and a real provider error. Configuration changes must not return a snapshot from a different identity/environment.

CLI-default profiles are host-owned. Explicit command-line provider/model arguments override the saved default; conflicting provider/model values are rejected before connection. Selecting a different provider must not carry over incompatible saved options.

Provider helpers that outlive one session belong to the daemon's managed-process lifecycle. Record and verify process-start identity before signalling a PID. Graceful shutdown, forced termination, restart, and cancellation must not affect another process that reused the same PID.

## Adding an integration

1. Decide whether the integration is a native built-in or an optional backend extension. Built-ins stay Rust; external-Node plugins remain opt-in.
2. Reuse the native provider interfaces and appropriate transport implementation. Keep protocol-specific details behind the provider boundary rather than branching throughout agent orchestration.
3. Add the provider definition/catalog/options and preserve stable wire IDs. Update TypeScript protocol definitions and Dart presentation where the capability is client-visible.
4. Preserve import/resume, native session identity, permissions, model/mode discovery, cancellation, errors and persistence behavior. Do not claim unsupported features.
5. Update usage integration only if the provider supplies real attributable data; missing data must not become a fabricated zero balance.
6. Add behavior checks for the actual boundary and run them in an authorized verification pass.

Provider UI belongs to native Flutter features under `packages/flutter_app/lib`, not a new TSX icon/registry in a retired client tree.

## Verification

Use an isolated native daemon and real provider credentials for provider end-to-end checks. Exercise a turn, permission decision, interruption, reconnect/history, import/resume and shutdown where supported. Report unavailable credentials/services explicitly. A mock provider can verify controlled states, but it cannot establish real provider interoperability.

Linux scenarios exercised the real Rust adapters and daemon against controlled Claude/ACP/MCP provider-protocol boundary fixtures, plus an optional external-Node extension. They were not paid live Claude/ACP LLM executions and do not establish live account authentication, every provider mode or other operating systems. See [testing](testing.md) for recorded evidence and its authority boundaries.

The separate [custom-provider configuration reference](custom-providers.md) is not changed by this migration documentation task; source implementation ownership for built-ins is the native path above.
