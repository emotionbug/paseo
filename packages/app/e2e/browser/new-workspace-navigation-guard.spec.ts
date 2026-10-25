import { test } from "../support/fixtures";
import { verifyDelayedWorkspaceCreation } from "../support/helpers/new-workspace-navigation";

test.describe("Delayed workspace creation", () => {
  test.describe.configure({ timeout: 120_000 });

  // Exercise each submit handler's navigation guard on current hosts' local creation flow.
  test("chat creation keeps the workspace chosen while it was pending", async ({ page }) => {
    await verifyDelayedWorkspaceCreation(page, "chat", "leave");
  });

  test("terminal creation keeps the workspace chosen while it was pending", async ({ page }) => {
    await verifyDelayedWorkspaceCreation(page, "terminal", "leave");
  });

  test("empty creation keeps the workspace chosen while it was pending", async ({ page }) => {
    await verifyDelayedWorkspaceCreation(page, "empty", "leave");
  });

  test("preserves a newer draft opened while creation is pending", async ({ page }) => {
    await verifyDelayedWorkspaceCreation(page, "chat", "new-draft");
  });
});
