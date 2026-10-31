---
title: 故障排查
description: 排查原生运行时启动错误、找不到 Provider 和环境不一致的问题。
nav: 常见问题
---

# 故障排查 {#troubleshooting}

## 原生客户端或 daemon 无法启动 {#native-client-or-daemon-will-not-start}

0.13.0 Rust/Flutter 迁移尚未发布。旧 Electron AppImage、npm CLI 包或商店下载不能证明包含原生运行时。请按[更新](/docs/updates)从迁移检出构建。

保留完整 Flutter 包和生成的 Rust 运行时资源。只复制 `paseo` 会遗漏所需库和技能。查看启动输出和 `paseo daemon status`；前台诊断使用 `paseo daemon run`。直接运行 `paseo` 打开聊天，不负责 daemon 设置或启动。

开发环境默认使用 `.dev/paseo-home` 和端口 `6768`，普通运行使用 `~/.paseo` 和 `6767`。诊断前用 `--home` 或 `--host` 选择正确实例。移动应用只连接远程主机，不能启动本地 daemon。

## Paseo 找不到我的 Provider {#paseo-cant-find-my-provider}

你已经安装的 Provider 显示为 **未安装**。

Paseo 启动的是你已经安装的 Agent CLI，它并不自带这些 CLI（参见 [Provider](/docs/providers)）。所以它必须在自己的 `PATH` 中找到命令。如果你的 shell 只在特定条件下才把该位置加入 `PATH`，Paseo 就可能找不到它。

### 查看 Paseo 看到的内容 {#see-what-paseo-sees}

打开 **设置 → 你的主机 → Providers**，点击该 Provider，然后点击 **诊断**。重要的几行：

- **Resolved path**：Paseo 找到二进制文件的位置，或 `not found`。
- **Daemon PATH**：Paseo 正在搜索的 `PATH`。把它和新终端中 `echo $PATH` 的结果进行比较。
- **Version**：二进制文件是否真的能运行。

在终端或 Agent 中，可以向受影响的 daemon 请求同样的诊断信息：

```bash
paseo provider diagnostic <provider>
paseo --host <host:port> provider diagnostic <provider> --json
```

当受影响的 daemon 不是 CLI 默认的本地 daemon 时，请使用全局的 `--host` 选项。

最常见的情况是：显示 `not found`，并且 **Daemon PATH** 中缺少你的二进制文件所在的目录。也就是说，这个目录在你终端的 `PATH` 中，却不在 Paseo 的 `PATH` 中。

### 修复 {#fix-it}

请让 Provider 命令在 daemon 启动时的 `PATH` 中可用，然后停止并重新启动 supervisor；参见 [Paseo 的环境为何可能与终端不同](#why-paseos-environment-can-differ-from-your-terminal)。

如果你更想直接固定路径，请在 `~/.paseo/config.json` 中设置二进制文件路径：

```json
{
  "agents": {
    "providers": {
      "claude": {
        "command": ["/absolute/path/to/claude"]
      }
    }
  }
}
```

`command` 的格式是 `[binary, ...args]`，它会完全替换该 Provider 的默认启动命令。用 `which -a claude` 查找真实路径。`type -a claude` 还能告诉你 `claude` 是否只是一个 shell 别名或函数；这些都不起作用，因为 Paseo 直接运行二进制文件，所以请使用它指向的路径。编辑后重新加载配置（见[下文](#i-changed-configjson-but-nothing-happened)）。

关于替代端点、多个配置、自定义二进制文件和 ACP Agent，请参见[自定义 Provider](/docs/custom-providers)。各 Agent 的安装链接见[支持的 Provider](/docs/supported-providers)。

## Paseo 的环境为何可能与终端不同 {#why-paseos-environment-can-differ-from-your-terminal}

操作系统启动器、服务管理器、容器或远程主机可能提供不同于终端的环境。这会影响 Agent Provider、脚本/设置任务 PTY 和其他主机工具。

请查看受影响 daemon 的 Provider 诊断，不要假定 Flutter 应用会导入交互式 shell 配置。`paseo daemon run` 使用终端启动环境；托管的 `paseo daemon start` 会有意移除 daemon 设置覆盖项，应将这些设置写入 `config.json`。

确保 Provider 二进制文件位于 daemon 启动时的 `PATH` 中，或配置绝对路径 `command`。修改启动环境后停止并重新启动 supervisor；只重启 worker 会保留原环境。可选 JavaScript 后端插件需要外部 Node 20+（`PASEO_PLUGIN_NODE`），并无内置 Node 运行时。

## 查看日志 {#reading-the-logs}

- **daemon**：`~/.paseo/daemon.log`，自定义主目录则为 `$PASEO_HOME/daemon.log`。
- **Flutter 桌面**：使用原生设置诊断中的 **Application logs** 和 **Daemon logs**。应用日志为支持目录中的每窗口 `desktop-<windowId>.log`；以返回的 `logPath` 为准，不要使用旧 Electron 日志路径。
- **脚本/设置任务**：查看只读日志。原生客户端没有交互式终端 UI。

## 我修改了 config.json，但什么都没发生 {#i-changed-configjson-but-nothing-happened}

编辑后重新加载文件：

```bash
paseo reload
```

Paseo 会应用可以在运行时安全生效的设置，并列出需要重启的路径。JSON 无效或 schema 错误时，不会应用任何内容；修复报告的错误后再次运行该命令。如果某个改动的设置由启动时的环境变量或参数决定，reload 会单独报告它。

只有当 reload 要求时才运行 `paseo daemon restart`。在应用中，打开 **设置 → 你的主机 → 概览** 并使用 **重启 Daemon**。正在运行的 Agent 会继续运行，客户端会自动重连。

## 仍然无法解决？ {#still-stuck}

- [自定义 Provider](/docs/custom-providers)：端点、多个配置、二进制文件、ACP Agent。
- [配置](/docs/configuration)：`config.json`、环境变量、日志。
- [原生桌面宿主](https://github.com/getpaseo/paseo/tree/main/packages/flutter_app/lib/platform/desktop)：Flutter 宿主启动与诊断实现。
- [报告问题](https://github.com/getpaseo/paseo/issues)。
