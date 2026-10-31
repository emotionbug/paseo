# Protocol Validation

The retained **TypeScript SDK** validates inbound WebSocket messages with a zod-aot generated validator instead of runtime Zod on its hot path. Zod remains the authoring source for TypeScript schemas and types. The Rust daemon and Dart client implement the same wire contract natively; they do not execute this JavaScript validator.

The earlier Hermes measurement motivated the SDK optimization but is not a Flutter benchmark. Native validation performance and correctness require Rust/Dart checks and actual native transport evidence.

## Runtime Path

`packages/protocol/src/validation/ws-outbound.ts` is the shipped boundary. It validates the envelope with a generated validator that checks only each session message's `type` tag, then runs the generated validator for that one message type and returns the validated data. Issue paths from the message validator, including nested union branches, are prefixed with `message`, so callers see the same issues as when one validator covered the whole envelope. It does not normalize, repair, or re-validate the generated result.

Keep one generated function per message type. zod-aot inlines nested schemas; a single enormous validator can cause expensive V8 compilation in external SDK consumers and optional Node workers. `packages/protocol/tests/validation/ws-outbound.test.ts` bounds generated function size. This is an SDK/tooling concern, not an Electron or mobile JavaScript runtime.

Generated validators preserve unknown keys where Zod object parsing strips them. The client dispatch path uses known `type` and payload fields, so this passthrough behavior is accepted for inbound messages. The wire format is unchanged.

Provider model normalization is a parser-side compatibility shim in the client consumers that need it. Newer daemons normalize at the provider registry source.

## Codegen Ownership

The protocol package owns generation.

- `packages/protocol/src/validation/ws-outbound-envelope.ts` defines the tag-only envelope schema and exposes the session message schemas that get their own validators.
- `packages/protocol/scripts/generate-validation-aot.mjs` writes the zod-aot discovery entry and the runtime schema metadata, runs the exact-pinned compiler, and applies the small local compiler patches before generation. It also emits the `type` → validator map the boundary dispatches through.
- `packages/protocol/scripts/watch-validation-aot.mjs` reruns generation while editing protocol sources.
- `packages/protocol/codegen/generated/ws-outbound.compile.ts`, `packages/protocol/src/generated/validation/ws-outbound.aot.ts`, and `packages/protocol/src/generated/validation/ws-outbound-schema-metadata.ts` are generated and gitignored. The metadata module lets generated code reach schema fallback/default values.

Generation runs from protocol-owned lifecycle hooks: `prebuild`, `pretypecheck`, `pretest`, and `watch`. Installs do not run generation: published packages consume protocol from prebuilt `dist`, and local build/typecheck/test flows generate the source file at the point it is actually needed.

## Regression Tests

zod-aot is exact-pinned and young enough that compiler patches are treated as part of this package. `packages/protocol/tests/validation/ws-outbound.test.ts` keeps small regression tests for the patched cases:

- discriminated-union branch output must propagate `.default()` fields
- current sequential item routing must accept `tool_call`-like status branches
- generated runtime imports must keep `.js` extensions for packaged Node ESM
- the generated WebSocket envelope accepts a minimal valid message and rejects a corrupted one

## Schema Purity

Message schemas are structural declarations. Do not put `.transform()`, `.catch()`, or `.preprocess()` on WebSocket message schemas. If parsed data needs normalization, put it in an explicit consumer or post-validation pass.

Use `z.discriminatedUnion()` when every branch has a shared literal tag. Plain `z.union()` is acceptable only when there is no shared literal discriminator or when a generated-code regression test proves that specific shape is miscompiled.

Defaults are allowed only on primitive leaves. Do not place `.default()` on large arrays, item schemas, or big containers in inbound message schemas.
