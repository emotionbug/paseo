import assert from "node:assert/strict";
import test from "node:test";


test("Codex title sync prefers named imports and tracks edits in both directions", async () => {
  const { resolveTitleSync } = await import("./sync-codex-session-titles.mjs");
  assert.deepEqual(resolveTitleSync({ paseo: "Long first prompt", codex: "Short title" }), {
    target: "paseo",
    title: "Short title",
  });
  assert.equal(resolveTitleSync({ paseo: "Long first prompt", codex: "" }), null);
  const previous = { paseo: "Short title", codex: "Short title" };
  assert.equal(resolveTitleSync({ ...previous, previous }), null);
  assert.deepEqual(resolveTitleSync({ paseo: "Phone edit", codex: "Short title", previous }), {
    target: "codex",
    title: "Phone edit",
  });
  assert.deepEqual(resolveTitleSync({ paseo: "Short title", codex: "Codex edit", previous }), {
    target: "paseo",
    title: "Codex edit",
  });
  assert.deepEqual(resolveTitleSync({ paseo: "Phone edit", codex: "Codex edit", previous }), {
    target: "paseo",
    title: "Codex edit",
  });
  assert.equal(resolveTitleSync({ paseo: "Phone edit", codex: "Phone edit", previous }), null);
});

test("Codex title sync reads latest appended names without consuming partial writes", async () => {
  const { readTitleIndex } = await import("./sync-codex-session-titles.mjs");
  const text = [
    JSON.stringify({ id: "thread", thread_name: "Original" }),
    JSON.stringify({ id: "thread", thread_name: "Renamed" }),
    '{"id":',
  ].join("\n");
  assert.equal(readTitleIndex(text).get("thread"), "Renamed");
  assert.equal(readTitleIndex("").size, 0);
  assert.throws(() => readTitleIndex('{"id": 1, "thread_name": "Bad"}'));
});

test("title watcher ignores streaming status, timestamps, and duplicate index entries", async () => {
  const { titleFingerprint } = await import("./watch-codex-session-titles.mjs");
  const agent = {
    id: "a",
    provider: "codex",
    title: "Original",
    persistence: { sessionId: "thread" },
  };
  const index = JSON.stringify({ id: "thread", thread_name: "Original" }) + "\n";
  const before = titleFingerprint([agent], index);
  assert.equal(
    titleFingerprint([{ ...agent, status: "running", updatedAt: "later" }], index + index),
    before,
  );
  assert.notEqual(titleFingerprint([{ ...agent, title: "Renamed" }], index), before);
  assert.notEqual(
    titleFingerprint([agent], JSON.stringify({ id: "thread", thread_name: "Codex edit" })),
    before,
  );
});
