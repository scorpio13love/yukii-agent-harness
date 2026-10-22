#!/usr/bin/env bash
# 从已构建 App 生成压缩 DMG；hdiutil 失败时明确退出，不伪造产物。
set -euo pipefail
cd "$(dirname "$0")/.."
APP="${1:-dist/macos/mac-arm64/Yukii.app}"
OUT="${2:-dist/macos/Yukii-0.0.0-arm64.dmg}"
[ -d "$APP" ] || { echo "错误：找不到 App：$APP"; exit 2; }
command -v hdiutil >/dev/null || { echo "错误：当前环境没有 hdiutil"; exit 2; }
rm -f "$OUT"
if ! hdiutil create -volname "Yukii" -srcfolder "$APP" -ov -format UDZO "$OUT"; then
  echo "错误：hdiutil 创建 DMG 失败；请在正常 macOS 构建机重试。"
  exit 3
fi
hdiutil verify "$OUT"
echo "DMG：$OUT"
