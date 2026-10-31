# Agent stream performance

Provider output flows through the Rust daemon's agent owner and transport into the Dart client and native Flutter timeline. It does not pass through React reducers, Hermes, a DOM canvas or an Electron renderer.

Ownership:

- `packages/daemon/src/agent/timeline_stream.rs`, `timeline.rs`, `timeline_storage.rs`, and adjacent agent modules: canonical history, event ordering and publication.
- `packages/daemon/src/transport`: connection lifecycle and output/backpressure.
- `packages/flutter_app/lib/core/client.dart`: wire events and authoritative requests.
- `packages/flutter_app/lib/features/chat`: timeline/controller and native rendering.

## Correctness before smoothing

Live events provide immediacy; authoritative history and sequence/cursor reconciliation recover gaps. Never drop events or invent text to make a stream appear smoother. Client disconnect, hidden views and slow consumers must not lose durable provider output. Cancelling a view subscription is not cancelling a provider turn.

Bound memory and update work. Avoid rereading/rebuilding the complete retained history for every delta, eagerly materializing offscreen content, or sending per-token filesystem writes without the owner's ordering guarantees. Measure provider arrival, daemon processing, network delivery, Dart state work and Flutter frames separately.

## Evidence

Use repeatable input for an isolated performance comparison and a real provider for interoperability. Report first visible output, sustained cadence, long-gap behavior, CPU/memory and retained-history size, not one average that hides stalls. A controlled development provider is useful for timing but must not be called a real-provider test.

Retired web/React benchmark paths and earlier Expo measurements are not native baselines. Capture actual native traces/screens and retain the run conditions. Script/setup PTY output is a separate pipeline described in [terminal-performance](terminal-performance.md). Current executed evidence is in [testing](testing.md).
