> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Pi tasks timeline plugin example

This example has no server entry, so a native Paseo daemon rejects it on install
(`Native Paseo requires index.server.ts or index.server.tsx`). The native client does not run
timeline transformers or renderers.

The transformer in `shared/pi-tasks.ts` recognizes Pi `todo` tool calls from both
`@juicesharp/rpiv-todo` and Pi's `examples/extensions/todo.ts`, and replaces the tool-call entry
with a versioned `pi-task-list` item. The rpiv shape preserves `pending`, `in_progress`, and
`completed`; deleted tombstones are omitted. Pi's example shape maps `done` to `completed` or
`pending`. Malformed results and unrelated tool calls return `undefined`, which keeps the original
entry. `client/pi-tasks.tsx` is the React Native renderer for the item.

`pi-tasks.test.ts` tests the transformer. From the repository root,
`npm run typecheck --workspace=@getpaseo/plugin` checks this example against the SDK's client types.
