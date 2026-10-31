> The native Paseo client does not run client UI plugins. The attachment source in
> `index.client.ts` only exercises the SDK's client types; the native app does not show it.

# Linear plugin example

The backend part works on a native daemon: `index.server.ts` registers the `issues.search` RPC,
which searches Linear by issue identifier or title and returns issue snapshots. The client entry
registers an **Attach Linear issue** composer attachment source that calls that RPC; the native
client does not run it.

Set a personal API key in the daemon environment before starting the daemon:

```bash
export LINEAR_API_KEY="lin_api_..."
```

With plugins enabled on the daemon (`paseo daemon config set pluginsEnabled true`), install the
example:

```bash
paseo plugin add /absolute/path/to/paseo/plugin-examples/linear
paseo plugin ls linear
```

Use `paseo plugin reload linear` after source edits. Changing the API key requires restarting the
daemon with the new environment; installing or reloading plugin source does not.

The layout shows how to keep credentials on the daemon:

- the validated search RPC and attachment metadata are in `shared/issues.ts`;
- the daemon-side handler is in `server/issues.ts` and `index.server.ts`;
- credentials and Linear API calls stay in `server/`.
