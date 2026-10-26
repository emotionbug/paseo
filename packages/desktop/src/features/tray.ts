import { execFile } from "node:child_process";
import { app, autoUpdater, BrowserWindow, Menu, nativeImage, powerMonitor, Tray } from "electron";
import log from "electron-log/main";

function hasLinuxTrayHost(): Promise<boolean> {
  // Electron can construct a Tray even when the desktop has no panel to display it.
  // Check the host, not just the watcher's bus name, before hiding the last window.
  return new Promise((resolve) => {
    execFile(
      "dbus-send",
      [
        "--session",
        "--print-reply",
        "--reply-timeout=1000",
        "--dest=org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.freedesktop.DBus.Properties.Get",
        "string:org.kde.StatusNotifierWatcher",
        "string:IsStatusNotifierHostRegistered",
      ],
      { timeout: 1500, maxBuffer: 4096, encoding: "utf8" },
      // Missing bus, panel, or dbus-send keeps normal window closing available.
      (error, stdout) => resolve(!error && /\bvariant\s+boolean true\s*$/.test(stdout)),
    );
  });
}

interface DesktopTray {
  attachWindow(win: BrowserWindow): void;
  destroy(): void;
}

interface DesktopTrayOptions {
  iconPath: string;
  onShow(): void;
}

export function createDesktopTray({ iconPath, onShow }: DesktopTrayOptions): DesktopTray | null {
  const image = nativeImage.createFromPath(iconPath);
  if (image.isEmpty()) {
    log.warn("[tray] No usable icon; keeping normal window closing");
    return null;
  }

  let tray: Tray;
  try {
    const size = process.platform === "darwin" ? 18 : 32;
    const icon = process.platform === "win32" ? image : image.resize({ width: size, height: size });
    tray = new Tray(icon);
  } catch (error) {
    log.warn("[tray] Failed to create tray; keeping normal window closing", error);
    return null;
  }

  let quitting = false;
  function beginQuit(): void {
    quitting = true;
  }
  function show(): void {
    if (!quitting) onShow();
  }
  tray.setToolTip(app.name);
  tray.setContextMenu(
    Menu.buildFromTemplate([
      { label: `Open ${app.name}`, click: show },
      { type: "separator" },
      { label: `Quit ${app.name}`, click: () => app.quit() },
    ]),
  );
  tray.on("click", show);
  tray.on("double-click", show);
  app.on("before-quit", beginQuit);
  autoUpdater.on("before-quit-for-update", beginQuit);
  powerMonitor.on("shutdown", beginQuit);

  function destroy(): void {
    beginQuit();
    app.removeListener("before-quit", beginQuit);
    app.removeListener("quit", destroy);
    autoUpdater.removeListener("before-quit-for-update", beginQuit);
    powerMonitor.removeListener("shutdown", beginQuit);
    if (!tray.isDestroyed()) tray.destroy();
  }
  app.once("quit", destroy);

  return {
    destroy,
    attachWindow(win) {
      let checkingHost = false;
      let allowClose = false;
      // Windows shutdown/logoff does not emit app's before-quit event.
      win.on("query-session-end", beginQuit);
      win.on("close", (event) => {
        if (quitting || allowClose || tray.isDestroyed()) return;
        if (BrowserWindow.getAllWindows().length > 1) return;
        event.preventDefault();
        if (checkingHost) return;
        checkingHost = true;
        const available = process.platform === "linux" ? hasLinuxTrayHost() : Promise.resolve(true);
        void available.then((hasHost) => {
          checkingHost = false;
          if (quitting || win.isDestroyed()) return;
          if (hasHost && BrowserWindow.getAllWindows().length === 1) {
            win.hide();
          } else {
            allowClose = true;
            win.close();
            allowClose = false;
          }
          return undefined;
        });
      });
    },
  };
}
