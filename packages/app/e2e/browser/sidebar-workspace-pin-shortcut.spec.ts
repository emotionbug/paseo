import { test, expect, type Page } from "../support/fixtures";
import { gotoAppShell } from "../support/helpers/app";
import { daemonWsRoutePattern } from "../support/helpers/daemon-port";
import {
  expectNewWorkspaceProjectSelected,
  openNewWorkspaceComposer,
} from "../support/helpers/new-workspace";
import { seedWorkspace, type SeededWorkspace } from "../support/helpers/seed-client";
import { getServerId } from "../support/helpers/server-id";
import { switchWorkspaceViaCommandCenter } from "../support/helpers/workspace-ui";

// The pin shortcut used to be registered by the sidebar row itself, so it silently did nothing
// whenever the row was unmounted — a collapsed project section being the common case. It now
// lives in a single always-mounted handler keyed on the active route selection.
const PIN_SHORTCUT = "ControlOrMeta+Shift+P";

async function expectPinned(workspace: SeededWorkspace, pinned: boolean): Promise<void> {
  await expect
    .poll(async () => {
      const { entries } = await workspace.client.fetchWorkspaces();
      return entries.find((entry) => entry.id === workspace.workspaceId)?.pinnedAt != null;
    })
    .toBe(pinned);
}

async function openWorkspace(page: Page, workspaceId: string) {
  await switchWorkspaceViaCommandCenter({ page, serverId: getServerId(), workspaceId });
}

function readSessionMessage(
  message: string | Buffer,
): { type?: unknown; requestId?: unknown } | null {
  const raw = typeof message === "string" ? message : message.toString("utf8");
  try {
    const envelope = JSON.parse(raw) as { type?: unknown; message?: unknown };
    if (envelope.type !== "session" || typeof envelope.message !== "object") {
      return null;
    }
    return envelope.message as { type?: unknown; requestId?: unknown };
  } catch {
    return null;
  }
}

const PIN_REJECTION_MESSAGE = "Pin rejected by test.";

interface PinRpcGate {
  /** Pin requests the client has sent so far. */
  sentCount(): number;
}

// Proxies everything so the app boots against the real daemon, counting pin RPCs and optionally
// rejecting the first `rejectFirst` of them. The count asserts how many pins one keypress actually
// dispatched, which the rendered pin state cannot show — a toggle that fired twice lands back
// where it started.
async function installPinRpcGate(
  page: Page,
  options: { rejectFirst?: number } = {},
): Promise<PinRpcGate> {
  const rejectFirst = options.rejectFirst ?? 0;
  let sent = 0;

  await page.routeWebSocket(daemonWsRoutePattern(), (ws) => {
    const server = ws.connectToServer();

    ws.onMessage((message) => {
      const sessionMessage = readSessionMessage(message);
      if (
        sessionMessage?.type === "workspace.pin.set.request" &&
        typeof sessionMessage.requestId === "string"
      ) {
        sent += 1;
        if (sent <= rejectFirst) {
          ws.send(
            JSON.stringify({
              type: "session",
              message: {
                type: "rpc_error",
                payload: {
                  requestId: sessionMessage.requestId,
                  requestType: "workspace.pin.set.request",
                  error: PIN_REJECTION_MESSAGE,
                  code: "transport",
                },
              },
            }),
          );
          return;
        }
      }

      try {
        server.send(message);
      } catch {
        // server socket already closed
      }
    });

    server.onMessage((message) => {
      try {
        ws.send(message);
      } catch {
        // client socket already closed
      }
    });
  });

  return { sentCount: () => sent };
}

test.describe("Pin workspace shortcut", () => {
  test("dispatches pin once and keeps project creation reachable", async ({ page }) => {
    const workspace = await seedWorkspace({
      repoPrefix: "pin-project-creation-",
      title: "Pinned workspace",
    });

    try {
      const gate = await installPinRpcGate(page);

      await gotoAppShell(page);
      await openWorkspace(page, workspace.workspaceId);

      await test.step("sends one RPC for each pin transition", async () => {
        await page.keyboard.press(PIN_SHORTCUT);
        await expectPinned(workspace, true);
        expect(gate.sentCount()).toBe(1);

        await page.keyboard.press(PIN_SHORTCUT);
        await expectPinned(workspace, false);
        expect(gate.sentCount()).toBe(2);
      });

      await test.step("keeps the pinned project available to workspace creation", async () => {
        await page.keyboard.press(PIN_SHORTCUT);
        await expectPinned(workspace, true);
        expect(gate.sentCount()).toBe(3);

        await openNewWorkspaceComposer(page, workspace);
        await expectNewWorkspaceProjectSelected(page, workspace.projectDisplayName);
      });
    } finally {
      await workspace.cleanup();
    }
  });

  test("shows an error toast when the host rejects the pin, and the next press succeeds", async ({
    page,
  }) => {
    const workspace = await seedWorkspace({ repoPrefix: "pin-shortcut-failure-" });

    try {
      const gate = await installPinRpcGate(page, { rejectFirst: 1 });

      await gotoAppShell(page);
      await openWorkspace(page, workspace.workspaceId);

      await page.keyboard.press(PIN_SHORTCUT);

      await expect(page.getByTestId("app-toast-message")).toContainText(PIN_REJECTION_MESSAGE, {
        timeout: 10_000,
      });
      await expectPinned(workspace, false);

      // The failure must leave the action usable: the in-flight guard has to release the key so a
      // retry is not swallowed. Without that release the workspace is unpinnable for the session.
      await page.keyboard.press(PIN_SHORTCUT);

      await expectPinned(workspace, true);
      expect(gate.sentCount()).toBe(2);
    } finally {
      await workspace.cleanup();
    }
  });
});
