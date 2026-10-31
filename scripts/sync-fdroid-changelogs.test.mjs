import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { formatFdroidChangelog, getFdroidVersionCodes, syncFdroidChangelogs } from "./sync-fdroid-changelogs.mjs";

const CHANGELOG_CHARACTER_LIMIT = 500;

function withTempRepo(fn, { changelog, version = "0.2.3" }) {
  const tempDir = mkdtempSync(path.join(tmpdir(), "paseo-fdroid-changelogs-"));
  try {
    writeFileSync(path.join(tempDir, "package.json"), JSON.stringify({ version }));
    writeFileSync(path.join(tempDir, "CHANGELOG.md"), changelog);
    fn(tempDir);
  } finally {
    rmSync(tempDir, { force: true, recursive: true });
  }
}

function readChangelogFile(dir, versionCode, locale = "en-US") {
  return readFileSync(
    path.join(dir, "fastlane", "metadata", "android", locale, "changelogs", `${versionCode}.txt`),
    "utf8",
  );
}

test("uses the Flutter Android build number and upgrade floor", () => {
  assert.deepEqual(getFdroidVersionCodes("0.13.0", {}), [{ abi: "universal", versionCode: 13000 }]);
  assert.deepEqual(getFdroidVersionCodes("0.13.0", { PASEO_BUILD_NUMBER: "14000", ANDROID_VERSION_CODE: "15000" }),
    [{ abi: "universal", versionCode: 14000 }]);
  assert.deepEqual(getFdroidVersionCodes("0.13.0", { ANDROID_VERSION_CODE: "15000" }),
    [{ abi: "universal", versionCode: 15000 }]);
  assert.deepEqual(getFdroidVersionCodes("0.13.0", { PASEO_BUILD_NUMBER: "12" }),
    [{ abi: "universal", versionCode: 13000 }]);
  assert.throws(() => getFdroidVersionCodes("0.13.0", { PASEO_BUILD_NUMBER: "invalid" }), /numeric/);
});

test("does not write or validate F-Droid changelogs for beta releases", () => {
  withTempRepo(
    (dir) => {
      const result = syncFdroidChangelogs([], { cwd: dir });
      assert.deepEqual(result, {
        contents: null,
        version: "0.2.3-beta.1",
        versionCodes: [],
        written: [],
      });
      assert.equal(existsSync(path.join(dir, "fastlane")), false);
    },
    { changelog: "", version: "0.2.3-beta.1" },
  );
});

test("writes the changelog for the native Android version code", () => {
  const changelog = "# Changelog\n\n## 0.2.3 - 2026-07-27\n\n### Fixed\n\n- Something broke.\n";
  withTempRepo(
    (dir) => {
      const result = syncFdroidChangelogs([], { cwd: dir });

      assert.equal(result.written.length, 1);
      for (const { versionCode } of getFdroidVersionCodes("0.2.3")) {
        assert.equal(readChangelogFile(dir, versionCode), result.contents);
      }
      assert.match(result.contents, /- Something broke\./);
    },
    { changelog },
  );
});

test("strips PR references and contributor attributions", () => {
  const changelog = [
    "## 0.2.3 - 2026-07-27",
    "",
    "### Added",
    "",
    "- Support for `GitLab` merge requests ([#1913](https://x/1913) by [@nllptrx](https://y))",
    "",
  ].join("\n");

  withTempRepo(
    (dir) => {
      const { contents } = syncFdroidChangelogs([], { cwd: dir });
      assert.equal(contents.includes("#1913"), false);
      assert.equal(contents.includes("nllptrx"), false);
      assert.equal(contents.includes("`"), false);
      assert.match(contents, /- Support for GitLab merge requests\n/);
    },
    { changelog },
  );
});

