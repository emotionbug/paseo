import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const RELEASE_WORKSPACES = [
  "packages/highlight",
  "packages/relay",
  "packages/protocol",
  "packages/client",
  "packages/plugin",
];

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const defaultRootDirectory = path.resolve(scriptDirectory, "..");

function describeCommandFailure(command, args, result) {
  const details = [result.stderr, result.stdout]
    .filter((value) => typeof value === "string" && value.trim().length > 0)
    .map((value) => value.trim())
    .join("\n");
  let outcome;
  if (result.error) {
    outcome = result.error.message;
  } else if (result.signal) {
    outcome = `terminated by ${result.signal}`;
  } else {
    outcome = `exited with status ${result.status}`;
  }
  return `${command} ${args.join(" ")} ${outcome}${details ? `\n${details}` : ""}`;
}

function parseJsonOutput(output, description) {
  try {
    return JSON.parse(output.trim());
  } catch (error) {
    throw new Error(`${description} returned invalid JSON`, { cause: error });
  }
}

function npmErrorCode(result) {
  for (const output of [result.stderr, result.stdout]) {
    if (typeof output !== "string" || output.trim().length === 0) {
      continue;
    }
    try {
      const parsed = JSON.parse(output);
      if (typeof parsed?.error?.code === "string") {
        return parsed.error.code;
      }
    } catch {
      const match = output.match(/(?:^|\n)npm (?:error|ERR!) code ([A-Z0-9]+)/i);
      if (match) {
        return match[1];
      }
    }
  }
  return undefined;
}

export function loadReleasePackages(rootDirectory = defaultRootDirectory) {
  return RELEASE_WORKSPACES.map((workspace) => {
    const manifestPath = path.join(rootDirectory, workspace, "package.json");
    const manifest = parseJsonOutput(readFileSync(manifestPath, "utf8"), manifestPath);
    if (typeof manifest.name !== "string" || typeof manifest.version !== "string") {
      throw new Error(`${manifestPath} must contain string name and version fields`);
    }
    return { workspace, name: manifest.name, version: manifest.version };
  });
}

export function createNpmRegistry({
  rootDirectory = defaultRootDirectory,
  runCommand = spawnSync,
} = {}) {
  const isWindows = process.platform === "win32";
  const command = isWindows ? "npm.cmd" : "npm";

  function run(args, options = {}) {
    return runCommand(command, args, {
      cwd: rootDirectory,
      encoding: "utf8",
      shell: isWindows,
      ...options,
    });
  }

  return {
    inspectVersion({ name, version }) {
      const exactSpec = `${name}@${version}`;
      const exactArgs = ["view", exactSpec, "version", "--json"];
      const exactResult = run(exactArgs);
      if (exactResult.status === 0 && !exactResult.error && !exactResult.signal) {
        const publishedVersion = parseJsonOutput(
          exactResult.stdout,
          `Registry lookup for ${exactSpec}`,
        );
        if (publishedVersion !== version) {
          throw new Error(
            `Registry lookup for ${exactSpec} returned unexpected version ${JSON.stringify(publishedVersion)}`,
          );
        }
        return "published";
      }

      if (npmErrorCode(exactResult) !== "E404") {
        throw new Error(
          `Registry lookup for ${exactSpec} failed; refusing to publish because absence was not confirmed:\n${describeCommandFailure(command, exactArgs, exactResult)}`,
        );
      }

      // An npm registry can use E404 for an unreadable package. Confirm that the
      // package itself is readable before treating the exact version as absent.
      const versionsArgs = ["view", name, "versions", "--json"];
      const versionsResult = run(versionsArgs);
      if (versionsResult.status !== 0 || versionsResult.error || versionsResult.signal) {
        throw new Error(
          `Registry lookup for ${exactSpec} returned E404, but package visibility could not be confirmed:\n${describeCommandFailure(command, versionsArgs, versionsResult)}`,
        );
      }
      const publishedVersions = parseJsonOutput(
        versionsResult.stdout,
        `Registry version listing for ${name}`,
      );
      if (
        !Array.isArray(publishedVersions) ||
        !publishedVersions.every((value) => typeof value === "string")
      ) {
        throw new Error(`Registry version listing for ${name} returned an unexpected value`);
      }
      return publishedVersions.includes(version) ? "published" : "missing";
    },

    publishPackage({ name }, { tag, dryRun }) {
      const args = ["publish", `--workspace=${name}`, "--access=public"];
      if (tag) {
        args.push(`--tag=${tag}`);
      }
      if (dryRun) {
        args.push("--dry-run");
      }
      const result = run(args, { stdio: "inherit" });
      if (result.status !== 0 || result.error || result.signal) {
        throw new Error(
          `Publication of ${name} failed:\n${describeCommandFailure(command, args, result)}`,
        );
      }
    },
  };
}

export async function publishReleasePackages({
  packages,
  inspectVersion,
  publishPackage,
  tag,
  dryRun = false,
  log = console.log,
}) {
  for (const releasePackage of packages) {
    const spec = `${releasePackage.name}@${releasePackage.version}`;
    const state = await inspectVersion(releasePackage);
    if (state === "published") {
      log(`Skipping ${spec}: already published.`);
      continue;
    }
    if (state !== "missing") {
      throw new Error(
        `Registry inspection for ${spec} returned unknown state ${JSON.stringify(state)}`,
      );
    }

    log(`${dryRun ? "Dry-running" : "Publishing"} ${spec}${tag ? ` with dist-tag ${tag}` : ""}.`);
    await publishPackage(releasePackage, { tag, dryRun });
  }
}

function parseArguments(args) {
  let tag;
  let dryRun = false;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--dry-run") {
      dryRun = true;
      continue;
    }
    if (argument === "--tag" && args[index + 1] === "beta") {
      tag = "beta";
      index += 1;
      continue;
    }
    throw new Error(`Unknown argument: ${argument}`);
  }
  return { tag, dryRun };
}

export async function main(args = process.argv.slice(2)) {
  const options = parseArguments(args);
  const registry = createNpmRegistry();
  await publishReleasePackages({
    packages: loadReleasePackages(),
    inspectVersion: registry.inspectVersion,
    publishPackage: registry.publishPackage,
    ...options,
  });
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  });
}
