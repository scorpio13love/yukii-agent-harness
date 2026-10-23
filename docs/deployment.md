# 部署与使用

本文说明如何在本地运行 Yukii Desktop，以及日常使用的基本流程。

---

## 1. 环境要求

- macOS（Computer Use 当前只在 macOS 完整验证）
- Node.js 22+
- pnpm 11+
- Rust 2024 edition 工具链
- 本机 C 编译器（SQLite）
- Docker（可选，仅 bash/python 沙箱工具需要）

---

## 2. 一键启动

```bash
./tooling/start-local.sh
```

脚本会：

1. 检查 `node`、`pnpm`、`cargo`；
2. 缺少 `.env` 时从 `.env.example` 复制；
3. 安装依赖并构建前端；
4. 构建 API、CUA sidecar 和桌面 Executor；
5. 启动 API（`127.0.0.1:8765`）；
6. 启动 Vite（`127.0.0.1:5173`）；
7. 启动 Electron。

退出 Electron 或 Ctrl+C 时，脚本会停止 API 和 Vite。

只检查环境：

```bash
./tooling/start-local.sh --check
```

## 3. 手动启动

```bash
pnpm install

# 终端 A
./tooling/cargo.sh run -p yukii-api-server

# 终端 B
pnpm dev
```

打开 `http://127.0.0.1:5173`。

Electron：

```bash
cd desktop/electron
pnpm start
```

---

## 4. 配置

复制 `.env.example` 为 `.env`，至少配置模型：

```env
YUKII_API_FORMAT=openai
YUKII_API_BASE_URL=https://api.example.com/v1
YUKII_API_KEY=
YUKII_MODEL=
```

说明：

- `YUKII_API_FORMAT` 支持 `openai`（Chat Completions 兼容）和 `anthropic`（Messages 兼容）。
- `YUKII_API_BASE_URL` 填完整版本前缀，程序会追加对应请求路径；建议显式配置实际使用的模型服务地址。
- 模型未配置时任务只保存为 queued，不会伪造执行。
- `.env` 已被 git 忽略；不要把密钥写进 README、文档或提交记录。
- Computer Use 需要模型支持图像输入，并设置 `YUKII_VISION_INPUT=true`。
- 可选配置见 `.env.example` 中的注释。

---

## 5. 数据目录

默认：

```text
.data/tasks.sqlite                 # 索引、队列、搜索结果
.data/sessions/<id>/session.jsonl  # Session 历史
.data/sessions/<id>/images/        # 图片缓存
.data/memory.json                  # 长期记忆
```

可用 `YUKII_DATA_DIR` 改变位置。

备份时至少覆盖：

```text
.data/tasks.sqlite
.data/sessions/
.data/memory.json
```

不要把 `.data/`、`.env`、运行日志和生成的文件提交到仓库。

### 工作区

未绑定项目时，新会话会自动创建 `~/Yukii/chats/<日期>/<session-id>` 作为默认工作区。左侧“项目”按项目路径分组，同一路径可以有多个 Session；“最近”显示使用默认会话目录的会话。右侧工作区面板可以打开文件夹、绑定项目目录或恢复会话目录。

可用 `YUKII_WORKSPACE_ROOT` 改变默认工作区根目录。

工作区行 hover 的 `+` 会在同一项目下新建对话；第一条消息发送时绑定到该路径。项目内会话不再重复显示“绑定项目”操作。

Office 产物会同时复制到当前会话工作区根目录，并在“对话产物”中保留下载入口。

`write_file` / `edit_file` 以及 `bash` / `python` 造成的文件新增或修改会记录原始内容、最新内容与行级 diff，在回复下方显示“已编辑 N 个文件”；点击文件打开右侧 diff 查看器，右上角按钮可在 Cursor 中打开绝对路径。实现不依赖工作区是否初始化 git，使用会话内保存的文件快照生成 diff；删除文件和二进制文件暂不展示。

### Agent 模式

