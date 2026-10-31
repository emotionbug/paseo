---
title: 插件参考
description: 后端扩展入口、SDK 契约、来源、准备命令和原生 CLI。
nav: 参考
---

# 插件参考 {#plugin-reference}

先阅读[快速上手](/docs/plugins)或[迁移指南](/docs/plugins/migration)。本页描述 **0.13.0 源码检出中尚未发布的原生迁移**。

## 运行时边界 {#runtime-boundary}

daemon、CLI 和内置功能由 Rust 实现。可选第三方 JavaScript 扩展在私有后端 worker 中运行，需要主机**另行安装 Node.js 20+**。在 daemon 环境中用 `PASEO_PLUGIN_NODE` 选择可执行文件，默认使用 `node`。必须提供可选生成 worker 资源，也可用 `PASEO_PLUGIN_WORKER` 指定位置。获取 npm 来源或执行 npm 准备命令还需要 npm；重新加载已安装文件需要 Node，不需要 npm。

Flutter 负责 Linux/macOS/Windows/Android/iOS 界面，不执行 React、React Native 插件代码或客户端包，也不加载插件界面、面板、时间线渲染器、主题或 Composer 回调。移动端只远程连接。托管候选包校验与激活要求实际存在的 `index.server.ts` 或 `index.server.tsx` 文件；同名目录和仅客户端插件会明确被拒绝，而不是作为空插件报告就绪。历史 SDK 导出不代表原生 UI 兼容性。

| 归属                        | 源码                                        |
| --------------------------- | ------------------------------------------- |
| Rust 管理、CLI、worker 桥接 | `packages/daemon/src`                       |
| 私有可选后端 worker         | `packages/daemon/plugin-worker/src`         |
| 公共扩展 SDK                | `packages/plugin`（`@getpaseo/plugin`）     |
| TypeScript 客户端 SDK       | `packages/client`（`@getpaseo/client`）     |
| 共享协议                    | `packages/protocol`（`@getpaseo/protocol`） |

仓库构建使用 Node.js 24、`npm ci`、`npm run build:daemon`；监视命令是 `npm run dev:daemon`。构建工具需要 Node，不代表产品捆绑 Node 或 daemon 由 JavaScript 实现。

## 项目文件 {#project-files}

`paseo plugin init /absolute/path/to/plugin` 创建纯后端项目，包含 `paseo-plugin.json`、`package.json`、`tsconfig.json`、`OVERVIEW.md`、`index.server.ts`、`server/greeting.ts` 和 `shared/greeting.ts`，但不安装依赖。在项目内运行 `npm install` 安装编写工具。

### 版本要求 {#requirements}

声明 manifest ID 和明确兼容的 Paseo semver 范围：

```json
{
  "id": "review-tools",
  "requirements": { "paseo": ">=0.13.0" }
}
```

脚手架使用源码检出的版本号。范围用于运行时兼容检查，不证明原生迁移已发布，也不恢复旧 UI API。省略 `requirements.paseo` 历史上表示 `<0.8.0`，当前运行时会拒绝；空范围、无效范围和未知要求键也会被拒绝。不要声明超出实际支持版本的范围。

### 运行时入口 {#runtime-entries}

`index.server.ts`（也接受 `.tsx`）默认导出接收 `PluginServerContext` 的贡献函数。支持 `handle`、`registerSettings`、`registerProvider`、`registerUsageSource`、`on` 和 `before`。Node 操作、凭据、进程传输及主机资源放在 `server/`；运行时无关契约放在 `shared/`。后端不要导入 `client/`。

### 运行时模块 {#runtime-modules}

从 SDK 根入口导入 `defineRpc`、`defineSettings`、schema 和普通类型。从 `@getpaseo/plugin/server` 导入服务端 context 和生命周期类型，从 `/server/provider` 导入 Provider 契约，从 `/server/acp` 导入 ACP 适配器，从 `/server/usage` 导入用量契约和辅助函数。worker 提供这些 SDK 模块和 Zod。宿主提供的库和仅用于类型的依赖放在 `devDependencies`；其他运行时库声明为依赖或打包。后端不支持 React 和客户端 SDK 入口；共享模块不能导入特定运行时模块或类型。

