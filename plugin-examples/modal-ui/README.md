> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Modal UI example

This example has no server entry, so a native Paseo daemon rejects it on install
(`Native Paseo requires index.server.ts or index.server.tsx`). This applies to the published
`npm:@getpaseo/plugin-example-modal-ui` package too.

It was written for the former JavaScript client. Its screens open host modals and sheets that show
default and custom padding, a full-width body, author-owned `ScrollView` and `FlatList` scrolling,
horizontal tabs, and clipboard actions with a keyboard-aware input.

The images in `screenshots/` are historical captures of the former JavaScript client on Android and
in a browser, not current behavior.

From the repository root, `npm run typecheck --workspace=@getpaseo/plugin` checks this example
against the SDK's client types.
