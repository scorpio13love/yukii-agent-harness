import { execFileSync } from 'node:child_process';

// 读取 Cargo 的真实依赖图，避免代码重构后只有目录分层、依赖方向却逐渐失守。
const metadata = JSON.parse(
  execFileSync('./tooling/cargo.sh', ['metadata', '--no-deps', '--format-version', '1'], {
    encoding: 'utf8',
  }),
);
const boundaries = {
  'yukii-contracts': [],
  'yukii-agent-domain': ['yukii-contracts'],
  'yukii-agent-ports': ['yukii-contracts', 'yukii-agent-domain'],
  'yukii-agent-core': ['yukii-contracts', 'yukii-agent-domain', 'yukii-agent-ports'],
  'yukii-agent-runtime': [
    'yukii-contracts',
    'yukii-agent-core',
    'yukii-agent-ports',
    'yukii-tool-gateway',
  ],
  'yukii-model-gateway': ['yukii-agent-ports'],
  'yukii-tool-gateway': ['yukii-agent-ports'],
  'yukii-agent-infra': ['yukii-contracts', 'yukii-agent-domain', 'yukii-agent-ports'],
  'yukii-agent-rag': ['yukii-contracts', 'yukii-agent-ports'],
  'yukii-agent-extensions': ['yukii-contracts', 'yukii-agent-ports', 'yukii-tool-gateway'],
  'yukii-sandbox-docker': ['yukii-agent-ports'],
  'yukii-desktop-executor': ['yukii-contracts', 'yukii-agent-domain', 'yukii-agent-ports'],
  'yukii-desktop-transport': [],
  'yukii-cua-sidecar': ['yukii-contracts', 'yukii-desktop-host', 'yukii-agent-infra'],
  'yukii-browser-use': ['yukii-contracts', 'yukii-agent-ports'],
  'yukii-desktop-tools': ['yukii-contracts', 'yukii-agent-ports'],
  'yukii-mcp-client': ['yukii-agent-ports'],
  'yukii-agent-memory': ['yukii-agent-ports'],
  'yukii-office-tools': ['yukii-agent-ports'],
  'yukii-desktop-executor-service': [
    'yukii-contracts',
    'yukii-agent-domain',
    'yukii-agent-infra',
    'yukii-agent-ports',
    'yukii-desktop-executor',
    'yukii-desktop-transport',
  ],
};
const pureLayers = new Set([
  'yukii-contracts',
  'yukii-agent-domain',
  'yukii-agent-ports',
  'yukii-agent-core',
]);
const ioDrivers = new Set(['reqwest', 'axum', 'rusqlite', 'dotenvy', 'libc']);
const errors = [];
for (const pkg of metadata.packages) {
  if (pkg.name === 'yukii-api-server' || pkg.name === 'yukii-desktop-host') continue; // 组合根负责注入具体实现。
  const allowed = boundaries[pkg.name];
  if (!allowed) {
    errors.push(`未定义依赖边界：${pkg.name}`);
    continue;
  }
  for (const dependency of pkg.dependencies.filter((item) => item.kind !== 'dev')) {
    if (dependency.path && !allowed.includes(dependency.name)) {
      errors.push(`禁止反向依赖：${pkg.name} → ${dependency.name}`);
    }
    if (pureLayers.has(pkg.name) && ioDrivers.has(dependency.name)) {
      errors.push(`核心层不得依赖 I/O 驱动：${pkg.name} → ${dependency.name}`);
    }
  }
}
if (errors.length) {
  console.error(errors.join('\n'));
  process.exitCode = 1;
} else {
  console.log('架构依赖检查通过：核心不依赖 HTTP、数据库或模型适配器。');
}
