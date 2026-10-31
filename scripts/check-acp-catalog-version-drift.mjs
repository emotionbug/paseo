#!/usr/bin/env node

import { execFile } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);

const CATALOG_PATH = new URL("../packages/flutter_app/lib/features/settings/provider_catalog.dart", import.meta.url);
const EXACT_VERSION_PATTERN = /^\d+\.\d+\.\d+(?:[-+].*)?$/;
const HELP_TEXT = `Usage: npm run acp:version-drift [-- --json] [-- --fail-on-drift] [-- --no-network] [-- --update]

Checks package-runner ACP catalog entries for exact latest registry pins.

By default this is report-only. Add --fail-on-drift to exit non-zero when drift is found.
Add --update to rewrite the catalog package-runner entries to the latest exact versions.`;

function parseArgs(argv) {
  const options = {
    json: false,
    failOnDrift: false,
    noNetwork: false,
    update: false,
  };

  for (const arg of argv) {
    if (arg === "--json") {
      options.json = true;
    } else if (arg === "--fail-on-drift") {
      options.failOnDrift = true;
    } else if (arg === "--no-network") {
      options.noNetwork = true;
    } else if (arg === "--update") {
      options.update = true;
    } else if (arg === "--help" || arg === "-h") {
      console.log(HELP_TEXT);
      process.exit(0);
    } else {
      throw new Error(`Unknown argument: ${arg}\n\n${HELP_TEXT}`);
    }
  }

  if (options.update && options.noNetwork) {
    throw new Error("--update requires registry access; remove --no-network");
  }

  return options;
}

