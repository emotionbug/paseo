> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Markdown file panel example

The server entry is empty, so installing this directory on a native daemon loads a plugin that does
nothing. The native client does not run file panels; it has its own Markdown view.

`index.client.tsx` registers a file panel for `.md` and `.markdown` files. The panel in `client/`
is a CodeMirror editor that reads the file with `readText`, saves with `writeText` 500 ms after the
last edit, reports unsaved edits with `tab.setModified`, and offers **Reload** and **Overwrite**
when the file changes on disk or a write rejects with `PluginFileConflictError`. DOM integration
lives in `client/web.ts`.

The editor dependencies are in `package.json`. Run `npm install` in this directory before
typechecking it from the repository root with `npm run typecheck --workspace=@getpaseo/plugin`.
