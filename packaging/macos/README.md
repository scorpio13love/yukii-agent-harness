# macOS 打包、签名与公证

## 当前状态

已生成未签名产物：

```text
dist/macos/mac-arm64/Yukii.app
dist/macos/Yukii-0.0.0-arm64.zip
```

App 内含：

```text
Contents/Resources/bin/yukii-api-server
Contents/Resources/bin/yukii-cua-sidecar
Contents/Resources/web/
bundle id = com.yukii.desktop
```

当前 App 是 ad-hoc 签名，只用于本地结构验证；DMG 生成和公证尚未验证。公证需要 Apple Developer 凭据。

## 公证是什么

公证不是代码签名。流程是：

1. Developer ID Application 证书签名 App；
2. 开启 Hardened Runtime，使用 `packaging/macos/entitlements.mac.plist`；
3. 用 `xcrun notarytool submit` 把 zip/dmg 提交 Apple；
4. Apple 扫描恶意软件和签名问题；
5. 通过后用 `xcrun stapler staple` 把票据钉到产物；
6. `spctl --assess` 验证 Gatekeeper 放行。

没有 Apple Developer Program 证书、Team ID 和 App 专用密码，无法真实公证。

## 构建

签名构建（要求 Developer ID）：

```bash
pnpm package:mac
```

未签名本地验证：

```bash
pnpm package:mac:unsigned
```

脚本在缺证书时会直接失败，不会静默产出可发布包。

脚本会：

- 构建 web；
- 构建 release 的 API server/CUA sidecar；
- 用 electron-builder 生成 App 和 zip/DMG。

若只做未签名验证：

```bash
CSC_IDENTITY_AUTO_DISCOVERY=false pnpm package:mac
```

## 签名

设置 electron-builder 支持的签名变量，例如：

```bash
export CSC_LINK=/path/to/DeveloperIDApplication.p12
export CSC_KEY_PASSWORD='...'
pnpm package:mac
```

签名后验证：

```bash
codesign --verify --deep --strict --verbose=2 dist/macos/mac-arm64/Yukii.app
spctl --assess --type execute -vv dist/macos/mac-arm64/Yukii.app
```

## 公证

```bash
APPLE_ID=... APPLE_TEAM_ID=... APPLE_APP_PASSWORD=... \
  pnpm notarize:mac dist/macos/Yukii-0.0.0-arm64.dmg
```

凭据只从环境变量读取，不写入仓库。

## TCC 注意

- 屏幕录制/辅助功能授权绑定最终签名身份；
- 不要让用户为 ad-hoc 包授权后再换正式签名，否则 TCC 身份会变化；
- CUA sidecar 作为 App 资源签名，不能单独用不同 Team ID；
- 发布前在干净 macOS 用户上验证首次授权、升级后授权保留和 Gatekeeper。

## 明确失败

```bash
pnpm package:mac
# 错误：找不到 Developer ID Application 证书。

pnpm verify:mac
# 错误：App 仍是 ad-hoc 签名，不能发布。
```

当前环境没有 Developer ID（`0 valid identities found`），DMG 的 `hdiutil` 也受限；这两项必须在正常 macOS 构建机完成。
