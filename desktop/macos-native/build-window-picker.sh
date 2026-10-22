#!/usr/bin/env bash
# 构建窗口缩略图选择器（临时原生 UI；最终统一到 Chromium 147）。
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT="${1:-$PROJECT_ROOT/target/native/window-picker}"
case "$(uname -m)" in
  arm64) TARGET="arm64-apple-macosx14.0" ;;
  x86_64) TARGET="x86_64-apple-macosx14.0" ;;
  *) echo "不支持的架构：$(uname -m)" >&2; exit 1 ;;
esac
mkdir -p "$(dirname "$OUTPUT")" "$PROJECT_ROOT/target/native/module-cache"
CLANG_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
SWIFT_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
swiftc -O -parse-as-library -target "$TARGET" \
  "$PROJECT_ROOT/desktop/macos-native/WindowPicker.swift" \
  -framework AppKit -framework ScreenCaptureKit -framework CoreGraphics \
  -o "$OUTPUT"
echo "已构建窗口选择器：$OUTPUT"
