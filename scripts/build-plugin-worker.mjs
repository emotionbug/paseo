import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, readFile, rm, access } from "node:fs/promises";
import { createRequire } from "node:module";
import { build } from "esbuild";
import { collectWorkerNotices, writeNotices } from "../packages/flutter_desktop_host/packaging/third-party-notices.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const output = path.resolve(process.argv[2] ?? path.join(root, "packages/daemon/plugin-worker/dist"));
const require = createRequire(import.meta.url);
// Resolve the public workspace exports to source, never stale generated dist.
// This is build-only; the emitted worker contains the backend SDK itself.
const workspaceExports = new Map(await Promise.all(["client", "relay", "protocol"].map(async name => {
  const directory = path.join(root, "packages", name);
  const metadata = JSON.parse(await readFile(path.join(directory, "package.json"), "utf8"));
  return [metadata.name, { directory, exports: metadata.exports }];
})));
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
const result = await build({
  entryPoints: [path.join(root, "packages/daemon/plugin-worker/index.ts")],
  outfile: path.join(output, "worker.mjs"),
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node20",
  conditions: ["source"],
  external: ["esbuild", "typescript"],
  banner: { js: 'import { createRequire as __createRequire } from "node:module"; const require = __createRequire(import.meta.url);' },
  metafile: true,
  plugins: [{ name: "workspace-source", setup(builder) {
    builder.onResolve({ filter: /^@getpaseo\/(?:client|relay|protocol)(?:\/.*)?$/ }, async ({ path: specifier }) => {
      const [scope, name, ...parts] = specifier.split("/");
      const workspace = workspaceExports.get(`${scope}/${name}`);
      const subpath = parts.length ? `./${parts.join("/")}` : ".";
      let target = workspace.exports[subpath]?.default;
      if (!target) {
        for (const [pattern, entry] of Object.entries(workspace.exports)) {
          if (!pattern.includes("*")) continue;
          const [prefix, suffix] = pattern.split("*");
          if (subpath.startsWith(prefix) && subpath.endsWith(suffix)) {
            const match = subpath.slice(prefix.length, suffix ? -suffix.length : undefined);
            target = entry.default?.replace("*", match);
            break;
          }
        }
      }
      if (typeof target !== "string" || !target.startsWith("./dist/") || !target.endsWith(".js") || target.split("/").includes("..")) {
        throw new Error(`No supported workspace source export: ${specifier}`);
      }
      const source = path.join(workspace.directory, "src", target.slice("./dist/".length, -3) + ".ts");
      await access(source);
      return { path: source };
    });
  } }],
});
// The compiler is optional alongside the worker, not a daemon startup dependency.
// Preserve package layouts: esbuild locates its platform executable relative to itself.
const { cp } = await import("node:fs/promises");
const packages = [];
const compilerDirectories = [];
for (const name of ["esbuild", "typescript", `@esbuild/${process.platform}-${process.arch}`]) {
  const source = path.dirname(require.resolve(`${name}/package.json`));
  compilerDirectories.push(source);
  await cp(source, path.join(output, "node_modules", name), { recursive: true });
  const metadata = JSON.parse(await readFile(path.join(source, "package.json"), "utf8"));
  packages.push({ name, version: metadata.version });
}
await writeFile(path.join(output, "package.json"), JSON.stringify({ private: true, type: "module" }) + "\n");
await writeFile(path.join(output, "paseo-provenance.json"), JSON.stringify({
  schemaVersion: 1,
  runtime: "external-node",
  platform: process.platform,
  arch: process.arch,
  packages,
  bundledInputs: Object.keys(result.metafile.inputs).sort(),
}, null, 2) + "\n");
writeNotices(output, collectWorkerNotices({
  bundledInputs: Object.keys(result.metafile.inputs), compilerDirectories, cwd: process.cwd(),
}));
