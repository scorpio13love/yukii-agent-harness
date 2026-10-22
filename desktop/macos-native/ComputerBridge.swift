// macOS 薄适配层：业务策略、授权和调用日志由 Rust 宿主管理。
// 不监听网络，不申请 TCC 权限；每条请求只操作显式指定的窗口所属进程。
import AppKit
import ScreenCaptureKit

/// 自身 UI 和测试工具不应该成为 CUA 的控制目标。
/// 默认按名称过滤，另支持 YUKII_EXCLUDED_APPS 追加（逗号分隔）。
func isExcludedApplication(_ name: String) -> Bool {
    let normalized = name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let defaults = ["yukii", "yukii · 办公助手", "yukii control", "window-picker", "preview-panel"]
    let configured = (ProcessInfo.processInfo.environment["YUKII_EXCLUDED_APPS"] ?? "")
        .split(separator: ",")
        .map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() }
        .filter { !$0.isEmpty }
    // 自身应用名可能恰好是 Yukii；测试窗口名是 Yukii CU Test，不能按前缀误伤。
    let exactOnly = ["yukii"]
    return (defaults + configured).contains { name in
        guard !normalized.isEmpty else { return false }
        return normalized == name || (!exactOnly.contains(name) && normalized.contains(name))
    }
}

/// 终端里的回车/输入等同于执行命令，CUA 不把终端窗口提供给应用选择。
func isSensitiveApplication(_ name: String) -> Bool {
    let normalized = name.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    return ["terminal", "iterm", "warp", "console", "kitty", "alacritty", "hyper", "tabby", "ghostty"]
        .contains { normalized == $0 || normalized.contains($0) }
}

struct BridgeFailure: Error { let message: String }
func fail(_ message: String) -> BridgeFailure { BridgeFailure(message: message) }

// MARK: - Accessibility 树（平台无关元素快照的 macOS 实现）

func axAttribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value
}

func axString(_ element: AXUIElement, _ name: String) -> String? {
    axAttribute(element, name) as? String
}

func axBool(_ element: AXUIElement, _ name: String) -> Bool? {
    (axAttribute(element, name) as? NSNumber)?.boolValue
}

func axActions(_ element: AXUIElement) -> [String] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success, let names else { return [] }
    return names as? [String] ?? []
}

func axChildren(_ element: AXUIElement) -> [AXUIElement] {
    guard let raw = axAttribute(element, kAXChildrenAttribute as String),
          CFGetTypeID(raw) == CFArrayGetTypeID() else { return [] }
    let array = unsafeBitCast(raw, to: CFArray.self)
    var children: [AXUIElement] = []
    for index in 0..<CFArrayGetCount(array) {
        if let value = CFArrayGetValueAtIndex(array, index) {
            children.append(unsafeBitCast(value, to: AXUIElement.self))
        }
    }
    return children
}

func axFrame(_ element: AXUIElement) -> CGRect? {
    guard let rawPosition = axAttribute(element, kAXPositionAttribute as String),
          let rawSize = axAttribute(element, kAXSizeAttribute as String),
          CFGetTypeID(rawPosition) == AXValueGetTypeID(),
          CFGetTypeID(rawSize) == AXValueGetTypeID() else { return nil }
    var position = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(unsafeBitCast(rawPosition, to: AXValue.self), .cgPoint, &position),
          AXValueGetValue(unsafeBitCast(rawSize, to: AXValue.self), .cgSize, &size) else { return nil }
    return CGRect(x: position.x, y: position.y, width: size.width, height: size.height)
}

func axWindowElement(pid: Int32, frame: CGRect) -> AXUIElement? {
    let application = AXUIElementCreateApplication(pid)
    if let raw = axAttribute(application, kAXWindowsAttribute as String),
       CFGetTypeID(raw) == CFArrayGetTypeID() {
        let array = unsafeBitCast(raw, to: CFArray.self)
        var windows: [AXUIElement] = []
        for index in 0..<CFArrayGetCount(array) {
            guard let value = CFArrayGetValueAtIndex(array, index) else { continue }
            windows.append(unsafeBitCast(value, to: AXUIElement.self))
        }
        for window in windows {
            guard let current = axFrame(window) else { continue }
            // SCWindow 与 AX 窗口可能因阴影/缩放有 1-2 点差异；用 4 点容差。
            if abs(current.minX - frame.minX) < 4,
               abs(current.minY - frame.minY) < 4,
               abs(current.width - frame.width) < 4,
               abs(current.height - frame.height) < 4 {
                return window
            }
        }
        // 该应用只有一个窗口时才允许无几何匹配回退；多窗口绝不猜测。
        if windows.count == 1 { return windows[0] }
    }
    // 不回退到 focused window：观察树必须严格对应当前已同意的目标窗口。
    return nil
}

