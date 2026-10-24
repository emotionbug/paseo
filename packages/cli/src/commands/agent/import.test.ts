import { afterEach, describe, expect, it, vi } from "vitest";
import { resolveImportCwd, runImportCommand } from "./import.js";

const importAgent = vi.fn();
const close = vi.fn();
const fetchRecentProviderSessions = vi.fn();
const { select, confirm, runChatCommand } = vi.hoisted(() => ({
  select: vi.fn(),
  confirm: vi.fn(),
  runChatCommand: vi.fn(),
}));
vi.mock("@clack/prompts", () => ({
  select,
  confirm,
  isCancel: (value: unknown) => typeof value === "symbol",
}));
vi.mock("../chat.js", () => ({ runChatCommand }));
const originalTTY = process.stdin.isTTY;
afterEach(() => {
  Object.defineProperty(process.stdin, "isTTY", { configurable: true, value: originalTTY });
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

vi.mock("../../utils/client.js", () => ({
  connectToDaemon: vi.fn(async () => ({
    importAgent,
    fetchRecentProviderSessions,
    close,
  })),
  getDaemonHost: vi.fn(() => "ws://127.0.0.1:6767"),
}));

describe("resolveImportCwd", () => {
  it("uses the invoking process cwd when --cwd is omitted", () => {
    expect(resolveImportCwd(undefined, "/Volumes/data/dev/rolepai")).toBe(
      "/Volumes/data/dev/rolepai",
    );
  });

  it("uses explicit --cwd when provided", () => {
    expect(resolveImportCwd(" /tmp/project ", "/Volumes/data/dev/rolepai")).toBe("/tmp/project");
  });

  it("rejects an empty explicit --cwd", () => {
    expect(() => resolveImportCwd("  ", "/Volumes/data/dev/rolepai")).toThrow(
      expect.objectContaining({
        code: "INVALID_CWD",
      }),
    );
  });

  it("accepts pi as an import provider", async () => {
    importAgent.mockResolvedValueOnce({
      id: "agent-1",
      status: "idle",
      provider: "pi",
      cwd: "/tmp/project",
      title: "Imported Pi session",
    });

    const result = await runImportCommand(
      "pi-session-1",
      {
        daemonTarget: { kind: "endpoint", host: "example.test:12345" },
        provider: "pi",
        cwd: "/tmp/project",
      },
      {} as never,
    );

    expect(importAgent).toHaveBeenCalledWith({
      provider: "pi",
      sessionId: "pi-session-1",
      cwd: "/tmp/project",
    });
    expect(result.data.provider).toBe("pi");
  });
});

describe("interactive import", () => {
  const options = {
    daemonTarget: { kind: "endpoint" as const, host: "example.test:12345" },
    cwd: "/tmp/project",
    provider: "pi",
  };
  function prepare() {
    Object.defineProperty(process.stdin, "isTTY", { configurable: true, value: true });
    fetchRecentProviderSessions.mockResolvedValue({
      entries: [
        {
          providerId: "pi",
          providerLabel: "Pi",
          providerHandleId: "native-1",
          cwd: "/tmp/project",
          title: "Old conversation",
          lastActivityAt: "2026-10-25",
        },
      ],
      providerErrors: [],
    });
    select.mockResolvedValue(0);
    confirm.mockResolvedValue(true);
    importAgent.mockResolvedValue({
      id: "paseo-1",
      provider: "pi",
      status: "idle",
      cwd: "/tmp/project",
      title: "Old conversation",
    });
  }
  it("discovers cwd sessions, imports the selection and opens its Paseo chat", async () => {
    prepare();
    const command = {} as never;
    await runImportCommand(undefined, options, command);
    expect(fetchRecentProviderSessions).toHaveBeenCalledWith({
      cwd: "/tmp/project",
      limit: 200,
      providers: ["pi"],
    });
    expect(importAgent).toHaveBeenCalledWith({
      provider: "pi",
      sessionId: "native-1",
      cwd: "/tmp/project",
      labels: {},
    });
    expect(runChatCommand).toHaveBeenCalledWith({ ...options, resume: "paseo-1" }, command);
    expect(close).toHaveBeenCalledOnce();
  });
  it("does not import when the original session has not been closed", async () => {
    prepare();
    confirm.mockResolvedValue(false);
    await expect(runImportCommand(undefined, options, {} as never)).rejects.toMatchObject({
      code: "CANCELLED",
    });
    expect(importAgent).not.toHaveBeenCalled();
    expect(runChatCommand).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
  });
  it("explains where already imported sessions can be resumed", async () => {
    prepare();
    fetchRecentProviderSessions.mockResolvedValue({ entries: [] });
    await expect(runImportCommand(undefined, options, {} as never)).rejects.toMatchObject({
      code: "NO_IMPORTABLE_SESSIONS",
      details: expect.stringContaining("paseo --resume"),
    });
    expect(importAgent).not.toHaveBeenCalled();
  });
});
