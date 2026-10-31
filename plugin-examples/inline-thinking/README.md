> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Inline thinking timeline example

This example has no server entry, so a native Paseo daemon rejects it on install
(`Native Paseo requires index.server.ts or index.server.tsx`). The native client does not run
timeline transformers or renderers.

`index.client.tsx` registers a timeline transformer that replaces reasoning rows with a plugin item,
and a renderer that draws that item with the component in `client/thinking.tsx`.

From the repository root, `npm run typecheck --workspace=@getpaseo/plugin` checks this example
against the SDK's client types.
