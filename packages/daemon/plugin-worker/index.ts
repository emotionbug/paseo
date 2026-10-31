// Optional JavaScript backend-extension host. Never imported by the native daemon.
// stdout is reserved for bounded JSON frames; plugin diagnostics go to stderr.
import { createPluginWorker, type PluginWorkerChannel } from "./src/plugin-process.js";
import { compilePlugin } from "./src/compiler.js";
import { evaluateBundle } from "./src/bundle-evaluator.js";
import type { PluginProcessRequest } from "./src/plugin-process-protocol.js";
import { readPluginManifest } from "./src/manifest.js";
import { assertPluginCompatibility } from "@getpaseo/protocol/plugin-requirements";
import { access } from "node:fs/promises";
import path from "node:path";
import { Console } from "node:console";
import { Writable } from "node:stream";

const MAX_FRAME = 16 * 1024 * 1024;
const MAX_PENDING = 32 * 1024 * 1024;
const protocolWrite = process.stdout.write.bind(process.stdout);
const listeners = new Set<(message: PluginProcessRequest) => void>();
let worker: { shutdown(): Promise<void> } | undefined;
let initialized = false;
let pendingBytes = 0;
if (process.argv[2] !== "--validate") {
  function capture(stream: "stdout" | "stderr") {
    return new Writable({
      write(chunk, encoding, callback) {
        try {
          const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding);
          for (let offset = 0; offset < bytes.length; offset += 8192)
            send({
              type: "log",
              stream,
              data: bytes.subarray(offset, offset + 8192).toString("base64"),
            });
          callback();
        } catch (error) {
          callback(error instanceof Error ? error : new Error(String(error)));
        }
      },
    });
  }
  const stdout = capture("stdout");
  const stderr = capture("stderr");
  globalThis.console = new Console(stdout, stderr);
  process.stdout.write = stdout.write.bind(stdout) as typeof process.stdout.write;
  process.stderr.write = stderr.write.bind(stderr) as typeof process.stderr.write;
}
/** JSON replacer: byte arrays and serialized Buffers travel as base64 `$bytes`. */
function frameValue(_key: string, item: unknown): unknown {
  if (item instanceof Uint8Array) return { $bytes: Buffer.from(item).toString("base64") };
  const buffer = item as { type?: unknown; data?: unknown } | null;
  if (buffer?.type === "Buffer" && Array.isArray(buffer.data)) {
    return { $bytes: Buffer.from(buffer.data).toString("base64") };
  }
  return item;
}
function send(value: unknown, callback?: () => void) {
  const frame = JSON.stringify(value, frameValue) + "\n";
  const length = Buffer.byteLength(frame);
  if (length > MAX_FRAME || pendingBytes + length > MAX_PENDING)
    throw new Error("Plugin IPC output limit exceeded");
  pendingBytes += length;
  protocolWrite(frame, () => {
    pendingBytes -= length;
    callback?.();
  });
}
if (Number(process.versions.node.split(".")[0]) < 20) {
  const error =
    "JavaScript backend extensions require externally installed Node.js 20+; set PASEO_PLUGIN_NODE to a compatible executable.";
  if (process.argv[2] === "--validate") {
    console.error(error);
    process.exit(1);
  }
  await new Promise<void>((resolve) => send({ type: "fatal", error }, resolve));
  process.exit(1);
}
const channel: PluginWorkerChannel = {
  send,
  onMessage(listener) {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  },
  disconnect() {
    process.stdin.pause();
    process.exitCode = 0;
    process.stdin.destroy();
  },
};
async function compile(directory: string, version: string) {
  const manifest = await readPluginManifest(directory);
  assertPluginCompatibility({ ...manifest, version, runtime: "daemon" });
  let server: string | null = null;
  for (const name of ["index.server.ts", "index.server.tsx"]) {
    const candidate = path.join(directory, name);
    if (
      await access(candidate).then(
        () => true,
        () => false,
      )
    ) {
      server = candidate;
      break;
    }
  }
  if (!server)
    throw new Error(
      "Plugin has no backend entry; Flutter does not execute React plugin contributions",
    );
  try {
    return (await compilePlugin({ server, client: null })).serverBundle!;
  } catch (error) {
    if (
      error instanceof Error &&
      /Cannot find module ['"](?:esbuild|typescript)['"]/.test(error.message)
    ) {
      throw new Error(
        "Optional JavaScript extension compiler is missing. Install the plugin-worker assets including esbuild, its platform executable, and TypeScript, or set PASEO_PLUGIN_WORKER to a complete worker installation.",
        { cause: error },
      );
    }
    throw error;
  }
}
if (process.argv[2] === "--validate") {
  try {
    await compile(path.resolve(process.argv[3]), process.argv[4]);
    process.exit(0);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
async function initialize(raw: PluginProcessRequest) {
  if (raw.type !== "initialize") throw new Error("Plugin worker requires initialize first");
  const contribute = evaluateBundle(
    await compile(path.resolve(raw.pluginDirectory), raw.appVersion),
  );
  worker = createPluginWorker({ channel, contribute });
  for (const listener of listeners) listener(raw);
}
let buffered: Buffer = Buffer.alloc(0);
process.stdin.on("data", (chunk: Buffer) => {
  if (chunk.length > MAX_FRAME) return fatal(new Error("Plugin IPC input limit exceeded"));
  buffered = buffered.length ? Buffer.concat([buffered, chunk]) : chunk;
  let newline: number;
  while ((newline = buffered.indexOf(10)) !== -1) {
    if (newline + 1 > MAX_FRAME) return fatal(new Error("Plugin IPC input limit exceeded"));
    const frame = buffered.subarray(0, newline);
    buffered = buffered.subarray(newline + 1);
    try {
      const message = JSON.parse(frame.toString("utf8"), (_key, value) =>
        value && typeof value === "object" && typeof value.$bytes === "string"
          ? new Uint8Array(Buffer.from(value.$bytes, "base64"))
          : value,
      ) as PluginProcessRequest;
      if (!initialized) {
        initialized = true;
        process.stdin.pause();
        void initialize(message).then(() => process.stdin.resume(), fatal);
      } else if (!worker) throw new Error("Plugin worker is still initializing");
      else for (const listener of listeners) listener(message);
    } catch (error) {
      fatal(error);
    }
  }
  if (buffered.length > MAX_FRAME) fatal(new Error("Plugin IPC input limit exceeded"));
});
process.stdin.on("end", () => {
  void worker?.shutdown();
});
process.stdin.on("error", fatal);
function fatal(error: unknown) {
  send({ type: "fatal", error: error instanceof Error ? error.message : String(error) }, () =>
    process.exit(1),
  );
  process.stdin.pause();
}
