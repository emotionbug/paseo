# Ad-hoc daemon testing

Run the real Rust executable against a disposable home and endpoint. Do not import a TypeScript server into an in-process harness, and do not touch the installed daemon on port 6767.

## Start an isolated instance

Build the executable and required runtime assets with `npm run build:daemon`. Use `npm run dev:daemon` for the checkout's `.dev/paseo-home` and `127.0.0.1:6768`, or explicitly supply a separate `PASEO_HOME` and `PASEO_LISTEN` to the native executable:

```sh
PASEO_HOME=/absolute/path/to/disposable-home \
PASEO_LISTEN=127.0.0.1:17678 \
./target/debug/paseo daemon run
```

Choose an unused endpoint and a genuinely disposable directory. Respect `CARGO_TARGET_DIR` and use `paseo.exe` on Windows. `daemon run` is the foreground mode; managed supervisor tests must deliberately use the managed start path instead.

Connect using the native CLI, the Dart client, or `@getpaseo/client` from `packages/client`. For example, in another terminal:

```sh
./target/debug/paseo --host 127.0.0.1:17678 ls
```

The SDK is an external client, not a Node daemon. Preserve request IDs, authentication and advertised capabilities in a protocol test rather than calling internal state directly and calling that end-to-end.

## What a run must record

Record the source revision, executable and runtime-asset identity, selected home/endpoint, actual commands, responses, process lifecycle and failures. Real provider tests require real installed providers and credentials; controlled development providers do not establish provider interoperability.

When testing persistence, restart the same isolated instance and read back through the public protocol. When testing desktop ownership, exercise the actual Dart service → Rust desktop-host IPC: direct daemon commands do not prove desktop policy. Keep unowned daemons running and verify process-start identity before signalling anything.

Clean up only processes and files created by the test. A failure must still release owned resources without deleting unrelated user state. Do not commit throwaway scripts, credentials or model downloads. [Testing](testing.md) records the current native evidence and missing platform execution.
