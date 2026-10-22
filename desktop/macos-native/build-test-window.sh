#!/usr/bin/env bash
# 构建项目自有的 Computer Use 验收窗口。产物写入被忽略的 target/native/。
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT="${1:-$PROJECT_ROOT/target/native/yukii-cu-test}"
case "$(uname -m)" in
  arm64) TARGET="arm64-apple-macosx14.0" ;;
  x86_64) TARGET="x86_64-apple-macosx14.0" ;;
  *) echo "不支持的架构：$(uname -m)" >&2; exit 1 ;;
esac
mkdir -p "$(dirname "$OUTPUT")" "$PROJECT_ROOT/target/native/module-cache"
CLANG_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
SWIFT_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
swiftc -O -parse-as-library -target "$TARGET" \
  "$PROJECT_ROOT/desktop/macos-native/TestWindow.swift" \
  -o "$OUTPUT"
echo "已构建测试窗口：$OUTPUT"
