import type { Page } from "@playwright/test";
import { test, expect } from "../support/fixtures";
import { runWorkspaceActionFromCommandCenter } from "../support/helpers/command-center-workspace-actions";

async function waitForResizePaint(page: Page): Promise<void> {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
}

async function previewExplorerResize(page: Page, distance: number) {
  const handle = page
    .getByTestId("workspace-explorer-sidebar-resize-handle")
    .getByRole("separator");
  const explorer = page.getByTestId("workspace-explorer-sidebar");
  const bounds = await handle.boundingBox();
  if (!bounds) throw new Error("Explorer resize handle is missing");
  const before = (await explorer.boundingBox())!.width;
  const x = bounds.x + bounds.width / 2;
  const y = bounds.y + 100;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await waitForResizePaint(page);
  for (let move = 1; move <= 20; move++) {
    await page.mouse.move(x + (distance * move) / 20, y);
    await waitForResizePaint(page);
  }
  const during = (await explorer.boundingBox())!.width;
  await page.mouse.up();
  await waitForResizePaint(page);
  const after = (await explorer.boundingBox())!.width;
  expect(during).toBeCloseTo(before - distance, 0);
  expect(after).toBeCloseTo(during, 0);
  return after;
}

async function expectExplorerWidthSurvivesReload(page: Page, width: number): Promise<void> {
  await page.reload();
  const explorer = page.getByTestId("workspace-explorer-sidebar");
  await expect(explorer).toBeVisible();
  await expect.poll(async () => (await explorer.boundingBox())?.width).toBeCloseTo(width, 0);
}

test("Explorer drag previews its width and preserves it after release and reload", async ({
  page,
  withWorkspace,
}) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  const workspace = await withWorkspace();
  await workspace.navigateTo();
  await runWorkspaceActionFromCommandCenter(page, "Split pane right");
  await runWorkspaceActionFromCommandCenter(page, "Split pane right");
  await page.getByTestId("workspace-explorer-toggle").first().click();
  await expect(page.getByTestId("workspace-explorer-sidebar")).toBeVisible();
  await previewExplorerResize(page, -100);
  const shrunk = await previewExplorerResize(page, 50);
  await expectExplorerWidthSurvivesReload(page, shrunk);
});