/// 路径从窗口根开始：ax-0 是窗口，ax-0.2.1 是第 3 个子节点的第 2 个子节点。
func axSnapshot(root: AXUIElement, windowFrame: CGRect) -> [[String: Any]] {
    var result: [[String: Any]] = []
    var count = 0
    func visit(_ element: AXUIElement, _ path: String, _ depth: Int) {
        guard count < 300, depth <= 12 else { return }
        let role = axString(element, kAXRoleAttribute as String) ?? ""
        let subrole = axString(element, kAXSubroleAttribute as String) ?? ""
        let title = axString(element, kAXTitleAttribute as String) ?? ""
        let description = axString(element, kAXDescriptionAttribute as String) ?? ""
        let value = subrole == "AXSecureTextField"
            ? ""
            : (axString(element, kAXValueAttribute as String) ?? "")
        let actions = axActions(element)
        var relative = [0.0, 0.0, 0.0, 0.0]
        if let frame = axFrame(element) {
            relative = [
                frame.minX - windowFrame.minX,
                frame.minY - windowFrame.minY,
                frame.width,
                frame.height,
            ]
        }
        if depth == 0 || !role.isEmpty || !title.isEmpty || !description.isEmpty || !actions.isEmpty {
            result.append([
                "element_id": "ax-\(path)",
                "role": role,
                "title": title,
                "description": description,
                "value": value,
                "frame": relative,
                "enabled": axBool(element, kAXEnabledAttribute as String) ?? true,
                "focused": axBool(element, kAXFocusedAttribute as String) ?? false,
                "actions": actions,
            ])
            count += 1
        }
        let children = axChildren(element)
        for (index, child) in children.enumerated() {
            visit(child, "\(path).\(index)", depth + 1)
        }
    }
    visit(root, "0", 0)
    return result
}

func axResolve(root: AXUIElement, elementID: String) -> AXUIElement? {
    guard elementID.hasPrefix("ax-") else { return nil }
    let parts = elementID.dropFirst(3).split(separator: ".", omittingEmptySubsequences: false)
    guard !parts.isEmpty, parts.count <= 32 else { return nil }
    var components: [Int] = []
    for part in parts {
        guard !part.isEmpty, part.allSatisfy(\.isNumber), let index = Int(part) else { return nil }
        components.append(index)
    }
    guard components.first == 0 else { return nil }
    components.removeFirst()
    var current = root
    for index in components {
        let children = axChildren(current)
        guard index >= 0, index < children.count else { return nil }
        current = children[index]
    }
    return current
}

func cgWindowOrder() -> [UInt32] {
    guard let list = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements],
        kCGNullWindowID
    ) as? [[String: Any]] else { return [] }
    return list.compactMap { entry in
        (entry[kCGWindowNumber as String] as? NSNumber)?.uint32Value
    }
}

