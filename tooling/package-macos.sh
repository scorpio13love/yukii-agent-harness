#!/usr/bin/env bash
# 构建 macOS 产物。默认要求 Developer ID；--unsigned 仅用于本地结构验证。
set -euo pipefail
cd "$(dirname "$0")/.."
MODE="${1:---signed}"
case "$MODE" in
  --unsigned)
    export CSC_IDENTITY_AUTO_DISCOVERY=false
    TARGETS="zip"
    ;;
  --signed)
    IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null | grep -c 'Developer ID Application' || true)"
    if [ "$IDENTITIES" -eq 0 ]; then
      echo "错误：找不到 Developer ID Application 证书。"
      echo "请安装证书后重试，或使用 --unsigned 生成仅本地验证的 ad-hoc 包。"
      exit 2
    fi
    TARGETS="dmg zip"
    ;;
  *)
    echo "用法：tooling/package-macos.sh [--signed|--unsigned]"
    exit 2
    ;;
esac
pnpm build
./tooling/cargo.sh build --release -p yukii-api-server -p yukii-cua-sidecar
HOME="${HOME}" \
ELECTRON_CACHE="$PWD/.tools/electron-cache" \
ELECTRON_BUILDER_CACHE="$PWD/.tools/electron-builder-cache" \
pnpm exec electron-builder \
  --projectDir desktop/electron \
  --config electron-builder.yml \
  --mac "$TARGETS" \
  --publish never
if [ "$MODE" = "--unsigned" ]; then
  # electron-builder 在无证书时不密封 App；本地结构验证用 ad-hoc 再签一次。
  codesign --force --deep --sign - dist/macos/mac-arm64/Yukii.app
fi
echo "产物：dist/macos/"
