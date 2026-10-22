// 受信 Electron main 使用的 CUA sidecar 客户端。
// 只允许 main 进程持有；renderer 只能通过 control-preload 的窄 IPC 调用。
const { spawn } = require('node:child_process');
const path = require('node:path');
const readline = require('node:readline');

const DEFAULT_SIDECAR = path.join(__dirname, '..', '..', 'target', 'debug', 'yukii-cua-sidecar');

class CuaSidecarClient {
  constructor(options = {}) {
    this.sidecarPath = options.sidecarPath || process.env.YUKII_CUA_SIDECAR || DEFAULT_SIDECAR;
    this.dataDir =
      options.dataDir || path.join(__dirname, '..', '..', 'target', 'native', 'electron-cua-data');
    this.executor = options.executor || process.env.YUKII_CUA_EXECUTOR;
    this.bridge = options.bridge || process.env.YUKII_CUA_BRIDGE;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Set();
    this.child = null;
    this.log = options.logger || (() => {});
  }

  start() {
    if (this.child) return;
    if (!require('node:fs').existsSync(this.sidecarPath)) {
      throw new Error(`找不到 CUA sidecar：${this.sidecarPath}`);
    }
    const args = ['--data-dir', this.dataDir];
    if (this.executor) args.push('--executor', this.executor);
    if (this.bridge) args.push('--bridge', this.bridge);
    this.child = spawn(this.sidecarPath, args, {
      stdio: ['pipe', 'pipe', 'pipe'],
      env: process.env,
    });
    this.child.stderr.on('data', (chunk) => {
      this.log('[cua-sidecar]', chunk.toString().trimEnd());
    });
    const lines = readline.createInterface({ input: this.child.stdout });
    lines.on('line', (line) => this.#onLine(line));
    this.child.on('exit', (code, signal) => {
      const error = new Error(`CUA sidecar exited (code=${code}, signal=${signal})`);
      for (const item of this.pending.values()) {
        clearTimeout(item.timer);
        item.reject(error);
      }
      this.pending.clear();
      this.child = null;
    });
  }

  onEvent(listener) {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  #onLine(line) {
    let response;
    try {
      response = JSON.parse(line);
    } catch {
      this.log('[cua-sidecar] invalid stdout line:', line);
      return;
    }
    if (!response.id && response.event) {
      for (const listener of this.listeners) {
        try {
          listener(response);
        } catch (error) {
          this.log('[cua-sidecar] event listener failed:', error);
        }
      }
      return;
    }
    const item = this.pending.get(response.id);
    if (!item) return;
    this.pending.delete(response.id);
    clearTimeout(item.timer);
    if (response.ok) item.resolve(response.result);
    else item.reject(new Error(response.error || 'CUA sidecar error'));
  }

  call(command, payload = {}, timeoutMs = 60_000) {
    if (!this.child) return Promise.reject(new Error('CUA sidecar is not running'));
    const id = `electron-${this.nextId++}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CUA ${command} timed out`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.child.stdin.write(`${JSON.stringify({ id, command, ...payload })}\n`, (error) => {
        if (error) {
          this.pending.delete(id);
          clearTimeout(timer);
          reject(error);
        }
      });
    });
  }

  close() {
    if (!this.child) return;
    this.child.stdin.end();
    const child = this.child;
    setTimeout(() => child.kill('SIGTERM'), 1_000).unref();
  }
}

module.exports = { CuaSidecarClient };
