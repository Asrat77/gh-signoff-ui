"use strict";

const { app, BrowserWindow, Tray, Menu, nativeImage, screen, ipcMain, dialog, shell, clipboard } = require("electron");
const { spawn } = require("node:child_process");
const path = require("node:path");
const readline = require("node:readline");

const POPOVER_WIDTH = 440;
const POPOVER_HEIGHT = 700;
const SETTINGS_TABS = new Set(["repositories", "commands", "rules", "history", "diagnostics"]);
const STATUS_COLORS = {
  neutral: "#8A8D91",
  ready: "#36A269",
  attention: "#C98B2E",
  failing: "#C94F45",
};

let backend = null;
let backendInfo = null;
let tray = null;
let popover = null;
let settings = null;
let quitting = false;
let statusTimer = null;
let trayRefreshInFlight = false;
let currentStatus = "neutral";
const ownsInstanceLock = app.requestSingleInstanceLock();

app.setName("Signoff");
if (!ownsInstanceLock) app.quit();

function backendExecutable() {
  if (app.isPackaged) return path.join(process.resourcesPath, "bin", "gh-signoff-service");
  return path.join(__dirname, "bin", "gh-signoff-service");
}

function startBackend() {
  return new Promise((resolve, reject) => {
    const args = ["-desktop", "-no-browser", "-startup-json"];
    if (process.env.SIGNOFF_DEMO === "1") args.push("-demo");
    if (process.env.SIGNOFF_READ_ONLY === "1") args.push("-read-only");
    if (process.env.SIGNOFF_CONFIG) args.push("-config", process.env.SIGNOFF_CONFIG);

    const executable = backendExecutable();
    backend = spawn(executable, args, { stdio: ["ignore", "pipe", "pipe"] });
    const output = readline.createInterface({ input: backend.stdout });
    const startupTimeout = setTimeout(() => reject(new Error("Signoff service did not start in time.")), 15000);

    let receivedStartup = false;
    output.on("line", (line) => {
      if (!receivedStartup) {
        receivedStartup = true;
        clearTimeout(startupTimeout);
        try {
          backendInfo = JSON.parse(line);
          resolve(backendInfo);
        } catch (error) {
          reject(new Error(`Invalid Signoff service response: ${error.message}`));
        }
        return;
      }
      process.stdout.write(`[signoff] ${line}\n`);
    });
    backend.stderr.on("data", (chunk) => process.stderr.write(`[signoff] ${chunk}`));
    backend.once("error", (error) => {
      clearTimeout(startupTimeout);
      reject(error);
    });
    backend.once("exit", (code, signal) => {
      backend = null;
      if (!quitting && backendInfo) {
        dialog.showErrorBox("Signoff stopped", `The local Signoff service exited (${signal || code}). Reopen Signoff to restart it.`);
        app.quit();
      } else if (!quitting) {
        clearTimeout(startupTimeout);
        reject(new Error(`The Signoff service exited before startup (${signal || code}).`));
      }
    });
  });
}

function serviceURL(surface, tab) {
  const url = new URL(backendInfo.url);
  url.searchParams.set("surface", surface);
  if (tab) url.searchParams.set("tab", tab);
  return url.toString();
}

