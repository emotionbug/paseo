import type { EventEmitter } from "node:events";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({
  host: true,
  probeFails: false,
  probe: null as (() => void) | null,
  deferProbe: false,
  tray: null as EventEmitter | null,
  iconEmpty: false,
  trayFails: false,
}));

vi.mock("node:child_process", () => ({
  execFile: (
    _file: string,
    _args: string[],
    _options: unknown,
    callback: (error: Error | null, stdout: string) => void,
  ) => {
    state.probe = () =>
      callback(
        state.probeFails ? new Error("No session bus") : null,
        `variant boolean ${state.host}`,
      );
    if (!state.deferProbe) state.probe();
  },
}));
vi.mock("electron-log/main", () => ({ default: { warn: vi.fn() } }));
vi.mock("electron", async () => {
  // Vitest hoists this Electron factory before static imports are initialized.
  const { EventEmitter } = await import("node:events");
  const windows: Window[] = [];
  class Window extends EventEmitter {
    visible = true;
    destroyed = false;
    constructor() {
      super();
      windows.push(this);
    }
    static getAllWindows() {
      return windows.filter((win) => !win.destroyed);
    }
    isVisible() {
      return this.visible;
    }
    isDestroyed() {
      return this.destroyed;
    }
    hide() {
      this.visible = false;
    }
    show() {
      this.visible = true;
    }
    destroy() {
      this.destroyed = true;
      this.emit("closed");
    }
    close() {
      let prevented = false;
      this.emit("close", { preventDefault: () => (prevented = true) });
      if (!prevented) this.destroy();
    }
  }
  class Tray extends EventEmitter {
    destroyed = false;
    constructor() {
      super();
      if (state.trayFails) throw new Error("Tray unavailable");
      state.tray = this;
    }
    isDestroyed() {
      return this.destroyed;
    }
    destroy() {
      this.destroyed = true;
    }
    setContextMenu() {}
    setToolTip() {}
  }
  const app = Object.assign(new EventEmitter(), {
    name: "Paseo Sessions",
    quit: () => {
      app.emit("before-quit");
      for (const win of Window.getAllWindows()) win.close();
      app.emit("quit");
    },
  });
  const image = { isEmpty: () => state.iconEmpty, resize: () => image };
  return {
    app,
    BrowserWindow: Window,
    Tray,
    autoUpdater: new EventEmitter(),
    powerMonitor: new EventEmitter(),
    nativeImage: { createFromPath: () => image },
    Menu: { buildFromTemplate: () => ({}) },
  };
});

import { app, autoUpdater, BrowserWindow } from "electron";
import { createDesktopTray } from "./tray.js";

let tray: ReturnType<typeof createDesktopTray>;
function openWindow() {
  const win = new BrowserWindow();
  tray = createDesktopTray({ iconPath: "/icon.png", onShow: () => win.show() });
  tray?.attachWindow(win);
  return win;
}

beforeEach(() => {
  vi.spyOn(process, "platform", "get").mockReturnValue("linux");
  Object.assign(state, {
    host: true,
    probeFails: false,
    deferProbe: false,
    probe: null,
    tray: null,
    iconEmpty: false,
    trayFails: false,
  });
});
afterEach(() => {
  tray?.destroy();
  for (const win of BrowserWindow.getAllWindows()) win.destroy();
  vi.restoreAllMocks();
});

describe("desktop tray window lifecycle", () => {
  it.each(["linux", "win32", "darwin"] as const)(
    "retains and restores the same last window on %s",
    async (platform) => {
      vi.spyOn(process, "platform", "get").mockReturnValue(platform);
      const win = openWindow();
      win.close();
      await vi.waitFor(() => expect(win.isVisible()).toBe(false));
      expect(win.isDestroyed()).toBe(false);
      state.tray?.emit("click");
      expect(win.isVisible()).toBe(true);
      expect(BrowserWindow.getAllWindows()).toEqual([win]);
    },
  );

  it("closes an additional window without hiding the remaining window", () => {
    const first = openWindow();
    const second = new BrowserWindow();
    tray?.attachWindow(second);
    second.close();
    expect(second.isDestroyed()).toBe(true);
    expect(first.isVisible()).toBe(true);
  });

  it.each(["missing-host", "failed-probe"])("closes normally for %s", async (failure) => {
    state.host = false;
    state.probeFails = failure === "failed-probe";
    const win = openWindow();
    win.close();
    await vi.waitFor(() => expect(win.isDestroyed()).toBe(true));
  });

  it.each(["iconEmpty", "trayFails"] as const)("closes normally after %s", (failure) => {
    state[failure] = true;
    const win = openWindow();
    expect(tray).toBeNull();
    win.close();
    expect(win.isDestroyed()).toBe(true);
  });

  it("does not intercept explicit quit while a tray availability check is pending", async () => {
    state.deferProbe = true;
    const win = openWindow();
    win.close();
    app.quit();
    expect(win.isDestroyed()).toBe(true);
    state.probe?.();
    await Promise.resolve();
    expect(BrowserWindow.getAllWindows()).toEqual([]);
  });

  it.each(["update", "logoff"])("does not block %s by hiding the window", (reason) => {
    const win = openWindow();
    if (reason === "update") autoUpdater.emit("before-quit-for-update");
    else win.emit("query-session-end");
    win.close();
    expect(win.isDestroyed()).toBe(true);
  });
});
