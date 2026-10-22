#!/usr/bin/env bash
# 触发 macOS 的屏幕录制/辅助功能同意对话框；不会自行授予权限。
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BRIDGE="${1:-$PROJECT_ROOT/target/native/computer-bridge}"
if [[ ! -x "$BRIDGE" ]]; then
  echo "找不到原生桥：$BRIDGE" >&2
  echo "请先运行 ./desktop/macos-native/build.sh" >&2
  exit 1
fi
printf '{"operation":"request_permissions"}\n' | "$BRIDGE"
printf '\n'
cat <<'TEXT'
请在“系统设置 → 隐私与安全性”中完成授权：
  1. 屏幕与系统音频录制：打开对应终端/宿主或 computer-bridge 的开关；
  2. 辅助功能：打开同一个应用的开关；
  3. 退出并重新打开该终端/宿主，再运行：
       echo '{"operation":"permissions"}' | target/native/computer-bridge
     看到两个 true 后再启动 yukii-desktop-host。
如果列表中没有目标应用，可点击“+”并用 Cmd+Shift+G 输入：
  target/native/computer-bridge
TEXT
