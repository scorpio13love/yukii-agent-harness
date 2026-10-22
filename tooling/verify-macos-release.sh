#!/usr/bin/env bash
# 发布前检查：签名、Gatekeeper、DMG 公证票据。缺少条件时明确失败。
set -euo pipefail
cd "$(dirname "$0")/.."
APP="${1:-dist/macos/mac-arm64/Yukii.app}"
DMG="${2:-dist/macos/Yukii-0.0.0-arm64.dmg}"
[ -d "$APP" ] || { echo "错误：找不到 App：$APP"; exit 2; }
if codesign -dv --verbose=2 "$APP" 2>&1 | grep -q 'TeamIdentifier=not set'; then
  echo "错误：App 仍是 ad-hoc 签名，不能发布。"; exit 3
fi
echo "签名身份："
codesign -dv --verbose=2 "$APP" 2>&1 | grep -E 'Authority|TeamIdentifier' || true
codesign --verify --deep --strict --verbose=2 "$APP"
spctl --assess --type execute -vv "$APP"
if [ -f "$DMG" ]; then
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature -v "$DMG"
else
  echo "错误：找不到 DMG：$DMG"; exit 4
fi
echo "发布校验通过：$APP / $DMG"
