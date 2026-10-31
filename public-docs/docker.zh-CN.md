---
title: Docker
description: 构建并运行原生 Rust daemon 容器，供远程 Flutter 客户端连接。
nav: Docker
---

# Docker {#docker}

原生容器为服务器和开发主机运行 Rust daemon 与 CLI。请使用远程 Flutter 桌面/移动客户端或 CLI 连接；它不提供 Web 产品客户端，也不包含 Node、Electron、Chromium 或内嵌浏览器自动化。

0.13.0 原生迁移检出尚未发布，已发布的 `latest` 镜像不能证明包含此运行时。请从当前检出构建本地镜像：

```bash
docker build -f docker/base/Dockerfile -t paseo-native:local .
```

这是本地构建，不是已签名或已验证的发布产物。

```bash
docker run -d --name paseo \
  -p 6767:6767 \
  -e PASEO_PASSWORD=change-me \
  -v "$PWD/paseo-home:/home/paseo" \
  -v "$PWD:/workspace" \
  paseo-native:local
```

在 Flutter 客户端中添加 `127.0.0.1:6767` 直连，使用配置的密码。从另一台机器连接时，请使用 daemon 主机的可达地址，而非回环地址。

## 镜像包含什么 {#what-the-image-includes}

该镜像：

- 安装 Rust Paseo daemon、CLI 及其相邻运行时资源
- 为原生客户端提供 API，不包含 Web UI
- 在容器内监听 `0.0.0.0:6767`
- 把 daemon 状态保存在 `/home/paseo/.paseo` 下
- 以非 root 的 `paseo` 用户运行 daemon 和启动的 Agent

该镜像不包含 Claude Code、Codex、OpenCode、Copilot、Pi 或 Muse Code 等 Agent CLI。请用一个小的子镜像添加你使用的 Agent。

在宿主机上运行 CLI 命令时需要显式指定容器，例如 `paseo project ls --host 127.0.0.1:6767`。如果不指定端点，CLI 会查找本地 home 的 supervisor。容器的环境变量设置属于部署层面的覆盖；重启 worker 会保留它们。完整替换 supervisor 由你的容器管理器负责。

## Docker Compose {#docker-compose}

```yaml
services:
  paseo:
    image: paseo-native:local
    container_name: paseo
    restart: unless-stopped
    ports:
      - "6767:6767"
    environment:
      PASEO_PASSWORD: "change-me"
      # PASEO_HOSTNAMES: "paseo.example.com,.lan"
    volumes:
      - ./paseo-home:/home/paseo
      - ./workspace:/workspace
```

启动：

```bash
docker compose up -d
```

## 安装 Agent CLI {#install-agent-clis}

可参考现有的 [`docker/Dockerfile.agents.example`](https://github.com/getpaseo/paseo/blob/main/docker/Dockerfile.agents.example)，将基础镜像改为本地原生镜像。按照各 Provider 支持的分发方式安装，并提供其所需运行时。基础镜像没有 `node` 或 `npm`；使用 npm 安装 Provider 前，需要在子镜像中显式添加这些工具。

可选 JavaScript 后端插件同样需要外部安装的 Node 20+，必要时用 `PASEO_PLUGIN_NODE` 指定。内置 Rust 集成不需要它。仓库构建使用 Node 24。

然后在 Compose 中使用构建出的子镜像。

保持子镜像的用户为 root。基础镜像的入口点只在首次运行配置挂载卷时使用 root，之后会以非 root 的 `paseo` 用户运行 daemon 和启动的 Agent。

你可以通过传入 Provider 环境变量来认证 Agent，也可以在容器内运行 Provider 的登录流程：

```bash
docker exec -it --user paseo paseo codex
docker exec -it --user paseo paseo claude
```

Agent 凭据会持久保存在 `/home/paseo` 中。

## 挂载卷 {#volumes}

大多数部署需要挂载两个路径：

| 挂载点        | 用途                                                     |
| ------------- | -------------------------------------------------------- |
| `/home/paseo` | Paseo 状态，以及 `.codex`、`.claude` 等 Agent 配置和凭据 |
| `/workspace`  | Paseo 和启动的 Agent 可以读写的代码                      |

在 Linux 上，内置的 `paseo` 用户的 uid/gid 是 `1000:1000`。请让挂载的目录对该用户可写，或者使用 Docker 的 `--user` / Compose 的 `user:` 选项运行容器。

## 反向代理 {#reverse-proxy}

把普通 HTTP 流量和 WebSocket 升级请求转发到容器。

Caddy：

```caddy
paseo.example.com {
  reverse_proxy 127.0.0.1:6767
}
```

Nginx：

```nginx
server {
    listen 443 ssl;
    server_name paseo.example.com;

    location / {
        proxy_pass http://127.0.0.1:6767;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

如果你通过 DNS 名称访问 Paseo，请允许该主机名：

```yaml
environment:
  PASEO_HOSTNAMES: "paseo.example.com,.lan"
```

IP 和 `localhost` 默认是允许的。

## 安全 {#security}

对于发布的端口或网络可达部署，请设置 `PASEO_PASSWORD`。在可信本地连接之外，请在反向代理使用 HTTPS/WSS。配置密码后，daemon API 和 WebSocket 需要认证；不存在静态 Web 产品界面。

Agent 可以访问你挂载到 `/workspace` 的任何内容，以及你放在 `/home/paseo` 中的任何凭据。请把这些挂载限制在 Agent 应该能使用的范围内。

完整的 daemon 信任模型见[安全](/docs/security)。

## 故障排查 {#troubleshooting}

- **客户端无法连接**：如果设置了 `PASEO_PASSWORD`，请使用相同的密码添加直连。
- **403 Host not allowed**：把 `PASEO_HOSTNAMES` 设置为你使用的 DNS 名称。
- **Provider 不可用**：在子镜像中安装该 Agent CLI，或确保二进制文件在 `PATH` 中。
- **`/workspace` 中的权限错误**：让挂载的目录对 uid/gid `1000:1000` 可写，或以宿主机的 uid/gid 运行容器。
- **日志**：运行 `docker logs paseo`，或在容器内查看 `/home/paseo/.paseo/daemon.log`。