function trayIcon(status = "neutral") {
  const color = STATUS_COLORS[status] || STATUS_COLORS.neutral;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="36" height="36" viewBox="0 0 36 36"><circle cx="18" cy="18" r="15" fill="${color}"/><path d="M11 11v9a6 6 0 0 0 6 6h3" fill="none" stroke="white" stroke-width="3" stroke-linecap="round"/><circle cx="11" cy="10" r="3" fill="white"/><circle cx="24" cy="25" r="3" fill="white"/><path d="M24 22V13" fill="none" stroke="white" stroke-width="3" stroke-linecap="round"/><circle cx="24" cy="10" r="3" fill="white"/></svg>`;
  return nativeImage.createFromDataURL(`data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`).resize({ width: 18, height: 18 });
}

function secureWindow(window) {
  window.webContents.setWindowOpenHandler(({ url }) => {
    if (trustedGitHubURL(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  window.webContents.on("will-navigate", (event, url) => {
    try {
      if (!backendInfo || new URL(url).origin !== new URL(backendInfo.url).origin) event.preventDefault();
    } catch {
      event.preventDefault();
    }
  });
}

function createPopover() {
  popover = new BrowserWindow({
    width: POPOVER_WIDTH,
    height: POPOVER_HEIGHT,
    show: false,
    frame: false,
    transparent: true,
    resizable: false,
    movable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    alwaysOnTop: true,
    skipTaskbar: true,
    hasShadow: false,
    type: "panel",
    webPreferences: {
      preload: path.join(__dirname, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  secureWindow(popover);
  popover.loadURL(serviceURL("popover"));
  popover.on("blur", () => {
    if (!quitting && !popover.webContents.isDevToolsOpened()) popover.hide();
  });
  popover.on("closed", () => { popover = null; });
}

function positionPopover() {
  const trayBounds = tray.getBounds();
  const display = screen.getDisplayNearestPoint({ x: Math.round(trayBounds.x), y: Math.round(trayBounds.y) });
  const work = display.workArea;
  const preferredX = Math.round(trayBounds.x + trayBounds.width / 2 - POPOVER_WIDTH / 2);
  const x = Math.max(work.x + 8, Math.min(preferredX, work.x + work.width - POPOVER_WIDTH - 8));
  const belowTray = trayBounds.y + trayBounds.height + 4;
  const y = Math.min(belowTray, work.y + work.height - POPOVER_HEIGHT - 8);
  popover.setPosition(x, y, false);
}

function togglePopover() {
  if (!popover) createPopover();
  if (popover.isVisible()) {
    popover.hide();
    return;
  }
  showPopover();
}

function showPopover() {
  if (!popover) createPopover();
  positionPopover();
  popover.show();
  popover.focus();
  popover.webContents.send("signoff:popover-shown");
}

async function openSettings(tab = "repositories") {
  const selectedTab = SETTINGS_TABS.has(tab) ? tab : "repositories";
  if (!settings) {
    settings = new BrowserWindow({
      width: 960,
      height: 720,
      minWidth: 820,
      minHeight: 620,
      show: false,
      title: "Signoff Settings",
      titleBarStyle: "hiddenInset",
      backgroundColor: "#F7F7F5",
      webPreferences: {
        preload: path.join(__dirname, "preload.cjs"),
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
      },
    });
    secureWindow(settings);
    settings.on("closed", () => { settings = null; });
  }
  const target = settings;
  await target.loadURL(serviceURL("settings", selectedTab));
  if (target.isDestroyed()) return;
  target.show();
  target.focus();
  if (popover) popover.hide();
}

function trustedGitHubURL(value) {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.hostname === "github.com";
  } catch {
    return false;
  }
}

async function refreshTrayStatus() {
  if (!backendInfo || !tray || trayRefreshInFlight) return;
  trayRefreshInFlight = true;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30_000);
  try {
    const response = await fetch(new URL("/api/product", backendInfo.url), {
      headers: { "X-Signoff-Token": backendInfo.token },
      signal: controller.signal,
    });
    if (!response.ok) return;
    const product = await response.json();
    currentStatus = product.summary?.status || "neutral";
    const count = product.summary?.needsAttention || 0;
    tray.setImage(trayIcon(currentStatus));
    tray.setToolTip(count ? `Signoff — ${count} pull request${count === 1 ? "" : "s"} need attention` : "Signoff — all clear");
  } catch {
    tray.setImage(trayIcon("neutral"));
    tray.setToolTip("Signoff — local service unavailable");
  } finally {
    clearTimeout(timeout);
    trayRefreshInFlight = false;
  }
}

function createTray() {
  tray = new Tray(trayIcon(currentStatus));
  tray.setToolTip("Signoff");
  tray.on("click", togglePopover);
  tray.on("right-click", () => {
    const menu = Menu.buildFromTemplate([
      { label: "Open Signoff", click: togglePopover },
      { label: "Refresh", click: async () => {
        try {
          await fetch(new URL("/api/refresh", backendInfo.url), { method: "POST", headers: { "X-Signoff-Token": backendInfo.token } });
          await refreshTrayStatus();
          if (popover) popover.webContents.send("signoff:popover-shown");
        } catch { /* The next poll will expose service availability. */ }
      } },
      { type: "separator" },
      { label: "Settings…", click: () => openSettings("repositories") },
      { type: "separator" },
      { label: "Quit Signoff", role: "quit" },
    ]);
    tray.popUpContextMenu(menu);
  });
  void refreshTrayStatus();
  statusTimer = setInterval(refreshTrayStatus, 30000);
}

function registerIPC() {
  ipcMain.handle("signoff:choose-repository", async () => {
    const owner = settings || popover || undefined;
    const result = await dialog.showOpenDialog(owner, {
      title: "Choose a Git repository",
      message: "Signoff will track pull requests from this local checkout.",
      properties: ["openDirectory", "createDirectory"],
    });
    return result.canceled ? null : result.filePaths[0];
  });
  ipcMain.handle("signoff:open-settings", (_event, tab) => openSettings(tab));
  ipcMain.handle("signoff:open-external", async (_event, url) => {
    if (!trustedGitHubURL(url)) throw new Error("Only github.com links can be opened.");
    await shell.openExternal(url);
  });
  ipcMain.handle("signoff:copy-text", (_event, value) => {
    if (typeof value !== "string" || value.length > 256 || !value.startsWith("gh signoff")) {
      throw new Error("Only a Signoff command can be copied.");
    }
    clipboard.writeText(value);
  });
  ipcMain.on("signoff:hide-popover", () => { if (popover) popover.hide(); });
}

app.on("second-instance", () => {
  if (!backendInfo) return;
  if (settings?.isVisible()) settings.focus();
  else showPopover();
});

app.whenReady().then(async () => {
  if (!ownsInstanceLock) return;
  if (process.platform === "darwin") app.dock.hide();
  registerIPC();
  try {
    await startBackend();
    createPopover();
    createTray();
  } catch (error) {
    dialog.showErrorBox("Signoff could not start", error.message);
    app.quit();
  }
});

app.on("activate", () => {
  if (backendInfo) togglePopover();
});

app.on("window-all-closed", () => {
  // A menu-bar app stays alive after its secondary windows close.
});

app.on("before-quit", () => {
  quitting = true;
  if (statusTimer) clearInterval(statusTimer);
  if (backend) backend.kill("SIGTERM");
});
