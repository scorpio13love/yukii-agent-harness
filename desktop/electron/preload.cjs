// 只暴露最小只读信息；CUA 能力必须经未来 Rust sidecar 的窄 IPC，不能从这里直达原生桥。
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld(
  'yukiiDesktop',
  Object.freeze({
    platform: process.platform,
    shell: 'electron',
    openCuaPanel: () => ipcRenderer.invoke('cua:open-panel'),
    selectWorkspace: () => ipcRenderer.invoke('workspace:select'),
    openWorkspace: (target) => ipcRenderer.invoke('workspace:open', target),
    openInCursor: (target) => ipcRenderer.invoke('workspace:open-in-cursor', target),
    readClipboard: () => ipcRenderer.invoke('clipboard:read'),
    writeClipboard: (text) => ipcRenderer.invoke('clipboard:write', text),
    onScheduleReminder: (handler) => {
      const listener = (_event, payload) => handler(payload);
      ipcRenderer.on('schedule:reminder', listener);
      return () => ipcRenderer.removeListener('schedule:reminder', listener);
    },
    localTask: {
      capabilities: () => ipcRenderer.invoke('desktop:task:capabilities'),
      apps: () => ipcRenderer.invoke('desktop:task:apps'),
      tasks: () => ipcRenderer.invoke('desktop:task:list'),
      conversation: (id) => ipcRenderer.invoke('desktop:task:conversation', id),
      task: (id) => ipcRenderer.invoke('desktop:task:get', id),
      approveTask: (id) => ipcRenderer.invoke('desktop:task:approve', id),
      denyTask: (id) => ipcRenderer.invoke('desktop:task:deny', id),
      events: (id) => ipcRenderer.invoke('desktop:task:events', id),
      createTask: (prompt, idempotencyKey, conversationId, appName) =>
        ipcRenderer.invoke('desktop:task:create', prompt, idempotencyKey, conversationId, appName),
      cancelTask: (id) => ipcRenderer.invoke('desktop:task:cancel', id),
      runTask: (id) => ipcRenderer.invoke('desktop:task:run', id),
    },
  }),
);