## 入口与清理 {#entry-point-and-cleanup}

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";

export default function contribute(server: PluginServerContext) {
  server.on("agent.turn_ended", (event) => {
    console.log(event.agent.id, event.outcome);
  });
  return () => {};
}
```

入口必须返回同步或异步清理函数；没有需要释放的资源时返回 `() => {}`。返回其他值的入口会被 worker 拒绝。释放创建的定时器、文件监视器、套接字、SDK 订阅和子进程。重新加载、停用、移除及 daemon 关闭会结束 worker 和注册。钩子不依赖应用连接。

## 生命周期钩子 {#lifecycle-hooks}

| 注册                            | 回调                               | 结果                                              |
| ------------------------------- | ---------------------------------- | ------------------------------------------------- |
| `server.on(name, callback)`     | `(event, { paseo, signal })`       | `void` 或 promise                                 |
| `server.before(name, callback)` | `({ request }, { paseo, signal })` | 修改后的请求，或 `undefined` 保持原请求；支持异步 |

事件包括 `agent.created`、`agent.closed`、`agent.turn_started`、`agent.turn_ended`、`agent.permission_requested`、`agent.permission_resolved`、`agent.archived`、`workspace.created` 和 `workspace.archived`。公开后台 Agent 使用相同生命周期，私有一次性辅助 Agent 不在范围内。归档通知不证明运行时或工作树清理已结束。

`agent.turn_ended` 包含 `agent`、`turnId` 和 `outcome`（`completed`、`failed` 或 `canceled`）。已弃用的 `timeline` 始终为空；应按需读取有界尾部：

```ts
server.on("agent.turn_ended", async (event, { paseo, signal }) => {
  if (signal.aborted) return;
  const agent = paseo.agents.ref(event.agent.id);
  const history = await agent.timeline.refetch({ direction: "tail", limit: 20 });
  console.log("Fetched turn history", event.agent.id, history);
});
```

生产插件不要记录私人历史，除非这是明确披露的预期行为。遵守取消信号，在清理时释放订阅。

| 前置钩子             | 可编辑请求                                   |
| -------------------- | -------------------------------------------- |
| `agent.create`       | 除 `cwd` 外的公开 Agent 配置和启动覆盖 `env` |
| `agent.session_open` | 仅 `env`                                     |
| `workspace.create`   | 显式工作区创建请求                           |

每次打开会话时注入环境覆盖：

```ts
server.before("agent.session_open", ({ request }) => ({
  ...request,
  env: { ...request.env, COMPANY_ENV: "development" },
}));
```

`env` 是启动覆盖，不是 daemon 继承的完整环境。会话打开包括创建、恢复、刷新和导入，也可能用于交互或历史读取。用普通 SDK 权限方法回答请求；权限请求不一定是 shell 命令，也不表示轮次结束。

## 已连接主机和 Paseo SDK {#connected-hosts}

处理函数收到 `{ paseo }`，生命周期回调收到 `{ paseo, signal }`，使用的是 `@getpaseo/client`，不是独立的旧 server SDK。普通 Agent、工作区、Provider 和配置操作优先使用该 API，不必创建重复 RPC。

`context.paseo.hosts` 描述所选 daemon 保存的主机连接。身份属于具体 daemon 连接，路径和工作区 ID 不能视为跨主机通用。见 [SDK 参考](/docs/sdk/reference)。此后端 API 不提供旧应用 React hook，也不能注入原生界面。

## 添加插件专属后端行为 {#add-plugin-specific-backend-behavior}

RPC 用于供应商 API、daemon 本地资源或应留在 daemon 上的凭据。在 `shared/greeting.ts` 中定义：

```ts
import { defineRpc } from "@getpaseo/plugin";
import { z } from "zod";

