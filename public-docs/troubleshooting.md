---
title: Troubleshooting
description: Diagnose native runtime launch errors, missing providers, and environment mismatches.
nav: Common problems
order: 90
category: Troubleshooting
---

# Troubleshooting

## Native client or daemon will not start

The 0.13.0 Rust/Flutter cutover is unpublished. Do not use an older Electron AppImage, npm CLI package, or store download as evidence of this native runtime. Build from the migration checkout as described in [Updates](/docs/updates).

Keep the complete Flutter bundle and generated Rust runtime assets together. Copying only `paseo` omits required libraries and skills. Inspect startup output and `paseo daemon status`; for foreground diagnostics use `paseo daemon run`. Bare `paseo` opens chat, not daemon setup or startup.

Development defaults to `.dev/paseo-home` and port `6768`; normal operation uses `~/.paseo` and `6767`. Select the intended `--home` or `--host` before diagnosing a different instance. Mobile apps are remote clients and cannot start a local daemon.

## Paseo can't find my provider

A provider you've installed shows as **Not installed**.

Paseo launches the agent CLIs you've already installed, it doesn't bundle them (see [Providers](/docs/providers)). So it has to find the command on its own `PATH`. If your shell only adds that location to `PATH` under certain conditions, Paseo can miss it.

### See what Paseo sees

Open **Settings → your host → Providers**, tap the provider, then tap **Diagnostic**. The rows that matter:

- **Resolved path** — where Paseo found the binary, or `not found`.
- **Daemon PATH** — the `PATH` Paseo is searching. Compare it to `echo $PATH` in a fresh terminal.
- **Version** — whether the binary actually runs.

From a terminal or agent, request the same diagnostic from the affected daemon:

```bash
paseo provider diagnostic <provider>
paseo --host <host:port> provider diagnostic <provider> --json
```

Use the global `--host` option when the affected daemon is not the CLI's default local daemon.

`not found` together with a **Daemon PATH** that's missing your binary's directory is the common case: that directory is on your terminal's `PATH` but not on Paseo's.

### Fix it

Make the provider command available on the daemon's launch `PATH`, then stop and relaunch its supervisor; see [why Paseo's environment can differ](#why-paseos-environment-can-differ-from-your-terminal).

If you'd rather pin it directly, set the binary path in `~/.paseo/config.json`:

```json
{
  "agents": {
    "providers": {
      "claude": {
        "command": ["/absolute/path/to/claude"]
      }
    }
  }
}
```

`command` is `[binary, ...args]` and fully replaces the default launch command for that provider. Find the real path with `which -a claude`. `type -a claude` also tells you if `claude` is only a shell alias or function, those won't work, Paseo runs the binary directly, so use the path it points to. Reload the configuration after editing (see [below](#i-changed-configjson-but-nothing-happened)).

For alternative endpoints, multiple profiles, custom binaries, and ACP agents, see [Custom providers](/docs/custom-providers). For per-agent install links, see [Supported providers](/docs/supported-providers).

## Why Paseo's environment can differ from your terminal

An OS launcher, service manager, container, or remote host can supply a different environment from your terminal. This affects agent providers and script/setup PTYs as well as other host tools.

Use the affected daemon's provider diagnostic rather than assuming the Flutter app imports your interactive shell configuration. Start a foreground instance with `paseo daemon run` to use your terminal's launch environment. Managed `paseo daemon start` intentionally removes daemon-setting overrides; save those settings in `config.json`.

Ensure provider binaries are on the daemon's launch `PATH`, or configure an absolute provider `command`. If you change the launch environment, stop and relaunch the owning supervisor; a worker restart retains its original environment. Optional JavaScript backend plugins need external Node 20+ (`PASEO_PLUGIN_NODE`), not a bundled Node runtime.

## Reading the logs

- **Daemon** — `~/.paseo/daemon.log` (`$PASEO_HOME/daemon.log` for a custom home).
- **Flutter desktop** — use the native settings diagnostics' **Application logs** and **Daemon logs** actions. Application logs are per-window `desktop-<windowId>.log` files in the app support directory; use the returned `logPath` rather than old Electron log locations.
- **Script/setup tasks** — inspect their read-only logs. The native client has no interactive terminal UI.

## I changed config.json but nothing happened

Reload the file after editing:

```bash
paseo reload
```

Paseo applies runtime-safe settings and names any paths that require a restart. Invalid JSON or a schema error applies nothing; fix the reported error and run the command again. If a launch environment variable or flag owns a changed setting, reload reports it separately.

Run `paseo daemon restart` only when reload requests it. In the app, open **Settings → your host → Overview** and use **Restart daemon**. Running agents keep going, and clients reconnect automatically.

## Still stuck?

- [Custom providers](/docs/custom-providers) — endpoints, profiles, binaries, ACP agents.
- [Configuration](/docs/configuration) — `config.json`, environment variables, logging.
- [Native desktop host](https://github.com/getpaseo/paseo/tree/main/packages/flutter_app/lib/platform/desktop) — Flutter host launch and diagnostics implementation.
- [Report an issue](https://github.com/getpaseo/paseo/issues).