@available(macOS 14.0, *)
@main struct ComputerBridge {
    static func main() async {
        // SCScreenshotManager 在某些 CLI 进程中会因 CoreGraphics 尚未初始化而触发
        // CGS_REQUIRE_INIT 断言；先建立 AppKit/CoreGraphics 连接，再处理请求。
        _ = NSApplication.shared
        _ = CGMainDisplayID()
        do {
            let data = FileHandle.standardInput.readDataToEndOfFile()
            guard data.count <= 65536,
                  let request = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let operation = request["operation"] as? String else { throw fail("invalid_request") }
            let result = try await execute(operation, request)
            FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result))
        } catch {
            let reason = (error as? BridgeFailure)?.message ?? "native_operation_failed"
            if let data = try? JSONSerialization.data(withJSONObject: ["error": reason]) {
                FileHandle.standardOutput.write(data)
            }
            exit(1)
        }
    }

    static func execute(_ operation: String, _ request: [String: Any]) async throws -> [String: Any] {
        if operation == "permissions" {
            return ["screen_capture": CGPreflightScreenCaptureAccess(), "accessibility": AXIsProcessTrusted()]
        }
        if operation == "request_permissions" {
            // 只触发 macOS 的同意对话框，不自行授予权限；用户仍须在系统设置中确认。
            let screen = CGRequestScreenCaptureAccess()
            let promptKey = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
            let accessibility = AXIsProcessTrustedWithOptions([promptKey: true] as CFDictionary)
            return [
                "screen_capture": screen,
                "accessibility": accessibility,
                "note": "授权后重启终端/宿主，并重新运行 permissions 检查。"
            ]
        }
        guard CGPreflightScreenCaptureAccess() else { throw fail("screen_capture_permission_required") }
        let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
        if operation == "windows" {
            return ["windows": content.windows.filter { $0.windowLayer == 0 && $0.frame.width > 1 && $0.frame.height > 1 }.map {
                ["window_id": $0.windowID, "pid": $0.owningApplication?.processID ?? 0,
                 "title": $0.title ?? "", "application": $0.owningApplication?.applicationName ?? ""] as [String: Any]
            }]
        }
        if operation == "apps" {
            // 应用列表不依赖 ScreenCaptureKit/屏幕录制权限；CGWindowList 的窗口元数据足够做应用选择。
            guard let list = CGWindowListCopyWindowInfo(
                [.optionOnScreenOnly, .excludeDesktopElements],
                kCGNullWindowID
            ) as? [[String: Any]] else { throw fail("window_list_unavailable") }
            var byPid: [Int32: (Int, [String: Any])] = [:]
            for (index, entry) in list.enumerated() {
                guard let pid = (entry[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
                      let windowID = (entry[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
                      let boundsDict = entry[kCGWindowBounds as String] as? [String: Any],
                      let bounds = CGRect(dictionaryRepresentation: boundsDict as CFDictionary),
                      bounds.width > 1, bounds.height > 1,
                      let layer = (entry[kCGWindowLayer as String] as? NSNumber)?.intValue,
                      layer == 0,
                      byPid[pid] == nil,
                      let app = NSRunningApplication(processIdentifier: pid) else { continue }
                let name = app.localizedName ?? (entry[kCGWindowOwnerName as String] as? String) ?? ""
                let windowTitle = (entry[kCGWindowName as String] as? String) ?? ""
                if isExcludedApplication(name)
                    || isExcludedApplication(windowTitle)
                    || isSensitiveApplication(name)
                    || isSensitiveApplication(windowTitle) { continue }
                let bundle = app.bundleIdentifier ?? ""
                let appId = bundle.isEmpty ? "name:\(name)" : bundle
                byPid[pid] = (index, [
                    "app_id": appId,
                    "bundle_id": bundle,
                    "application": name,
                    "pid": pid,
                    "window_id": windowID,
                    "title": windowTitle,
                ])
            }
            let apps = byPid.values.map(\.1).sorted {
                ($0["application"] as? String ?? "") < ($1["application"] as? String ?? "")
            }
            return ["apps": apps]
        }
        guard let windowID = (request["window_id"] as? NSNumber)?.uint32Value,
              let pid = (request["pid"] as? NSNumber)?.int32Value,
              let window = content.windows.first(where: { $0.windowID == windowID && $0.owningApplication?.processID == pid }),
              window.windowLayer == 0, window.frame.width > 1, window.frame.height > 1 else { throw fail("target_window_missing") }
        let frame = window.frame
        if operation == "preview" {
            // 产品侧栏的预览帧：不创建 observation、不消耗动作令牌，也不读取 AX 树。
            let filter = SCContentFilter(desktopIndependentWindow: window)
            let configuration = SCStreamConfiguration()
            let scale = min(1.0, 1280.0 / max(frame.width, frame.height))
            configuration.width = max(1, Int(frame.width * scale))
            configuration.height = max(1, Int(frame.height * scale))
            configuration.showsCursor = false
            configuration.ignoreShadowsSingleWindow = true
            let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
            guard let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]), png.count <= 5 * 1024 * 1024 else { throw fail("screenshot_encoding_failed") }
            return ["png_base64": png.base64EncodedString(), "width": image.width, "height": image.height,
                    "frame": [frame.origin.x, frame.origin.y, frame.width, frame.height]]
        }
        if operation == "observe" {
            let filter = SCContentFilter(desktopIndependentWindow: window)
            let configuration = SCStreamConfiguration()
            // 固定为窗口逻辑点大小；返回像素尺寸用于 Rust 校验和坐标转换。
            let scale = min(1.0, 1600.0 / max(frame.width, frame.height))
            configuration.width = max(1, Int(frame.width * scale))
            configuration.height = max(1, Int(frame.height * scale))
            configuration.showsCursor = false
            configuration.ignoreShadowsSingleWindow = true
            let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
            guard let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]), png.count <= 5 * 1024 * 1024 else { throw fail("screenshot_encoding_failed") }
            var elements: [[String: Any]] = []
            if AXIsProcessTrusted(), let root = axWindowElement(pid: pid, frame: frame) {
                elements = axSnapshot(root: root, windowFrame: frame)
            }
            return ["png_base64": png.base64EncodedString(), "width": image.width, "height": image.height,
                    "frame": [frame.origin.x, frame.origin.y, frame.width, frame.height],
                    "elements": elements]
        }
        guard AXIsProcessTrusted() else { throw fail("accessibility_permission_required") }
        // 用户切换前台或窗口位置变化后，动作会在派发前失败并要求重新观察。
        guard let rawExpected = request["frame"] as? [NSNumber], rawExpected.count == 4 else { throw fail("window_moved_reobserve") }
        let expected = rawExpected.map(\.doubleValue)
        let current = [frame.origin.x, frame.origin.y, frame.width, frame.height]
        guard zip(expected, current).allSatisfy({ abs($0 - $1) < 0.5 }) else { throw fail("window_moved_reobserve") }
        // 每次动作前把已同意的目标应用带到前台；终端审批可能刚抢走焦点。
        // 只激活已选定 pid，不遍历或启动其他应用；激活是异步的，重试几次再判定失败。
        if let target = NSRunningApplication(processIdentifier: pid) {
            for _ in 0..<3 {
                if NSWorkspace.shared.frontmostApplication?.processIdentifier == pid {
                    break
                }
                target.unhide()
                target.activate()
                try? await Task.sleep(for: .milliseconds(300))
            }
        }
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid else { throw fail("target_not_frontmost") }
        let application = AXUIElementCreateApplication(pid)
        var focusedValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(application, kAXFocusedWindowAttribute as CFString, &focusedValue) == .success,
              let focusedValue, CFGetTypeID(focusedValue) == AXUIElementGetTypeID() else { throw fail("focused_window_unavailable") }
        let focused = unsafeBitCast(focusedValue, to: AXUIElement.self)
        var positionValue: CFTypeRef?
        var sizeValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(focused, kAXPositionAttribute as CFString, &positionValue) == .success,
              AXUIElementCopyAttributeValue(focused, kAXSizeAttribute as CFString, &sizeValue) == .success,
              let positionValue, let sizeValue,
              CFGetTypeID(positionValue) == AXValueGetTypeID(), CFGetTypeID(sizeValue) == AXValueGetTypeID() else { throw fail("focused_geometry_unavailable") }
        var position = CGPoint.zero; var size = CGSize.zero
        guard AXValueGetValue(unsafeBitCast(positionValue, to: AXValue.self), .cgPoint, &position),
              AXValueGetValue(unsafeBitCast(sizeValue, to: AXValue.self), .cgSize, &size),
              abs(position.x - frame.minX) < 0.5, abs(position.y - frame.minY) < 0.5,
              abs(size.width - frame.width) < 0.5, abs(size.height - frame.height) < 0.5 else { throw fail("different_focused_window") }
        // 把已同意的窗口抬到最前，避免同一应用其他窗口遮挡目标。
        AXUIElementPerformAction(focused, kAXRaiseAction as CFString)
        guard let source = CGEventSource(stateID: .privateState) else { throw fail("event_source_failed") }

        func postText(_ text: String) throws {
            guard !text.isEmpty, text.utf16.count <= 256,
                  !text.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { throw fail("invalid_text") }
            guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                  let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else { throw fail("event_creation_failed") }
            let units = Array(text.utf16)
            units.withUnsafeBufferPointer { buffer in
                down.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
                up.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
            }
            down.postToPid(pid); up.postToPid(pid)
        }

        switch operation {
        case "click":
            guard let x = (request["x"] as? NSNumber)?.doubleValue,
                  let y = (request["y"] as? NSNumber)?.doubleValue,
                  x.isFinite, y.isFinite, x >= 0, y >= 0, x < frame.width, y < frame.height else { throw fail("coordinates_outside_window") }
            let point = CGPoint(x: frame.minX + x, y: frame.minY + y)
            // 按钮/链接等控件优先使用 AXPress：postToPid 鼠标事件在部分 AppKit 控件上
            // 不携带窗口上下文，可能不触发 action。命中点仍由已同意窗口的 frame 约束。
            let system = AXUIElementCreateSystemWide()
            var hit: AXUIElement?
            if AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &hit) == .success,
               let hit {
                var roleValue: CFTypeRef?
                if AXUIElementCopyAttributeValue(hit, kAXRoleAttribute as CFString, &roleValue) == .success,
                   let role = roleValue as? String,
                   role == kAXTextFieldRole as String || role == kAXTextAreaRole as String {
                    AXUIElementSetAttributeValue(hit, kAXFocusedAttribute as CFString, kCFBooleanTrue)
                }
                var actionNames: CFArray?
                if AXUIElementCopyActionNames(hit, &actionNames) == .success,
                   let names = actionNames as? [String],
                   names.contains(kAXPressAction as String),
                   AXUIElementPerformAction(hit, kAXPressAction as CFString) == .success {
                    return ["dispatched": true, "verification_required": true]
                }
            }
            guard let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
                  let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left) else { throw fail("event_creation_failed") }
            // AppKit 控件需要知道事件属于哪个窗口；仅 postToPid 时窗口号为 0，按钮可能不响应。
            for event in [down, up] {
                event.setIntegerValueField(.mouseEventClickState, value: 1)
                event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(windowID))
                event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(windowID))
            }
            down.postToPid(pid); up.postToPid(pid)
        case "scroll":
            guard let x = (request["x"] as? NSNumber)?.doubleValue,
                  let y = (request["y"] as? NSNumber)?.doubleValue,
                  let deltaY = (request["delta_y"] as? NSNumber)?.doubleValue,
                  x.isFinite, y.isFinite, deltaY.isFinite,
                  x >= 0, y >= 0, x < frame.width, y < frame.height,
                  abs(deltaY) >= 1, abs(deltaY) <= 1000 else { throw fail("coordinates_outside_window") }
            let point = CGPoint(x: frame.minX + x, y: frame.minY + y)
            guard let scroll = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 1,
                                       wheel1: Int32(deltaY.rounded()), wheel2: 0, wheel3: 0) else { throw fail("event_creation_failed") }
            scroll.location = point
            scroll.postToPid(pid)
        case "type_text":
            guard let text = request["text"] as? String else { throw fail("invalid_text") }
            try postText(text)
        case "replace_text":
            guard let text = request["text"] as? String,
                  !text.isEmpty, text.utf16.count <= 256,
                  !text.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { throw fail("invalid_text") }
            // 原生输入框优先通过 AX 直接写值，避免 Cmd+A 依赖键盘焦点。
            let system = AXUIElementCreateSystemWide()
            var focusedValue: CFTypeRef?
            if AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute as CFString, &focusedValue) == .success,
               let focusedValue, CFGetTypeID(focusedValue) == AXUIElementGetTypeID() {
                let focused = unsafeBitCast(focusedValue, to: AXUIElement.self)
                var roleValue: CFTypeRef?
                if AXUIElementCopyAttributeValue(focused, kAXRoleAttribute as CFString, &roleValue) == .success,
                   let role = roleValue as? String,
                   role == kAXTextFieldRole as String || role == kAXTextAreaRole as String || role == kAXComboBoxRole as String,
                   AXUIElementSetAttributeValue(focused, kAXValueAttribute as CFString, text as CFString) == .success {
                    var check: CFTypeRef?
                    if AXUIElementCopyAttributeValue(focused, kAXValueAttribute as CFString, &check) == .success,
                       let check = check as? String, check == text {
                        return ["dispatched": true, "verification_required": true]
                    }
                }
            }
            // 回退：全选后输入，替换整个焦点输入框。
            guard let selectDown = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                  let selectUp = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else { throw fail("event_creation_failed") }
            selectDown.flags = .maskCommand
            selectUp.flags = .maskCommand
            selectDown.postToPid(pid); selectUp.postToPid(pid)
            try? await Task.sleep(for: .milliseconds(80))
            try postText(text)
        case "click_element":
            guard let elementID = request["element_id"] as? String,
                  elementID.count <= 128,
                  let element = axResolve(root: focused, elementID: elementID) else {
                throw fail("element_unavailable")
            }
            // 优先 AXPress；没有 press action 的控件再按元素中心投递鼠标。
            if axActions(element).contains(kAXPressAction as String) {
                guard AXUIElementPerformAction(element, kAXPressAction as CFString) == .success else {
                    throw fail("element_press_failed")
                }
                return ["dispatched": true, "verification_required": true]
            }
            guard let rect = axFrame(element) else { throw fail("element_frame_unavailable") }
            let x = rect.midX - frame.minX
            let y = rect.midY - frame.minY
            guard x >= 0, y >= 0, x < frame.width, y < frame.height else { throw fail("element_outside_window") }
            let point = CGPoint(x: frame.minX + x, y: frame.minY + y)
            guard let down = CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
                  let up = CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left) else {
                throw fail("event_creation_failed")
            }
            for event in [down, up] {
                event.setIntegerValueField(.mouseEventClickState, value: 1)
                event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(windowID))
                event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(windowID))
            }
            down.postToPid(pid); up.postToPid(pid)
        case "type_element":
            guard let elementID = request["element_id"] as? String,
                  elementID.count <= 128,
                  let element = axResolve(root: focused, elementID: elementID) else {
                throw fail("element_unavailable")
            }
            let subrole = axString(element, kAXSubroleAttribute as String) ?? ""
            guard subrole != "AXSecureTextField" else { throw fail("secure_element_blocked") }
            guard let text = request["text"] as? String,
                  !text.isEmpty, text.utf16.count <= 256,
                  !text.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else {
                throw fail("invalid_text")
            }
            let clear = (request["clear"] as? Bool) ?? true
            guard AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success else {
                throw fail("element_focus_failed")
            }
            let role = axString(element, kAXRoleAttribute as String) ?? ""
            if ["AXTextField", "AXTextArea", "AXComboBox"].contains(role) {
                let current = axString(element, kAXValueAttribute as String) ?? ""
                let target = clear ? text : current + text
                if AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, target as CFString) == .success {
                    var check: CFTypeRef?
                    if AXUIElementCopyAttributeValue(element, kAXValueAttribute as CFString, &check) == .success,
                       let check = check as? String, check == target {
                        return ["dispatched": true, "verification_required": true]
                    }
                }
            }
            // 非标准输入控件回退到全选 + 向目标 pid 输入；仍绑定同一已观察窗口。
            if clear {
                guard let selectDown = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                      let selectUp = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
                    throw fail("event_creation_failed")
                }
                selectDown.flags = .maskCommand
                selectUp.flags = .maskCommand
                selectDown.postToPid(pid); selectUp.postToPid(pid)
                try? await Task.sleep(for: .milliseconds(80))
            }
            try postText(text)
        case "key":
            let codes: [String: CGKeyCode] = ["enter":36,"tab":48,"escape":53,"backspace":51,"left":123,"right":124,"down":125,"up":126,"a":0,"l":37]
            guard let rawKey = request["key"] as? String else { throw fail("unsupported_key") }
            let command = rawKey.hasPrefix("cmd+")
            let key = command ? String(rawKey.dropFirst(4)) : rawKey
            guard let code = codes[key],
                  let down = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true),
                  let up = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false) else { throw fail("unsupported_key") }
            if command {
                down.flags = .maskCommand
                up.flags = .maskCommand
            }
            down.postToPid(pid); up.postToPid(pid)
        default: throw fail("unsupported_operation")
        }
        // Quartz 投递没有应用消费回执；绝不把已投递称作目标任务完成。
        return ["dispatched": true, "verification_required": true]
    }
}
