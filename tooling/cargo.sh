#!/usr/bin/env bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# 项目内工具链不改写用户 PATH；同时允许开发者使用已安装的系统 Cargo。
if [[ -x "$PROJECT_ROOT/.tools/cargo/bin/cargo" ]]; then
  export CARGO_HOME="$PROJECT_ROOT/.tools/cargo"
  export RUSTUP_HOME="$PROJECT_ROOT/.tools/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
cd "$PROJECT_ROOT"
exec cargo "$@"
