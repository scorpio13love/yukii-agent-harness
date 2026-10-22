#!/usr/bin/env bash
# 开源本机启动：构建 API/CUA/web，然后启动 API + Vite + Electron。
set -euo pipefail
cd "$(dirname "$0")/.."

missing=0
for command in node pnpm; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "缺少依赖：$command"
    missing=1
  fi
done
if ! ./tooling/cargo.sh --version >/dev/null 2>&1; then
  echo "缺少依赖：cargo（可通过 .tools 或 rustup 安装）"
  missing=1
fi
[ "$missing" -eq 0 ] || exit 2

if [ ! -f .env ]; then
  cp .env.example .env
  echo "已创建 .env（来自 .env.example）；请填写模型配置后再执行任务。"
fi

if [ "${1:-}" = "--check" ]; then
  echo "依赖检查通过；本机数据目录：${YUKII_DATA_DIR:-.data}"
  exit 0
fi

pnpm install
pnpm build
./tooling/cargo.sh build -p yukii-api-server -p yukii-cua-sidecar

cleanup() {
  [ -n "${API_PID:-}" ] && kill "$API_PID" 2>/dev/null || true
  [ -n "${WEB_PID:-}" ] && kill "$WEB_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

./target/debug/yukii-api-server &
API_PID=$!
pnpm --filter @yukii/web dev &
WEB_PID=$!

for _ in $(seq 1 40); do
  curl -fsS http://127.0.0.1:8765/api/ready >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS http://127.0.0.1:8765/api/ready >/dev/null || {
  echo "API 未就绪，检查 .env 和 8765 端口。"
  exit 3
}

cd desktop/electron
pnpm start
