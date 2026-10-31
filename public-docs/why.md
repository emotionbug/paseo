---
title: Why Paseo?
description: A full agentic development environment for running coding agents, editing files, reviewing changes, and testing your work.
nav: Why Paseo?
order: 2
category: Getting started
---

# Why Paseo?

Paseo is a native environment for agent-driven development: run coding agents, edit source, read Markdown, review diffs and pull requests, and inspect script output. Flutter clients connect to a Rust daemon that owns the work and preserves it across devices.

## Develop in one workspace

Open the native desktop client, choose a host and directory, and start a session. Keep the conversation, editor, summary-first diff and read-only script logs together. The sidebar is host → directory → session; workspaces still organize worktrees and orchestration.

The client targets Linux, macOS, Windows, Android and iOS. Desktop can manage a local Rust daemon; mobile connects remotely. This native cutover is not yet published; use the [source-build setup](/docs) rather than assuming existing upstream downloads contain it.

## Run tasks in parallel

Give independent tasks their own worktrees and branches. Run several agents at once, inspect each diff, test each implementation, and choose what to merge. Use the same provider for every task or choose a different provider and model for each.

Configure [workspace scripts](/docs/worktrees#scripts-and-services) to start development servers with separate ports for each worktree. [Try the parallel development workflow](/docs/parallel-development).

## Use your agents and tools

Paseo runs the coding agents you already use, with your subscriptions, credentials, configuration, skills, and MCP servers. Your agents work with the files and tools on your machine. See [supported providers](/docs/supported-providers).

Agents can speak to you through [text-to-speech](/docs/voice) that runs locally by default, with cloud providers available in settings. The native clients do not capture microphone input.

## Continue from another device

The daemon owns running agents and workspaces. Native desktop/mobile clients and the Rust CLI connect to the same environment. Existing IDs, history, configuration and host-side files are preserved.

Use Paseo on your laptop or connect to another machine. Android and iOS provide remote chat, files, diffs and read-only script logs, without running a daemon. [Connect your devices](/docs/connectivity) directly or through the optional end-to-end encrypted relay. There is no product web client, embedded browser/automation or interactive terminal UI; use your own external browser/terminal when needed.

## Automate and extend your workflow

Give agents [Paseo tools](/docs/orchestration) to create worktrees, launch other agents, send prompts, and collect results. Use the [CLI](/docs/cli) or [TypeScript SDK](/docs/sdk) to automate agent and workspace operations.

[Backend extensions](/docs/plugins) add providers, RPCs, lifecycle behavior and usage integrations through an optional external-Node worker. Built-ins run in Rust; executable React UI plugins are not supported. The default installed runtime includes neither Node, Electron nor Chromium. [Hub](/docs/hub) connects external events to agents running on your machines.

Paseo source is Apache-2.0, with no telemetry, tracking or required Paseo account. Dependency licenses remain their own. Generated notices do not resolve the native speech libraries' source/licensing distribution requirements; consult the checkout's `docs/release.md` before distributing binaries. Model weights are downloaded on demand, not bundled.
