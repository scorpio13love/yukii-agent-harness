// 跨平台启动器：清掉 ELECTRON_RUN_AS_NODE，确保进入 Electron 主进程而不是 Node。
const { spawn } = require('node:child_process');
const path = require('node:path');
const electron = require('electron'); // 在 Node 下这是 Electron 可执行文件路径。

const env = { ...process.env };
delete env.ELECTRON_RUN_AS_NODE;
const child = spawn(electron, [path.join(__dirname), ...process.argv.slice(2)], {
  stdio: 'inherit',
  env,
});
child.on('exit', (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 0);
});
