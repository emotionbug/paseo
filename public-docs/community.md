---
title: Related projects
description: Projects related to Paseo and built by the community.
nav: Related projects
order: 7
category: Getting started
---

# Related projects

These projects are related to Paseo and built by the community.

## Plugins

Browse the [plugin registry](/plugins) for community extensions, checking each one's runtime requirements. The native client supports backend extensions through an external-Node worker, not executable React UI entries. To share a backend extension, follow [Publish a plugin](/docs/plugins/publishing).
Extensions run unsandboxed; read their listing and source before installing.

## Tools and integrations

| Project                                                                                      | What it does                                                                                                                                        |
| -------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Paseo for VS Code](https://marketplace.visualstudio.com/items?itemName=hinnes.paseo-vscode) | Opens the matching Paseo project or worktree in VS Code and adds agent chat, file mentions, and editor links.                                       |
| [VS Code Web plugin](https://github.com/itsjustanks/paseo-plugin-vscode-web)                 | Third-party VS Code tunnel integration. Its former Paseo UI entry is not a supported Flutter plugin; use an external browser/tool workflow instead. |
| [Paseo Icon](https://github.com/gpambrozio/paseo-menubar)                                    | Shows workspace status across Paseo hosts in the macOS menu bar and opens a workspace with one click.                                               |
| [Paseo Cross-Daemon Comms](https://github.com/xpufx/paseo-cross-daemon-comms)                | Lets agents communicate with agents on another Paseo daemon through an MCP server.                                                                  |
| [Paseo Antigravity ACP](https://github.com/tiezbro/paseo-agy-acp)                            | Connects Google Antigravity CLI to Paseo through ACP, with Paseo-specific context, permissions, and concurrency handling.                           |
| [Desvio](https://github.com/cleiter/desvio)                                                  | Rebuilds a personal fork from a set of branches, using an agent to resolve new conflicts and Git rerere to replay known resolutions.                |

## Hosting and infrastructure

| Project                                                                     | What it does                                                                                                                                                                       |
| --------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Paseo Self-hosted](https://github.com/blockfeed/paseo-selfhosted)          | Third-party deployment of the former web UI/runtime, not a native-cutover container. Build the native daemon-only image from this checkout as described in [Docker](/docs/docker). |
| [Devbox Fleet](https://github.com/omrihaviv/devbox-fleet)                   | Provisions and maintains per-developer GCP devboxes with Paseo, coding agents, and Tailscale access.                                                                               |
| [Paseo Vercel Sandbox](https://github.com/vercel-labs/paseo-vercel-sandbox) | Creates persistent Vercel Sandboxes running a Paseo daemon with Codex, Claude Code, OpenCode, or Pi through Vercel AI Gateway. Pair each sandbox as a remote Paseo host.           |
