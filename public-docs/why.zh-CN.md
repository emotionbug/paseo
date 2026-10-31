---
title: 为什么选择 Paseo？
description: 一个完整的 Agent 开发环境，用于运行编程 Agent、编辑文件、审查改动和测试你的工作。
nav: 为什么选择 Paseo？
---

# 为什么选择 Paseo？ {#why-paseo}

Paseo 是面向 Agent 开发的原生环境：运行编程 Agent、编辑源码、阅读 Markdown、审查 diff 和 PR，并查看脚本输出。Flutter 客户端连接 Rust daemon，由 daemon 管理工作并跨设备保留状态。

## 在一个工作区中开发 {#develop-in-one-workspace}

打开原生桌面客户端，选择主机和目录，然后开始会话。对话、编辑器、摘要优先的 diff 和只读脚本日志一起使用。侧边栏按主机 → 目录 → 会话组织；工作区仍负责 worktree 与编排。

客户端目标平台为 Linux、macOS、Windows、Android 和 iOS。桌面端可管理本地 Rust daemon；移动端远程连接。此次原生迁移尚未发布，请按[源码构建说明](/docs)使用，不要假定现有上游下载已包含它。

## 并行运行任务 {#run-tasks-in-parallel}

为相互独立的任务分配各自的 worktree 和分支。同时运行多个 Agent，检查每份 diff，测试每个实现，再决定合并哪个。你可以为每个任务使用同一个 Provider，也可以为每个任务选择不同的 Provider 和模型。

配置[工作区脚本](/docs/worktrees#scripts-and-services)，为每个 worktree 在不同端口上启动开发服务器。[试试并行开发工作流](/docs/parallel-development)。

## 使用你的 Agent 和工具 {#use-your-agents-and-tools}

Paseo 运行你已经在用的编程 Agent，沿用你的订阅、凭据、配置、技能和 MCP 服务器。你的 Agent 使用你机器上的文件和工具。参见[支持的 Provider](/docs/supported-providers)。

Agent 可以通过[文字转语音](/docs/voice)对你说话，默认在本地运行，也可以在设置中选择云端 Provider。原生客户端不采集麦克风输入。

## 在另一台设备上继续 {#continue-from-another-device}

daemon 持有正在运行的 Agent 和工作区。原生桌面/移动客户端与 Rust CLI 连接同一个环境。现有 ID、历史、配置和主机侧文件保持不变。

可以在笔记本电脑上使用，也可连接另一台机器。Android 和 iOS 提供远程聊天、文件、diff 和只读脚本日志，不运行 daemon。通过直连或可选端到端加密中继[连接设备](/docs/connectivity)。产品没有网页客户端、内嵌浏览器/自动化或交互式终端界面；需要时使用自己的外部浏览器/终端。

## 自动化并扩展你的工作流 {#automate-and-extend-your-workflow}

为 Agent 提供 [Paseo 工具](/docs/orchestration)，让它们创建 worktree、启动其他 Agent、发送提示词并收集结果。使用 [CLI](/docs/cli) 或 [TypeScript SDK](/docs/sdk) 自动化 Agent 和工作区操作。

[后端扩展](/docs/plugins)通过可选的外部 Node worker 添加 Provider、RPC、生命周期行为和用量集成。内置集成使用 Rust，不支持可执行 React UI 插件。默认安装运行时不包含 Node、Electron 或 Chromium。[Hub](/docs/hub)将外部事件连接到你机器上的 Agent。

Paseo 源码采用 Apache-2.0，没有遥测和跟踪，也不要求 Paseo 账号。依赖项遵循各自许可证。生成的声明不能解决原生语音库的源码和许可证分发义务；分发二进制文件前请阅读仓库 `docs/release.md`。模型权重按需下载，不随运行时捆绑。