test("stays inside the 500 character limit and keeps the full-notes link", () => {
  const bullets = Array.from(
    { length: 40 },
    (_, index) => `- Shipped improvement number ${index}.`,
  );
  const changelog = ["## 0.2.3 - 2026-07-27", "", "### Added", "", ...bullets, ""].join("\n");

  withTempRepo(
    (dir) => {
      const { contents } = syncFdroidChangelogs([], { cwd: dir });
      assert.ok(
        contents.length <= CHANGELOG_CHARACTER_LIMIT,
        `expected <= ${CHANGELOG_CHARACTER_LIMIT}, got ${contents.length}`,
      );
      assert.match(contents, /Full notes: https:\/\/paseo\.sh\/changelog\n$/);
      // Truncation happens at a bullet boundary, never mid-sentence.
      assert.equal(contents.includes("Shipped improvement number 0."), true);
      assert.equal(/- Shipped improvement number \d+\.\.\.$/m.test(contents), false);
    },
    { changelog },
  );
});

test("never leaves an orphan section heading when bullets get cut", () => {
  const filler = Array.from({ length: 20 }, (_, index) => `- Improvement ${index} with some text.`);
  const changelog = [
    "## 0.2.3 - 2026-07-27",
    "",
    "### Improved",
    "",
    ...filler,
    "",
    "### Fixed",
    "",
    "- A late fix that will not fit.",
    "",
  ].join("\n");

  withTempRepo(
    (dir) => {
      const { contents } = syncFdroidChangelogs([], { cwd: dir });
      assert.equal(contents.includes("Fixed"), false);
      assert.equal(contents.trimEnd().endsWith("Improved"), false);
    },
    { changelog },
  );
});

test("salvages a truncated first bullet rather than emitting only a link", () => {
  const contents = formatFdroidChangelog(["### Added", `- ${"x".repeat(600)}`]);
  assert.ok(contents.length <= CHANGELOG_CHARACTER_LIMIT);
  assert.match(contents, /^- x+\.\.\.\n\nFull notes:/);
});

// Releases use blockquotes for action-required notices. Dropping one silently is
// worse than dropping any bullet, so these pin the behaviour.
test("preserves a blockquoted notice ahead of the bullets", () => {
  const contents = formatFdroidChangelog([
    "> **Important update notice**",
    ">",
    "> If you installed Paseo Desktop 0.1.108, you need to [reinstall manually](https://paseo.sh/download).",
    "",
    "### Fixed",
    "",
    "- Desktop no longer gets stuck connecting.",
  ]);

  assert.match(
    contents,
    /^Important update notice\nIf you installed Paseo Desktop 0\.1\.108, you need to reinstall manually\.\n\nFixed\n- Desktop no longer gets stuck connecting\./,
  );
  assert.ok(contents.length <= CHANGELOG_CHARACTER_LIMIT);
});

test("keeps the notice whole and cuts bullets when the budget runs out", () => {
  const notice = `- ${"y".repeat(10)}`;
  const bullets = Array.from({ length: 30 }, () => `- ${"b".repeat(60)}`);
  const contents = formatFdroidChangelog([
    "> Reinstall Paseo manually before updating, the old updater cannot install this build.",
    "",
    "### Fixed",
    "",
    ...bullets,
    notice,
  ]);

  assert.ok(contents.length <= CHANGELOG_CHARACTER_LIMIT);
  assert.match(
    contents,
    /^Reinstall Paseo manually before updating, the old updater cannot install this build\.\n/,
  );
  // The notice survived intact, so some bullets must have been dropped.
  assert.ok(contents.split("\n").filter((line) => line.startsWith("- b")).length < bullets.length);
});

test("truncates an over-long notice instead of dropping it", () => {
  const contents = formatFdroidChangelog([`> ${"n".repeat(900)}`, "", "### Fixed", "", "- A fix."]);

  assert.ok(contents.length <= CHANGELOG_CHARACTER_LIMIT);
  assert.match(contents, /^n+\.\.\.\n\nFull notes:/);
  assert.equal(contents.includes("A fix."), false);
});

