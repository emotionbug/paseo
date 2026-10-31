---
title: TypeScript SDK
description: Run Paseo coding agents from a TypeScript program.
nav: Overview
order: 50
category: TypeScript SDK
---

# TypeScript SDK

`@getpaseo/client` is a TypeScript library that drives a Paseo daemon from your own program. You pick a provider and model, give an agent a prompt and a directory, and wait for the answer.

The daemon does the work: it launches the provider CLI, keeps the session alive, and streams it to the Paseo app. Your program is a client. Agents you create show up in Paseo next to the ones you started by hand, and they stay there after your program exits.

Use it to:

- turn an issue, alert, or webhook into a coding task;
- run several agents in parallel and collect their answers;
- hold a session open and send follow-up prompts;
- build a dashboard over your agents.

## Start a daemon

The SDK remains a JavaScript/TypeScript client of the **Rust** daemon. Install/build the native runtime as described in [Getting started](/docs), then use its executable:

```bash
paseo onboard
paseo daemon start
```

The default endpoint is `ws://127.0.0.1:6767/ws`. Bare `paseo` opens CLI chat; it does not install or start a daemon. This native cutover has not been published, so an old npm CLI package is not its installer. Source builds are available from the migration checkout. Keep all runtime assets with the executable.

The external SDK still uses `@getpaseo/client` (`packages/client` in the repository); using Node for your integration does not make Node part of the default installed Paseo runtime.

## Run an agent

```ts
import { createPaseoClient } from "@getpaseo/client";

const client = createPaseoClient({ url: "ws://127.0.0.1:6767/ws" });
await client.connect();

const agent = await client.agents.create({
  config: { provider: "codex/gpt-5.5" },
  cwd: "/Users/me/dev/storefront",
  prompt: "Review the current diff and name the riskiest change.",
});

const result = await agent.waitForFinish();
console.log(result.lastMessage);

await client.close();
```

[Quickstart](/docs/sdk/quickstart) walks through this line by line and covers passwords and remote daemons.
