# Paseo Sessions

[English](README.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

Paseo Sessions 是运行和审阅编程智能体工作的原生环境。Flutter 客户端面向 **Linux、macOS、Windows、Android 和 iOS**；守护进程与 `paseo` CLI 使用 Rust。桌面客户端可以管理本地守护进程，移动客户端仅连接其他机器上的守护进程。

当前检出版本为 **0.13.0**，原生迁移**尚未发布**。[上游下载页](https://paseo.sh/zh-CN/download)和[历史 GitHub releases](https://github.com/getpaseo/paseo/releases)不是此次迁移的构建产物。请从本检出源码构建，并在分发前阅读[发布要求](docs/release.md)。

## 跨主机工作与功能范围

侧栏按 **主机 → 目录 → 会话**组织。即使仅配置一台主机，也有独立的主机行。空目录会保留，会话按最近活动排序；主机和目录可独立折叠，目录折叠状态会持久保存。不同主机上的相同路径不会合并。

搜索匹配主机、目录名称和路径、会话标题及提供商，并展开匹配项的祖先节点，不覆盖已保存的折叠偏好。目录旁的加号在对应主机和检出目录内创建会话。“添加项目”使用主机端文件夹浏览器，远程路径不会通过本机系统文件选择器选择。工作区仍用于编排、worktree、脚本和 Command Center 导航，但不再是侧栏分组控件。关闭聊天标签不会归档会话；现有会话 ID、历史、文件、配置与存储保留。

- 智能体聊天、权限、提供商/模型选择、配置档、会话导入和历史。
- Markdown 渲染、文件浏览与源码编辑；Git diff 先显示摘要，按需加载补丁正文。
- 工作区初始化和服务脚本由守护进程在 PTY 中运行；应用仅显示**只读日志**，不提供交互式终端模拟器。
- 定时任务、MCP 工具与服务器、语音、已连接主机和远程目录选择。
- 原生桌面集成与本地守护进程生命周期管理；移动端仅远程连接。

默认安装运行时**不捆绑 Node、Electron 或 Chromium**；不提供产品 Web 客户端、嵌入式浏览器、浏览器自动化或可执行 React UI 插件兼容性。外部链接在应用外打开，网站是独立的文档/营销项目。智能体 CLI 是外部程序，可能有自己的运行时要求。

内置提供商与用量集成使用 Rust。可选 JavaScript **后端**插件需要外部安装的 Node 20+，可通过 `PASEO_PLUGIN_NODE` 指定；其编译器和 SDK 资源不等于捆绑 Node 守护进程。参见[插件文档](docs/plugins.md)。

## 从源码构建和运行

安装 Rust/Cargo、Flutter SDK、目标平台原生工具链，以及构建工具所需的 **Node 24 和 npm**。至少安装并配置一个智能体 CLI，例如 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Copilot](https://github.com/features/copilot/cli/)、[OpenCode](https://github.com/anomalyco/opencode) 或 [Pi](https://pi.dev)。完整前置条件见[开发指南](docs/development.md)。

```sh
npm ci
npm run build:daemon
npm run dev:desktop
```

也可分别启动开发守护进程和原生客户端：

```sh
npm run dev:daemon
# 在另一个终端中：
npm run dev:app
```

开发环境使用本检出的 `.dev/paseo-home` 和端口 `6768`，与正常运行的 `~/.paseo`、端口 `6767` 隔离。构建包装脚本使用 Node 工具启动 Rust，并非 Node 服务器。

```sh
npm run build:flutter -- linux
npm run build:flutter -- macos
npm run build:flutter -- windows
npm run build:apk
npm run build:apk:arm64
npm run build:ios
```

桌面构建须在对应操作系统上执行，iOS 需要 macOS/Xcode。产物和构建描述写入 `artifacts/flutter/`。Android/iOS 不含守护进程；Android 名称为 **Paseo Sessions**，包名 `sh.paseo.sessions`。调试签名 APK 或未签名 iOS 应用归档不等于可上架版本，参见 [Android](docs/android.md) 与[发布指南](docs/release.md)。

无头主机可使用生成的 `target/debug/paseo`（Windows 为 `paseo.exe`）；设置 `CARGO_TARGET_DIR` 时使用对应目录。必须保留相邻生成的运行时资源，无需 npm 全局安装 CLI：

```sh
./target/debug/paseo onboard
./target/debug/paseo daemon start
./target/debug/paseo daemon status
```

`build:daemon` 生成调试构建及资源；`npm run build:daemon:release` 生成 `target/release/` 下的优化**本地构建**及资源，不代表经过发布验证的产物。`paseo onboard` 执行显式设置；直接运行 `paseo` 打开 CLI 聊天，不执行安装或引导。

正常数据目录为 `~/.paseo`，可通过 `PASEO_HOME` 或 `--home` 覆盖。`config.json`、智能体历史、项目/工作区元数据与连接凭据沿用原有持久化约定。用 `--host` 连接远程守护进程，需要时设置 `PASEO_PASSWORD`。SSH 传输连接已存在的守护进程，不会代为安装。[Docker 指南](docs/docker.md)构建的是原生守护进程容器，不在 HTTP 根路径提供应用。

## CLI 与 SDK

在主机设置中的智能体配置档上选择 **Use for CLI**，可设置默认提供商、模型、模式和思考级别。显式 CLI 选项优先；冲突的提供商/模型参数会在连接前报错。

```sh
paseo
paseo --resume <session-id>
paseo run --provider codex "review this checkout"
paseo ls
paseo attach <session-id>
paseo send <session-id> "also add tests"
paseo import --provider pi <native-session-id>
paseo run --host workstation.local:6767 --cwd /workspace --provider codex "review the diff"
```

TypeScript SDK `@getpaseo/client` 位于 `packages/client`，用于外部集成；共享协议定义位于 `packages/protocol`。Rust 守护进程和 Dart 客户端实现同一通信协议，使用 SDK 不意味着产品运行时依赖 Node。

## 源码与验证范围

| 部分                                                       | 路径                                                    |
| ---------------------------------------------------------- | ------------------------------------------------------- |
| Rust 守护进程、CLI、提供商、存储、supervisor、桌面主机 IPC | `packages/daemon`                                       |
| 五平台 Flutter 客户端                                      | `packages/flutter_app`                                  |
| 桌面打包与运行时资源                                       | `packages/flutter_desktop_host`                         |
| 原生音频集成                                               | `packages/flutter_audio`                                |
| TypeScript SDK 与协议                                      | `packages/client`、`packages/protocol`                  |
| 可选扩展 SDK / 私有后端 worker                             | `packages/plugin` / `packages/daemon/plugin-worker/src` |
| 文档网站                                                   | `packages/website`                                      |

修改运行时边界前请阅读[架构](docs/architecture.md)、[开发](docs/development.md)、[测试](docs/testing.md)和[发布](docs/release.md)文档。Linux 已有原生守护进程/客户端和桌面主机生命周期的实际执行证据；Android ARM64 release 模式构建已通过，但使用调试签名，未安装或在设备上运行。此次迁移未在 Windows、macOS 或 iOS 实际运行。跨平台 CI 定义不等于执行证据，桌面后续验证及完整限制以测试文档为准。

## 许可与分发

Paseo 源码采用 Apache-2.0，依赖保留各自许可证。打包生成 Cargo、可选 worker 与语音依赖声明；这些声明**不是合规认证或对应源码提供承诺**。官方 Sherpa 1.12.28 启用 TTS 的预构建库静态包含 GPL espeak-ng；源码交付/提供承诺、组合作品、MPL/LGPL、CRT 与 Rust 工具链义务仍是未解决的发布分发要求。语音模型权重不捆绑，按需下载。详见[分发要求](docs/release.md#distribution-requirements)。

## 社区与相关项目

[Discord](https://discord.gg/jz8T2uahpH) · [Reddit](https://www.reddit.com/r/PaseoAI/) · [GitHub Sponsors](https://github.com/sponsors/boudra) · [赞助](https://paseo.sh/zh-CN/sponsor)

- [getpaseo/paseo-relay](https://github.com/getpaseo/paseo-relay)：独立的 Elixir 分布式 relay 项目。
- [paseo-vscode](https://marketplace.visualstudio.com/items?itemName=hinnes.paseo-vscode)：VS Code 扩展；其兼容性不代表本次原生迁移已验证。
