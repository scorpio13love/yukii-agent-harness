# Yukii Electron 壳（本地控制台骨架）

这是多端桌面壳的起步代码，不是完整产品：

- 主窗口加载现有 React 工作台（开发默认 `http://127.0.0.1:5173`，可用 `YUKII_WEB_URL` 覆盖；打包后为 `8765`）。工作台有显式“办公任务 / 本机操作”模式：默认办公任务走 HTTP API，只有本机操作模式通过 `localTask` 窄 IPC 调用 Rust desktop-host；工作台侧栏有 Computer Use 按钮，只调用 `openCuaPanel` 打开受信侧栏；
- 主窗口右侧的产品级 **Computer Use 侧栏**（`product/`）通过独立 `panel-preload.cjs` 调用 sidecar 窄 IPC；用户选择**应用**、首次允许后记住，只看到当前画面和暂停/停止，不看到窗口列表/element_id；
- 本地受信 CUA Console（`control/`）是开发/验收工具，**默认不自动打开**；
- 产品侧栏用菜单 `Computer Use → Show/Hide Computer Use`（`Cmd/Ctrl+Shift+U`）或 `YUKII_CUA_PANEL=1` 打开；侧栏会列出本机应用，首次允许后持久化到 sidecar 的 `allowed_apps.json`。控制期间由 `preview-stream`（SCStream，约 8fps）推送 JPEG 帧，不消耗 Agent observation。调试台用 `Developer → Open CUA Console`（`Cmd/Ctrl+Shift+C`）或 `YUKII_DEV_CUA_CONSOLE=1`。
- 普通网页主窗口只有最小 `preload.cjs`，**不能调用 CUA**；
- `contextIsolation`、`sandbox` 开启，renderer 无 Node、无本机执行；
- CUA Console 已接入 permissions/windows/pick/begin/observe/act/pause/resume/end；普通用户流程应使用主窗口内的紧凑控制面板，而不是这个调试台；
- SCStream 实时预览已接入；签名/公证/DMG 尚未接入。

## 安装与启动

`desktop/electron` 已加入根 pnpm workspace，先在仓库根安装依赖：

```bash
cd <repo>
pnpm install
./tooling/cargo.sh build -p yukii-cua-sidecar -p yukii-desktop-executor-service

# 终端 1：API
./tooling/cargo.sh run -p yukii-api-server

# 终端 2：React/Vite 工作台
pnpm dev

# 终端 3：Electron
cd desktop/electron
pnpm start
```

开发默认加载 `http://127.0.0.1:5173`。CUA 侧栏不依赖 Vite，主窗口离线时仍可用菜单 `Computer Use` 打开。

`start.cjs` 会清掉 `ELECTRON_RUN_AS_NODE`，确保进入 Electron 主进程；直接 `electron .` 在受限执行环境中可能被当成 Node 运行，出现 `ipcMain` 为空。

主窗口需要 API/工作台；控制台窗口是本地文件，不依赖网页。若 Electron 没有屏幕录制/辅助功能权限，控制台会显示失败，不会伪造权限。

## 已知环境问题

- 从受限执行环境启动 Electron 时，Chromium 的 GPU/sandbox/缓存可能因 `~/Library` 权限失败；这属于运行环境限制，请在普通终端或已签名的 App 中试。
- Electron 运行时下载缓存若被沙箱拦截，可设置 `electron_config_cache` 到可写目录后重新安装。

## 常见启动问题

### `AddrInUse`（8765 / 5173）

说明已有旧 API/Vite 进程占着端口。先查再决定是否结束：

```bash
lsof -nP -iTCP:8765 -sTCP:LISTEN
lsof -nP -iTCP:5173 -sTCP:LISTEN
```

API 正常时应返回 `GET /api/health` 200；如果 TCP 在监听但空回复，说明旧进程卡死，结束该 PID 后重启 API。

### 主窗口显示离线页

- Vite 没启动：在仓库根运行 `pnpm dev`，访问 http://127.0.0.1:5173 确认 200；
- API 没启动：`./tooling/cargo.sh run -p yukii-api-server`；
- 旧 Electron 还缓存着错误 URL：完全退出后重新 `pnpm start`（开发默认 5173）。

### CUA 侧栏没出现

菜单 **Computer Use → Show/Hide Computer Use**，或 `Cmd/Ctrl+Shift+U`。侧栏是产品入口；`Developer → Open CUA Console` 才是调试台。
