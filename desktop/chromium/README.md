> 桌面壳采用 Electron 多端；本目录的 Chromium 147 资料仅保留为历史/可选参考，不再是当前目标。

# Chromium 147 桌面宿主

`version.json` 是版本约束，`args.gn` 是开发构建参数。

已通过公开源码 VERSION 与 `git ls-remote` 核实 `147.0.7727.149`，锁定提交 `c1a48370d4fac0748be3924b6f0564974e9ea17b`。版本锁定不代表已编译或验证桌面宿主。

## 构建准备

1. 安装完整 Xcode 并完成许可设置。Command Line Tools 不等于完整 Xcode。
2. 安装 Chromium 官方 depot_tools，按官方流程检出源码及依赖。
3. 检出已锁定的 `147.0.7727.149` 提交，再同步对应 DEPS。
4. 使用 `pnpm chromium:check /path/to/chromium/src` 校验真实源码。
5. 将 `args.gn` 复制到源码构建目录 `out/Yukii/args.gn`，再执行 `gn gen out/Yukii` 和 `autoninja -C out/Yukii chrome`。

仅完成上游构建仍不是 Yukii 桌面端。下一步需要加入受控 WebUI/原生桥接、Rust 服务生命周期、BU 组件、XPC/CU 接口与打包测试。

源码不放入业务 Git 仓库，使用外部 checkout 或已忽略的 `checkout/`。不为节省磁盘清理用户其他项目。

官方构建参考：https://chromium.googlesource.com/chromium/src/+/main/docs/mac_build_instructions.md 。实际构建要求以锁定的 147 源码内文档与 DEPS 为准。
