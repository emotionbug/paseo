---
title: Choosing external test tools
description: Use separately installed tools for browser testing of your projects.
nav: External test tools
order: 36
category: Native scope
---

# Choosing external test tools

There is no built-in Paseo browser in the native Flutter application. Use your normal external browser for manual web-project checks, or separately installed tools such as Playwright for automated browser tests. A project's existing test suite remains the first place to reuse coverage.

Keep these distinctions clear:

- A script running a web project's tests is a host-side process. Paseo can run the script and display read-only logs without embedding a browser.
- External automation tools may install browser binaries and use their own profiles. That is an opt-in project/tool dependency, not part of Paseo's default installed runtime.
- A remote service URL must be reachable from the machine running the browser. `localhost` on your phone is not the daemon host.
- Browser tests of a project or the documentation website are not tests of the native Flutter client.

See [external browser workflow](/docs/browser), [workspace scripts](/docs/worktrees#scripts-and-services), and [connectivity](/docs/connectivity).
