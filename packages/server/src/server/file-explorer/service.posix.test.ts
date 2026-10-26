// POSIX-only: symlink fixtures
/* eslint-disable max-nested-callbacks */
import { promises as fs } from "node:fs";
import { mkdtemp, mkdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { describe, expect, it, vi } from "vitest";
import { getDownloadableFileInfo, listDirectoryEntries, readExplorerFile } from "./service.js";
import { isPlatform } from "../../test-utils/platform.js";

async function createTempDir(prefix: string): Promise<string> {
  return mkdtemp(path.join(os.tmpdir(), prefix));
}

describe.skipIf(isPlatform("win32"))("service POSIX-only", () => {
  it("lists directory entries even when a dangling symlink exists", async () => {
    const root = await createTempDir("paseo-file-explorer-");

    try {
      await mkdir(path.join(root, "packages", "server"), { recursive: true });
      const serverDir = path.join(root, "packages", "server");
      await writeFile(path.join(serverDir, "README.md"), "# server\n", "utf-8");
      await symlink("CLAUDE.md", path.join(serverDir, "AGENTS.md"));

      const result = await listDirectoryEntries({
        root,
        relativePath: "packages/server",
      });

      expect(result.path).toBe("packages/server");
      const names = result.entries.map((entry) => entry.name);
      expect(names).toContain("README.md");
      expect(names).not.toContain("AGENTS.md");
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("classifies contained directory symlinks without exposing directory escapes", async () => {
    const root = await createTempDir("paseo-file-explorer-");
    const outsideRoot = await createTempDir("paseo-file-explorer-outside-");

    try {
      const directory = path.join(root, "notes");
      await mkdir(directory);
      await writeFile(path.join(directory, "today.txt"), "safe\n", "utf-8");
      await mkdir(path.join(outsideRoot, "private"));
      await symlink("notes", path.join(root, "notes-link"));
      await symlink(path.join(outsideRoot, "private"), path.join(root, "escape-link"));

      const result = await listDirectoryEntries({ root });
      const directoryLink = result.entries.find((entry) => entry.name === "notes-link");

      expect(directoryLink).toMatchObject({
        name: "notes-link",
        path: "notes-link",
        kind: "directory",
      });
      expect(result.entries.map((entry) => entry.name)).not.toContain("escape-link");
      await expect(
        listDirectoryEntries({ root, relativePath: "notes-link" }),
      ).resolves.toMatchObject({
        path: "notes-link",
        entries: [expect.objectContaining({ name: "today.txt", kind: "file" })],
      });
    } finally {
      await rm(root, { recursive: true, force: true });
      await rm(outsideRoot, { recursive: true, force: true });
    }
  });

  it("omits an inaccessible child but still rejects an inaccessible parent", async () => {
    const root = await createTempDir("paseo-file-explorer-");

    try {
      const deniedPath = path.join(root, "denied.txt");
      await writeFile(deniedPath, "denied\n", "utf-8");
      await writeFile(path.join(root, "visible.txt"), "visible\n", "utf-8");
      const actualStat = fs.stat.bind(fs);
      let denyRoot = false;
      const statSpy = vi.spyOn(fs, "stat").mockImplementation(async (entryPath) => {
        const requestedPath = String(entryPath);
        if (requestedPath === deniedPath || (denyRoot && requestedPath === root)) {
          throw Object.assign(new Error("permission denied"), { code: "EACCES" });
        }
        return actualStat(entryPath);
      });

      try {
        const result = await listDirectoryEntries({ root });

        expect(result.entries.map((entry) => entry.name)).toEqual(["visible.txt"]);
        denyRoot = true;
        await expect(listDirectoryEntries({ root })).rejects.toMatchObject({ code: "EACCES" });
      } finally {
        statSpy.mockRestore();
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("rejects symlinked files that resolve outside the workspace", async () => {
    const root = await createTempDir("paseo-file-explorer-");
    const outsideRoot = await createTempDir("paseo-file-explorer-outside-");

    try {
      const externalFile = path.join(outsideRoot, "secret.txt");
      await writeFile(externalFile, "top secret\n", "utf-8");
      await symlink(externalFile, path.join(root, "secret-link.txt"));

      await expect(
        readExplorerFile({
          root,
          relativePath: "secret-link.txt",
        }),
      ).rejects.toThrow("Access outside of workspace is not allowed");
    } finally {
      await rm(root, { recursive: true, force: true });
      await rm(outsideRoot, { recursive: true, force: true });
    }
  });

  it("skips listed symlink entries that resolve outside the workspace", async () => {
    const root = await createTempDir("paseo-file-explorer-");
    const outsideRoot = await createTempDir("paseo-file-explorer-outside-");

    try {
      await writeFile(path.join(root, "visible.txt"), "visible\n", "utf-8");
      const externalFile = path.join(outsideRoot, "secret.txt");
      await writeFile(externalFile, "top secret\n", "utf-8");
      await symlink(externalFile, path.join(root, "secret-link.txt"));

      const result = await listDirectoryEntries({ root });

      const names = result.entries.map((entry) => entry.name);
      expect(names).toContain("visible.txt");
      expect(names).not.toContain("secret-link.txt");
    } finally {
      await rm(root, { recursive: true, force: true });
      await rm(outsideRoot, { recursive: true, force: true });
    }
  });

  it("uses canonical paths for downloadable symlink targets inside the workspace", async () => {
    const root = await createTempDir("paseo-file-explorer-");

    try {
      const target = path.join(root, "safe.txt");
      const link = path.join(root, "safe-link.txt");
      await writeFile(target, "safe\n", "utf-8");
      await symlink("safe.txt", link);

      const file = await readExplorerFile({
        root,
        relativePath: "safe-link.txt",
      });
      const info = await getDownloadableFileInfo({
        root,
        relativePath: "safe-link.txt",
      });

      expect(file.path).toBe("safe-link.txt");
      expect(file.content).toBe("safe\n");
      expect(info.path).toBe("safe-link.txt");
      expect(info.fileName).toBe("safe-link.txt");
      expect(info.absolutePath).toBe(await realpath(target));
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
});
