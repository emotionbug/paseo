#!/usr/bin/env bash
# Run against an already built image; never uses host homes, credentials or ports.
set -euo pipefail
image="${PASEO_TEST_IMAGE:-${1:-}}"
: "${image:?Set PASEO_TEST_IMAGE or pass the locally built image tag}"
name="paseo-native-smoke-$$-${RANDOM}-${RANDOM}"
volume="${name}-home"
created_volume=false
created_container=false
cleanup() {
  if [[ "$created_container" == true ]]; then docker rm -fv "$name" >/dev/null; fi
  if [[ "$created_volume" == true ]]; then docker volume rm "$volume" >/dev/null; fi
}
trap cleanup EXIT
if docker volume inspect "$volume" >/dev/null 2>&1; then
  echo "Refusing to reuse existing volume $volume" >&2
  exit 1
fi
docker volume create "$volume" >/dev/null
created_volume=true

# Root preparation may claim root-owned directory entries, never recurse into
# mounted data or change files/directories already owned by another host UID.
docker run --rm --network none --entrypoint bash \
  --mount "type=volume,src=$volume,dst=/home/paseo" "$image" -euc '
  chown 0:0 /home/paseo /home/paseo/.paseo
  touch /home/paseo/.paseo/root-owned-sentinel
  chown 0:0 /home/paseo/.paseo/root-owned-sentinel
  chown 23456:23457 /home/paseo/.claude
'
docker run --rm --network none \
  --mount "type=volume,src=$volume,dst=/home/paseo" "$image" bash -euc '
  test "$(id -u):$(id -g)" = 1000:1000
  test "$(stat -c %u:%g /home/paseo)" = 1000:1000
  test "$(stat -c %u:%g /home/paseo/.paseo)" = 1000:1000
  test "$(stat -c %u:%g /home/paseo/.paseo/root-owned-sentinel)" = 0:0
  test "$(stat -c %u:%g /home/paseo/.claude)" = 23456:23457
  ! command -v node
  ! command -v npm
  ! command -v electron
  test "$(readlink -f /usr/local/bin/paseo)" = /opt/paseo/paseo
  test -x /opt/paseo/paseo
  test -f /opt/paseo/lib/libsherpa-onnx-c-api.so
  test -d /opt/paseo/skills/paseo
  test -f /opt/paseo/plugin-worker/worker.mjs
  test -s /opt/paseo/RUST-THIRD-PARTY-NOTICES.json
  test -s /opt/paseo/RUST-THIRD-PARTY-NOTICES.txt
  test -s /opt/paseo/plugin-worker/THIRD-PARTY-NOTICES.json
  test -s /opt/paseo/plugin-worker/THIRD-PARTY-NOTICES.txt
  test -s /opt/paseo/lib/SPEECH-THIRD-PARTY-NOTICES.json
  test -s /opt/paseo/lib/SPEECH-THIRD-PARTY-NOTICES.txt
  test -s /opt/paseo/THIRD-PARTY-NOTICES.txt
  test ! -e /etc/paseo-server-entry
  case "$(cat /opt/paseo/paseo-runtime.json)" in
    *\"installationKind\":\ \"container\"*) ;;
    *) exit 1 ;;
  esac
  paseo --version
'
# Explicit non-root execution must retain the selected UID/GID.
docker run --rm --network none --user 23456:23457 \
  --tmpfs /scratch:mode=1777 -e HOME=/scratch -e PASEO_HOME=/scratch/.paseo \
  -e CLAUDE_CONFIG_DIR=/scratch/.claude -e CODEX_HOME=/scratch/.codex \
  -e XDG_CONFIG_HOME=/scratch/.config -e XDG_DATA_HOME=/scratch/.local/share \
  -e XDG_STATE_HOME=/scratch/.local/state -e XDG_CACHE_HOME=/scratch/.cache \
  "$image" bash -euc 'test "$(id -u):$(id -g)" = 23456:23457'
status=0
docker run --rm --network none "$image" bash -c 'exit 37' || status=$?
test "$status" = 37

# No published ports or outbound network. Use a non-default listen port to
# exercise healthcheck address selection rather than a hard-coded endpoint.
docker create --name "$name" --network none \
  --mount "type=volume,src=$volume,dst=/home/paseo" \
  -e PASEO_LISTEN=0.0.0.0:6779 -e PASEO_PASSWORD=isolated-smoke-only \
  -e PASEO_RELAY_ENABLED=false -e PASEO_DICTATION_ENABLED=false \
  -e PASEO_VOICE_MODE_ENABLED=false "$image" >/dev/null
created_container=true
docker start "$name" >/dev/null
healthy=false
for ((attempt=0; attempt<90; attempt++)); do
  if docker exec "$name" paseo-docker-healthcheck; then healthy=true; break; fi
  if [[ "$(docker inspect --format '{{.State.Running}}' "$name")" != true ]]; then break; fi
  sleep 1
done
if [[ "$healthy" != true ]]; then docker logs "$name"; exit 1; fi
docker exec --user 1000:1000 "$name" bash -euc '
  printf persistent > "$PASEO_HOME/container-smoke-sentinel"
  test "$(stat -c %u /proc/1)" = 0
  test -s "$PASEO_HOME/paseo.pid"
'
# Docker sends SIGTERM to tini. Foreground CLI must stop its owned supervisor
# and worker before the grace period; SIGKILL (137) is not an acceptable exit.
docker stop --time 20 "$name" >/dev/null
code="$(docker inspect --format '{{.State.ExitCode}}' "$name")"
if [[ "$code" == 137 ]]; then docker logs "$name"; exit 1; fi
docker run --rm --network none \
  --mount "type=volume,src=$volume,dst=/home/paseo" "$image" bash -euc '
  test "$(cat "$PASEO_HOME/container-smoke-sentinel")" = persistent
  test "$(stat -c %u:%g "$PASEO_HOME/root-owned-sentinel")" = 0:0
'
echo 'Native container isolation, ownership, health, persistence and exit checks passed.'
