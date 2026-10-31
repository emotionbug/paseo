---
title: 安全
description: "Paseo 的安全模型：架构概览、连接方式、中继加密和最佳实践。"
nav: 安全
---

# 安全 {#security}

Paseo 采用客户端-服务器架构，与 Docker 类似。daemon 运行在你的机器上，管理你的编程 Agent。客户端（移动应用、CLI 或 Web 界面）连接到 daemon，用来监控和控制这些 Agent。

你的代码从不离开你的机器。Paseo 是一个本地优先的工具，直接连接到你的开发环境。

## 架构 {#architecture}

Paseo daemon 可以运行在任何你想运行 Agent 的地方：你的笔记本电脑、Mac Mini、VPS 或 Docker 容器。daemon 监听连接并管理 Agent 的生命周期。

客户端通过 WebSocket 连接到 daemon。建立这个连接有两种方式：

- **中继连接（推荐）**：daemon 主动向我们的中继服务器发起出站连接，客户端在那里与它会合。无需开放端口。
- **直接连接**：daemon 监听一个网络地址，客户端直接连接。

## 中继连接（推荐） {#relay-connections-recommended}

中继是从手机连接最简单的方式。它不需要设置 VPN、端口转发或防火墙。daemon 可以保持绑定在 localhost 或套接字文件上，它向中继发起*出站*连接，你的手机在那里与它会合。官方中继服务器是开源的 Elixir 服务 [getpaseo/paseo-relay](https://github.com/getpaseo/paseo-relay)。

新安装默认关闭中继。当你通过 `paseo`、`paseo daemon pair` 或 Paseo 桌面应用配对设备时，Paseo 会先询问再启用它。选择不启用中继时，daemon 仍可通过直接 TCP、Tailscale 或其他 VPN 连接访问，并且不会生成配对二维码。在 CLI 的配对或启动命令中使用 `--relay`，可以不经交互提示直接启用。

> **中继在设计上就是不受信任的**。你的手机与 daemon 之间的所有流量都经过端到端加密。中继服务器无法读取你的消息、看到你的代码，也无法在不被发现的情况下篡改流量。即使中继被攻破，你的数据仍然受到保护。

### 工作原理 {#how-it-works}

1. daemon 生成一个持久的 ECDH 密钥对，并保存在 `$PASEO_HOME/daemon-keypair.json` 中
2. 当你扫描二维码或点击配对链接时，你的手机会收到 daemon 的公钥
3. 你的手机发送一条包含其自身公钥的握手消息。在握手完成之前，daemon 不会接受任何命令。
4. 双方各自把自己的 Curve25519 密钥与对方的公钥结合，然后使用由此得到的 NaCl `box` 通道传输受 XSalsa20-Poly1305 保护的负载。

中继只能看到：IP 地址、时间、消息大小和会话 ID。它无法读取消息内容、伪造消息，也无法通过观察握手推导出加密密钥。

### 为什么中继无法攻击你 {#why-the-relay-cant-attack-you}

daemon 在处理任何命令之前都要求一次有效的密码学握手。被攻破的中继无法：

- **发送命令**：没有你手机的私钥，它无法完成握手
- **读取你的流量**：只有已配对的两端才能打开 NaCl `box` 负载
- **伪造消息**：带认证的加密算法会拒绝被篡改的负载
- **重放旧消息**：每个会话都会派生新的加密密钥

### 信任模型 {#trust-model}

二维码或配对链接是信任锚点。它包含 daemon 的公钥，这是建立加密连接所必需的。请像对待密码一样对待它，不要公开分享。

## 直接连接 {#direct-connections}

daemon 默认监听 `127.0.0.1:6767`（仅限 localhost）。这对本地 CLI 使用是安全的，但你的手机或其他设备无法访问。

中继和 Tailscale 的设置说明见[连接方式](/docs/connectivity)。

### 套接字文件（仅限 CLI） {#socket-file-cli-only}

为了获得最大程度的隔离，你可以把 daemon 配置为监听 Unix 套接字文件而不是 TCP 端口。这会完全阻止任何网络访问，只有同一台机器上的进程才能连接。CLI 支持这种模式，但移动应用和 Web 界面需要网络连接。

### 通过 VPN 访问 {#vpn-access}

当你需要在本地网络之外直接连接时，请使用 [Tailscale](https://tailscale.com) 之类的 VPN。VPN 会加密流量，并让 daemon 不暴露在公共互联网上。把 daemon 绑定到它的 VPN 地址，设置 Paseo 密码，然后在客户端中把该地址添加为直接连接。

### 绑定到 0.0.0.0 {#binding-to-0000}

> **警告**：绑定到 `0.0.0.0` 会让 daemon 在所有网络接口上都可被访问，包括公共 Wi-Fi 和本地网络。这可能让你的 daemon 遭到未经授权的访问。如果你必须绑定到所有接口，请确保配置了合适的防火墙规则，并检查你的 `hostnames` 配置。

## DNS 重绑定防护 {#dns-rebinding-protection}

**CORS 并不是完整的安全边界**。它控制哪些浏览器来源可以发起请求，但无法阻止恶意网站把它的域名解析到你的本地机器（DNS 重绑定）。

Paseo 使用主机白名单校验传入请求的 `Host` 请求头。主机无法识别的请求会被拒绝。

通过 `config.json` 中的 `daemon.hostnames` 配置：

- 默认（`[]`）：允许 `localhost`、`*.localhost` 以及所有 IP 地址
- `['.example.com']`：允许 `example.com` 及其任何子域名，外加默认值
- `true`：允许任何主机（不推荐）

## 密码验证 {#password-authentication}

默认情况下，任何能访问 daemon 监听地址的人都可以连接。在 localhost 上这没有问题，只有本地进程能访问。但如果你绑定到某个网络接口（例如你的局域网 IP 或 `0.0.0.0`），或者你不完全信任本地网络，可以要求提供密码。

配置密码后，所有 HTTP 请求都必须包含 `Authorization: Bearer <password>` 请求头，所有 WebSocket 客户端（包括中继客户端）都必须在连接时发送密码。中继客户端在端到端加密通道内发送密码，中继无法看到它。未经验证的 HTTP 请求会收到 `401 Unauthorized` 响应。只有 `/api/health` 存活检查端点例外，以便进程管理器和负载均衡器无需凭据即可探测。

原生 daemon 不提供内置 Web 产品客户端。Flutter 客户端和 Rust CLI 通过 API 认证，不需要公开的静态登录页。

密码以 bcrypt 哈希的形式保存在 `config.json` 中，daemon 从不以明文保存它。设置说明见[配置](/docs/configuration#password-authentication)。

### 密码验证能做什么、不能做什么 {#what-password-auth-does-and-does-not-do}

- **能**：阻止未经授权的客户端控制你的 Agent，即使它们能通过网络访问 daemon。
- **不能**：加密流量。密码验证保护的是访问，而不是机密性。如果你需要在不受信任的网络上建立加密连接，请使用中继（提供端到端加密）或 Tailscale 之类的 VPN。

### 何时使用 {#when-to-use-it}

- 你想把 daemon 绑定到局域网或 Tailscale 地址，并限制谁可以连接。
- 你不完全信任本地网络（共享办公室、通过 VPN 使用的公共 Wi-Fi 等）。
- 你通过反向代理暴露 daemon，并且想要额外一层身份验证。

对于移动端访问，我们仍然推荐使用中继，它开箱即用地把身份验证和端到端加密结合在一起。密码验证主要适用于直接的局域网或 VPN 连接，在不使用中继的情况下提供访问控制。

## 用 Docker 自托管 {#docker-self-hosting}

原生 Docker 定义运行 Rust daemon，不包含 Web UI 或 Node 运行时。容器内监听 `0.0.0.0:6767`。此次迁移尚未发布，请按 [Docker](/docs/docker) 构建本地镜像，不要假定已发布镜像包含它。

对于 Docker 部署：

- 在把端口发布到局域网、VPN 或公网地址之前，设置 `PASEO_PASSWORD`。
- 在可信本地连接之外，在反向代理使用 HTTPS/WSS。
- 为你用来访问容器的所有 DNS 名称设置 `PASEO_HOSTNAMES`。
- 让 `/workspace` 挂载只包含 Agent 应该能读写的仓库。
- 把 `/home/paseo` 视为敏感目录，它可能包含 daemon 状态和 Provider 凭据。

镜像以非 root 的 `paseo` 用户运行 daemon 和它启动的 Agent，但容器用户隔离不能替代谨慎的挂载。Agent 仍然可以访问你挂载到容器中的任何代码和凭据。

Compose 和反向代理的示例见 [Docker](/docs/docker)。

## 原生桌面信任边界 {#native-desktop-trust-boundary}

Flutter 桌面客户端不包含 Electron 或 Chromium，Chromium 沙盒参数及辅助程序安装说明不适用。这并不隔离 Agent：Provider 和脚本/设置任务 PTY 拥有 daemon 用户的文件系统与凭据访问权限。客户端提供只读任务日志，不提供交互式终端 UI。

内置集成在 Rust 中运行。可选 JavaScript 后端插件通过外部安装的 Node 20+ 执行受信任代码（可用 `PASEO_PLUGIN_NODE` 选择）。独立 worker 进程不是安全沙盒，只安装可信插件。原生产品不包含可执行 React UI 插件或内嵌浏览器自动化。

## Agent 身份验证 {#agent-authentication}

Paseo 封装了 Agent CLI（Claude Code、Codex、OpenCode、Muse Code），但不管理它们的身份验证。每个 Agent Provider 自行处理凭据：

- **Claude Code**：通过 Anthropic 的 OAuth 流程验证，保存在 `~/.claude/` 中
- **Codex**：使用你的 OpenAI API 密钥或 OAuth 会话
- **OpenCode**：通过各 Provider 专用的 API 密钥配置

Agent 在你的用户上下文中使用现有凭据运行。Paseo 也会读取显式配置的 Provider 凭据，包括 OpenAI 语音密钥，因此应将 `config.json`、daemon 环境和备份视为敏感状态保护。这些凭据与中继配对及 Hub 注册不同。

## Hub 身份与凭据 {#hub-identities-and-credentials}

Hub CLI 登录和 daemon 注册是两个独立的身份。`paseo hub login [origin]` 会在 `PASEO_HOME` 下的一个私有文件中保存一个持久的、组织范围的个人凭据，以规范化后的 Hub 地址为键。保存的凭据从不会发送到其他地址。请把 `PASEO_HOME` 当作敏感的本地状态加以保护。

Hub CLI 凭据是 bearer 密钥。远程 Hub 地址必须使用 HTTPS；只有回环地址的开发环境（`localhost`、`127.0.0.1` 和 `[::1]`）才接受明文 HTTP。

`paseo hub connect [origin]` 只用这个凭据（或显式提供的 API 密钥）申请一个短期有效的一次性注册令牌。daemon 用这个令牌完成交换，并保留它自己独立生成的关系凭据。退出 CLI 登录不会悄悄移除 daemon 的权限。交互式退出会先完成已确认的同一地址 daemon 断开，再删除登录。在 JSON 和非交互使用中，`logout` 从不提示也不断开；只有当自动化打算同时移除两个身份时，才传入 `--disconnect-daemon`。

`--api-key` 和 `PASEO_HUB_API_KEY` 会覆盖已保存的登录，但不会被持久化。用于自动化时，优先通过环境变量或密钥管理器注入；当本地进程列表或 shell 历史对其他用户可见时，避免使用命令行参数。

## 建议 {#recommendations}

- **使用中继**进行移动端访问，这是最简单的选择，且所有流量都经过端到端加密
- **像对待密码一样对待二维码**，除非你同时设置了密码，否则任何拿到配对信息的人都可以连接你的 daemon
- **设置密码**，如果你绑定到网络地址，它可以阻止未经授权的客户端控制你的 Agent
- **不要在没有密码的情况下绑定到 0.0.0.0**，否则你网络中的任何设备都可以连接
- **严格限定 Docker 挂载范围**，Agent 可以访问挂载的工作区和 Provider 凭据
- **保持 daemon 为最新版本**，安全改进会定期发布
- **保护 Hub 配置分支**，对 `.paseo` 文件包的推送权限决定了该项目能访问什么，见 [Hub 的工作原理](/docs/hub/concepts)
