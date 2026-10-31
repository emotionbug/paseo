import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { matchesGlob } from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";

const repoRoot = new URL("../", import.meta.url);
const ciWorkflowPath = new URL(".github/workflows/ci.yml", repoRoot);
const filtersPath = new URL(".github/ci-paths.yml", repoRoot);

const gatedCiJobs = new Map([
  ["quality", { contracts: ["format", "quality", "relay", "sdk"] }],
  ["typecheck", { contract: "quality" }],
  ["native-daemon-linux", { contract: "daemon" }],
  ["native-daemon-windows", { contract: "daemon" }],
  ["native-daemon-macos", { contract: "daemon" }],
  ["flutter-analyze", { contract: "flutter" }],
  ["flutter-test", { contract: "flutter" }],
  ["flutter-build-linux", { contracts: ["flutter", "flutter_linux"] }],
  ["flutter-build-windows", { contracts: ["flutter", "flutter_windows"] }],
  ["flutter-build-macos", { contracts: ["flutter", "flutter_macos"] }],
  ["flutter-build-android", { contracts: ["flutter", "flutter_android"] }],
  ["flutter-build-ios", { contracts: ["flutter", "flutter_ios"] }],
]);

function jobBlocks(source) {
  const jobs = new Map();
  let currentJob;

  for (const line of (source.split("\njobs:\n")[1] ?? source).split("\n")) {
    const jobMatch = /^  ([a-z0-9_-]+):\s*$/.exec(line);
    if (jobMatch) {
      currentJob = jobMatch[1];
      jobs.set(currentJob, []);
      continue;
    }
    if (currentJob) jobs.get(currentJob).push(line);
  }
  return jobs;
}

function loadFilters(path) {
  const filters = {};
  let currentFilter;

  for (const line of readFileSync(path, "utf8").split("\n")) {
    const filterMatch = /^([a-z_]+):\s*$/.exec(line);
    if (filterMatch) {
      currentFilter = filterMatch[1];
      filters[currentFilter] = [];
      continue;
    }
    const patternMatch = /^  - "([^"]+)"\s*$/.exec(line);
    if (currentFilter && patternMatch) filters[currentFilter].push(patternMatch[1]);
  }
  return filters;
}


test("recovery runs only the requested check without narrowing normal CI", () => {
  const jobs = jobBlocks(readFileSync(ciWorkflowPath, "utf8"));
  const jobIds = [...gatedCiJobs.keys()];
  function selected(recoveryJob, cancelled = false) {
    return jobIds.filter((jobId) => {
      const expression = /^    if: \$\{\{ (.+) \}\}$/m.exec(jobs.get(jobId).join("\n"))[1];
      return runInNewContext(expression, {
        inputs: { recovery_job: recoveryJob },
        needs: { changes: { outputs: { full: "true" } } },
        cancelled: () => cancelled,
      });
    });
  }
  assert.deepEqual(selected(undefined), jobIds);
  assert.deepEqual(selected("all"), jobIds);
  for (const jobId of jobIds) {
    assert.deepEqual(selected(jobId), [jobId]);
    assert.deepEqual(selected(jobId, true), []);
  }
});

test("every runnable check is independently recoverable and allowlisted", () => {
  const jobs = jobBlocks(readFileSync(ciWorkflowPath, "utf8"));
  const runnable = [...jobs.keys()].filter((jobId) => jobId !== "changes").sort();
  assert.deepEqual([...gatedCiJobs.keys()].sort(), runnable);
  const recovery = readFileSync(new URL(".github/workflows/ci-recovery.yml", repoRoot), "utf8");
  const options = [...recovery.matchAll(/^          - ([a-z0-9-]+)$/gm)].map((match) => match[1]);
  assert.deepEqual(options.sort(), runnable);
  const validation = jobs.get("changes").join("\n");
  const patterns = /^\s+([^\n]+)\) ;;/m.exec(validation)[1].split("|");
  const allowed = (value) => patterns.some((pattern) => matchesGlob(value, pattern));
  for (const jobId of runnable) assert.ok(allowed(jobId), jobId);
  for (const invalid of ["all", "changes", "playwright-1", "desktop-tests-ubuntu", "cli-tests-4", "flutter-build-web", "$(echo unsafe)"]) {
    assert.equal(allowed(invalid), false, invalid);
  }
});

