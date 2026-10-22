#!/usr/bin/env bash
# 开发者证书签名后的公证与 staple。凭据必须从环境变量读取，不写入仓库。
set -euo pipefail
cd "$(dirname "$0")/.."
APPLE_ID="${APPLE_ID:?需要 APPLE_ID}"
APPLE_TEAM_ID="${APPLE_TEAM_ID:?需要 APPLE_TEAM_ID}"
APPLE_APP_PASSWORD="${APPLE_APP_PASSWORD:?需要 APPLE_APP_PASSWORD（App 专用密码）}"
ARTIFACT="${1:?用法: tooling/notarize-macos.sh <Yukii-*.dmg>}"

xcrun notarytool submit "$ARTIFACT" \
  --apple-id "$APPLE_ID" \
  --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_PASSWORD" \
  --wait
xcrun stapler staple "$ARTIFACT"
spctl --assess --type open --context context:primary-signature -v "$ARTIFACT"
echo "公证与 staple 完成：$ARTIFACT"
