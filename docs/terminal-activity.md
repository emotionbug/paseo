# Script and setup activity

Workspace setup and service scripts remain **Rust daemon-owned PTY operations**. Their native client surface is read-only output with lifecycle controls, not an interactive terminal emulator. Do not remove this backend while removing terminal UI code.

`packages/daemon/src/terminals` owns process startup, output, screen/replay state, hooks, setup and scripts. Workspace lifecycle under `projects/` determines when setup and teardown operations belong to a workspace. The native process layer verifies process identity and owns termination; a client must not send signals to an arbitrary PID.

Activity and output have different meanings. Receiving bytes does not establish successful completion; silence does not establish that a command stopped. Preserve the script's running/completed/failed state and actual exit outcome, and present errors in the log surface. Agent hook activity must remain associated with the correct host/session rather than inferred from whichever tab is visible.

Client reconnect or closing a log view changes observation, not daemon ownership. Explicit script stop and workspace teardown must release owned resources according to their lifecycle. Avoid duplicate subscriptions and unbounded replay buffers when multiple clients observe the same script.

Interactive shell input, xterm rendering and embedded browser terminals are outside the native product scope. Native CLI chat still exists as a separate terminal program. For measurement and verification, see [script/log performance](terminal-performance.md) and [testing](testing.md).
