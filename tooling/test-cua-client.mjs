// 无 Electron 依赖的 CUA sidecar 客户端冒烟测试；验证 Electron main 将使用的同一客户端。
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { CuaSidecarClient } = require('../desktop/electron/cua-client.cjs');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const dataDir = fs.mkdtempSync(path.join(root, 'target', 'native', 'cua-client-test-'));
const client = new CuaSidecarClient({ dataDir });

try {
  client.start();
  const hello = await client.call('hello');
  if (!hello.capabilities?.includes('elements')) throw new Error('sidecar 未声明 elements 能力');
  const permissions = await client.call('permissions');
  if (!permissions.screen_capture || !permissions.accessibility) throw new Error('TCC 权限不完整');
  const windows = await client.call('windows');
  console.log(`CUA client smoke passed: ${windows.length} windows, protocol ${hello.protocol}`);
} finally {
  client.close();
  setTimeout(() => process.exit(0), 300);
}
