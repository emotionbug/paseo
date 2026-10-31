---
title: 将插件迁移到原生后端运行时
description: 为原生迁移保留后端扩展行为，移除可执行客户端贡献。
nav: 迁移
---

# 将插件迁移到原生后端运行时 {#migrate-a-plugin-to-the-native-backend-runtime}

本指南针对 **0.13.0 源码检出中尚未发布的原生迁移**，不表示新的原生发行版已经可用。历史上的 0.8 迁移将客户端与服务端入口分离，但这个分离不让客户端部分兼容 Flutter。

内置功能现在由 Rust 实现。可选 JavaScript 后端插件继续通过私有 worker 运行，需要单独安装 Node.js 20+。Flutter 不执行插件 JavaScript、React 组件或可执行 UI 贡献。移动端连接桌面/服务器 daemon，不运行 worker。

## 1. 对插件分类 {#1-classify-the-plugin}

| 现有行为                                                     | 原生迁移                                                 |
| ------------------------------------------------------------ | -------------------------------------------------------- |
| RPC 处理函数和 daemon 本地供应商调用                         | 保留在后端 worker。                                      |
| 生命周期钩子和 SDK 自动化                                    | 保留服务端注册及清理。                                   |
| 持久化设置 schema 和迁移                                     | 保留后端注册，移除可执行设置界面。                       |
| 直接或 ACP Provider                                          | 保留后端协议实现，输出标准时间线/配置数据。              |
| 用量发现和获取                                               | 保留后端用量来源注册。                                   |
| 界面、侧边栏/标题栏项、面板、文件渲染器                      | 移除可执行客户端贡献，不承诺存在原生插件等价物。         |
| React 时间线渲染器或转换器                                   | 移除；Provider 输出改用标准协议条目。                    |
| 客户端命令、Composer 标签、附件选择器、主题、浏览器/音频回调 | 移除客户端注册和导入；独立有用的后端数据与操作可以保留。 |
| 仅客户端插件                                                 | 不属于受支持的原生扩展；不要把空后端入口称为迁移。       |

不要把 React 组件移进 `server/`、捆绑浏览器或让 worker 加载客户端包。原生 Markdown/源码编辑等内置界面是产品功能，不是可执行扩展槽。

## 2. 保留显式后端入口 {#2-retain-an-explicit-backend-entry}

使用以下结构：

```text
paseo-plugin.json
package.json
tsconfig.json
index.server.ts
server/
shared/
```

