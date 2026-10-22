// 产品级 CUA 侧栏专用 preload；只暴露 sidecar 请求和关闭侧栏。
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld(
  'yukiiCua',
  Object.freeze({
    request: (command, payload = {}, timeoutMs = 60_000) =>
      ipcRenderer.invoke('cua:request', command, payload, timeoutMs),
    close: () => ipcRenderer.send('cua:panel-close'),
    onEvent: (callback) => {
      const listener = (_event, payload) => callback(payload);
      ipcRenderer.on('cua:preview-event', listener);
      return () => ipcRenderer.removeListener('cua:preview-event', listener);
    },
    onTaskTarget: (callback) => {
      const listener = (_event, payload) => callback(payload);
      ipcRenderer.on('cua:task-target', listener);
      return () => ipcRenderer.removeListener('cua:task-target', listener);
    },
    onTaskUpdate: (callback) => {
      const listener = (_event, payload) => callback(payload);
      ipcRenderer.on('cua:task-update', listener);
      return () => ipcRenderer.removeListener('cua:task-update', listener);
    },
    taskApprove: (id) => ipcRenderer.invoke('cua:task:approve', id),
    taskPause: (id) => ipcRenderer.invoke('cua:task:pause', id),
    taskResume: (id) => ipcRenderer.invoke('cua:task:resume', id),
    taskCancel: (id) => ipcRenderer.invoke('cua:task:cancel', id),
  }),
);
