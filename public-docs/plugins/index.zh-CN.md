---
title: 插件快速上手
description: 构建、安装、分享和更新受信任的后端扩展。
nav: 快速上手
---

# 插件快速上手 {#plugin-quickstart}

可选 JavaScript 插件为一个 Paseo daemon 提供 RPC 处理函数、生命周期钩子、设置持久化、用量来源或编程 Agent Provider。内置功能由 Rust 实现。第三方 JavaScript 在独立后端 worker 中运行，使用 daemon 主机上**另行安装的 Node.js 20+**，不在 Flutter 应用中执行。

本指南描述 **0.13.0 源码检出中的原生迁移，迁移尚未发布**。Linux、macOS、Windows、Android 和 iOS 的 Flutter 客户端不执行 React 插件。旧 JavaScript 客户端的界面、面板、主题、侧边栏组件、Composer 回调和自定义时间线渲染器都不是原生扩展点。移动端只远程连接，不运行 daemon 或插件 worker。

## 准备 daemon 主机 {#prepare-the-daemon-host}

需要 JavaScript 扩展时，单独安装 Node.js 20+，让 daemon 可以找到 `node`，或在 daemon 环境中用 `PASEO_PLUGIN_NODE` 指定可执行文件。获取 npm 来源或执行 npm 准备命令还需要 npm；设置 `PASEO_PLUGIN_NODE` 不会安装 npm。原生运行时的生成资源中必须包含可选 worker，也可以用 `PASEO_PLUGIN_WORKER` 指定它。

从仓库构建时，使用仓库要求的 **Node.js 24**，执行 `npm ci` 和 `npm run build:daemon`，生成 Rust 调试可执行文件及相邻资源。监视开发命令是 `npm run dev:daemon`。不要使用已移除的 `dev:server` 或为插件安装旧 JavaScript daemon。

## 创建插件 {#create-a-plugin}

在 daemon 主机上执行：

```bash
paseo plugin init /absolute/path/to/workspace-plugin
cd /absolute/path/to/workspace-plugin
npm install
```

Rust CLI 创建纯后端 TypeScript 项目，不自动运行包管理器：

```text
workspace-plugin/
  paseo-plugin.json
  package.json
  tsconfig.json
  OVERVIEW.md
  index.server.ts
  server/greeting.ts
  shared/greeting.ts
```

脚手架注册问候 RPC。`npm install` 安装编写插件所需的依赖；worker 在运行时提供插件 SDK 和 Zod。主机操作放在 `server/`，普通数据和契约放在 `shared/`。不要通过添加客户端入口来创建原生界面。

要观察一个简单的后端贡献，可以将 `index.server.ts` 替换为：

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log("Turn ended", event.agent.id, event.outcome);
  });
  return () => {};
}
```

启用期间，即使没有应用连接，该钩子也会运行。[参考](/docs/plugins/reference)介绍 RPC、设置、生命周期、用量和 SDK 契约；[Provider 指南](/docs/plugins/providers)介绍直接接入 Agent 或通过 ACP 适配。

## 安装并试用 {#install-and-try-it}

插件是受信任且没有沙盒的代码。后端及准备命令拥有 daemon 用户对文件、进程、凭据和网络的访问权限。安装前应信任来源、依赖及更新。

```bash
npm run typecheck
paseo plugin install /absolute/path/to/workspace-plugin
paseo plugin ls workspace-plugin
paseo plugin logs workspace-plugin
```

daemon 根设置 `pluginsEnabled` 和插件自身的启用状态都必须允许执行。修改 `config.json` 中的根设置后运行 `paseo reload --json`；安装项的修改使用插件命令。`paseo plugin enable workspace-plugin` 只启用该安装项，不打开根开关。

在所选 daemon 上完成一轮 Agent 对话，再查看钩子日志。不会出现插件界面或侧边栏项。`running` 仅表示后端激活，不证明旧客户端贡献可用。

远程操作时，将 `--host <url>` 放在命令前，使用**远程主机上存在的绝对路径**。CLI 不会上传本地插件文件。

## 编辑并重新加载 {#edit-and-reload}

```bash
npm run typecheck
paseo plugin reload workspace-plugin
paseo plugin ls workspace-plugin
paseo plugin logs workspace-plugin
```

加载会编译后端，但不能替代作者执行 TypeScript 类型检查。入口返回的清理函数应释放定时器、文件监视器、套接字、订阅和子进程。重新加载会重启 worker；确认错误状态后再判断新代码是否运行。

## 安装和更新共享插件 {#install-a-published-plugin}

```bash
paseo plugin install npm:@acme/paseo-review@1.2.0
paseo plugin install github:acme/paseo-review
paseo plugin install github:acme/monorepo:plugins/review
paseo plugin update paseo-review --check
paseo plugin update paseo-review --yes
```

使用实际安装 ID，它不一定等于 npm 包名。原生 CLI 显示更新提案，普通更新需要 `--yes`；它不会弹出交互式批准提示。`--check` 不应用更新。显式 `--version` 或 `--ref` 为单个插件选择并授权目标版本。目录来源应直接修改后重新加载。

npm 用于获取包，后续加载不需要 npm；**后端执行仍需要 Node**。来源选择器、注册表固定版本、准备命令和更新规则见[参考](/docs/plugins/reference#plugin-sources)。分享插件见[发布指南](/docs/plugins/publishing)，移除旧客户端贡献见[迁移指南](/docs/plugins/migration)。

## 源码归属 {#source-ownership}

- `packages/daemon/src`：Rust daemon、CLI、插件管理和 worker 桥接。
- `packages/daemon/plugin-worker/src`：提取出的私有 JavaScript 后端 worker。
- `packages/plugin`：公共 `@getpaseo/plugin` 作者 SDK。
- `packages/client`：后端处理函数使用的 `@getpaseo/client` TypeScript SDK。
- `packages/protocol`：共享协议契约。

私有 worker 不是应用 SDK，也不是可执行 UI 宿主。遗留 SDK 客户端导出和历史示例并不意味着支持原生 UI 插件。
