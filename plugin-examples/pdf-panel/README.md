> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# PDF file panel example

The server entry is empty, so installing this directory on a native daemon loads a plugin that does
nothing. The native client does not run file panels.

`index.client.tsx` registers a file panel for `.pdf` files. The panel resolves the file's URL with
`useFileUrl`, shows `Loading` until it does, and passes it to `DocumentFrame`.

From the repository root, `npm run typecheck --workspace=@getpaseo/plugin` checks this example
against the SDK's client types.
