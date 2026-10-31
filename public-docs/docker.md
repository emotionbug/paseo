---
title: Docker
description: Build and operate the native Rust daemon container with remote Flutter clients.
nav: Docker
order: 6
category: Getting started
---

# Docker

The native container runs the Rust daemon and CLI for servers and development hosts. Connect with a remote Flutter desktop/mobile client or the CLI; it does not serve a web product client or include Node, Electron, Chromium, or embedded browser automation.

The 0.13.0 native migration checkout is unpublished. Published `latest` images are not evidence of this runtime. Build a local image from this checkout:

```bash
docker build -f docker/base/Dockerfile -t paseo-native:local .
```

This is a local build, not a signed or verified release artifact.

```bash
docker run -d --name paseo \
  -p 6767:6767 \
  -e PASEO_PASSWORD=change-me \
  -v "$PWD/paseo-home:/home/paseo" \
  -v "$PWD:/workspace" \
  paseo-native:local
```

Add a direct connection to `127.0.0.1:6767` in the Flutter client, using the configured password. On another machine, use the daemon host's reachable address instead of loopback.

## What the image includes

The image:

- installs the Rust Paseo daemon and CLI with their adjacent runtime assets
- provides APIs for native clients, without a bundled web UI
- listens on `0.0.0.0:6767` inside the container
- stores daemon state under `/home/paseo/.paseo`
- runs the daemon and launched agents as the non-root `paseo` user

The image does not bundle agent CLIs such as Claude Code, Codex, OpenCode, Copilot, Pi, or Muse Code. Add the agents you use with a small child image.

Host-side CLI commands select the container explicitly, for example `paseo project ls --host 127.0.0.1:6767`. Without an endpoint selector the CLI looks for a local home’s supervisor. Container environment settings are deployment overrides; worker restart preserves them. Your container manager owns full supervisor replacement.

## Docker Compose

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

Start it:

```bash
docker compose up -d
```

## Install agent CLIs

Use the existing [`docker/Dockerfile.agents.example`](https://github.com/getpaseo/paseo/blob/main/docker/Dockerfile.agents.example) as a starting point, setting its base image to your local native image. Install each provider using its supported distribution and include any runtime it requires. The base runtime has no `node` or `npm`, so an npm-based provider installation requires explicitly adding those tools to your child image first.

Optional JavaScript backend plugins likewise require an externally installed Node 20+ runtime, selected with `PASEO_PLUGIN_NODE` when needed. This is not required by built-in Rust integrations. Repository builds use Node 24.

Use the resulting child image in Compose.

Leave the child image user as root. The base entrypoint uses root only for first-run mounted-volume setup, then drops the daemon and launched agents to the non-root `paseo` user.

You can authenticate agents either by passing provider environment variables or by running the provider login flow inside the container:

```bash
docker exec -it --user paseo paseo codex
docker exec -it --user paseo paseo claude
```

Agent credentials persist in `/home/paseo`.

## Volumes

Mount two paths for most deployments:

| Mount         | Purpose                                                                   |
| ------------- | ------------------------------------------------------------------------- |
| `/home/paseo` | Paseo state plus agent config and credentials such as `.codex`, `.claude` |
| `/workspace`  | Code that Paseo and launched agents can read and write                    |

On Linux, the built-in `paseo` user is uid/gid `1000:1000`. Make mounted directories writable by that user, or run the container with Docker's `--user` / Compose `user:` option.

## Reverse proxy

Forward normal HTTP traffic and WebSocket upgrades to the container.

Caddy:

```caddy
paseo.example.com {
  reverse_proxy 127.0.0.1:6767
}
```

Nginx:

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

If you reach Paseo by DNS name, allow that host:

```yaml
environment:
  PASEO_HOSTNAMES: "paseo.example.com,.lan"
```

IPs and `localhost` are allowed by default.

## Security

Set `PASEO_PASSWORD` for any published port or network-reachable deployment. Use HTTPS/WSS at your reverse proxy outside trusted local connections. The daemon API and WebSocket require password authentication when configured; there is no static product web UI.

Agents can access whatever you mount into `/workspace` and whatever credentials you place in `/home/paseo`. Keep those mounts scoped to what the agents should be able to use.

See [Security](/docs/security) for the full daemon trust model.

## Troubleshooting

- **Client cannot connect:** if `PASEO_PASSWORD` is set, add a direct connection with the same password.
- **403 Host not allowed:** set `PASEO_HOSTNAMES` to the DNS names you use.
- **Provider not available:** install that agent CLI in a child image or make sure the binary is on `PATH`.
- **Permission errors in `/workspace`:** make the mounted directory writable by uid/gid `1000:1000`, or run the container as the host uid/gid.
- **Logs:** run `docker logs paseo`, or inspect `/home/paseo/.paseo/daemon.log` inside the container.
