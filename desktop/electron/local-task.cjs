// 受信 Electron main 的本地任务 runner。
// 通过 desktop-host CLI 运行现有 Rust Agent/Computer Use 工具循环；renderer 只拿任务事件，不碰原生。
const { spawn } = require('node:child_process');
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const ACTIVE = new Set(['queued', 'planning', 'running', 'waiting_for_user', 'verifying']);
const TERMINAL = new Set(['completed', 'failed', 'cancelled']);

function nowSeconds() {
  return Math.floor(Date.now() / 1000);
}

class LocalTaskRunner {
  constructor({ repoRoot, dataDir, logger = () => {}, onTarget = () => {}, onUpdate = () => {} }) {
    this.repoRoot = repoRoot;
    this.dataDir = dataDir;
    this.log = logger;
    this.onTarget = onTarget;
    this.onUpdate = onUpdate;
    this.tasks = new Map();
    this.events = new Map();
    this.children = new Map();
    this.allowedPath = path.join(dataDir, 'allowed_apps.json');
    this.allowed = this.loadAllowed();
  }

  loadAllowed() {
    try {
      const value = JSON.parse(fs.readFileSync(this.allowedPath, 'utf8'));
      return value && typeof value.allowed === 'object' ? value.allowed : {};
    } catch {
      return {};
    }
  }

  saveAllowed() {
    try {
      fs.mkdirSync(this.dataDir, { recursive: true });
      fs.writeFileSync(this.allowedPath, JSON.stringify({ allowed: this.allowed }, null, 2));
    } catch (error) {
      this.log('[local-task] save allowed apps failed:', error);
    }
  }

  capabilities() {
    return {
      protocol_version: 2,
      chromium_major: 152,
      task_creation: true,
      agent_execution: true,
      local_computer: true,
      cloud_computer: false,
    };
  }

  list() {
    return [...this.tasks.values()]
      .filter((task) => task.target === 'local_computer')
      .sort((a, b) => b.created_at - a.created_at);
  }

  get(id) {
    return this.tasks.get(id) || null;
  }

  conversation(id) {
    return [...this.tasks.values()]
      .filter((task) => task.conversation_id === id)
      .sort((a, b) => a.created_at - b.created_at);
  }

  taskEvents(id) {
    return this.events.get(id) || [];
  }

  async create(prompt, idempotencyKey, conversationId, appName) {
    const text = String(prompt || '').trim();
    if (!text) throw new Error('任务内容不能为空');
    if (text.length > 8000) throw new Error('任务内容过长');
    // 同一提交标识重放时返回原任务，避免重复启动本地执行。
    const existing = [...this.tasks.values()].find(
      (task) => task.submission_key === idempotencyKey,
    );
    if (existing) return this.clone(existing);
    const active = this.list().find((task) => ACTIVE.has(task.status));
    if (active) throw new Error('已有本地任务在执行；请先等待或停止它。');
    const id = crypto.randomUUID();
    const conversation = conversationId || crypto.randomUUID();
    const task = {
      id,
      conversation_id: conversation,
      conversation_title: text.slice(0, 60),
      prompt: text,
      target: 'local_computer',
      status: 'queued',
      created_at: nowSeconds(),
      result: null,
      error: null,
      input_tokens: 0,
      output_tokens: 0,
      sources: [],
      submission_key: idempotencyKey,
      awaiting_approval: false,
    };
    this.tasks.set(id, task);
    this.events.set(id, []);
    const target = appName ? await this.resolveApp(appName) : await this.resolveAppFromPrompt(text);
    if (target) {
      task._target = target;
      task._appName = appName;
      task.cua_target = {
        app_id: target.app_id,
        application: target.application,
        title: target.title,
      };
      const key = target.app_id || `name:${target.application}`;
      if (this.allowed[key]) {
        task.status = 'queued';
        this.addEvent(task, 'queued', `应用 ${target.application} 已授权，开始执行。`);
        this.startProcess(task, appName, target);
        this.onTarget(this.clone(task), target);
        this.onUpdate(this.clone(task));
      } else {
        task.status = 'waiting_for_user';
        task.awaiting_approval = true;
        this.addEvent(task, 'waiting_for_user', `需要在对话中允许使用 ${target.application}。`);
        this.onTarget(this.clone(task), target);
        this.onUpdate(this.clone(task));
      }
    } else {
      this.addEvent(task, 'queued', '本地 Computer Use 任务已提交。');
      this.startProcess(task, appName, null);
      this.onUpdate(this.clone(task));
    }
    return this.clone(task);
  }

