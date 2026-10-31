# Direct provider example

This plugin implements `ProviderRegistration` directly in `index.server.ts` and `server/`. It
demonstrates connection negotiation, catalog and session listing, session creation, prompt and
command disposition, settings, persistence and replay, archive actions, and an ordinary child
session.

The provider also emits versioned `provider-result` plugin timeline items for its root and child
sessions. The native client shows those rows as formatted JSON. `index.client.tsx` registers a React
Native renderer for them; the native client does not run it, so that file only exercises the SDK's
client types. Emit standard timeline items for output users need to read.

The provider receives Paseo host tools through `config.mcpServers`; it does not receive a second
callback tool API.
