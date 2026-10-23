// Yukii Electron shell.
// 主窗口加载 React 工作台；CUA 只暴露给本地受信侧栏/开发控制台。
const {
  app,
  BrowserWindow,
  Menu,
  WebContentsView,
  ipcMain,
  shell,
  Notification,
  dialog,
  clipboard,
} = require('electron');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { CuaSidecarClient } = require('./cua-client.cjs');
const { LocalTaskRunner } = require('./local-task.cjs');
const { initLog, safeLog } = require('./log.cjs');

// 原生桥据此区分自身窗口；开发态默认会显示成 Electron。
app.setName('Yukii');

const WEB_URL =
  process.env.YUKII_WEB_URL || (app.isPackaged ? 'http://127.0.0.1:8765' : 'http://127.0.0.1:5173');
// 开发/验收控制台不自动出现；产品入口是主窗口右侧的 Computer Use 侧栏。
const DEV_CUA_CONSOLE = process.env.YUKII_DEV_CUA_CONSOLE === '1';
initLog(path.join(process.env.YUKII_USER_DATA || app.getPath('userData'), 'yukii.log'));

let mainWindow = null;
let controlWindow = null;
let productPanel = null;
let pendingTaskTarget = null;
let cua = null;
let localTasks = null;
let apiProcess = null;
const notifiedSchedules = new Set();
let scheduleWatcherPrimed = false;
const trustedCuaContents = new Set();

async function waitForApi(timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch('http://127.0.0.1:8765/api/ready');
      if (response.ok) return true;
    } catch {
      // API 尚未就绪。
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return false;
}

async function watchScheduleReminders() {
  try {
    const response = await fetch('http://127.0.0.1:8765/api/schedules');
    if (!response.ok) return;
    const { schedules = [] } = await response.json();
    if (!scheduleWatcherPrimed) {
      for (const schedule of schedules) {
        if (schedule.task_id) notifiedSchedules.add(schedule.task_id);
      }
      scheduleWatcherPrimed = true;
      return;
    }
    for (const schedule of schedules) {
      if (!schedule.task_id || notifiedSchedules.has(schedule.task_id)) continue;
      const taskResponse = await fetch(`http://127.0.0.1:8765/api/tasks/${schedule.task_id}`);
      if (!taskResponse.ok) continue;
      const task = await taskResponse.json();
      if (!['completed', 'failed', 'cancelled'].includes(task.status)) continue;
      notifiedSchedules.add(schedule.task_id);
      const body = task.result || task.error || schedule.prompt;
      if (Notification.isSupported()) {
        new Notification({ title: 'Yukii 提醒', body }).show();
      }
      if (mainWindow && !mainWindow.isDestroyed()) {
        mainWindow.webContents.send('schedule:reminder', {
          sessionId: schedule.conversation_id,
          taskId: schedule.task_id,
          prompt: schedule.prompt,
          body,
        });
      }
      safeLog('计划任务完成通知：', schedule.prompt, '→', body);
    }
  } catch (error) {
    safeLog('计划任务轮询失败：', error.message);
  }
}

// 打包态由 Electron 启动内置 Rust API；开发态继续使用外部服务。
async function ensureApiServer() {
  if (!app.isPackaged || process.env.YUKII_API_URL) return;
  if (await waitForApi(800)) return;
  const binary = path.join(process.resourcesPath, 'bin', 'yukii-api-server');
  if (!fs.existsSync(binary)) {
    safeLog('找不到内置 API server：', binary);
    return;
  }
  const dataDir = path.join(app.getPath('userData'), 'agent');
  const webDist = path.join(process.resourcesPath, 'web');
  apiProcess = spawn(binary, [], {
    cwd: app.getPath('userData'),
    env: {
      ...process.env,
      YUKII_DATA_DIR: dataDir,
      YUKII_WEB_DIST: webDist,
      YUKII_UPLOADS_DIR: path.join(dataDir, 'uploads'),
    },
    stdio: 'ignore',
  });
  apiProcess.on('exit', (code, signal) => {
    safeLog(`内置 API 退出 code=${code} signal=${signal}`);
    apiProcess = null;
  });
  if (!(await waitForApi())) safeLog('内置 API 启动超时');
}