test("captures multiple blockquotes and blockquotes that follow sections", () => {
  const contents = formatFdroidChangelog([
    "> First notice.",
    "",
    "### Fixed",
    "",
    "- A fix.",
    "",
    "> Second notice.",
  ]);

  assert.match(contents, /^First notice\.\n\nSecond notice\.\n\nFixed\n- A fix\./);
});

test("renders the real 0.1.109 notice, which is the entry's whole point", () => {
  const changelog = [
    "## 0.1.109 - 2026-07-16",
    "",
    "> **Important update notice**",
    ">",
    "> If you installed Paseo Desktop 0.1.108, you need to [download and reinstall Paseo manually](https://paseo.sh/download) to get this fix. The bug in 0.1.108 prevents its automatic updater from installing 0.1.109. Users on 0.1.107 or earlier can update normally.",
    "",
    "### Fixed",
    "",
    "- Paseo Desktop no longer gets stuck connecting or loses native window controls after updating ([#2111](https://github.com/getpaseo/paseo/pull/2111) by [@cleiter](https://github.com/cleiter))",
    "",
  ].join("\n");

  withTempRepo(
    (dir) => {
      const { contents } = syncFdroidChangelogs([], { cwd: dir });
      assert.ok(contents.length <= CHANGELOG_CHARACTER_LIMIT);
      assert.match(contents, /^Important update notice\n/);
      assert.match(contents, /download and reinstall Paseo manually/);
      assert.match(contents, /Users on 0\.1\.107 or earlier can update normally\./);
      assert.match(contents, /\nFixed\n- Paseo Desktop no longer gets stuck connecting/);
      assert.equal(contents.includes("#2111"), false);
    },
    { changelog, version: "0.1.109" },
  );
});

test("preserves the standalone 0.1.96 Android notice", () => {
  const changelog = readFileSync(path.resolve("CHANGELOG.md"), "utf8");
  const entry = changelog.match(/## 0\.1\.96 - 2026-06-13\n([\s\S]*?)(?=\n## |$)/)?.[1];
  assert.ok(entry);

  const contents = formatFdroidChangelog(entry.split("\n"));
  assert.match(contents, /This release only fixes an Android issue/);
  assert.equal(contents.includes("_This release"), false);
});

test("--check fails when the generated files are missing or stale", () => {
  const changelog = "## 0.2.3 - 2026-07-27\n\n### Fixed\n\n- Something broke.\n";
  withTempRepo(
    (dir) => {
      assert.throws(() => syncFdroidChangelogs(["--check"], { cwd: dir }), /out of date/);

      syncFdroidChangelogs([], { cwd: dir });
      assert.doesNotThrow(() => syncFdroidChangelogs(["--check"], { cwd: dir }));

      const [{ versionCode }] = getFdroidVersionCodes("0.2.3");
      writeFileSync(
        path.join(
          dir,
          "fastlane",
          "metadata",
          "android",
          "en-US",
          "changelogs",
          `${versionCode}.txt`,
        ),
        "drifted\n",
      );
      assert.throws(() => syncFdroidChangelogs(["--check"], { cwd: dir }), /out of date/);
    },
    { changelog },
  );
});

test("fails loudly when the release has no changelog entry", () => {
  const changelog = "## 0.2.2 - 2026-07-25\n\n### Fixed\n\n- Older release.\n";
  withTempRepo(
    (dir) => {
      assert.throws(() => syncFdroidChangelogs([], { cwd: dir }), /No CHANGELOG\.md entry/);
    },
    { changelog, version: "0.2.3" },
  );
});

test("honours a non-default locale directory", () => {
  const changelog = "## 0.2.3 - 2026-07-27\n\n### Fixed\n\n- Something broke.\n";
  withTempRepo(
    (dir) => {
      mkdirSync(path.join(dir, "fastlane", "metadata", "android", "ja"), { recursive: true });
      syncFdroidChangelogs(["--locale", "ja"], { cwd: dir });
      const [{ versionCode }] = getFdroidVersionCodes("0.2.3");
      assert.match(readChangelogFile(dir, versionCode, "ja"), /- Something broke\./);
    },
    { changelog },
  );
});
