---
title: 工作区
description: 了解按主机区分的目录、Agent 会话及拥有执行生命周期的工作区。
nav: 工作区
---

# 工作区 {#workspaces}

原生侧边栏按 **主机 → 目录 → 会话** 组织，包括只有一个主机时的主机行和空的已注册目录。搜索临时展开匹配的上级，不覆盖保存的折叠状态。不同主机上的相同路径保持独立。

工作区仍拥有工作目录、脚本、文件和 Agent 会话，可通过 Command Center 或 CLI 访问，但不是侧边栏分组行。原生界面保留现有工作区/会话 ID、历史和存储；关闭聊天标签页不会归档会话。

## 项目包含工作区 {#projects-contain-workspaces}

项目是某个 daemon 主机上注册的根目录，可以是 Git 仓库或其他目录。即使侧边栏显示目录和会话，项目归属与工作区身份仍持久保存。

例如，一个项目的主检出和两个受管理 worktree 都可在各自的主机目录下显示会话。需要选择工作区作为编排实体时，使用 Command Center 或 CLI。

每个工作区都是一个独立的工作场所。你可以为主检出保留一个，为某个功能再创建一个，或者把一个 GitHub PR 作为另一个工作区打开。

使用 [CLI 项目命令](/docs/cli#projects)注册、列出、重命名或删除项目。

## 工作区包含会话 {#workspaces-contain-sessions}

Agent 以会话形式在工作区中运行。一个工作区可以有多个 Agent 会话、源码/Markdown 视图、diff 和脚本/设置日志。

例如，让一个 Agent 实现功能，通过工作区脚本运行服务，再让另一个 Agent 审查 diff。daemon 保留它们共享的工作目录和生命周期关系，不需要内嵌终端或浏览器。

在 Paseo 中，工作区是稳定的容器，会话是你在其中运行的东西。

## 选择隔离方式 {#choose-the-isolation}

创建工作区时选择隔离方式：

- **本地** 使用一个现有目录，例如你的主检出。当会话应该共享磁盘上已有的文件时使用。
- **新建 worktree** 创建或打开一个受管理的 git worktree。当任务需要自己的目录和分支时使用。

工作区是产品概念；git worktree 是隔离其文件的一种方式。多个工作区可以指向同一个受管理的 worktree，Paseo 会在最后一个引用它的工作区被归档后删除该 worktree。

## 创建工作区 {#creating-a-workspace}

关于从启动并行 Agent 到审查和测试其改动的桌面端完整流程，请参见[运行并行任务](/docs/parallel-development)。

你可以在应用中或通过 CLI 创建工作区：

```bash
paseo workspace create --isolation local --path ~/dev/my-app --title main
paseo workspace create --isolation worktree --path ~/dev/my-app --base origin/main
```

可以先创建工作区而不立即启动 Agent。其工作目录已可用于服务、脚本和文件浏览。

之后可添加审查改动的 Agent，或查看运行中脚本的只读输出。手动 shell 操作或查看网页项目界面时，请使用自己的外部终端/浏览器。原生客户端没有交互式终端模拟器或内嵌浏览器。

创建 Agent 和创建工作区是两个独立的操作。如果你想让 Agent 位于某个已有的工作区，请传入工作区 ID。在人工操作的 shell 中直接运行 `paseo run` 会创建一个新的本地工作区；当某个 Agent 运行它时，Paseo 会识别调用者，并在调用者的工作区中创建一个子 Agent。

## Worktree {#worktrees}

Paseo 中的每个工作区都由一个工作目录支撑。当这个目录是 git worktree 时，每个任务都会获得独立的分支和隔离的环境。

如果想了解配置 setup 钩子、脚本和服务的细节，请继续阅读 [Git worktree](/docs/worktrees)。
