# Script and setup log performance

The native application has **read-only script/setup logs**, not an interactive terminal emulator. The Rust daemon retains PTYs for workspace setup and service scripts. Removing a terminal UI does not remove process, PTY, replay or stream responsibilities.

Implementation ownership is `packages/daemon/src/terminals.rs` and `terminals/`, including runtime, streams, screen state, scripts and setup. Flutter presentation lives under `packages/flutter_app/lib/features/logs` and the retained terminal-output feature code. A source directory name does not imply interactive input is part of the product.

## Boundaries to measure

Measure process spawn/start, first output, sustained output, replay after reconnect, backpressure, memory under large logs, completion/error publication and cancellation. Separate server PTY cost, network delivery and Flutter rendering. An isolated parser benchmark does not prove end-to-end log latency.

Slow or hidden clients must not create unbounded buffers or stall unrelated sessions. Subscriptions have explicit owners and cleanup. Script output remains observable across view changes; closing a log view must not silently terminate a running service. Conversely, stopping a script must release its owned process tree and PTY.

Use a disposable workspace and daemon home. Preserve stdout/stderr, exit status and timing evidence. Test failure, worker restart and client reconnect, not only a short successful command. Do not run the retired xterm/browser/React terminal benchmarks as native acceptance checks.

No hardware/platform measurements are inferred from CI definitions. See [testing](testing.md) for actual recorded native execution and [terminal activity](terminal-activity.md) for the retained lifecycle scope.
