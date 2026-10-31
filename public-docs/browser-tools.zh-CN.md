---
title: 浏览器工具边界
description: 浏览器自动化不属于原生 Paseo 工具集。
nav: 浏览器工具边界
---

# 浏览器工具边界 {#browser-tool-boundary}

原生产品不提供内嵌浏览器宿主。旧 `browser_*` 操作，包括打开标签页、快照/截图、点击、输入、执行页面脚本和读取浏览器日志，都不是原生客户端支持的功能。保留的配置字段或旧工具名称并不说明存在可用的浏览器。

不要指示 Agent 调用旧 Paseo 浏览器工具、启用浏览器宿主、反复重试等待桌面浏览器连接，或隐式安装 Chromium 作为替代。Flutter 应用没有可执行浏览器/React UI 界面。

保留的 [MCP 工具](/docs/mcp)提供支持的 Agent、工作区、脚本、计划任务和已连接主机操作。Agent 可以使用用户另外配置的浏览器测试 CLI 或 MCP 服务器，但这些工具有自己的参数、进程归属、凭据和安全规则。请阅读相应工具文档，不要复用旧 Paseo `browserId` 或元素引用约定。

支持的工作流见[外部浏览器测试](/docs/browser)和[并行任务](/docs/parallel-development)。
