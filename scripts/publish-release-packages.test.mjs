import assert from "node:assert/strict";
import test from "node:test";
import {
  createNpmRegistry,
  publishReleasePackages,
  RELEASE_WORKSPACES,
} from "./publish-release-packages.mjs";

const packages = ["highlight", "relay", "protocol", "client", "plugin"].map(
  (name) => ({
    workspace: `packages/${name}`,
    name: `@getpaseo/${name}`,
    version: "1.2.3-beta.4",
  }),
);

function successfulResult(stdout = "") {
  return { status: 0, signal: null, error: undefined, stdout, stderr: "" };
}

function failedResult(code, message = code) {
  return {
    status: 1,
    signal: null,
    error: undefined,
    stdout: "",
    stderr: JSON.stringify({ error: { code, summary: message } }),
  };
}

function createStatefulNpm({ failFirstPublishOf } = {}) {
  const versions = new Map(packages.map(({ name }) => [name, new Set()]));
  const commands = [];
  let pendingFailure = failFirstPublishOf;

  function runCommand(_command, args) {
    commands.push(args);
    if (args[0] === "view" && args[2] === "version") {
      const separator = args[1].lastIndexOf("@");
      const name = args[1].slice(0, separator);
      const version = args[1].slice(separator + 1);
      return versions.get(name)?.has(version)
        ? successfulResult(JSON.stringify(version))
        : failedResult("E404", "Version not found");
    }
    if (args[0] === "view" && args[2] === "versions") {
      return successfulResult(JSON.stringify([...versions.get(args[1])]));
    }
    if (args[0] === "publish") {
      const name = args.find((argument) => argument.startsWith("--workspace=")).slice(12);
      if (name === pendingFailure) {
        pendingFailure = undefined;
        return failedResult("E500", "Publication interrupted");
      }
      const releasePackage = packages.find((candidate) => candidate.name === name);
      versions.get(name).add(releasePackage.version);
      return successfulResult();
    }
    throw new Error(`Unexpected npm command: ${args.join(" ")}`);
  }

  return { versions, commands, runCommand };
}

async function publishWith(registry, options = {}) {
  return publishReleasePackages({
    packages,
    inspectVersion: registry.inspectVersion,
    publishPackage: registry.publishPackage,
    log: () => {},
    ...options,
  });
}

test("keeps the release packages in dependency order", () => {
  assert.deepEqual(
    RELEASE_WORKSPACES,
    packages.map(({ workspace }) => workspace),
  );
});

test("a rerun after a mid-chain failure skips published versions and resumes in order", async () => {
  const npm = createStatefulNpm({ failFirstPublishOf: packages[3].name });
  const registry = createNpmRegistry({ runCommand: npm.runCommand });

  await assert.rejects(publishWith(registry, { tag: "beta" }), /Publication interrupted/);
  assert.deepEqual(
    packages
      .filter(({ name, version }) => npm.versions.get(name).has(version))
      .map(({ name }) => name),
    packages.slice(0, 3).map(({ name }) => name),
  );

  await publishWith(registry, { tag: "beta" });

  assert.deepEqual(
    packages
      .filter(({ name, version }) => npm.versions.get(name).has(version))
      .map(({ name }) => name),
    packages.map(({ name }) => name),
  );
  const publishCommands = npm.commands.filter(([command]) => command === "publish");
  assert.deepEqual(
    publishCommands.map((args) => args.find((argument) => argument.startsWith("--workspace="))),
    [...packages.slice(0, 4), ...packages.slice(3)].map(({ name }) => `--workspace=${name}`),
  );
  assert.ok(publishCommands.every((args) => args.includes("--tag=beta")));
});

test("a registry lookup failure aborts without publishing", async () => {
  const commands = [];
  const registry = createNpmRegistry({
    runCommand(_command, args) {
      commands.push(args);
      return failedResult("ECONNRESET", "Registry connection reset");
    },
  });

  await assert.rejects(
    publishReleasePackages({
      packages,
      inspectVersion: registry.inspectVersion,
      publishPackage: registry.publishPackage,
      log: () => {},
    }),
    /absence was not confirmed/,
  );
  assert.deepEqual(commands, [
    ["view", `${packages[0].name}@${packages[0].version}`, "version", "--json"],
  ]);
});

test("an exact E404 does not hide a package visibility failure", async () => {
  const commands = [];
  const registry = createNpmRegistry({
    runCommand(_command, args) {
      commands.push(args);
      return args[2] === "version"
        ? failedResult("E404", "Version not found")
        : failedResult("E401", "Authentication required");
    },
  });

  await assert.rejects(
    publishReleasePackages({
      packages,
      inspectVersion: registry.inspectVersion,
      publishPackage: registry.publishPackage,
      log: () => {},
    }),
    /package visibility could not be confirmed/,
  );
  assert.deepEqual(commands, [
    ["view", `${packages[0].name}@${packages[0].version}`, "version", "--json"],
    ["view", packages[0].name, "versions", "--json"],
  ]);
});

test("stable dry runs retain npm's default latest tag", async () => {
  const npm = createStatefulNpm();
  const registry = createNpmRegistry({ runCommand: npm.runCommand });

  await publishReleasePackages({
    packages: packages.slice(0, 1),
    inspectVersion: registry.inspectVersion,
    publishPackage: registry.publishPackage,
    dryRun: true,
    log: () => {},
  });

  const publishCommand = npm.commands.find(([command]) => command === "publish");
  assert.ok(publishCommand.includes("--dry-run"));
  assert.equal(
    publishCommand.some((argument) => argument.startsWith("--tag=")),
    false,
  );
});