function createMainWindow() {
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 900,
    minWidth: 1024,
    minHeight: 720,
    title: 'Yukii',
    backgroundColor: '#0f172a',
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  mainWindow.loadURL(WEB_URL).catch((error) => {
    safeLog('无法加载 Yukii 工作台：', error.message);
    mainWindow.loadFile(path.join(__dirname, 'offline.html')).catch((offlineError) => {
      safeLog('无法加载离线页：', offlineError.message);
    });
  });
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    shell.openExternal(url);
    return { action: 'deny' };
  });
  // localTask 只应暴露给受信本地工作台；拒绝导航到外部 origin。
  mainWindow.webContents.on('will-navigate', (event, url) => {
    try {
      if (new URL(url).origin !== new URL(WEB_URL).origin) event.preventDefault();
    } catch {
      event.preventDefault();
    }
  });
  mainWindow.on('resize', layoutProductPanel);
  mainWindow.on('closed', () => {
    if (productPanel) {
      trustedCuaContents.delete(productPanel.webContents);
      productPanel = null;
    }
    mainWindow = null;
  });
}

// 开发/验收控制台：默认不打开，只在 Developer 菜单或 YUKII_DEV_CUA_CONSOLE=1 时出现。
function createControlWindow() {
  if (controlWindow) {
    controlWindow.focus();
    return;
  }
  controlWindow = new BrowserWindow({
    width: 1120,
    height: 940,
    minWidth: 900,
    minHeight: 700,
    title: 'Yukii CUA Console',
    backgroundColor: '#0b1120',
    webPreferences: {
      preload: path.join(__dirname, 'control-preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  trustedCuaContents.add(controlWindow.webContents);
  const contents = controlWindow.webContents;
  controlWindow.loadFile(path.join(__dirname, 'control', 'control.html'));
  controlWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  controlWindow.on('closed', () => {
    trustedCuaContents.delete(contents);
    controlWindow = null;
  });
}

// 产品侧栏：本地受信 WebContentsView，嵌在主窗口右侧，不是网页工作台的一部分。
function ensureProductPanel() {
  if (!mainWindow) return null;
  if (productPanel) return productPanel;
  productPanel = new WebContentsView({
    webPreferences: {
      preload: path.join(__dirname, 'panel-preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  trustedCuaContents.add(productPanel.webContents);
  productPanel.webContents.loadFile(path.join(__dirname, 'product', 'panel.html'));
  productPanel.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  productPanel.webContents.once('did-finish-load', () => {
    if (pendingTaskTarget) {
      productPanel.webContents.send('cua:task-target', pendingTaskTarget);
      pendingTaskTarget = null;
    }
  });
  mainWindow.contentView.addChildView(productPanel);
  productPanel.setVisible(false);
  layoutProductPanel();
  return productPanel;
}

function layoutProductPanel() {
  if (!productPanel || !mainWindow) return;
  const [width, height] = mainWindow.getContentSize();
  const panelWidth = Math.min(420, Math.max(340, Math.round(width * 0.3)));
  productPanel.setBounds({ x: width - panelWidth, y: 0, width: panelWidth, height });
}

function toggleProductPanel() {
  const panel = ensureProductPanel();
  if (!panel) return;
  const visible = !panel.getVisible();
  panel.setVisible(visible);
  if (visible) {
    layoutProductPanel();
    panel.webContents.focus();
  }
}

function hideProductPanel() {
  if (productPanel) productPanel.setVisible(false);
}

function trustedControl(event) {
  return trustedCuaContents.has(event.sender);
}

function showTaskTarget(task, target) {
  // 首次使用某应用时，权限提示在聊天里完成；侧栏等允许后再打开。
  if (task.awaiting_approval) {
    pendingTaskTarget = { task, target };
    return;
  }
  const panel = ensureProductPanel();
  if (!panel) return;
  panel.setVisible(true);
  layoutProductPanel();
  pendingTaskTarget = { task, target };
  if (panel.webContents.isLoading()) return;
  panel.webContents.send('cua:task-target', pendingTaskTarget);
  pendingTaskTarget = null;
}

function updateTaskPanel(task) {
  if (pendingTaskTarget && pendingTaskTarget.task.id === task.id) {
    if (['cancelled', 'failed'].includes(task.status)) {
      pendingTaskTarget = null;
    } else if (!task.awaiting_approval) {
      const target = pendingTaskTarget.target;
      pendingTaskTarget = null;
      showTaskTarget(task, target);
    }
  }
  if (productPanel && !productPanel.webContents.isDestroyed()) {
    productPanel.webContents.send('cua:task-update', task);
  }
}

ipcMain.handle('cua:request', async (event, command, payload = {}, timeoutMs = 60_000) => {
  if (!trustedControl(event)) {
    throw new Error('CUA API 只在受信控制台/侧栏可用');
  }
  if (!cua) {
    throw new Error('CUA sidecar 尚未启动');
  }
  const boundedTimeout = Math.min(Math.max(Number(timeoutMs) || 60_000, 1_000), 310_000);
  return cua.call(command, payload, boundedTimeout);
});

function trustedMain(event) {
  return Boolean(mainWindow) && event.sender === mainWindow.webContents;
}

ipcMain.handle('desktop:task:capabilities', (event) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.capabilities();
});

ipcMain.handle('desktop:task:apps', async (event) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.listApps();
});

ipcMain.handle('desktop:task:list', (event) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  return localTasks ? localTasks.list() : [];
});

ipcMain.handle('desktop:task:conversation', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  return localTasks ? localTasks.conversation(String(id)) : [];
});

ipcMain.handle('desktop:task:get', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  return localTasks ? localTasks.get(String(id)) : null;
});

ipcMain.handle('desktop:task:events', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  return localTasks ? localTasks.taskEvents(String(id)) : [];
});

ipcMain.handle('desktop:task:create', (event, prompt, idempotencyKey, conversationId, appName) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.create(prompt, idempotencyKey, conversationId, appName);
});

ipcMain.handle('cua:task:cancel', (event, id) => {
  if (!trustedControl(event)) throw new Error('只有受信 CUA 侧栏可以停止本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.cancel(String(id));
});

ipcMain.handle('desktop:task:approve', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以允许本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.approve(String(id));
});

ipcMain.handle('desktop:task:deny', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以拒绝本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.deny(String(id));
});

