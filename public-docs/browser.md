---
title: External browser workflow
description: Test web projects outside the native Paseo client.
nav: External browser
order: 35
category: Native scope
---

# External browser workflow

The native Flutter client does **not** embed a browser or browser automation. It does not ship Electron, Chromium, webview tabs, a shared browser profile, or an agent-controlled browser surface. There is no setting that enables the former Paseo browser tools in this client.

You can still develop and test web projects:

1. Start the project's development service through a [workspace script](/docs/worktrees#scripts-and-services) or your own host terminal.
2. Inspect its read-only output in Paseo and use its reachable service URL.
3. Open that URL in an external browser, or run an independently installed browser-testing tool in your project/CI environment.
4. Review the resulting files, test output and screenshots alongside the agent conversation and diff.

An external testing tool owns its own installation, processes, credentials, cookies and permissions. It is not bundled with Paseo, does not inherit a Paseo browser login, and must not be advertised as a built-in capability. If an agent uses such a tool through its own CLI or MCP configuration, authorize its consequential actions at that tool's boundary.

Paseo retains native Markdown/source editing, summary-first diffs, remote folder selection, and script/setup PTYs with read-only logs. See [parallel tasks](/docs/parallel-development), [MCP](/docs/mcp), and [native setup](/docs).
