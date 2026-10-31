# QA

QA is the main bottleneck of Paseo's product development.

The bar is four questions:

1. Does it work well?
2. Does it regress anything else?
3. Was it tested on every platform it affects?
4. Is there automated coverage, and is that coverage real?

Every pull request needs evidence for the questions its change touches. Pull requests without evidence get closed.

For plugin changes, verify the [backend extension boundary](plugins.md#runtime-and-source-ownership).
Exercise the Rust owner and optional external-Node worker; Flutter does not load React client plugins.

## Evidence

Evidence is something someone else can look at:

- The commands you ran, pasted with their output
- The tests you added and their results
- Before and after screenshots
- A video of the interaction
- Logs, requests, responses

Redact what you need to, keep the technical details. If an agent did the work, submit its raw output. A summary drops the details someone else needs to check it.

## Does it work well

A feature that works but stutters, shifts the layout, or misaligns by two pixels is not done.

For UI, that means matching the design system in visuals, in interaction, and in performance. [design.md](design.md) is the reference.

The two things that go wrong most often:

- **Layout shift.** Content that jumps as data arrives, a row that resizes when a badge appears, a panel that reflows on first render. Watch the change on a slow connection and on first load, not just after everything is warm.
- **Alignment.** Things align to their glyphs, not to their touch targets. See the alignment rules in [design.md](design.md).

## Does it regress anything else

Paseo is composable by design, which means your change sits next to features you didn't touch. Open the surfaces around it. A change to the agent list affects archive, subagents, and tabs; a change to git actions affects worktrees and the checkout flow.

Performance is part of this. The client is native Flutter on five platforms, not a browser shell. Measure native layout/frame behavior rather than React, CSS or Chromium behavior. A desktop result does not establish phone performance.

If your change touches a hot path such as script logs, the message list or Git polling, submit before/after measurements. [Script/log performance](terminal-performance.md) and [development](development.md) describe the current boundaries.

## Every platform it affects

The client targets iOS, Android, macOS, Windows and Linux. The Rust daemon runs on desktop/server operating systems and in a native container. There is no product web client.

You aren't expected to own every device. You are expected to say what you covered:

| Platform        | Tested | Notes |
| --------------- | ------ | ----- |
| iOS             |        |       |
| Android         |        |       |
| Desktop macOS   |        |       |
| Desktop Windows |        |       |
| Desktop Linux   |        |       |

Install what you reasonably can. An iOS simulator and an Android emulator cover most of the gap on a single machine, see [development.md](development.md) and [android.md](android.md).

Use [architecture](architecture.md), [native styling](unistyles.md), [native navigation](expo-router.md), and the affected feature's current Flutter implementation. Old React/Electron source paths are not the native acceptance surface.

App and daemon versions also drift, in both directions. That has its own contract: [protocol-compatibility.md](protocol-compatibility.md).

## Automated coverage that means something

Tests are evidence only when they exercise the real thing.

- A bug fix needs a regression test that fails on the old code for the reported reason.
- A feature needs tests that go through the same interface a user or a caller uses.
- Native client flows need actual Flutter execution. Website Playwright checks cover the separate website, not the product client.
- Agent provider work needs manual verification against the real provider by someone who can tell that it worked. A fixture and a mock prove the fixture parses, nothing more.

Tests that mock away the behavior, assert on internals, or pass against the broken code claim coverage that isn't there.

[testing.md](testing.md) is the standard, including exact recorded evidence and unverified platforms. For an isolated Rust daemon, see [ad-hoc daemon testing](ad-hoc-daemon-testing.md); for devices, [mobile testing](mobile-testing.md); for the removed browser capture boundary, [native capture](browser-capture-harness.md).