test("routing fails open, but native-only changes do not select backend checks", () => {
  const filters = loadFilters(filtersPath);
  const jobs = jobBlocks(readFileSync(ciWorkflowPath, "utf8"));
  const fullExpression = /^      full: \$\{\{ (.+) \}\}$/m.exec(jobs.get("changes").join("\n"))[1];
  function selected(paths, event = "pull_request", missing = false) {
    const outputs = Object.fromEntries(Object.entries(filters).map(([key, patterns]) => [
      key,
      String(paths.some((path) => patterns.some((pattern) => matchesGlob(path, pattern)))),
    ]));
    const routed = missing ? {} : outputs;
    const full = runInNewContext(fullExpression, {
      github: { event_name: event },
      steps: { filter: { outputs: routed } },
    });
    return [...gatedCiJobs.keys()].filter((jobId) => {
      const expression = /^    if: \$\{\{ (.+) \}\}$/m.exec(jobs.get(jobId).join("\n"))[1];
      return runInNewContext(expression, {
        inputs: {},
        needs: { changes: { outputs: { ...routed, full: String(full) } } },
        cancelled: () => false,
      });
    }).sort();
  }
  const native = [...gatedCiJobs.keys()].filter((id) => id.startsWith("flutter-")).sort();
  assert.deepEqual(selected(["packages/flutter_app/lib/main.dart"]), native);
  assert.deepEqual(selected(["packages/flutter_app/assets/locales/en.json"]), [...native, "quality"].sort());
  for (const platform of ["linux", "windows", "macos", "android", "ios"]) {
    assert.deepEqual(selected([`packages/flutter_app/${platform}/native-source`]), [`flutter-build-${platform}`]);
  }
  assert.deepEqual(selected(["packages/flutter_audio/android/src/main/Plugin.kt"]), ["flutter-build-android"]);
  assert.deepEqual(selected(["packages/flutter_desktop_host/integrations/windows/plugin.cpp"]), ["flutter-build-windows"]);
  for (const event of ["push", "merge_group", "workflow_dispatch", "workflow_call"]) {
    assert.deepEqual(selected([], event), [...gatedCiJobs.keys()].sort());
  }
  assert.deepEqual(selected([], "pull_request", true), [...gatedCiJobs.keys()].sort());
  for (const path of ["package-lock.json", ".github/ci-paths.yml", ".github/workflows/ci.yml"]) {
    assert.deepEqual(selected([path]), [...gatedCiJobs.keys()].sort());
  }
});

test("recovery cannot cancel full CI or a different repair target", () => {
  const source = readFileSync(ciWorkflowPath, "utf8");
  const template = /^  group: (.+)$/m.exec(source)[1];
  function group(workflow, job, ref = "refs/heads/main") {
    return template.replace(/\$\{\{ (.+?) \}\}/g, (_match, expression) =>
      runInNewContext(expression, {
        github: { workflow, ref },
        inputs: { recovery_job: job },
      }),
    );
  }
  assert.equal(group("CI", undefined), group("CI", "all"));
  assert.notEqual(group("CI", undefined), group("CI Recovery", "quality"));
  assert.notEqual(group("CI Recovery", "quality"), group("CI Recovery", "typecheck"));
  assert.notEqual(group("CI", undefined), group("CI", undefined, "refs/heads/fix"));
});

test("changed paths select their owning CI contracts", () => {
  const filters = loadFilters(filtersPath);
  const cases = [
    [".github/workflows/ci-recovery.yml", ["ci", "format"]],
    ["packages/flutter_app/lib/main.dart", ["flutter"]],
    ["packages/flutter_app/assets/locales/en.json", ["flutter", "format"]],
    ["packages/flutter_app/pubspec.lock", ["flutter"]],
    ["packages/flutter_app/pubspec.yaml", ["flutter", "format"]],
    ["packages/flutter_app/linux/runner/my_application.cc", ["flutter_linux"]],
    ["packages/flutter_app/windows/runner/main.cpp", ["flutter_windows"]],
    ["packages/flutter_app/macos/Runner/AppDelegate.swift", ["flutter_macos"]],
    ["packages/flutter_app/android/app/build.gradle.kts", ["flutter_android"]],
    ["packages/flutter_app/ios/Runner/AppDelegate.swift", ["flutter_ios"]],
    ["packages/flutter_audio/lib/paseo_flutter_audio.dart", ["flutter"]],
    ["packages/flutter_audio/android/src/main/PaseoAudioPlugin.kt", ["flutter_android"]],
    ["packages/flutter_audio/ios/Classes/PaseoAudioPlugin.swift", ["flutter_ios"]],
    ["packages/flutter_desktop_host/integrations/linux/plugin.cc", ["flutter_linux"]],
    ["packages/flutter_desktop_host/integrations/windows/plugin.cpp", ["flutter_windows"]],
    ["packages/flutter_desktop_host/integrations/macos/Classes/Plugin.swift", ["flutter_macos"]],
    ["packages/daemon/src/main.rs", ["daemon"]],
    ["Cargo.lock", ["daemon"]],
    ["Cargo.toml", ["daemon"]],
    ["packages/daemon/plugin-worker/src/atomic-file.ts", ["daemon", "format", "quality"]],
    ["scripts/clean-package-dist.mjs", ["format", "quality", "workspace"]],
    ["README.md", ["format"]],
  ];
  for (const [path, expected] of cases) {
    const selected = Object.entries(filters)
      .filter(([, patterns]) => patterns.some((pattern) => matchesGlob(path, pattern)))
      .map(([contract]) => contract)
      .sort();
    assert.deepEqual(selected, expected, path);
  }
});