// Parse the catalog's literal-only Dart subset, never Dart expressions. Keep
// source ranges so updates leave metadata, comments and untouched arguments intact.
function readCatalogLiteral(source, start) {
  let index = start;
  const fail = (message) => {
    throw new Error(`Invalid ACP catalog literal at ${index}: ${message}`);
  };
  function skipTrivia() {
    while (index < source.length) {
      if (/\s/.test(source[index])) {
        index += 1;
      } else if (source.startsWith("//", index)) {
        const end = source.indexOf("\n", index + 2);
        index = end === -1 ? source.length : end + 1;
      } else if (source.startsWith("/*", index)) {
        index += 2;
        let depth = 1;
        while (depth && index < source.length) {
          if (source.startsWith("/*", index)) {
            depth += 1;
            index += 2;
          } else if (source.startsWith("*/", index)) {
            depth -= 1;
            index += 2;
          } else {
            index += 1;
          }
        }
        if (depth) fail("unterminated comment");
      } else {
        break;
      }
    }
  }
  function readString() {
    const raw = source[index] === "r";
    if (raw) index += 1;
    const quote = source[index++];
    if (source.startsWith(quote.repeat(2), index)) fail("multiline strings are not supported");
    let value = "";
    while (index < source.length) {
      const char = source[index++];
      if (char === quote) return value;
      if (char === "\n" || char === "\r") fail("newline in single-line string");
      if (!raw && char === "$") fail("string interpolation is not supported");
      if (raw || char !== "\\") {
        value += char;
        continue;
      }
      const escape = source[index++];
      const escapes = { n: "\n", r: "\r", t: "\t", b: "\b", f: "\f", v: "\v", "\\": "\\", "'": "'", '"': '"', $: "$" };
      if (Object.hasOwn(escapes, escape)) {
        value += escapes[escape];
      } else if (escape === "x" || escape === "u") {
        const braced = escape === "u" && source[index] === "{";
        if (braced) index += 1;
        const length = escape === "x" ? 2 : 4;
        const end = braced ? source.indexOf("}", index) : index + length;
        const hex = source.slice(index, end);
        if (end === -1 || !/^[0-9a-fA-F]+$/.test(hex) || (braced ? hex.length > 6 : hex.length !== length)) {
          fail("invalid character escape");
        }
        const codePoint = Number.parseInt(hex, 16);
        if (codePoint > 0x10ffff) fail("invalid Unicode code point");
        value += String.fromCodePoint(codePoint);
        index = end + (braced ? 1 : 0);
      } else {
        fail("unsupported string escape");
      }
    }
    fail("unterminated string");
  }
  function readValue() {
    skipTrivia();
    const start = index;
    const char = source[index];
    if (char === '"' || char === "'" || (char === "r" && /['"]/.test(source[index + 1] ?? ""))) {
      const value = readString();
      return { start, end: index, value };
    }
    if (char === "[" || char === "{") {
      index += 1;
      const close = char === "[" ? "]" : "}";
      const items = [];
      const fields = new Map();
      skipTrivia();
      while (source[index] !== close) {
        const item = readValue();
        if (char === "{") {
          if (typeof item.value !== "string") fail("map keys must be strings");
          skipTrivia();
          if (source[index++] !== ":") fail("expected ':'");
          if (fields.has(item.value)) fail("duplicate map key");
          fields.set(item.value, readValue());
        } else {
          items.push(item);
        }
        skipTrivia();
        if (source[index] === close) break;
        if (source[index++] !== ",") fail("expected ','");
        skipTrivia();
      }
      index += 1;
      return { start, end: index, ...(char === "[" ? { items } : { fields }) };
    }
    const scalar = /^(?:true|false|null|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(source.slice(index));
    if (!scalar) fail("expected a literal");
    index += scalar[0].length;
    return { start, end: index, value: JSON.parse(scalar[0]) };
  }
  return readValue();
}

function readCatalog(source) {
  const startToken = "const acpProviderCatalog = <Map<String, dynamic>>[";
  const startIndex = source.indexOf(startToken);
  if (startIndex === -1) {
    throw new Error("Could not find acpProviderCatalog in native ACP provider catalog");
  }
  return readCatalogLiteral(source, startIndex + startToken.length - 1).items;
}

export function parseCatalogEntries(source) {
  return readCatalog(source).map((entry) => {
    if (!entry.fields) throw new Error("ACP catalog entries must be maps");
    const command = entry.fields.get("command");
    if (command && (!command.items || command.items.some((arg) => typeof arg.value !== "string"))) {
      throw new Error("ACP catalog commands must be lists of string literals");
    }
    return {
      id: entry.fields.get("id")?.value ?? null,
      title: entry.fields.get("title")?.value ?? null,
      version: entry.fields.get("version")?.value ?? null,
      command: command?.items.map((arg) => arg.value) ?? null,
      start: entry.start,
      end: entry.end,
    };
  });
}

function findNpmPackageSpec(command) {
  const argsStartIndex = command[0] === "npm" && command[1] === "exec" ? 2 : 1;
  for (let index = argsStartIndex; index < command.length; index += 1) {
    const arg = command[index];
    if (arg === "--") {
      continue;
    }
    if (arg.startsWith("-")) {
      continue;
    }
    return { registry: "npm", spec: arg, index };
  }
  return null;
}

function findUvxPackageSpec(command) {
  const fromIndex = command.indexOf("--from");
  if (fromIndex !== -1 && command[fromIndex + 1]) {
    return { registry: "pypi", spec: command[fromIndex + 1], index: fromIndex + 1, usesFrom: true };
  }

  for (let index = 1; index < command.length; index += 1) {
    const arg = command[index];
    if (arg.startsWith("-")) {
      continue;
    }
    return { registry: "pypi", spec: arg, index, usesFrom: false };
  }

  return null;
}

function findPackageRunnerSpec(command) {
  if (!Array.isArray(command) || command.length === 0) {
    return null;
  }

  if (command[0] === "npx" || command[0] === "npm") {
    return findNpmPackageSpec(command);
  }
  if (command[0] === "uvx") {
    return findUvxPackageSpec(command);
  }
  return null;
}

function parsePackageSpec(spec) {
  if (!spec) {
    return null;
  }

  const pythonRequirementMatch = /^([A-Za-z0-9_.-]+)(==.+|>=.+)$/.exec(spec);
  if (pythonRequirementMatch?.[1] && pythonRequirementMatch[2]) {
    return {
      packageName: pythonRequirementMatch[1],
      selector: pythonRequirementMatch[2],
    };
  }

  if (spec.startsWith("@")) {
    const versionSeparatorIndex = spec.indexOf("@", 1);
    if (versionSeparatorIndex === -1) {
      return { packageName: spec, selector: null };
    }
    return {
      packageName: spec.slice(0, versionSeparatorIndex),
      selector: spec.slice(versionSeparatorIndex + 1),
    };
  }

  const versionSeparatorIndex = spec.lastIndexOf("@");
  if (versionSeparatorIndex === -1) {
    return { packageName: spec, selector: null };
  }
  return {
    packageName: spec.slice(0, versionSeparatorIndex),
    selector: spec.slice(versionSeparatorIndex + 1),
  };
}

function getPinnedVersion(selector) {
  if (!selector) {
    return null;
  }
  if (EXACT_VERSION_PATTERN.test(selector)) {
    return selector;
  }
  if (selector.startsWith("==") && EXACT_VERSION_PATTERN.test(selector.slice(2))) {
    return selector.slice(2);
  }
  return null;
}

async function getLatestNpmVersion(packageName) {
  const { stdout } = await execFileAsync("npm", ["view", packageName, "version"], {
    encoding: "utf8",
    timeout: 30_000,
    maxBuffer: 1024 * 1024,
  });
  return stdout.trim();
}

async function getLatestPypiVersion(packageName) {
  const response = await fetch(`https://pypi.org/pypi/${encodeURIComponent(packageName)}/json`, {
    signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) {
    throw new Error(`PyPI responded ${response.status}`);
  }
  const metadata = await response.json();
  if (!metadata?.info?.version || typeof metadata.info.version !== "string") {
    throw new Error("PyPI response did not include info.version");
  }
  return metadata.info.version;
}

async function getLatestVersion(registry, packageName) {
  if (registry === "npm") {
    return getLatestNpmVersion(packageName);
  }
  if (registry === "pypi") {
    return getLatestPypiVersion(packageName);
  }
  throw new Error(`Unsupported package registry: ${registry}`);
}

function buildStatus({ selector, catalogVersion, latestVersion, registryError }) {
  const pinnedVersion = getPinnedVersion(selector);
  const reasons = [];

  if (registryError) {
    reasons.push("registry version lookup failed");
  }
  if (!selector) {
    reasons.push("missing package version selector");
  } else if (!pinnedVersion) {
    reasons.push("command selector is not an exact version pin");
  }

  if (latestVersion) {
    if (pinnedVersion && pinnedVersion !== latestVersion) {
      reasons.push("command selector is not latest registry version");
    }
    if (catalogVersion !== latestVersion) {
      reasons.push("catalog version is not latest registry version");
    }
  }

  return reasons;
}

function buildUpdatedPackageSpec(spec, packageName, latestVersion) {
  if (spec.registry === "pypi" && spec.usesFrom) {
    return `${packageName}==${latestVersion}`;
  }
  return `${packageName}@${latestVersion}`;
}

function buildUpdatedCommand(command, spec, parsedSpec, latestVersion) {
  const updatedCommand = [...command];
  updatedCommand[spec.index] = buildUpdatedPackageSpec(spec, parsedSpec.packageName, latestVersion);
  return updatedCommand;
}

async function inspectEntry(entry, options) {
  const spec = findPackageRunnerSpec(entry.command);
  const parsedSpec = parsePackageSpec(spec?.spec);

  if (!spec || !parsedSpec) {
    return {
      id: entry.id,
      title: entry.title,
      command: entry.command,
      checked: false,
      status: "skipped",
      reasons: ["not a supported package-runner command"],
    };
  }

  let latestVersion = null;
  let registryError = null;
  if (!options.noNetwork) {
    try {
      latestVersion = await getLatestVersion(spec.registry, parsedSpec.packageName);
    } catch (error) {
      registryError = error instanceof Error ? error.message : String(error);
    }
  }

  const reasons = buildStatus({
    selector: parsedSpec.selector,
    catalogVersion: entry.version,
    latestVersion,
    registryError,
  });

  return {
    id: entry.id,
    title: entry.title,
    registry: spec.registry,
    packageName: parsedSpec.packageName,
    selector: parsedSpec.selector,
    catalogVersion: entry.version,
    latestVersion,
    currentCommand: entry.command,
    updatedCommand: latestVersion
      ? buildUpdatedCommand(entry.command, spec, parsedSpec, latestVersion)
      : null,
    checked: true,
    status: reasons.length === 0 ? "ok" : "drift",
    reasons,
    registryError,
    start: entry.start,
    end: entry.end,
  };
}

function serializeString(value) {
  return JSON.stringify(value).replace(/\$/g, () => "\\$");
}

export function applyUpdates(source, results) {
  let updatedSource = source;
  const updates = results
    .filter((result) => result.checked && result.latestVersion && result.updatedCommand)
    .sort((left, right) => right.start - left.start);

  for (const result of updates) {
    const entry = readCatalogLiteral(source, result.start);
    const version = entry.fields.get("version");
    const command = entry.fields.get("command");
    if (!version || !command?.items) {
      throw new Error("Cannot update ACP entry without version and command literals");
    }
    const replacements = [{ ...version, text: serializeString(result.latestVersion) }];
    if (command.items.length === result.updatedCommand.length) {
      command.items.forEach((arg, index) => {
        if (arg.value !== result.updatedCommand[index]) {
          replacements.push({ ...arg, text: serializeString(result.updatedCommand[index]) });
        }
      });
    } else {
      replacements.push({
        ...command,
        text: `[${result.updatedCommand.map(serializeString).join(", ")}]`,
      });
    }
    for (const replacement of replacements.sort((left, right) => right.start - left.start)) {
      updatedSource = `${updatedSource.slice(0, replacement.start)}${replacement.text}${updatedSource.slice(replacement.end)}`;
    }
  }

  return updatedSource;
}

function printReport(results, options) {
  const reportResults = results.map(({ start: _start, end: _end, ...result }) => result);
  if (options.json) {
    console.log(JSON.stringify(reportResults, null, 2));
    return;
  }

  const checked = reportResults.filter((result) => result.checked);
  const drift = checked.filter((result) => result.status === "drift");
  const skipped = reportResults.filter((result) => !result.checked);

  console.log("ACP catalog version drift");
  console.log("=========================");
  console.log(`checked: ${checked.length}`);
  console.log(`drift:   ${drift.length}`);
  console.log(`skipped: ${skipped.length}`);

  if (drift.length > 0) {
    console.log("\nDrift / stale pins:");
    for (const result of drift) {
      const latest = result.latestVersion ?? (options.noNetwork ? "not checked" : "unknown");
      const recommended = result.updatedCommand?.join(" ") ?? "unavailable";
      console.log(
        `- ${result.id}: ${result.registry}:${result.packageName}@${result.selector ?? "<none>"} ` +
          `(catalog ${result.catalogVersion ?? "n/a"}, latest ${latest}) -> ${recommended}`,
      );
      for (const reason of result.reasons) {
        console.log(`  - ${reason}`);
      }
    }
  }

  if (skipped.length > 0) {
    console.log("\nSkipped non-package-runner commands:");
    for (const result of skipped) {
      console.log(`- ${result.id}: ${result.command?.join(" ") ?? "<no command>"}`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const options = parseArgs(process.argv.slice(2));
  const source = await readFile(CATALOG_PATH, "utf8");
  const entries = parseCatalogEntries(source);
  const results = await Promise.all(entries.map((entry) => inspectEntry(entry, options)));
  const hasDrift = results.some((result) => result.checked && result.status === "drift");

  if (options.update) {
    await writeFile(CATALOG_PATH, applyUpdates(source, results));
  }

  printReport(results, options);

  if (options.failOnDrift && hasDrift) {
    process.exitCode = 1;
  }
}
