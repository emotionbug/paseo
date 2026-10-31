---
title: Browser tool boundary
description: Browser automation is not part of the native Paseo toolset.
nav: Browser tool boundary
order: 37
category: Native scope
---

# Browser tool boundary

The native product does not expose an embedded browser host. Former `browser_*` operations for opening tabs, taking snapshots/screenshots, clicking, typing, evaluating page scripts and reading browser logs are not supported native client features. A saved configuration key or an old tool name is not evidence that a browser exists.

Do not instruct agents to call the old Paseo browser tools, enable browser hosting, retry until a desktop browser connects, or install Chromium as an implicit fallback. The Flutter app has no executable browser/React UI surface.

Paseo's retained [MCP tools](/docs/mcp) cover supported agent, workspace, script, schedule and connected-host operations. An agent may separately use a browser-testing CLI or MCP server configured by its user, but those tools have their own schemas, process ownership, credentials and safety rules. Consult that tool's documentation rather than reusing retired Paseo `browserId`/element-ref contracts.

For the supported workflow, see [external browser testing](/docs/browser) and [parallel tasks](/docs/parallel-development).
