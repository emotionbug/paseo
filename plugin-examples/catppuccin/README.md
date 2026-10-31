> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Catppuccin plugin example

This example has no server entry, so a native Paseo daemon rejects it on install
(`Native Paseo requires index.server.ts or index.server.tsx`). The native client does not load
plugin themes.

The whole plugin is one `addTheme` call in `index.client.ts`. It maps the
[Catppuccin Mocha](https://catppuccin.com/palette/) palette (`base`, `text`, `surface0`,
`surface1`, `mauve`, `subtext0`, and `overlay0`) onto the SDK's theme color fields.

From the repository root, `npm run typecheck --workspace=@getpaseo/plugin` checks this example
against the SDK's client types.
