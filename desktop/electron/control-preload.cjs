// 只加载到受信控制台窗口；不加载到普通网页工作台。
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld(
  'yukiiCua',
  Object.freeze({
    request: (command, payload = {}, timeoutMs = 60_000) =>
      ipcRenderer.invoke('cua:request', command, payload, timeoutMs),
  }),
);
