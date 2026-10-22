// 项目自有的 Computer Use 验收窗口：显示随机验证码，接收输入并落盘提交结果。
// 不含用户资料；只用于受控测试。
import AppKit

final class Controller: NSObject, NSApplicationDelegate {
    private let token = String(UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(6)).uppercased()
    private let window = NSWindow(
        contentRect: NSRect(x: 220, y: 220, width: 560, height: 300),
        styleMask: [.titled, .closable, .miniaturizable, .resizable],
        backing: .buffered,
        defer: false
    )
    private let field = NSTextField()
    private let status = NSTextField(labelWithString: "等待提交")
    private var stateURL: URL {
        URL(fileURLWithPath: CommandLine.arguments[0])
            .deletingLastPathComponent()
            .appendingPathComponent("cu-test-state.json")
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        window.title = "Yukii CU Test"
        window.isReleasedWhenClosed = false
        window.contentView = NSView(frame: window.contentRect(forFrameRect: window.frame))

        let heading = NSTextField(labelWithString: "Yukii Computer Use 验收窗口")
        heading.font = .systemFont(ofSize: 20, weight: .semibold)
        heading.frame = NSRect(x: 24, y: 236, width: 512, height: 28)

        let hint = NSTextField(labelWithString: "请读取验证码，点击输入框输入并提交")
        hint.textColor = .secondaryLabelColor
        hint.frame = NSRect(x: 24, y: 206, width: 512, height: 20)

        let tokenLabel = NSTextField(labelWithString: "验证码：\(token)")
        tokenLabel.font = .monospacedSystemFont(ofSize: 32, weight: .bold)
        tokenLabel.frame = NSRect(x: 24, y: 146, width: 512, height: 44)

        field.frame = NSRect(x: 24, y: 92, width: 360, height: 30)
        field.placeholderString = "在此输入验证码"
        field.font = .systemFont(ofSize: 16)
        field.target = self
        field.action = #selector(submit)

        let button = NSButton(title: "提交", target: self, action: #selector(submit))
        button.frame = NSRect(x: 398, y: 92, width: 110, height: 30)
        button.keyEquivalent = "\r"

        status.frame = NSRect(x: 24, y: 40, width: 512, height: 36)
        status.font = .systemFont(ofSize: 16)
        status.lineBreakMode = .byTruncatingTail

        for view in [heading, hint, tokenLabel, field, button, status] {
            window.contentView?.addSubview(view)
        }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        window.makeFirstResponder(field)

        print("Yukii CU Test window ready")
        print("state=\(stateURL.path)")
        print("token=\(token)")
        writeState(submitted: nil)
    }

    @objc private func submit() {
        let text = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            status.stringValue = "请输入验证码"
            return
        }
        status.stringValue = "已收到：\(text)"
        writeState(submitted: text)
        print("submitted=\(text)")
    }

    private func writeState(submitted: String?) {
        let screen = NSScreen.main?.frame ?? .zero
        let frame = window.frame
        var payload: [String: Any] = [
            "window_id": UInt32(window.windowNumber),
            "pid": ProcessInfo.processInfo.processIdentifier,
            "title": window.title,
            "token": token,
            "submitted": submitted ?? NSNull(),
            "screen_frame": [screen.origin.x, screen.origin.y, screen.size.width, screen.size.height],
            "window_frame": [frame.origin.x, frame.origin.y, frame.size.width, frame.size.height],
        ]
        payload["submitted_at"] = submitted == nil ? NSNull() : Int(Date().timeIntervalSince1970)
        if let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys, .prettyPrinted]) {
            try? data.write(to: stateURL, options: .atomic)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

@main
struct TestWindowMain {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let controller = Controller()
        app.delegate = controller
        app.run()
    }
}