export const greeting = defineRpc({
  name: "greeting.create",
  input: z.object({ name: z.string() }),
  output: z.object({ message: z.string() }),
});
```

在 `index.server.ts` 中注册：

```ts
import type { PluginServerContext } from "@getpaseo/plugin/server";
import { greeting } from "./shared/greeting";

export default function contribute(server: PluginServerContext) {
  server.handle(greeting, ({ name }) => ({ message: `Hello, ${name}` }));
  return () => {};
}
```

契约校验输入和输出；SDK 根入口的 `RpcInput`、`RpcOutput` 可推导类型。注册只创建后端行为，不创建界面或自动添加原生按钮。消费者需要使用 daemon 插件 RPC 协议；不要把 `useRpc` 或 React 客户端入口作为原生集成方案。

## 持久化设置 {#settings-screens}

后端设置 API 保留，可执行设置界面不受支持。在 `shared/` 定义文档：

```ts
import { defineSettings } from "@getpaseo/plugin";
import { z } from "zod";

export const preferences = defineSettings({
  id: "review",
  scope: "host",
  version: 1,
  schema: z.object({ enabled: z.boolean().default(true) }),
});
```

通过 `server.registerSettings(preferences)` 注册。句柄提供 `read()`、`subscribe(listener)`，后者返回清理函数。读取结果为带 `values` 和不透明 `revision` 的 `ready`，或带 `error`、`revision` 的 `invalid`；无效的保存数据会保留。

`id` 匹配 `[a-z][a-z0-9_-]*`，在安装项内唯一。`scope` 是 `host`，由该 daemon 的授权客户端共享，不跨主机同步。`version` 是正整数 schema 版本，不是写入修订号。提供默认值使 `{}` 能解析成完整文档。可选 `migrate(values, fromVersion)` 支持异步，结果必须通过当前 schema。无效写入和版本冲突不会通知订阅者；监听器失败不会撤销已提交写入。

## Provider {#providers}

用 `server.registerProvider()` 注册 `@getpaseo/plugin/server/provider` 的 `ProviderRegistration`，或使用 `/server/acp` 的 `runAcpProvider()`。连接通过 `send()` 接受输入、`onEvent()` 发布事件、`close()` 释放资源。`send()` 表示接受，不表示轮次完成。

消息、结构化命令和引导使用 `session.prompt`。在用户时间线条目重复 `clientMessageId`，发出一个对应的 `session.prompt_result`。通过协议返回模型、模式、思考选项、能力、配置、权限、持久化和时间线数据。Provider 创建的子会话使用 `parentSessionId`。刷新会用当前配置和持久化信息关闭并重新打开会话。

开关/选择配置描述符及标准时间线条目是原生客户端渲染的数据，不是可执行插件 UI。私有选项放在 `ProviderSessionConfig.providerOptions` 并校验。宿主工具通过 MCP 服务器传入。见 [Provider 选项](/docs/sdk/provider-options)及完整的[直接/ACP Provider 指南](/docs/plugins/providers)。

### SVG 规则 {#svg-rules}

`ProviderRegistration.icon` 是相对插件目录的文件路径，不是内联 SVG 或 URL。解析后必须是目录内的普通文件，包括符号链接的最终目标。用量来源图标采用相同规则。SVG 必须以 `<svg>` 文档开始，最大 64 KiB；禁止 script、`foreignObject`、style 元素、事件处理属性、JavaScript URL 及非片段 `href`。展示页 PNG 图标使用单独的[发布契约](/docs/plugins/publishing#icons-and-screenshots)。

## 用量来源 {#usage-sources}

通过 `server.registerUsageSource()` 注册，从 `@getpaseo/plugin/server/usage` 导入契约和辅助函数。来源包含 `id`、`label`、可选图标路径、输入 Zod schema、`discover(scope)` 和 `fetch(input)`。

- `discover` 返回账户：稳定的 `key`、可选 `label` 和 `harness`，以及标识凭据存储的 JSON `input`，不要包含凭据。
- scope 是全局或会话级（`provider`、可选 `model`、启动 `env`）。会话发现只解析该会话账户；无关 Provider 或没有登录时返回 `[]`，不要回退扫描其他 Agent 或默认存储。
- `fetch` 返回带窗口及可选余额/详情的 `available`、带类型化问题的 `unavailable`，或带消息的 `error`。输入需按 schema 校验。
- 账户 key 使用 1–128 个 `[A-Za-z0-9._-]` 字符，不使用原始凭据或邮箱。敏感稳定身份可用 `hashAccountKey()`，优先选择令牌轮换后仍稳定的身份。
- 获取时重新读取存储，不兑换刷新令牌，不重写其他 CLI 的凭据文件。
- `unavailable(problem)` 必须带 `expired`（`expiresAt`、可选 `refreshedBy`）、`rejected`（`status`、可选 `refreshedBy`）或 `no_quota`（`detail`）。`refreshedBy` 是 CLI 名称，不是给用户的指令。
- 周期名称来自 Provider 数据，不从响应顺序或重置倒计时猜测。`windowFromReportedDuration()` 接收报告的秒数或 `null` 和中性未知名称/身份；`windowFromUsedPct()` 可用于 Provider 命名的周期。窗口 ID 应在重排和重置后保持稳定。

内置用量来源由 Rust 实现，不属于此可选 worker。后端注册不会创建自定义原生用量面板。

## 插件来源 {#plugin-sources}

向 `paseo plugin install` 传入来源（也接受 `add`）：

| 来源        | 示例                                                                           |
| ----------- | ------------------------------------------------------------------------------ |
| 注册表      | `owner/slug` 或 `registry.example/owner/slug`                                  |
| 主机目录    | `/srv/paseo/plugins/review` 或 `./review`                                      |
| GitHub      | `github:acme/paseo-review` 或 `git:acme/paseo-review`                          |
| Git URL/SCP | `git:https://git.example/acme/review.git` 或 `git@git.example:acme/review.git` |
| npm         | `npm:@acme/paseo-review@1.2.0`                                                 |
| 来源子目录  | `github:acme/monorepo:plugins/review`                                          |

