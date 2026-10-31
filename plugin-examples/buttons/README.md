> The native Paseo client does not run client UI plugins. This example only exercises the SDK's client types; it does not add anything to the native app.

# Header buttons and composer pills

This example has no server entry, so a native Paseo daemon rejects it on install
(`Native Paseo requires index.server.ts or index.server.tsx`).

It was written for the former JavaScript client. It registers one header button and one composer
pill per agent, and Command Center items that switch them between five modes:

| Command Center command                | Header                                | Composer                        |
| ------------------------------------- | ------------------------------------- | ------------------------------- |
| **Button examples: action**           | Refresh icon only                     | Refresh icon and label          |
| **Button examples: menu**             | Wrench, Tools, chevron                | Wrench and Tools                |
| **Button examples: popover**          | Reactive status dot, Details, chevron | Reactive status dot and Details |
| **Button examples: hide / show**      | Hide or restore the button            | Hide or restore the pill        |
| **Button examples: disable / enable** | Disable or enable the button          | Disable or enable the pill      |

[client/examples.tsx](client/examples.tsx) contains the descriptors, `registration.update()` calls,
and React content. The images in `screenshots/` are historical captures of the former JavaScript
client, not current behavior.

From the repository root, `npm run typecheck --workspace=@getpaseo/plugin` checks this example
against the SDK's client types.
