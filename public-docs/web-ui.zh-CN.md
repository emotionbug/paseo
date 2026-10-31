---
title: 原生客户端与自托管
description: 原生客户端连接自托管 Rust daemon，不包含网页界面。
nav: 原生客户端
---

# 原生客户端与自托管 {#native-clients-and-self-hosting}

Rust daemon **不捆绑或提供产品网页客户端**。原生客户端使用 Flutter，支持 Linux、macOS、Windows、Android 和 iOS。文档网站与产品应用是两个独立项目。

不要把启用 `features.webUi`、设置 `PASEO_WEB_UI_ENABLED` 或访问 `http://localhost:6767/` 当作安装步骤；这些旧操作不会生成原生客户端。HTTP/WebSocket API、MCP 和工作区服务代理仍是 daemon 功能，但不代表包含浏览器 UI。

## 自托管 daemon {#self-host-the-daemon}

按照[快速上手](/docs)从当前源码构建原生 daemon 及资源，再运行显式设置和启动命令：

```sh
paseo onboard
paseo daemon start
paseo daemon status
```

请使用实际构建的可执行文件或安装路径。直接运行 `paseo` 打开 CLI 聊天，不执行设置。生成的运行时库、技能和可选 worker 资源必须保留在可执行文件旁。此次原生迁移尚未发布，不能假定现有 npm 包或上游下载已包含它。

配置和历史仍位于 `PASEO_HOME`（默认 `~/.paseo`），或显式指定的 `--home`。桌面客户端可管理本地 daemon；Android/iOS 连接其他机器上的 daemon。多个原生客户端和外部 SDK 程序可使用同一个 daemon，Agent 不会随客户端迁移到手机。

## 安全连接 {#connect-safely}

在原生客户端添加 daemon 主机。可使用端到端加密中继、可信 VPN 或受保护的直连。暴露端口前先配置认证。TLS/反向代理需转发 WebSocket 升级并正确处理主机和协议；仅有密码认证并不加密传输。

网络与持久化详情见[连接](/docs/connectivity)、[配置](/docs/configuration)、[安全](/docs/security)和 [Docker](/docs/docker)。Docker 实例仅运行 daemon，不提供自托管浏览器应用。

## 审查与测试 {#review-and-test-work}

原生客户端提供聊天、Markdown、源码编辑、远程文件夹、摘要优先的 diff、计划任务和只读脚本/设置日志。工作区服务仍可运行你自己项目的网页应用，请在外部浏览器打开其 URL；这与 Paseo 网页客户端或内嵌自动化不同。
