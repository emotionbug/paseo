import type { DaemonClient } from "@getpaseo/client/internal/daemon-client";
import { BROWSER_AUTOMATION_COMMAND_NAMES } from "@getpaseo/protocol/browser-automation/rpc-schemas";
import type { SessionInboundMessage, SessionOutboundMessage } from "@getpaseo/protocol/messages";

type BrowserAutomationExecuteRequest = Extract<
  SessionOutboundMessage,
  { type: "browser.automation.execute.request" }
>;
type BrowserAutomationExecuteResponse = Extract<
  SessionInboundMessage,
  { type: "browser.automation.execute.response" }
>;

type BrowserAutomationDaemonClient = Pick<
  DaemonClient,
  "registerBrowserHost" | "sendBrowserAutomationExecuteResponse"
>;

export function mountBrowserAutomationDaemonClientHandler(
  client: BrowserAutomationDaemonClient,
  options?: { serverId?: string },
): () => void {
  const observation = client.registerBrowserHost({
    hostKind: "desktop app",
    supportedCommands: [...BROWSER_AUTOMATION_COMMAND_NAMES],
  });
  let disposed = false;
  const unsubscribe = observation.subscribe({
    snapshot: () => {},
    update: (message) => {
      if (message.type !== "browser.automation.execute.request") return;
      void dispatchBrowserAutomationRequest(client, message, options).catch((error) => {
        if (disposed) return;
        client.sendBrowserAutomationExecuteResponse(importFailureResponse(message, error));
      });
    },
  });
  return () => {
    disposed = true;
    unsubscribe();
    void observation
      .release()
      .catch((error) => console.warn("Failed to release browser host", error));
  };
}

async function dispatchBrowserAutomationRequest(
  client: BrowserAutomationDaemonClient,
  request: BrowserAutomationExecuteRequest,
  options?: { serverId?: string },
): Promise<void> {
  const { handleBrowserAutomationDaemonRequest } = await import("./handler");
  await handleBrowserAutomationDaemonRequest(client, request, options);
}

function importFailureResponse(
  request: BrowserAutomationExecuteRequest,
  error: unknown,
): BrowserAutomationExecuteResponse {
  return {
    type: "browser.automation.execute.response",
    payload: {
      ok: false,
      requestId: request.requestId,
      error: {
        code: "browser_unknown_error",
        message: error instanceof Error ? error.message : "Browser automation failed to load.",
        retryable: false,
      },
    },
  };
}
