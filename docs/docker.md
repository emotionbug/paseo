# Running the native daemon in Docker

The Dockerfile builds the **Rust daemon/CLI** and stages its native runtime assets. The final image has no Node executable, Electron, Chromium, bundled web UI, or speech model weights. Connect with a native Flutter client or the CLI; the HTTP root is not an application page.

This cutover has not been published as a new container release. Do not assume an existing registry `latest` image contains this implementation. Native speech-library distribution requirements remain unresolved; see [release](release.md#distribution-requirements) before publishing an image.

## Build locally

From the repository root:

```sh
docker build -f docker/base/Dockerfile -t paseo:local .
```

The multi-stage build uses Node 24 only in the build stage for compiler/SDK assets and packaging, and Rust for the daemon. Build stages target the final platform so the executable, optional extension compiler and native libraries agree on architecture. The final image is Debian-based and includes shell, Git, SSH, certificates and process-management utilities.

## Run an isolated instance

```sh
docker run -d --name paseo-native \
  -p 6767:6767 \
  -e PASEO_PASSWORD=change-me \
  -v "$PWD/paseo-home:/home/paseo" \
  -v "$PWD/workspace:/workspace" \
  paseo:local
```

Choose a real secret before exposing a port. The default container listen address is `0.0.0.0:6767`; the persistent daemon home is `/home/paseo/.paseo`. The example maps a separate workspace directory rather than the whole host filesystem. Connect the native client to the machine's address and port with the same password.

For local-only access bind the published port to loopback (`-p 127.0.0.1:6767:6767`). For remote access use a trusted VPN, encrypted relay, or a correctly secured TLS reverse proxy. A proxy must forward WebSocket upgrades, not just ordinary HTTP. No static web-app configuration is needed.

## Persistence and agents

The `/home/paseo` volume holds `.paseo` state plus provider credential/config directories such as `.claude`, `.codex` and XDG directories. The `/workspace` mount contains project files. Preserve these separately from the image; recreating a container must not reset session history or credentials.

Install and authenticate the agent CLIs you need in an explicitly extended image or controlled runtime environment. The base runtime is not a bundled set of provider executables. A provider CLI may itself require Node; that is an opt-in provider dependency, not a requirement of the Rust daemon.

JavaScript backend extensions similarly need an **externally supplied Node** executable. Their worker/compiler assets can be present without starting Node; built-in integrations run in Rust. Do not add Node to the default image merely to launch built-ins.

The daemon retains workspace setup/service PTYs. Native clients show their read-only logs; there is no interactive terminal emulator or embedded browser in the app.

## Diagnostics

Use container logs and `paseo daemon status` against the selected instance. The Docker health check checks daemon health, not page rendering. Check the listen address, published port, password, provider installation, mount permissions and retained home when connection or startup fails. A browser page failing to load at `/` is not evidence that a web UI needs to be rebuilt: this runtime does not serve one.

The files under `docker/` contain build/compose examples, not proof that the native image has been published or executed on every architecture. See [testing](testing.md) for actual evidence boundaries.
