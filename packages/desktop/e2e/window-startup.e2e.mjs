import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { _electron as electron, expect } from "playwright/test";

const root = await mkdtemp(path.join(tmpdir(), "paseo-window-startup-"));
const modulePath = fileURLToPath(new URL("../dist/window/show-when-ready.js", import.meta.url));
const managerPath = fileURLToPath(new URL("../dist/window/window-manager.js", import.meta.url));
const statePath = fileURLToPath(new URL("../dist/settings/window-state.js", import.meta.url));

async function launchWindowWithoutHiddenFrames() {
  const main = path.join(root, "main.cjs");
  await writeFile(
    main,
    `const { app, BrowserWindow } = require("electron");
     const { showWindowWhenReady } = require(${JSON.stringify(modulePath)});
     const { setupWindowStatePersistence } = require(${JSON.stringify(managerPath)});
     const { createWindowStateStore } = require(${JSON.stringify(statePath)});
     app.setPath("userData", ${JSON.stringify(path.join(root, "user-data"))});
     app.whenReady().then(async () => {
       const win = global.mainWindow = new BrowserWindow({ show: false, paintWhenInitiallyHidden: false });
       setupWindowStatePersistence(win, createWindowStateStore({ userDataPath: app.getPath("userData") }));
       showWindowWhenReady(win, { showOnLoad: true });
       await win.loadURL("data:text/html,<h1>Desktop ready</h1>");
       global.visibleAfterLoad = win.isVisible();
     });`,
  );
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => key !== "ELECTRON_RUN_AS_NODE"),
  );
  const displayArgs = process.platform === "linux" ? ["--ozone-platform=x11", "--disable-gpu"] : [];
  return electron.launch({
    args: ["--no-sandbox", ...displayArgs, main],
    env,
  });
}

async function expectLoadedDesktopIsShown(desktop) {
  await expect.poll(() => desktop.evaluate(() => global.visibleAfterLoad)).toBe(true);
  const page = await desktop.firstWindow();
  await expect(page.getByRole("heading", { name: "Desktop ready" })).toBeVisible();
}

async function expectGeometryPersistsAfterRetainedClose(desktop) {
  await desktop.evaluate(async () => {
    const win = global.mainWindow;
    const retainWindow = (event) => {
      event.preventDefault();
      win.hide();
    };
    win.on("close", retainWindow);
    try {
      win.close();
      win.show();
      // Close during resize, before the debounced save can conceal a skipped flush.
      await new Promise((resolve) => {
        win.once("resize", () => {
          win.close();
          resolve();
        });
        win.setSize(880, 640);
      });
    } finally {
      win.removeListener("close", retainWindow);
    }
  });
  const document = JSON.parse(
    await readFile(path.join(root, "user-data", "window-state.json"), "utf8"),
  );
  expect(document.state.width).toBe(880);
  expect(document.state.height).toBe(640);
}

let desktop;
try {
  desktop = await launchWindowWithoutHiddenFrames();
  await expectLoadedDesktopIsShown(desktop);
  console.log("Loaded desktop is visible without hidden compositor frames");
  await expectGeometryPersistsAfterRetainedClose(desktop);
  console.log("Restored window geometry is flushed on subsequent close");
} finally {
  await desktop?.close();
  await rm(root, { recursive: true, force: true });
}
