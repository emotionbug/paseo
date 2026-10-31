---
title: Workspaces
description: Understand host-scoped directories, agent sessions, and the workspaces that own their execution.
nav: Workspaces
order: 10
category: Workspaces
---

# Workspaces

The native sidebar is **host → directory → session**, including a host row for a single host and empty registered directories. Search reveals matching ancestors without replacing saved collapse state. Equal paths on different hosts remain separate.

Workspaces still own working directories, scripts, files and agent sessions. They are available through Command Center and the CLI, not as sidebar grouping rows. The native interface preserves existing workspace/session IDs, history and storage; closing a chat tab does not archive its session.

A workspace is the place where a task happens. It has a working directory and can contain multiple sessions running at the same time. In the app, each session opens as a tab.

## Projects contain workspaces

A project is a registered root on one daemon host. It can be a Git repository or another directory. Project membership and workspace identity remain durable even though the sidebar presents directories and sessions rather than workspace groups.

For example, a project's main checkout and two managed worktrees can each have sessions visible under their own host-side directory. Use Command Center or the CLI when selecting a workspace as an orchestration entity.

Each workspace is a separate place to work. You can keep one for your main checkout, create another for a feature, or open a GitHub PR as another workspace.

Use the [CLI project commands](/docs/cli#projects) to register, list, rename, or delete projects.

## Workspaces contain sessions

Agents run inside a workspace as sessions. One workspace can have several agent sessions, source/Markdown views, diffs and script/setup logs.

For example, ask one agent to implement a feature, run a service through a workspace script, and ask another agent to review the diff. The daemon retains their shared working-directory and lifecycle relationships without requiring an embedded terminal or browser.

In Paseo, the workspace is the stable container. The sessions are what you run inside it.

## Choose the isolation

When creating a workspace, choose its isolation:

- **Local** uses an existing directory, such as your main checkout. Use it when sessions should share the files already on disk.
- **Worktree** creates or opens a managed git worktree. Use it when a task needs its own directory and branch.

The workspace is the product concept; a git worktree is one way to isolate its files. More than one workspace can refer to the same managed worktree, and Paseo removes that worktree after its last workspace is archived.

## Creating a workspace

For a desktop walkthrough from starting parallel agents to reviewing and testing their changes, see [Run parallel tasks](/docs/parallel-development).

You can create a workspace in the app or from the CLI:

```bash
paseo workspace create --isolation local --path ~/dev/my-app --title main
paseo workspace create --isolation worktree --path ~/dev/my-app --base origin/main
```

You can also create a workspace without starting an agent immediately. Its working directory is ready for services, scripts and file browsing.

Add another agent to review changes or inspect a running script's read-only output. For manual shell interaction or a web project's UI, use your own external terminal/browser. The native client has no interactive terminal emulator or embedded browser.

Creating an agent and creating a workspace are separate actions. Pass a workspace ID when you want an agent in a specific existing workspace. A bare `paseo run` from a human shell creates a new local workspace; when one agent runs it, Paseo recognizes the caller and creates a subagent in the caller's workspace.

## Worktrees

Every workspace in Paseo is backed by a working directory. When that directory is a git worktree, you get a separate branch and isolated environment for each task.

If you want the details on configuring setup hooks, scripts, and services, continue to [Git worktrees](/docs/worktrees).
