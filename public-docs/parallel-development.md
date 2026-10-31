---
title: Run parallel tasks
description: Run agents in separate worktrees, review summary-first diffs, and inspect script output in the native client.
nav: Run parallel tasks
order: 12
category: Workspaces
---

# Run parallel tasks

Use the native client to give several coding agents independent tasks, review their changes, and run project scripts. Each task can use a workspace with its own Git worktree and branch, alongside conversations, source/Markdown views, diffs and read-only script logs.

Start with the [desktop app](/docs#desktop-app-recommended), a local git repository, and at least one [coding agent installed and authenticated](/docs/supported-providers). The examples below use a repository with an `origin/main` branch.

## Start two tasks

1. Open **New workspace** and select your project.
2. Set **Isolation** to **New worktree** and choose `origin/main` as the starting ref.
3. Choose an agent and model, then submit a task, for example: “Add filtering to the project list. Run the relevant tests.”
4. Create a second workspace with **New worktree** from the same starting ref. Give its agent another task: “Add keyboard navigation to the settings page. Run the relevant tests.”

Both agents run concurrently. Select their sessions under the host and worktree directory in the sidebar, or open a workspace through Command Center, to read progress and send follow-ups. Choose the same provider for both tasks or a different provider for each.

Separate worktrees keep their file edits on separate branches. For two alternative implementations of the same feature, give both agents the same task and compare their results before choosing one.

## Review each change

Select a workspace and open **Changes** from the Command Center to inspect its diff. Open **Files** to browse and edit the source. Use split panes to keep the agent conversation beside the code you're reviewing.

Send corrections to that workspace's agent. To get another opinion, use **New agent** in the same workspace and ask it to review the changes without editing files. Agents in the same workspace share its working directory.

## Run and test the app

Run the project's test or development command through a configured workspace script or ask the agent to run it. Inspect the script's read-only output in Paseo. For manual interactive shell work, use your own external terminal on the daemon host; the native app has no interactive terminal emulator.

Each worktree has its own files, but processes still share the machine's network ports. Assign a different dev-server port to each running copy. For repeatable setup and service ports, configure [worktree setup and scripts](/docs/worktrees#paseojson).

For a web project, open the service URL in an external browser. Ensure the URL is reachable from that browser's machine; `localhost` on another device is not the daemon host.

Automated browser checks use your project's separately installed test tools or an explicitly configured external MCP service. Paseo does not embed a browser or inject its former browser-automation tools. Give any external tool concrete verification steps and review its actual results rather than treating an unexecuted test plan as success.

## Choose what to merge

Review each workspace's diff and actual test results. Ask for remaining changes before committing or opening a pull request, using the agent, native Git actions or your usual external host terminal.

For independent tasks, review and merge each branch. For competing implementations, choose the version you want to keep. Archive a workspace when you no longer need it; [worktree cleanup](/docs/worktrees#manage-the-workspace) removes the managed directory after its last workspace is archived.

## Let an agent coordinate the tasks

When you want an agent to create the workspaces, launch workers, and collect their results, use [orchestration](/docs/orchestration). The workers appear in Paseo so you can inspect their conversations and changes as they work.
