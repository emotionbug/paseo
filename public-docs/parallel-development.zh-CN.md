---
title: 运行并行任务
description: 在不同 worktree 运行 Agent，审查摘要优先的 diff，并在原生客户端查看脚本输出。
nav: 运行并行任务
---

# 运行并行任务 {#run-parallel-tasks}

使用原生客户端为多个编程 Agent 分配独立任务、审查改动并运行项目脚本。每个任务可使用拥有独立 Git worktree 和分支的工作区，同时查看对话、源码/Markdown、diff 和只读脚本日志。

开始之前，请准备好[桌面应用](/docs#desktop-app-recommended)、一个本地 git 仓库，以及至少一个[已安装并完成认证的编程 Agent](/docs/supported-providers)。下面的示例使用一个带有 `origin/main` 分支的仓库。

## 启动两个任务 {#start-two-tasks}

1. 打开 **新建 workspace** 并选择你的项目。
2. 把 **隔离** 设置为 **新建 worktree**，并选择 `origin/main` 作为起始 ref。
3. 选择 Agent 和模型，然后提交一个任务，例如：“为项目列表添加筛选功能。运行相关测试。”
4. 从同一个起始 ref 用 **新建 worktree** 创建第二个工作区。给它的 Agent 另一个任务：“为设置页面添加键盘导航。运行相关测试。”

两个 Agent 同时运行。可在侧边栏的主机和 worktree 目录下选择会话，或通过 Command Center 打开工作区，查看进度和发送后续提示词。两个任务可使用相同或不同的 Provider。

不同的 worktree 让各自的文件修改保留在不同的分支上。如果想得到同一功能的两种实现，可以给两个 Agent 相同的任务，比较结果后再选择一个。

## 审查每个改动 {#review-each-change}

选择一个工作区，从 Command Center 打开 **变更** 查看它的 diff。打开 **文件** 浏览和编辑源码。使用分栏，让 Agent 对话和你正在审查的代码并排显示。

把修改意见发送给该工作区的 Agent。如果想听听别的意见，在同一个工作区中使用 **新建 Agent**，让它在不编辑文件的前提下审查改动。同一工作区中的 Agent 共享它的工作目录。

## 运行并测试应用 {#run-and-test-the-app}

通过配置好的工作区脚本运行项目测试/开发命令，或让 Agent 执行，再在 Paseo 查看只读输出。手动交互式 shell 操作请使用 daemon 主机上的外部终端；原生应用没有交互式终端模拟器。

每个 worktree 都有自己的文件，但进程仍然共享机器的网络端口。请为每个正在运行的副本分配不同的开发服务器端口。要让 setup 和服务端口可重复配置，请设置 [worktree setup 和脚本](/docs/worktrees#paseojson)。

网页项目请在外部浏览器打开服务 URL，并确保浏览器所在机器能够访问；其他设备上的 localhost 不是 daemon 主机。

自动化浏览器检查使用项目另外安装的测试工具，或显式配置的外部 MCP 服务。Paseo 不内嵌浏览器，也不注入旧浏览器自动化工具。向外部工具提供具体步骤并审查真实结果，不要把未执行的测试计划当作成功。

## 选择要合并的内容 {#choose-what-to-merge}

审查每个工作区的 diff 和实际测试结果。提交或创建 PR 前先提出剩余修改，可使用 Agent、原生 Git 操作或自己惯用的外部主机终端。

对于相互独立的任务，审查并合并每个分支。对于相互竞争的实现，选择你想保留的版本。不再需要某个工作区时就把它归档；[worktree 清理](/docs/worktrees#manage-the-workspace)会在最后一个工作区归档后删除受管理的目录。

## 让 Agent 协调这些任务 {#let-an-agent-coordinate-the-tasks}

如果你想让 Agent 创建工作区、启动执行者并收集结果，请使用[编排](/docs/orchestration)。执行者会出现在 Paseo 中，你可以在它们工作时查看对话和改动。
