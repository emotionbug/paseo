---
title: 外部浏览器工作流
description: 在原生 Paseo 客户端之外测试网页项目。
nav: 外部浏览器
---

# 外部浏览器工作流 {#external-browser-workflow}

原生 Flutter 客户端**不内嵌浏览器或浏览器自动化**，不附带 Electron、Chromium、webview 标签页、共享浏览器配置或 Agent 控制的浏览器界面。没有可以启用旧 Paseo 浏览器工具的开关。

你仍然可以开发和测试网页项目：

1. 通过[工作区脚本](/docs/worktrees#scripts-and-services)或主机自己的终端启动开发服务。
2. 在 Paseo 查看只读输出，获取可访问的服务 URL。
3. 在外部浏览器打开该 URL，或在项目/CI 环境运行单独安装的浏览器测试工具。
4. 结合 Agent 对话和 diff 审查文件、测试输出及截图。

外部测试工具自行负责安装、进程、凭据、Cookie 和权限。它不随 Paseo 捆绑，不继承 Paseo 浏览器登录状态，也不是内置功能。如果 Agent 通过自己的 CLI 或 MCP 配置使用这些工具，应在相应工具的权限边界授权有实际后果的操作。

Paseo 保留原生 Markdown/源码编辑、摘要优先的 diff、远程文件夹选择，以及只读日志形式的脚本/设置 PTY。参见[并行任务](/docs/parallel-development)、[MCP](/docs/mcp)和[原生设置](/docs)。
