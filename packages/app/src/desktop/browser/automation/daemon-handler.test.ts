import { beforeEach, describe, expect, test, vi } from "vitest";
import type { SessionOutboundMessage } from "@getpaseo/protocol/messages";
import { BROWSER_AUTOMATION_COMMAND_NAMES } from "@getpaseo/protocol/browser-automation/rpc-schemas";
import { mountBrowserAutomationDaemonClientHandler } from "./daemon-handler";

type BrowserAutomationExecuteRequest = Extract<
  SessionOutboundMessage,
  { type: "browser.automation.execute.request" }
>;

const handleBrowserAutomationDaemonRequest = vi.hoisted(() => vi.fn(async () => {}));

vi.mock("./handler", () => ({ handleBrowserAutomationDaemonRequest }));

describe("mountBrowserAutomationDaemonClientHandler", () => {
  beforeEach(() => {
    handleBrowserAutomationDaemonRequest.mockClear();
  });

  test("registers capabilities eagerly and loads the command handler only for a request", async () => {
    const updates: Array<(message: BrowserAutomationExecuteRequest) => void> = [];
    const unsubscribe = vi.fn();
    const release = vi.fn(async () => {});
    const observation = {
      subscribe: vi.fn(
        (observer: {
          snapshot: () => void;
          update: (message: BrowserAutomationExecuteRequest) => void;
        }) => {
          updates.push(observer.update);
          return unsubscribe;
        },
      ),
      release,
    };
    const client = {
      registerBrowserHost: vi.fn(() => observation),
      sendBrowserAutomationExecuteResponse: vi.fn(),
    } as unknown as Parameters<typeof mountBrowserAutomationDaemonClientHandler>[0];

    const unmount = mountBrowserAutomationDaemonClientHandler(client, { serverId: "host-1" });

    expect(client.registerBrowserHost).toHaveBeenCalledWith({
      hostKind: "desktop app",
      supportedCommands: [...BROWSER_AUTOMATION_COMMAND_NAMES],
    });
    expect(handleBrowserAutomationDaemonRequest).not.toHaveBeenCalled();

    const request: BrowserAutomationExecuteRequest = {
      type: "browser.automation.execute.request",
      requestId: "request-1",
      command: { command: "list_tabs", args: {} },
    };
    updates[0]?.(request);
    await vi.waitFor(() => {
      expect(handleBrowserAutomationDaemonRequest).toHaveBeenCalledWith(client, request, {
        serverId: "host-1",
      });
    });

    unmount();
    expect(unsubscribe).toHaveBeenCalledOnce();
    expect(release).toHaveBeenCalledOnce();
  });
});
