---
title: Security
description: "Security model for Paseo: architecture overview, connection methods, relay encryption, and best practices."
nav: Security
order: 5
category: Getting started
---

# Security

Paseo follows a client-server architecture, similar to Docker. The daemon runs on your machine and manages your coding agents. Clients (the mobile app, CLI, or web interface) connect to the daemon to monitor and control those agents.

Your code never leaves your machine. Paseo is a local-first tool that connects directly to your development environment.

## Architecture

The Paseo daemon can run anywhere you want to execute agents: your laptop, a Mac Mini, a VPS, or a Docker container. The daemon listens for connections and manages agent lifecycles.

Clients connect to the daemon over WebSocket. There are two ways to establish this connection:

- **Relay connection (recommended)**, The daemon connects outbound to our relay server, and clients meet it there. No open ports required.
- **Direct connection**, The daemon listens on a network address and clients connect directly.

## Relay connections (recommended)

The relay is the simplest way to connect from your phone. It requires no VPN setup, no port forwarding, and no firewall configuration. The daemon can stay bound to localhost or a socket file, it connects _outbound_ to the relay, and your phone meets it there. The official relay server is the open-source Elixir service at [getpaseo/paseo-relay](https://github.com/getpaseo/paseo-relay).

Relay is off on new installations. When you pair a device from `paseo`, `paseo daemon pair`, or Paseo Desktop, Paseo asks before enabling it. Choosing not to enable relay leaves the daemon available for direct TCP, Tailscale, or other VPN connections and does not create a pairing QR code. Use `--relay` with the CLI pairing or startup command to opt in without an interactive prompt.

> **The relay is designed to be untrusted.** All traffic between your phone and daemon is end-to-end encrypted. The relay server cannot read your messages, see your code, or modify traffic without detection. Even if the relay is compromised, your data remains protected.

### How it works

1. The daemon generates a persistent ECDH keypair and stores it in `$PASEO_HOME/daemon-keypair.json`
2. When you scan the QR code or click the pairing link, your phone receives the daemon's public key
3. Your phone sends a handshake message with its own public key. The daemon will not accept any commands until this handshake completes.
4. Each side combines its Curve25519 key with the peer public key, then uses the resulting NaCl
   `box` channel for XSalsa20-Poly1305 protected payloads.

The relay sees only: IP addresses, timing, message sizes, and session IDs. It cannot read message contents, forge messages, or derive encryption keys from observing the handshake.

### Why the relay can't attack you

The daemon requires a valid cryptographic handshake before processing any commands. A compromised relay cannot:

- **Send commands**, Without your phone's private key, it cannot complete the handshake
- **Read your traffic**, Only the paired endpoints can open the NaCl `box` payloads
- **Forge messages**, The authenticated cipher rejects modified payloads
- **Replay old messages**, Each session derives fresh encryption keys

### Trust model

The QR code or pairing link is the trust anchor. It contains the daemon's public key, which is required to establish the encrypted connection. Treat it like a password, don't share it publicly.

## Direct connections

By default, the daemon listens on `127.0.0.1:6767` (localhost only). This is safe for local CLI usage but not reachable from your phone or other devices.

For relay and Tailscale setup instructions, see [Connectivity](/docs/connectivity).

### Socket file (CLI only)

For maximum isolation, you can configure the daemon to listen on a Unix socket file instead of a TCP port. This prevents any network access entirely, only processes on the same machine can connect. The CLI supports this mode, but the mobile app and web interface require a network connection.

### VPN access

Use a VPN such as [Tailscale](https://tailscale.com) when you want a direct connection outside your local network. The VPN encrypts the traffic and keeps the daemon off the public internet. Bind the daemon to its VPN address, set a Paseo password, then add that address as a direct connection in the client.

### Binding to 0.0.0.0

> **Warning:** Binding to `0.0.0.0` makes the daemon reachable on all network interfaces, including public Wi-Fi and local networks. This can expose your daemon to unauthorized access. If you must bind to all interfaces, ensure you have proper firewall rules and review your `hostnames` configuration.

## DNS rebinding protection

**CORS is not a complete security boundary.** It controls which browser origins can make requests, but does not prevent a malicious website from resolving its domain to your local machine (DNS rebinding).

Paseo uses a host allowlist to validate the `Host` header on incoming requests. Requests with unrecognized hosts are rejected.

Configure via `daemon.hostnames` in `config.json`:

- Default (`[]`): allow `localhost`, `*.localhost`, and all IP addresses
- `['.example.com']`: allow `example.com` and any subdomain, plus defaults
- `true`: allow any host (not recommended)

## Password authentication

By default, anyone who can reach the daemon's listening address can connect. On localhost this is fine, only local processes have access. But if you bind to a network interface (e.g. your LAN IP or `0.0.0.0`), or if you don't fully trust your local network, you can require a password.

When a password is configured, all HTTP requests must include an `Authorization: Bearer <password>` header, and every WebSocket client, including relay clients, must send the password when it connects. Relay clients send it inside the end-to-end encrypted channel, so the relay never sees it. Unauthenticated HTTP requests receive a `401 Unauthorized` response. Only the `/api/health` liveness endpoint is exempt, so that process supervisors and load balancers can probe without credentials.

The native daemon does not serve a bundled web product client. Flutter clients and the Rust CLI authenticate to its API; no public static login page is required.

The password is stored as a bcrypt hash in `config.json`, the daemon never stores it in plaintext. See [Configuration](/docs/configuration#password-authentication) for setup instructions.

### What password auth does and does not do

- **Does:** Prevents unauthorized clients from controlling your agents, even if they can reach the daemon over the network.
- **Does not:** Encrypt traffic. Password auth protects access, not confidentiality. If you need encrypted connections over an untrusted network, use the relay (which provides end-to-end encryption) or a VPN like Tailscale.

### When to use it

- You want to bind the daemon to a LAN or Tailscale address and restrict who can connect.
- You don't fully trust your local network (shared office, public Wi-Fi with a VPN, etc.).
- You're exposing the daemon via a reverse proxy and want an additional authentication layer.

We still recommend the relay for mobile access, it combines authentication with end-to-end encryption out of the box. Password auth is primarily useful for direct LAN or VPN connections where you want access control without the relay.

## Docker self-hosting

The native Docker definition runs the Rust daemon, without a bundled web UI or Node runtime. It binds to `0.0.0.0:6767` inside the container. This migration is unpublished; build the local image as described in [Docker](/docs/docker), rather than assuming a published image contains it.

For Docker deployments:

- Set `PASEO_PASSWORD` before publishing the port to a LAN, VPN, or public address.
- Use HTTPS/WSS at your reverse proxy outside trusted local connections.
- Set `PASEO_HOSTNAMES` for any DNS names you use to reach the container.
- Keep `/workspace` mounts scoped to repositories the agents should be able to read and write.
- Treat `/home/paseo` as sensitive, it can contain daemon state and provider credentials.

The image runs the daemon and launched agents as the non-root `paseo` user, but container user isolation is not a substitute for careful mounts. Agents can still access whatever code and credentials you mount into the container.

See [Docker](/docs/docker) for Compose and reverse proxy examples.

## Native desktop trust boundary

The Flutter desktop client does not bundle Electron or Chromium. Chromium sandbox switches and helper-installation instructions do not apply. This does not sandbox agents: providers and script/setup PTYs run with the daemon user's filesystem and credential access. The client exposes read-only task logs, not an interactive terminal UI.

Built-in integrations run in Rust. Optional JavaScript backend plugins execute trusted code using externally installed Node 20+ (`PASEO_PLUGIN_NODE` may select it). Do not treat the separate worker process as a security sandbox; install only plugins you trust. Executable React UI plugins and embedded browser automation are not part of the native product.

## Agent authentication

Paseo wraps agent CLIs (Claude Code, Codex, OpenCode, Muse Code) but does not manage their authentication. Each agent provider handles its own credentials:

- **Claude Code**, authenticates via Anthropic's OAuth flow, stored in `~/.claude/`
- **Codex**, uses your OpenAI API key or OAuth session
- **OpenCode**, configured via provider-specific API keys

Agents run in your user context with their existing credentials. Paseo can also read explicitly configured provider credentials, including OpenAI speech keys, so protect `config.json`, the daemon environment, and backups as sensitive state. These credentials are distinct from relay pairing and Hub enrollment.

## Hub identities and credentials

Hub CLI login and daemon enrollment are separate identities. `paseo hub login [origin]` stores a durable organization-scoped human credential in a private file under `PASEO_HOME`, keyed by the normalized Hub origin. A stored credential is never sent to another origin. Protect `PASEO_HOME` as sensitive local state.

Hub CLI credentials are bearer secrets. Remote Hub origins must use HTTPS; cleartext HTTP is accepted only for loopback development origins (`localhost`, `127.0.0.1`, and `[::1]`).

`paseo hub connect [origin]` uses that credential, or an explicit API key, only to request a short-lived one-time enrollment token. The daemon exchanges the token and retains its own independently generated relationship credential. Logging out of the CLI does not silently remove daemon authority. Interactive logout completes any accepted same-origin daemon disconnection before deleting the login. In JSON and noninteractive use, `logout` never prompts or disconnects; pass `--disconnect-daemon` only when automation intends to remove both identities.

`--api-key` and `PASEO_HUB_API_KEY` override stored login without being persisted. Prefer environment or secret-manager injection for automation, and avoid command-line flags when local process listings or shell history are visible to other users.

## Recommendations

- **Use the relay** for mobile access, it's the simplest option and all traffic is end-to-end encrypted
- **Treat the QR code like a password**, anyone with the pairing offer can connect to your daemon unless you also set a password
- **Set a password** if you bind to a network address, it prevents unauthorized clients from controlling your agents
- **Never bind to 0.0.0.0 without a password**, without one, any device on your network can connect
- **Scope Docker mounts tightly**, agents can access mounted workspaces and provider credentials
- **Keep your daemon updated**, security improvements are released regularly
- **Protect the Hub configuration branch**, push access to the `.paseo` bundle controls what that project can reach, see [How Hub works](/docs/hub/concepts)