裸 `owner/slug` 是注册表 ID，不是本地目录或 GitHub 简写。本地相对路径用 `./` 或 `../`。绝对路径、`~`、Windows 驱动器路径和 UNC 路径标识目录。CLI 在本地解析相对路径和 `~`；使用 `--host` 时应提供 daemon 主机上的绝对路径，不会上传文件。`file://` 选择 Git 获取，不是目录安装。

npm 支持 `@` 后的确切版本、tag 或 semver 范围，默认 `latest`。含 shell 操作符的范围要加引号。不接受 tarball URL、npm 别名或 npm `file:` 规范。无 scope 的包同时指定版本和子目录时，用明确的 `npm:` 避免与 SCP 混淆。

子目录必须位于来源内：不含空段、`.` 或 `..`，单独的 `.` 表示根目录。URL 端口和 SCP 分隔符仍属于来源。跨主机优先使用 `/`。`--ref` 选择首次安装的 Git 分支、标签或提交，默认获取 HEAD；也接受 `--path` 指定相对来源的插件目录。

默认注册表是 `https://plugins.paseo.sh`。`PASEO_PLUGIN_REGISTRY` 可选择其他默认 URL，含路径前缀。daemon 配置 `pluginRegistries: { "host": { "authorization": "Bearer token" } }` 提供私有注册表凭据，修改启动设置后重启。凭据不会发送给制品主机或重定向目标。已有安装保留记录的注册表和来源；注册表记录决定审查过的修订和插件路径。

