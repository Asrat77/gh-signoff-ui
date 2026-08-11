"use strict";

const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("signoffDesktop", {
  chooseRepository: () => ipcRenderer.invoke("signoff:choose-repository"),
  openSettings: (tab) => ipcRenderer.invoke("signoff:open-settings", tab),
  openExternal: (url) => ipcRenderer.invoke("signoff:open-external", url),
  copyText: (value) => ipcRenderer.invoke("signoff:copy-text", value),
  hidePopover: () => ipcRenderer.send("signoff:hide-popover"),
  onPopoverShown: (callback) => {
    const handler = () => callback();
    ipcRenderer.on("signoff:popover-shown", handler);
    return () => ipcRenderer.removeListener("signoff:popover-shown", handler);
  },
});
