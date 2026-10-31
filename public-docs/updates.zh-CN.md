---
title: 更新
description: 原生构建身份、更新通道与迁移可用性。
nav: 更新
---

# 更新 {#updates}

当前检出版本为 **0.13.0**，但 Rust/Flutter 迁移**尚未发布**。不能假定已有下载、npm CLI 包、应用商店条目或容器 `latest` 标签包含此迁移。

## 原生构建身份 {#native-build-identity}

产品客户端使用 Flutter，目标为 Linux、macOS、Windows、Android 和 iOS。桌面/服务器运行 Rust daemon 与 Rust `paseo` CLI；移动客户端只连接远程 daemon。

本地源码构建需要 Node 24、Rust/Cargo、Flutter 及目标平台的原生工具：

```bash
npm ci
npm run build:daemon
# 优化的本地构建，不代表发布来源身份：
npm run build:daemon:release
npm run build:flutter -- linux
```

在匹配的桌面宿主系统选择 `linux`、`macos` 或 `windows`。Flutter 产物位于 `artifacts/flutter/`。daemon 位于 `target/debug/paseo` 或 `target/release/paseo`（Windows 为 `paseo.exe`），设置 `CARGO_TARGET_DIR` 时以其为根目录。请保留可执行文件旁生成的全部资源。

## 更新本地安装 {#updating-a-local-installation}

替换运行时前先备份 Paseo 主目录。默认仍为 `~/.paseo`，保留原有配置和历史。使用 `paseo daemon stop` 停止托管 daemon，用目标修订构建的完整运行时替换，然后运行 `paseo daemon start` 和 `paseo daemon status`。重启 worker 不会替换 supervisor 可执行文件。

直接运行 `paseo` 打开 CLI 聊天；显式设置请用 `paseo onboard`，它不是更新命令。

## 通道与发布 {#channels-and-publication}

稳定版和测试版是发布身份，不证明此次迁移已经发布。优化本地二进制文件不等于启用官方更新来源身份。原生发布需要验证源代码身份、通道/仓库元数据、完整运行时资源、适用的签名以及明确发布授权。

本地 Flutter 构建不应沿用旧桌面应用更新界面的操作说明。当前更新清单、灰度控制和打包要求见仓库[发布指南](https://github.com/getpaseo/paseo/blob/main/docs/release.md)。

## 移动端与验证边界 {#mobile-and-verification-boundaries}

`npm run build:apk` 或 `npm run build:apk:arm64` 构建 Android 远程客户端。没有发布签名凭据时，产物明确标记为调试签名。`npm run build:ios` 需要 macOS/Xcode；没有 provisioning/export 配置时输出未签名的设备应用归档。这些产物不自动具备上架条件。

已记录的运行时执行位于 Linux。Android ARM64 release 模式构建已通过，使用调试签名，未安装 APK 或在设备上运行；不声称已在 Windows/macOS/iOS 实际运行。桌面后续验证及限制见[测试证据](https://github.com/getpaseo/paseo/blob/main/docs/testing.md)；工作流定义不是成功运行记录。

生成第三方声明不代表可合法分发。启用 TTS 的 Sherpa 1.12.28 预构建库静态包含 GPL espeak-ng；对应源码/要约、组合软件范围、MPL/LGPL、CRT 和 Rust 工具链义务仍未解决。具体要求由[发布指南](https://github.com/getpaseo/paseo/blob/main/docs/release.md)说明。这不意味着许可证变更或移除本地 TTS。