`index.server.ts` 默认导出接收 `PluginServerContext` 的函数。主机操作和凭据放在 `server/`，普通值与 Zod 契约放在 `shared/`。移动受支持行为后删除旧混合 `index.ts`。从面向原生的包中移除 `index.client.tsx`、客户端模块及纯 UI 依赖，不要把不受支持的入口留在旁边。

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log(event.agent.id, event.outcome);
  });
  return () => {};
}
```

旧混合 API 的 daemon 处理函数改为 `server.handle(contract, handler)`。钩子用 `server.on`/`server.before`，设置用 `server.registerSettings`，Provider 用 `server.registerProvider`，用量来源用 `server.registerUsageSource`。可执行客户端界面没有替代注册。

保留定时器、订阅、套接字、文件监视器和子进程的真实清理逻辑。重新加载、停用、移除或 daemon 关闭时 worker 会停止。没有应用连接时 Agent 钩子仍可运行。

## 3. 修正导入和 SDK 归属 {#3-correct-imports-and-sdk-ownership}

| 需要                                             | 导入/源码                                      |
| ------------------------------------------------ | ---------------------------------------------- |
| `defineRpc`、`defineSettings`、普通类型和 schema | `@getpaseo/plugin`                             |
| 服务端 context、生命周期和进程辅助函数           | `@getpaseo/plugin/server`                      |
| Provider 契约                                    | `@getpaseo/plugin/server/provider`             |
| ACP 适配器                                       | `@getpaseo/plugin/server/acp`                  |
| 用量契约/辅助函数                                | `@getpaseo/plugin/server/usage`                |
| TypeScript daemon API                            | `@getpaseo/client`，位于 `packages/client`     |
| 共享协议类型                                     | `@getpaseo/protocol`，位于 `packages/protocol` |

扩展 SDK 位于 `packages/plugin`。私有 worker 位于 `packages/daemon/plugin-worker/src`，原生管理和 CLI 位于 `packages/daemon/src`。不要从已移除的 server/app/desktop/CLI 目录导入，也不要把私有 worker 复制为公共插件。

从迁移后的运行时移除 `/client`、`/client/ui`、React、React Native 和 UI hook 导入。旧 SDK 文档中的兼容别名或日期不延伸到 Flutter。保留真正需要的 Node 库和类型依赖；这是纯后端迁移，不是禁止 JavaScript。

## 4. 保留数据契约而非自定义渲染 {#4-preserve-data-contracts}

保留 RPC 输入/输出 Zod schema 和服务端处理函数。RPC 注册不生成原生按钮或面板，消费者使用 daemon 插件 RPC 协议。普通 Paseo 操作用处理函数和生命周期钩子获得的 SDK。

保留 `defineSettings` schema、默认值、版本迁移及 `registerSettings` 的读取/订阅行为。移除旧 React 设置界面不意味着删除保存值或创造新存储格式。

Provider 保留能力协商、提示词回执、权限、持久化、取消和重放。输出标准用户/助手文本、推理、工具、待办、错误、通知和压缩条目；关键结果不能依赖自定义 React 渲染器。开关/选择配置是原生客户端渲染的协议数据，不是可执行插件 UI。见 [Provider 指南](/docs/plugins/providers)。

## 5. 准备实际 daemon 主机 {#5-prepare-the-daemon-host}

单独安装 Node.js 20+，让 daemon 找到 `node`，或配置 `PASEO_PLUGIN_NODE`。提供可选 worker 资源，必要时用 `PASEO_PLUGIN_WORKER` 指定。npm 获取及声明的 npm 准备步骤需要另行提供 npm；Git 获取需要 Git。即使用户远程连接，这些工具和权限也属于 daemon 主机。

源码构建使用 Node.js 24、`npm ci` 和 `npm run build:daemon`，生成 Rust 调试可执行文件及相邻资源。监视命令是 `npm run dev:daemon`，`dev:server` 已移除。`build:daemon:release` 生成优化的本地资源，不赋予发行来源证明。

## 6. 更新包和发布元数据 {#6-update-package-and-publication-metadata}

修改 `package.json` 的 `files`，包含 manifest、服务端入口、`server/`、`shared/`、被导入资源和展示页文件。移除客户端入口和仅客户端资源。宿主提供库和类型依赖留在 `devDependencies`；按[发布指南](/docs/plugins/publishing)锁定运行时依赖树。

声明实际支持版本的 `requirements.paseo`。原生检出当前报告 `0.13.0`；只改范围无法区分或迁移旧 UI 插件。在 README 和展示页说明纯后端支持及外部 Node 要求，不要把未发布的迁移称为已发布版本。历史发行描述应保持历史性质，不要静默改写。

## 7. 运行迁移后的行为 {#7-exercise-the-migrated-behavior}

源码修改后，在合适的测试 daemon 上执行：

```bash
npm run typecheck
paseo plugin reload <plugin-id>
paseo plugin ls <plugin-id>
paseo plugin logs <plugin-id>
```

新安装先执行 `paseo plugin install /absolute/path/to/plugin`。daemon 根设置 `pluginsEnabled` 和安装项启用状态都必须允许加载。运行每个保留的 RPC、钩子、Provider、设置迁移和用量来源，并确认活跃工作期间的清理。仅 `running` 不能证明行为或原生 UI 兼容性。在声称支持的平台上实际测试；CI 定义或旧 JavaScript 客户端测试不是原生实际运行证据。

此迁移不包含 UI 兼容层。如果插件唯一有用行为是客户端界面，目前就不能把该功能提供给 Flutter；应说明限制，而不是发布无操作后端。
