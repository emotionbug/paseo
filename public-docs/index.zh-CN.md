---
title: 快速上手
description: 从当前源码构建原生 Paseo 客户端和 Rust daemon。
nav: 快速上手
---

# 快速上手 {#getting-started}

Paseo Sessions 使用 Flutter 构建 **Linux、macOS、Windows、Android 和 iOS** 原生客户端，daemon 与 CLI 使用 **Rust**。桌面或服务器运行 Agent，Android 和 iOS 远程连接。客户端保留 Markdown、源码编辑、摘要优先的 diff、远程文件夹选择和只读脚本日志，侧边栏按 **主机 → 目录 → 会话** 组织。

**此次原生迁移尚未发布。** 现有上游下载和应用商店版本不代表包含这些改动。请从迁移后的源码检出构建本地版本。分发二进制文件前必须解决仓库 `docs/release.md` 中记录的分发要求，包括原生语音库的源码和许可证义务。

## 前置条件 {#prerequisites}

在 daemon 所在机器安装并认证至少一个 Provider CLI。Paseo 管理这些程序，但不提供你的账号凭据。Git 以及相关代码托管操作所需的 CLI 也应配置好。

源码构建需要 Rust/Cargo，以及作为**构建工具**使用的 Node 24/npm。客户端还需要 Flutter 和目标平台的原生工具链。桌面构建须在相应操作系统上运行；iOS 需要 macOS/Xcode。默认安装运行时不捆绑 Node；可选 JavaScript 后端扩展使用另外安装的 Node。

## 桌面应用 {#desktop-app-recommended}

在仓库根目录运行：

```sh
npm ci
npm run dev:desktop
```

此命令构建并运行原生桌面客户端，使用当前检出的 `.dev/paseo-home` 和端口 `6768`，不会替换已安装的应用和 daemon。优化构建使用 `npm run build:flutter -- linux`、`macos` 或 `windows`，须在对应主机执行。构建与打包产物位于 `artifacts/flutter/`；本地构建不等于已发布的签名安装包。

## 服务器 / CLI {#server--cli}

构建原生运行时，并保留可执行文件旁的全部资源：

```sh
npm ci
npm run build:daemon
./target/debug/paseo onboard
./target/debug/paseo daemon start
./target/debug/paseo daemon status
```

Windows 使用 `paseo.exe`；设置 `CARGO_TARGET_DIR` 时使用对应路径。`npm run build:daemon:release` 在 release 目标目录生成优化的本地运行时；优化构建本身不赋予正式发布身份。

daemon 通常监听 `ws://127.0.0.1:6767/ws`。配置和持久状态仍在 `PASEO_HOME`（默认 `~/.paseo`），也可通过 `--home` 指定。`paseo onboard` 显式执行设置；直接运行 `paseo` 打开 CLI 聊天，而不是安装 daemon。隔离的开发 daemon 使用 `npm run dev:daemon`。

若从经过单独授权的分发渠道获得原生 daemon 压缩包，应解压**整个**压缩包，保留库、技能和 worker 资源，再运行可执行文件。不要通过 npm 安装旧 Node CLI，也不要假定旧 npm 包提供 Rust 运行时。

## 移动客户端 {#mobile-clients}

```sh
npm run build:apk:arm64
npm run build:ios
```

Android 包名为 `sh.paseo.sessions`。产物在 `artifacts/flutter/`；未配置正式签名或描述文件时，产物明确标记为调试签名或未签名，不是商店提交包。客户端需添加远程 daemon 的可达地址和密码。手机的 localhost 并不是 daemon 主机。

## Docker {#docker}

从当前源码构建仅含原生 daemon 的镜像：

```sh
docker build -f docker/base/Dockerfile -t paseo:local .
docker run -d --name paseo-native \
  -p 127.0.0.1:6767:6767 \
  -e PASEO_PASSWORD=change-me \
  -v "$PWD/paseo-home:/home/paseo" \
  -v "$PWD/workspace:/workspace" \
  paseo:local
```

请设置真实密码。通过原生客户端或 CLI 连接；HTTP 根路径**不提供网页客户端**。Agent CLI 需另外提供。现有镜像仓库中的版本不代表此次原生迁移已发布。

## 运行范围 {#runtime-scope}

原生产品没有网页客户端、Electron/Chromium、内嵌浏览器或浏览器自动化、交互式终端界面，也不执行 React UI 插件。工作区和服务脚本仍在 daemon 管理的 PTY 中运行，客户端显示只读日志。后端扩展及外部 TypeScript SDK 仍受支持。

实际 Linux 验证与未验证的平台边界记录在仓库 `docs/testing.md`。CI 矩阵定义不等于设备测试通过。原生包不包含语音模型权重，模型按需下载。

连接与开发详情见[连接](/docs/connectivity)、[CLI 参考](/docs/cli)、[TypeScript SDK](/docs/sdk)，以及源码中的 `README.md` 和 `docs/development.md`。
