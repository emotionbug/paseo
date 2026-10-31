import assert from "node:assert/strict";
import test from "node:test";
import {
  computeNextReleaseVersion,
  getReleaseInfoFromSourceTag,
  parseReleaseVersion,
  syncCargoReleaseVersion,
} from "./release-version-utils.mjs";

test("computes the next beta patch from a stable version", () => {
  assert.equal(computeNextReleaseVersion("0.1.59", "beta-patch"), "0.1.60-beta.1");
});

test("advances beta versions", () => {
  assert.equal(computeNextReleaseVersion("0.1.60-beta.1", "beta-next"), "0.1.60-beta.2");
});

test("promotes beta versions to stable", () => {
  assert.equal(computeNextReleaseVersion("0.1.60-beta.2", "promote"), "0.1.60");
});

test("parses beta release metadata", () => {
  assert.deepEqual(parseReleaseVersion("0.1.60-beta.1"), {
    version: "0.1.60-beta.1",
    major: 0,
    minor: 1,
    patch: 60,
    prerelease: "beta.1",
    baseVersion: "0.1.60",
    isPrerelease: true,
    isBeta: true,
    betaNumber: 1,
  });
});

test("emits beta release info from tags", () => {
  assert.deepEqual(getReleaseInfoFromSourceTag("v0.1.60-beta.1"), {
    sourceTag: "v0.1.60-beta.1",
    releaseTag: "v0.1.60-beta.1",
    version: "0.1.60-beta.1",
    baseVersion: "0.1.60",
    prerelease: "beta.1",
    isPrerelease: true,
    isBeta: true,
    betaNumber: 1,
    releaseType: "prerelease",
    releaseChannel: "beta",
    isSmokeTag: false,
  });
});

test("rejects non-beta prerelease versions", () => {
  assert.throws(() => parseReleaseVersion("0.1.60-canary.1"), /Expected beta prerelease versions/);
});

test("release synchronization updates only the native workspace and lockfile package versions", () => {
  const workspace = '[workspace]\nmembers = ["packages/daemon"]\n\n[workspace.package]\nversion = "0.13.0"\nedition = "2024"\n\n[profile.release]\nlto = "thin"\n';
  assert.equal(syncCargoReleaseVersion(workspace, "0.14.0-beta.1"), workspace.replace('"0.13.0"', '"0.14.0-beta.1"'));
  const lock = 'version = 4\n\n[[package]]\nname = "dependency"\nversion = "0.13.0"\n\n[[package]]\nname = "paseo-daemon"\nversion = "0.13.0"\ndependencies = ["dependency"]\n';
  const updated = syncCargoReleaseVersion(lock, "0.14.0", true);
  assert.match(updated, /name = "dependency"\nversion = "0.13.0"/);
  assert.match(updated, /name = "paseo-daemon"\nversion = "0.14.0"/);
  assert.equal(syncCargoReleaseVersion(updated, "0.14.0", true), updated);
  assert.throws(() => syncCargoReleaseVersion('[workspace]\n', "0.14.0"), /exactly one/);
  assert.throws(() => syncCargoReleaseVersion(lock + lock, "0.14.0", true), /exactly one/);
});