- 输入框只暴露 `/goal` 和 `/plan` 两个命令；暂停、恢复、编辑、清除等控制放在目标条/更多菜单。
- `/goal <目标>`：建立长期目标；同一 Session 在每轮完成后自动进入下一轮，最多 6 轮。目标会固定显示在输入框上方，模型验证完成后调用 `update_goal action=complete`。
- `/plan`：进入计划模式，先探索和设计方案，不执行有副作用的修改；模型用 `update_plan_steps` 维护步骤并调用 `exit_plan_mode` 提交评审。评审显示为消息流中的临时卡片，批准后才开始执行。步骤状态固定在输入框上方，全部完成后自动隐藏；计划条上的“退出计划”可关闭计划。
- 模型也可以在对话中自行调用 `create_goal`、`enter_plan_mode`、`update_plan_steps` 和 `exit_plan_mode`，无需用户输入斜杠命令。

### 工作台布局

左栏、中间、右栏三列，左/右边界可拖拽并记忆宽度。右栏在“工作摘要 / 文件 diff / 轨迹详情”之间互斥切换；diff 和轨迹详情使用右栏宽度，不覆盖中间对话。

---

## 6. macOS 权限

Computer Use 首次使用需要给 Electron/Yukii 授予：

- 屏幕录制；
- 辅助功能。

授权后完全退出并重开 Electron。

原生桥可以检查权限：

```bash
echo '{"operation":"permissions"}' | target/native/computer-bridge
```

预期返回两个 `true`。未授权时宿主会拒绝启动，不会伪造画面或成功状态。

---

## 7. 日常使用

### 文档任务

1. 创建工作区或上传附件；
2. 输入目标，例如“把这份资料整理成周报 PPT”；
3. 在轨迹中查看工具调用；
4. 下载生成的 DOCX/XLSX/PPTX/PDF，人工检查后再使用。

### 浏览器任务

浏览器工具通过 CDP 操作专用 Chrome profile 或显式 CDP 端点。

- 页面内容是不可信数据；
- 不要用它访问网银、邮箱或输入密码/验证码；
- 只操作当前 page target，弹窗/下载/文件选择器未完全处理。

### 桌面任务

1. 打开 Computer Use 侧栏；
2. 选择目标应用并授权；
3. 输入任务；
4. 首次授权后，控制期间可以暂停或停止。

高风险窗口建议保留逐动作确认。

### 知识库

支持导入本地文档，使用 BM25 + 可选向量检索，并在回答中返回引用编号。

检索结果不足时会返回 warning，不会伪造引用。

### 子 Agent

Agent 可以创建独立子 Session 并行处理子任务：

- 子 Session 有独立历史；
- 可以发送后续消息；
- 可以中断当前 turn；
- 完成后会向父 Session 发送通知。

子 Agent 结果仍需要人工复核。

---

## 8. 可选：本地未签名 App

```bash
pnpm package:mac:unsigned
```

产物：

```text
dist/macos/mac-arm64/Yukii.app
dist/macos/Yukii-0.0.0-arm64.zip
```

这是 ad-hoc 本地包，不是公开发行版。Developer ID 签名、公证和 DMG 分发不在本阶段范围。

---

## 9. 安全边界

- API 只监听回环地址，当前没有账号、租户和公网认证。
- 不要把服务暴露到公网，也不要用于多租户部署。
- Docker 沙箱默认禁网；容器工具不能访问外部网络。
- CUA 只操作用户显式授权并选择的应用窗口。
- 模型输出和生成文件都要人工验收。

---

## 10. 常见问题

### 任务一直 queued

检查 `.env` 的模型配置和 API 地址；未配置模型时不会执行。

### Computer Use 启动失败

检查系统设置中的屏幕录制/辅助功能授权，并完全退出重开应用。

### bash/python 工具不可用

检查 Docker 是否安装并运行；也可以关闭容器工具，不影响文档和知识库。

### 生成的 Office 文件格式不理想

复杂模板仍需要人工调整；当前不保证所有企业模板 100% 保真。

### 端口被占用

API 默认 `8765`，Vite 默认 `5173`；先停止旧进程，或按启动脚本提示调整。
