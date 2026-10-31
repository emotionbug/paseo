---
title: Native clients and self-hosting
description: Connect native clients to a self-hosted Rust daemon without a bundled web UI.
nav: Native clients
order: 6
category: Getting started
---

# Native clients and self-hosting

The Rust daemon does **not** bundle or serve a product web client. The native clients are Flutter applications for Linux, macOS, Windows, Android and iOS. The documentation website is separate from the application.

Do not enable `features.webUi`, set `PASEO_WEB_UI_ENABLED`, or look for an app at `http://localhost:6767/` as an installation step. These former instructions do not create a native client. The HTTP/WebSocket API, MCP and workspace service proxy remain daemon functions; their existence does not imply a browser UI.

## Self-host the daemon

Build the native daemon and its assets from this checkout using the [getting-started instructions](/docs), then run its explicit setup/start commands:

```sh
paseo onboard
paseo daemon start
paseo daemon status
```

Use the actual built executable or its installed path. Bare `paseo` opens CLI chat, not setup. Keep generated runtime libraries, skills and optional-worker assets with the executable. The native migration has not been published; existing npm packages or upstream downloads must not be assumed to contain it.

Configuration and history remain under `PASEO_HOME` (default `~/.paseo`), or explicit `--home`. Desktop clients can manage a local daemon; Android/iOS connect to one running elsewhere. Multiple native clients and external SDK programs can use the same daemon without moving the agents to their devices.

## Connect safely

Add the daemon as a host in the native client. Use the optional encrypted relay, a trusted VPN, or a secured direct connection. Set authentication before exposing a daemon port. TLS/reverse proxies must forward WebSocket upgrades and preserve the intended host/scheme; password authentication alone does not encrypt traffic.

See [connectivity](/docs/connectivity), [configuration](/docs/configuration), [security](/docs/security), and [Docker](/docs/docker) for the daemon's network and persistence boundaries. A Docker instance is daemon-only, not a self-hosted browser app.

## Review and test work

Native clients provide chat, Markdown, source editing, remote folders, summary-first diffs, schedules and read-only script/setup logs. Workspace services can still serve your project's own web application; open those URLs in an external browser. That is distinct from a Paseo product web client or embedded automation.