npm 在 daemon 主机上运行，使用该用户的注册表/认证配置。获取过程把候选包和生产依赖安装到托管存储；安装锁记录实际解析结果。npm 作者还应在需要时提供[发布锁](/docs/plugins/publishing#2-lock-installed-dependencies)。Git 获取需要主机上的 Git 和访问权限。

`--id` 覆盖 manifest 安装 ID；重复 ID 被拒绝。移除目录安装保留源码；移除托管 npm/Git 安装会删除托管文件。安装授权代码、依赖及准备命令以 daemon 用户权限无沙盒运行。

## 准备命令 {#preparation-commands}

普通 TypeScript 扩展不需要单独构建，由 worker 编译。只有必须安装依赖或生成源码/资源时才声明 manifest `build`：

```json
{
  "id": "review-tools",
  "requirements": { "paseo": ">=0.13.0" },
  "build": [["npm", "ci", "--omit=dev", "--ignore-scripts"]]
}
```

每项是非空 argv 数组，在暂存插件目录直接执行，不隐式调用 shell，不从锁文件推断命令。安装和更新在校验、激活前执行准备。失败候选会被丢弃；更新保留旧安装。即使获取时禁止 npm 生命周期脚本，显式声明的命令仍是受信任的可执行代码。锁定安装依赖树并披露额外设置，见[发布指南](/docs/plugins/publishing#plugins-with-a-build-step)。

## CLI 参考 {#cli-reference}

```bash
paseo plugin init /absolute/path/to/plugin
paseo plugin install /absolute/path/to/plugin
paseo plugin install npm:@acme/paseo-review@1.2.0 --id review
paseo plugin ls review
paseo plugin logs review
paseo plugin update review --check
paseo plugin update review --yes
paseo plugin update --all --check
paseo plugin update --all --yes
paseo plugin update review --version 1.2.0
paseo plugin update review --ref v2
paseo plugin reload review
paseo plugin disable review
paseo plugin enable review
paseo plugin remove review
```

远程管理把全局 `--host <url>` 放在命令前。`ls` 读取安装身份/修订，不查询远程。普通原生 CLI 更新显示提案并要求 `--yes`，没有交互式批准提示。`--check` 始终只预览。显式 `--version`（npm）或 `--ref`（Git）授权单个插件目标，不能彼此组合或与 `--all` 组合。

直接 npm 更新考虑较新的 `latest`，直接 Git 更新考虑默认 HEAD，不受首次选择器约束。显式 npm 目标允许降级。注册表安装使用已审查固定版本。目录安装跳过更新，应修改后重新加载。批量结果独立；提案变化或不可用时失败，不静默换成其他内容。

根配置 `pluginsEnabled` 和安装项自身启用标记共同控制执行。修改根配置后用 `paseo reload --json`；安装项通过插件命令管理。安装/重新加载前在插件项目中执行类型检查，加载编译不是类型检查。

## 调试后端输出 {#debug-backend-output}

使用普通 `console.log`/`console.error`。`paseo plugin logs <id>` 返回近期快照，包含时间戳、流、序号和消息，不是实时跟随。日志包括加载/编译失败和 worker 输出；不要记录凭据或访问令牌。`running` 不证明 Provider 行为正确或原生 UI 贡献可用。

## 加载失败 {#load-failures}

| 症状                 | 处理                                                                                                              |
| -------------------- | ----------------------------------------------------------------------------------------------------------------- |
| worker/Node 不可用   | 安装外部 Node 20+，检查 daemon 环境、`PASEO_PLUGIN_NODE`，并提供可选 worker 资源。                                |
| 旧入口或缺少入口     | 提供实现受支持后端行为的真实 `index.server.ts` 或 `index.server.tsx` 文件；仅客户端包和入口路径上的目录会被拒绝。 |
| 后端导入客户端代码   | 移除依赖本身，只保留后端逻辑，不要仅隐藏编译错误。                                                                |
| RPC 拒绝             | 检查输入/输出 Zod schema 和处理函数错误。                                                                         |
| 改动未生效           | 类型检查、重新加载，然后查看状态和日志。                                                                          |
| 重新加载/worker 失败 | 修复报告的源码或运行时错误再加载，不要假设旧 worker 仍在运行。                                                    |
| 旧插件界面不存在     | 这是原生边界，不应向应用安装 React 或浏览器。                                                                     |
