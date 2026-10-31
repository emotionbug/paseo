# Git forge integrations

Forge operations are native daemon behavior. `packages/daemon/src/git/forge.rs` and `git/forge/` own service behavior; Flutter Git presentation is under `packages/flutter_app/lib/features/git`, with chat forge attachments under `features/chat/forge_attachments.dart`. Shared TypeScript protocol definitions remain in `packages/protocol` for external SDK consumers.

Do not add a TypeScript server adapter or React client registry in a removed application tree. Keep forge-specific logic behind the current native boundary rather than branching by vendor throughout workspace orchestration.

## Wire contract

Shared messages carry forge-neutral facts and open service-specific metadata. `forgeSpecific.forge` identifies the **facts family**, not necessarily the visible brand: Gitea, Forgejo and Codeberg may share Gitea-shaped facts while retaining distinct top-level brand IDs.

Unknown or schema-mismatched service-specific facts should render neutrally without rejecting an otherwise valid message. Check results preserve normalized status plus optional traits and raw native detail. Do not put one service's vocabulary into every consumer's normalized status enum.

Preserve existing compatibility fields for supported peers and keep their serialization policy at one boundary. The Rust/Dart cutover does not authorize silently changing persisted IDs, URLs or request names. The protocol manifest and external SDK must stay aligned when adding a capability.

## Native implementation

The adapter owns authentication, command/API invocation, pagination, facts validation, error mapping and mutation semantics. A cached status is not authority for a destructive mutation: confirm the current remote identity/state at the owning operation. Keep host/cwd and repository identity explicit; an equal path on another host is unrelated.

Client presentation owns labels, links, status and action availability. Open external forge links outside the app; there is no embedded browser or browser automation fallback. Use the forge's own PR/MR terminology in localized copy.

## Verification

Exercise real read and mutation boundaries with authorized test repositories/credentials. Retain errors and service-native evidence; a fixture validates decoding, not authentication or remote effects. Use isolated daemon state and bounded queries so a large repository does not cause one process/request per visible row.

For native changes use the Rust/Flutter checks in [testing](testing.md); retained protocol/SDK changes use their own workspace checks. `npm run build:daemon` stages native runtime assets, and `npm run build:client` builds the external TypeScript SDK. A successful SDK build does not compile or test a Node product daemon.
