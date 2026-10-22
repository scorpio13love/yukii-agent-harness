#!/usr/bin/env bash
# 验证原生桥不会把 Yukii 自身窗口提供给 CUA 应用选择。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BRIDGE="${1:-$ROOT/target/native/computer-bridge}"
if [ ! -x "$BRIDGE" ]; then
  echo "缺少原生桥：$BRIDGE" >&2
  exit 1
fi
echo '{"operation":"apps"}' | "$BRIDGE" | python3 -c '
import json, sys
apps = json.load(sys.stdin)["apps"]
blocked = ("window-picker", "preview-panel")
for app in apps:
    name = app.get("application", "").lower()
    title = app.get("title", "").lower()
    if name == "yukii" or "yukii · 办公助手" in title:
        raise SystemExit(f"自身应用未被过滤: {app}")
print(f"CUA app filter ok: {len(apps)} selectable apps")
'
