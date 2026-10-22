#!/usr/bin/env bash
# 构建 macOS 原生桥。产物不是业务仓库提交内容；默认写入 target/native/。
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT="${1:-$PROJECT_ROOT/target/native/computer-bridge}"
case "$(uname -m)" in
  arm64) TARGET="arm64-apple-macosx14.0" ;;
  x86_64) TARGET="x86_64-apple-macosx14.0" ;;
  *) echo "不支持的架构：$(uname -m)" >&2; exit 1 ;;
esac
mkdir -p "$(dirname "$OUTPUT")" "$PROJECT_ROOT/target/native/module-cache"
# 沙箱环境常限制默认 module cache；显式放到项目 target 下，避免依赖用户临时目录权限。
CLANG_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
SWIFT_MODULE_CACHE_PATH="$PROJECT_ROOT/target/native/module-cache" \
swiftc -O -parse-as-library -target "$TARGET" \
  "$PROJECT_ROOT/desktop/macos-native/ComputerBridge.swift" \
  -o "$OUTPUT"
echo "已构建原生桥：$OUTPUT"
