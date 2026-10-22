// Electron main 的日志：优先写 userData 文件，stderr 只做 best-effort。
// 终端/管道关闭时 console.error 会抛 EPIPE；这里绝不把日志失败变成未捕获异常。
const fs = require('node:fs');
const path = require('node:path');

let logPath = null;
process.stderr.on('error', () => {});

function initLog(filePath) {
  logPath = filePath;
  try {
    fs.mkdirSync(path.dirname(filePath), { recursive: true });
  } catch {
    // 日志目录不可写时仍允许应用启动；后续写入会被静默跳过。
  }
}

function safeLog(...parts) {
  const line = `[${new Date().toISOString()}] ${parts
    .map((part) => (part instanceof Error ? part.stack || part.message : String(part)))
    .join(' ')}\n`;
  if (logPath) {
    try {
      fs.appendFileSync(logPath, line);
    } catch {
      // 不把日志 I/O 故障升级为应用故障。
    }
  }
  try {
    process.stderr.write(line);
  } catch {
    // stderr 可能已关闭；忽略。
  }
}

module.exports = { initLog, safeLog };