  async listApps() {
    const bridge = path.join(this.repoRoot, 'target/native/computer-bridge');
    return new Promise((resolve, reject) => {
      const child = spawn(bridge, [], { stdio: ['pipe', 'pipe', 'pipe'] });
      let stdout = '';
      let stderr = '';
      const timer = setTimeout(() => {
        child.kill('SIGKILL');
        reject(new Error('读取本机应用列表超时'));
      }, 5000);
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', (chunk) => (stdout += chunk));
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', (chunk) => (stderr += chunk));
      child.on('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on('close', () => {
        clearTimeout(timer);
        try {
          const value = JSON.parse(stdout);
          if (value.error) throw new Error(value.error);
          resolve(
            (value.apps || []).filter(
              (app) =>
                app.application !== 'window-picker' &&
                app.application !== 'preview-panel' &&
                app.pid !== process.pid &&
                !String(app.title || '').includes('Yukii · 办公助手'),
            ),
          );
        } catch (error) {
          reject(new Error(stderr.trim() || error.message));
        }
      });
      child.stdin.end(JSON.stringify({ operation: 'apps' }));
    });
  }

  async bridge(request) {
    const bridge = path.join(this.repoRoot, 'target/native/computer-bridge');
    return new Promise((resolve, reject) => {
      const child = spawn(bridge, [], { stdio: ['pipe', 'pipe', 'pipe'] });
      let stdout = '';
      let stderr = '';
      const timer = setTimeout(() => {
        child.kill('SIGKILL');
        reject(new Error('原生桥响应超时'));
      }, 5000);
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', (chunk) => (stdout += chunk));
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', (chunk) => (stderr += chunk));
      child.on('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on('close', () => {
        clearTimeout(timer);
        try {
          const value = JSON.parse(stdout);
          if (value.error) throw new Error(value.error);
          resolve(value);
        } catch (error) {
          reject(new Error(stderr.trim() || error.message));
        }
      });
      child.stdin.end(JSON.stringify(request));
    });
  }

  async permissions() {
    return this.bridge({ operation: 'permissions' });
  }

  async ensurePermissions() {
    const permissions = await this.permissions();
    if (permissions.screen_capture && permissions.accessibility) return;
    await this.bridge({ operation: 'request_permissions' }).catch(() => {});
    throw new Error(
      '需要屏幕录制和辅助功能权限：请在系统设置 → 隐私与安全性 给 Electron 授权后重启应用。',
    );
  }

  async resolveAppFromPrompt(prompt) {
    const host = path.join(this.repoRoot, 'target/debug/yukii-desktop-host');
    const bridge = path.join(this.repoRoot, 'target/native/computer-bridge');
    return new Promise((resolve, reject) => {
      const child = spawn(host, ['--select-app', '--prompt', prompt, '--bridge', bridge], {
        cwd: this.repoRoot,
        env: { ...process.env, YUKII_VISION_INPUT: 'true' },
      });
      let stdout = '';
      let stderr = '';
      const timer = setTimeout(() => {
        child.kill('SIGKILL');
        reject(new Error('模型选择应用超时'));
      }, 30_000);
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', (chunk) => (stdout += chunk));
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', (chunk) => (stderr += chunk));
      child.on('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on('close', () => {
        clearTimeout(timer);
        try {
          const line = stdout.trim().split('\n').filter(Boolean).at(-1);
          const target = JSON.parse(line || '');
          if (!target.window_id || !target.pid) throw new Error('应用解析结果不完整');
          resolve(target);
        } catch (error) {
          reject(new Error(stderr.trim() || `模型没有选出可用应用：${error.message}`));
        }
      });
    });
  }

  async resolveApp(appName) {
    const apps = await this.listApps();
    const app = apps.find((item) => item.application === appName || item.app_id === appName);
    if (!app) throw new Error('目标应用当前没有可控制窗口');
    return app;
  }

  startProcess(task, appName, target) {
    const host = path.join(this.repoRoot, 'target/debug/yukii-desktop-host');
    const executor = path.join(this.repoRoot, 'target/debug/yukii-desktop-executor-service');
    const bridge = path.join(this.repoRoot, 'target/native/computer-bridge');
    const picker = path.join(this.repoRoot, 'target/native/window-picker');
    const preview = path.join(this.repoRoot, 'target/native/preview-panel');
    const args = [
      '--prompt',
      task.prompt,
      '--data-dir',
      path.join(this.dataDir, 'tasks'),
      '--executor',
      executor,
      '--bridge',
      bridge,
      '--window-picker',
      picker,
      '--preview-panel',
      preview,
      '--json-events',
      '--no-confirm-actions',
      '--no-preview',
    ];
    if (target) {
      args.push('--window-id', String(target.window_id), '--pid', String(target.pid), '--yes');
    } else {
      args.push('--panel-select');
    }
    const child = spawn(host, args, {
      cwd: this.repoRoot,
      // 桌面 CUA 的 observe 需要视觉输入；这是受信本地任务，不向 renderer 暴露模型配置。
      env: { ...process.env, YUKII_VISION_INPUT: 'true' },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    this.children.set(task.id, child);
    let buffer = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      buffer += chunk;
      let index = buffer.indexOf('\n');
      while (index >= 0) {
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (line) this.handleLine(task, line);
        index = buffer.indexOf('\n');
      }
    });
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
      this.log('[local-task]', chunk.trimEnd());
    });
    child.on('error', (error) => {
      this.finish(task, 'failed', `无法启动本地任务：${error.message}`);
    });
    child.on('close', (code, signal) => {
      this.children.delete(task.id);
      if (!TERMINAL.has(task.status)) {
        const detail = stderr.trim().split('\n').slice(-3).join(' ');
        const cancelled =
          task.cancel_requested ||
          signal === 'SIGINT' ||
          signal === 'SIGTERM' ||
          detail.includes('用户停止') ||
          detail.includes('收到 Ctrl+C');
        this.finish(
          task,
          cancelled ? 'cancelled' : 'failed',
          cancelled
            ? '本地任务已取消。'
            : detail || `本地任务退出（code=${code}, signal=${signal}）`,
        );
      }
    });
  }

  handleLine(task, line) {
    let value;
    try {
      value = JSON.parse(line);
    } catch {
      return;
    }
    if (value.event === 'status') {
      const status = String(value.status || 'running');
      task.status = ACTIVE.has(status) || TERMINAL.has(status) ? status : 'running';
      this.addEvent(task, status, String(value.message || ''));
      return;
    }
    if (value.event === 'result' && value.task) {
      const result = value.task;
      task.status = TERMINAL.has(result.status) ? result.status : 'completed';
      task.result = result.result ?? task.result;
      task.error = result.error ?? task.error;
      task.input_tokens = Number(result.input_tokens || 0);
      task.output_tokens = Number(result.output_tokens || 0);
      this.addEvent(task, task.status, task.error || '本地任务完成。');
    }
  }

  addEvent(task, status, message) {
    if (!String(message).trim()) return;
    const events = this.events.get(task.id) || [];
    const sequence = events.length ? events[events.length - 1].sequence + 1 : 1;
    events.push({
      sequence,
      task_id: task.id,
      status,
      message,
      created_at: nowSeconds(),
    });
    this.events.set(task.id, events);
    this.onUpdate(this.clone(task));
  }

  finish(task, status, detail) {
    task.status = status;
    if (status === 'failed') task.error = detail;
    if (status === 'cancelled') task.error = null;
    this.addEvent(task, status, detail);
  }

  cancel(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    if (TERMINAL.has(task.status)) return this.clone(task);
    const child = this.children.get(id);
    if (child) {
      task.cancel_requested = true;
      if (task.paused) child.kill('SIGCONT');
      child.kill('SIGINT');
      setTimeout(() => {
        if (this.children.has(id)) child.kill('SIGTERM');
      }, 2000).unref();
    } else {
      this.finish(task, 'cancelled', '本地任务已取消。');
    }
    return this.clone(task);
  }

  async approve(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    if (!task.awaiting_approval) return this.clone(task);
    const target = task._target;
    if (!target) throw new Error('任务没有可控制目标');
    await this.ensurePermissions();
    const key = target.app_id || `name:${target.application}`;
    this.allowed[key] = target.application;
    this.saveAllowed();
    task.awaiting_approval = false;
    task.status = 'queued';
    this.addEvent(task, 'queued', `已允许使用 ${target.application}，开始执行。`);
    this.startProcess(task, task._appName, target);
    this.onTarget(this.clone(task), target);
    return this.clone(task);
  }

  deny(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    if (!task.awaiting_approval) return this.clone(task);
    task.awaiting_approval = false;
    this.finish(task, 'cancelled', '用户拒绝应用权限。');
    return this.clone(task);
  }

  pause(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    if (TERMINAL.has(task.status) || task.status === 'waiting_for_user') return this.clone(task);
    const child = this.children.get(id);
    if (child) child.kill('SIGSTOP');
    task.status = 'waiting_for_user';
    task.paused = true;
    this.addEvent(task, 'waiting_for_user', '本地任务已暂停；预览停留在当前帧。');
    return this.clone(task);
  }

  resume(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    if (!task.paused) return this.clone(task);
    const child = this.children.get(id);
    if (child) child.kill('SIGCONT');
    task.paused = false;
    task.status = 'running';
    this.addEvent(task, 'running', '本地任务已继续。');
    return this.clone(task);
  }

  run(id) {
    const task = this.tasks.get(id);
    if (!task) throw new Error('任务不存在');
    return this.clone(task);
  }

  dispose() {
    for (const [id, child] of this.children) {
      const task = this.tasks.get(id);
      if (task && task.paused) child.kill('SIGCONT');
      child.kill('SIGINT');
      setTimeout(() => child.kill('SIGKILL'), 1500).unref();
    }
    this.children.clear();
  }

  clone(task) {
    const { submission_key, _target, _appName, cancel_requested, ...rest } = task;
    return structuredClone(rest);
  }
}

module.exports = { LocalTaskRunner };
