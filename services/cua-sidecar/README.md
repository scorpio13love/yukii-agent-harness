# Yukii CUA sidecar

受信宿主（Electron main、CLI）与受控执行器之间的窄 stdio 协议。它**不运行模型**、不监听网络，只暴露 CUA 原语。

## 启动

```bash
./tooling/cargo.sh build -p yukii-cua-sidecar -p yukii-desktop-executor-service
target/debug/yukii-cua-sidecar --data-dir target/native/cua-sidecar-data
```

可指定 `--executor`、`--bridge`、`--window-picker`、`--session-minutes`。默认使用同目录 executor、`target/native/computer-bridge` 和 `target/native/window-picker`。

## 协议

stdin/stdout 均为一行一个 JSON：

```json
{"id":"1","command":"hello"}
{"id":"2","command":"windows"}
{"id":"3","command":"begin","window_id":123,"pid":456,"application":"TextEdit","title":"doc.txt"}
{"id":"4","command":"observe"}
{"id":"5","command":"act","observation_id":"...","action":{"type":"type_element","element_id":"ax-0.3","text":"hello","clear":true}}
{"id":"6","command":"end"}
```

响应：

```json
{
  "id": "1",
  "ok": true,
  "result": {
    "protocol": 1,
    "platform": "macos",
    "capabilities": [
      "permissions",
      "windows",
      "picker",
      "control",
      "elements",
      "coordinate_fallback"
    ]
  }
}
```

命令：

| 命令                       | 语义                                                  |
| -------------------------- | ----------------------------------------------------- |
| `hello`                    | 协议/平台/能力                                        |
| `permissions`              | 屏幕录制/辅助功能状态                                 |
| `windows`                  | 可控制窗口                                            |
| `apps`                     | 本机应用列表（bundle id/前台窗口）                    |
| `allowed_apps`             | 已允许应用                                            |
| `allow_app` / `revoke_app` | 添加/撤销应用级授权，持久化到 data-dir                |
| `begin_app`                | 对已允许应用自动定位前台窗口并开始控制                |
| `pick`                     | 打开原生窗口选择器，返回 window_id/pid                |
| `begin`                    | 按窗口建立控制会话（调试/兼容）                       |
| `observe`                  | 截图 + AX 元素树                                      |
| `act`                      | `GuiAction`，包括 click_element/type_element/坐标回退 |
| `pause` / `resume`         | 暂停/恢复控制                                         |
| `end`                      | 结束控制、取消审计任务                                |
| `status`                   | 当前任务/窗口                                         |
| `shutdown`                 | 结束并退出                                            |

## 安全边界

- sidecar 只由受信父进程通过管道启动；不能从网页/远程访问。
- `allow_app` 是应用级同意，写入 `allowed_apps.json`；`begin_app` 只接受已允许应用，自动定位该应用前台窗口。
- 当前 sidecar 使用 `AllowAllActions`，因为调用方必须在产品侧栏中完成应用授权。
- Electron renderer **尚未暴露** sidecar API；下一步只应通过 Electron main 的窄 IPC，并放在受信控制台，不放到普通网页工作台。
- 所有动作仍经过执行器控制会话、观察绑定和 SQLite 审计；sidecar 自己不做动作。
- 原生桥路径、窗口 ID 和任意命令不能由网页内容提供。

## 当前状态

- macOS：端到端验证（hello/apps/allow_app/begin_app/observe/type_element/click_element/end）。
- 应用级 allowlist 持久化在 `<data-dir>/allowed_apps.json`；产品侧栏已改为应用选择。
- 产品侧栏已改用 `preview_start` 的 SCStream 事件（约 8fps）；`preview` 保留为单帧回退。
- 还没有 Any App / Locked use；多窗口应用默认取该应用第一个窗口，后续需按前台窗口细化。
- Electron main 尚未接入；预览帧/暂停停止 UI 仍由 AppKit 临时面板承载。
- `pick` 使用 AppKit 窗口选择器；Electron 控制台的工作是把选择/预览/控制搬到 React。
