import { readFileSync, existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { execFileSync } from 'node:child_process';

const config = JSON.parse(
  readFileSync(new URL('../desktop/chromium/version.json', import.meta.url), 'utf8'),
);
const checkout = process.argv[2];
if (!checkout) {
  console.log(`目标 Chromium 主版本：${config.major}`);
  console.log('请传入 Chromium 源码目录：pnpm chromium:check /path/to/chromium/src');
  console.log(`已锁定上游提交：${config.pinnedRevision}；尚需提供真实 checkout 验证。`);
  process.exitCode = 1;
} else {
  try {
    // 检查真实源码与 Git 提交，避免仅修改配置便宣称版本兼容。
    const versionPath = resolve(checkout, 'chrome/VERSION');
    if (!existsSync(versionPath)) throw new Error('目录中不存在 chrome/VERSION');
    const version = readFileSync(versionPath, 'utf8');
    const major = Number(version.match(/^MAJOR=(\d+)$/m)?.[1]);
    if (major !== config.major) throw new Error(`需要 Chromium ${config.major}，实际为 ${major}`);
    const commit = execFileSync('git', ['-C', resolve(checkout), 'rev-parse', 'HEAD'], {
      encoding: 'utf8',
    }).trim();
    if (!config.pinnedRevision)
      throw new Error(`主版本通过，但版本尚未锁定。待审核提交：${commit}`);
    if (commit !== config.pinnedRevision) throw new Error('源码提交与锁定版本不一致');
    console.log(`Chromium ${major} 版本与提交检查通过：${commit}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