ipcMain.handle('cua:task:approve', (event, id) => {
  if (!trustedControl(event)) throw new Error('只有受信 CUA 侧栏可以允许本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.approve(String(id));
});

ipcMain.handle('cua:task:pause', (event, id) => {
  if (!trustedControl(event)) throw new Error('只有受信 CUA 侧栏可以暂停本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.pause(String(id));
});

ipcMain.handle('cua:task:resume', (event, id) => {
  if (!trustedControl(event)) throw new Error('只有受信 CUA 侧栏可以继续本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.resume(String(id));
});

ipcMain.handle('desktop:task:cancel', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.cancel(String(id));
});

ipcMain.handle('desktop:task:run', (event, id) => {
  if (!trustedMain(event)) throw new Error('只有主工作台可以调用本地任务');
  if (!localTasks) throw new Error('本地任务运行时尚未启动');
  return localTasks.run(String(id));
});

ipcMain.handle('cua:open-panel', (event) => {
  if (!mainWindow || event.sender !== mainWindow.webContents) {
    throw new Error('只有主工作台可以打开 CUA 侧栏');
  }
  toggleProductPanel();
  return true;
});

ipcMain.on('cua:panel-close', (event) => {
  if (productPanel && event.sender === productPanel.webContents) hideProductPanel();
});

ipcMain.handle('clipboard:read', () => clipboard.readText());
ipcMain.handle('clipboard:write', (_event, text) => {
  clipboard.writeText(String(text ?? ''));
});

ipcMain.handle('workspace:select', async () => {
  const result = await dialog.showOpenDialog(mainWindow, {
    properties: ['openDirectory'],
    title: '选择 Session 工作区',
  });
  return result.canceled ? null : (result.filePaths[0] ?? null);
});

ipcMain.handle('workspace:open', async (_event, target) => {
  const path = String(target ?? '').trim();
  if (!path) return '工作区路径为空';
  const error = await shell.openPath(path);
  return error || null;
});

ipcMain.handle('workspace:open-in-cursor', async (_event, target) => {
  const path = String(target ?? '').trim();
  if (!path) return '文件路径为空';
  if (process.platform === 'darwin') {
    return await new Promise((resolve) => {
      const child = spawn('open', ['-a', 'Cursor', path], { stdio: 'ignore' });
      child.on('error', () => resolve('无法启动 Cursor，请确认已安装。'));
      child.on('exit', (code) => resolve(code === 0 ? null : '无法启动 Cursor，请确认已安装。'));
    });
  }
  try {
    const child = spawn('cursor', [path], { detached: true, stdio: 'ignore' });
    child.unref();
    return null;
  } catch {
    return '无法启动 Cursor，请确认 cursor 命令在 PATH 中。';
  }
});

app.whenReady().then(async () => {
  await ensureApiServer();
  setInterval(() => void watchScheduleReminders(), 5000);
  try {
    cua = new CuaSidecarClient({
      dataDir: process.env.YUKII_CUA_DATA || path.join(app.getPath('userData'), 'cua'),
      executor: process.env.YUKII_CUA_EXECUTOR,
      bridge: process.env.YUKII_CUA_BRIDGE,
      sidecarPath:
        process.env.YUKII_CUA_SIDECAR ||
        (app.isPackaged ? path.join(process.resourcesPath, 'bin', 'yukii-cua-sidecar') : undefined),
      logger: safeLog,
    });
    cua.start();
    cua.onEvent((event) => {
      if (productPanel && !productPanel.webContents.isDestroyed()) {
        productPanel.webContents.send('cua:preview-event', event);
      }
    });
  } catch (error) {
    safeLog('CUA sidecar 启动失败：', error);
    cua = null;
  }
  localTasks = new LocalTaskRunner({
    repoRoot: path.join(__dirname, '..', '..'),
    // 开发态放在工作区，避免沙箱/权限问题；打包态仍使用应用 userData。
    dataDir: app.isPackaged
      ? path.join(app.getPath('userData'), 'local-tasks')
      : path.join(__dirname, '..', '..', '.data', 'local-tasks'),
    logger: safeLog,
    onTarget: (task, target) => showTaskTarget(task, target),
    onUpdate: (task) => updateTaskPanel(task),
  });
  createMainWindow();
  installMenu();
  if (DEV_CUA_CONSOLE) createControlWindow();
  if (process.env.YUKII_CUA_PANEL === '1') toggleProductPanel();
  app.on('activate', () => {
    if (!mainWindow) createMainWindow();
  });
});

function installMenu() {
  const template = [
    ...(process.platform === 'darwin' ? [{ role: 'appMenu' }] : []),
    { label: 'File', submenu: [{ role: 'quit' }] },
    { role: 'editMenu' },
    {
      label: 'Computer Use',
      submenu: [
        {
          label: 'Show/Hide Computer Use',
          accelerator: 'CmdOrCtrl+Shift+U',
          click: () => toggleProductPanel(),
        },
      ],
    },
    {
      label: 'Developer',
      submenu: [
        {
          label: 'Open CUA Console',
          accelerator: 'CmdOrCtrl+Shift+C',
          click: () => createControlWindow(),
        },
      ],
    },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

app.on('before-quit', () => {
  if (cua) cua.close();
  if (localTasks) localTasks.dispose();
  if (apiProcess) {
    apiProcess.kill('SIGTERM');
    apiProcess = null;
  }
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit();
});
