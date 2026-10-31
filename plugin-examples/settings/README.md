> The native Paseo client does not run client UI plugins. The settings screen in `client/` only
> exercises the SDK's client types; the native app has no plugin settings screen.

# Settings example

The backend part works on a native daemon: `index.server.ts` registers the `display` settings
document from `shared/preferences.ts` with `server.registerSettings()`. That adds the
`settings.display.read`, `settings.display.write`, and `settings.display.reset` RPCs. Values are
stored on the daemon host and shared by every client of that host. Every field has a default, so a
read returns complete values before anything is saved.

Install it with `paseo plugin install /absolute/path/to/plugin-examples/settings`.

`client/display-settings.tsx` was written for the former JavaScript client. Its title editor keeps
the revision it started from, so another client's save produces a conflict instead of being
overwritten. The images in `screenshots/` are historical captures of that client on Android,
Electron, and the web, not current behavior.
