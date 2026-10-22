// Yukii 窗口选择器：展示可控制窗口缩略图，用户显式允许后才交给宿主。
// 只负责选择和同意，不执行任何桌面动作；stdout 仅输出 JSON 事件。
import AppKit
import CoreGraphics
import ScreenCaptureKit

private let outputQueue = DispatchQueue(label: "yukii.picker.output")
func debug(_ message: String) {
    guard ProcessInfo.processInfo.environment["YUKII_PICKER_DEBUG"] == "1" else { return }
    FileHandle.standardError.write(Data(("picker: " + message + "\n").utf8))
}
func emit(_ event: String, _ extra: [String: Any] = [:]) {
    var payload = extra
    payload["event"] = event
    guard let data = try? JSONSerialization.data(withJSONObject: payload) else { return }
    var line = data
    line.append(0x0a)
    outputQueue.sync { FileHandle.standardOutput.write(line) }
}

final class WindowPickerController: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var panel: NSWindow!
    private let statusLabel = NSTextField(labelWithString: "正在读取窗口…")
    private var allowButton: NSButton!
    private var refreshButton: NSButton!
    private var stack: NSStackView!
    private var rows: [NSButton] = []
    private var candidates: [SCWindow] = []
    private var selected: Int?
    private var generation = 0

    func applicationDidFinishLaunching(_ notification: Notification) {
        debug("picker launched")
        buildPanel()
        Task {
            debug("refresh task started")
            await refresh()
            debug("refresh task finished")
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    private func buildPanel() {
        let rect = NSRect(x: 220, y: 180, width: 980, height: 720)
        panel = NSWindow(
            contentRect: rect,
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        panel.title = "Yukii Control — 选择窗口"
        panel.delegate = self
        panel.isReleasedWhenClosed = false
        let content = NSView(frame: NSRect(origin: .zero, size: rect.size))
        panel.contentView = content

        let title = NSTextField(labelWithString: "选择要交给 Agent 控制的窗口")
        title.frame = NSRect(x: 20, y: 676, width: 940, height: 26)
        title.font = .systemFont(ofSize: 19, weight: .semibold)
        let hint = NSTextField(labelWithString: "点击窗口缩略图，然后点“允许控制”。终端/控制台不会被列出。允许后 Agent 可在该窗口内点击/输入，直到你暂停或停止。")
        hint.frame = NSRect(x: 20, y: 648, width: 940, height: 20)
        hint.font = .systemFont(ofSize: 12)
        hint.textColor = .secondaryLabelColor
        hint.lineBreakMode = .byTruncatingTail

        let scroll = NSScrollView(frame: NSRect(x: 20, y: 96, width: 940, height: 540))
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        scroll.documentView = stack
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: scroll.contentView.trailingAnchor, constant: -8),
            stack.topAnchor.constraint(equalTo: scroll.contentView.topAnchor, constant: 8),
            stack.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor, constant: -16),
        ])

        statusLabel.frame = NSRect(x: 20, y: 62, width: 600, height: 20)
        statusLabel.font = .systemFont(ofSize: 12)
        statusLabel.lineBreakMode = .byTruncatingTail

        allowButton = NSButton(title: "允许控制", target: self, action: #selector(allowControl))
        allowButton.frame = NSRect(x: 700, y: 20, width: 120, height: 32)
        allowButton.isEnabled = false
        allowButton.keyEquivalent = "\r"
        refreshButton = NSButton(title: "刷新", target: self, action: #selector(refreshTapped))
        refreshButton.frame = NSRect(x: 830, y: 20, width: 100, height: 32)
        let cancel = NSButton(title: "取消", target: self, action: #selector(cancel))
        cancel.frame = NSRect(x: 590, y: 20, width: 100, height: 32)

        content.addSubview(title)
        content.addSubview(hint)
        content.addSubview(scroll)
        content.addSubview(statusLabel)
        content.addSubview(allowButton)
        content.addSubview(refreshButton)
        content.addSubview(cancel)
        panel.orderFrontRegardless()
    }

    @objc private func refreshTapped() {
        Task { await refresh() }
    }

    private func refresh() async {
        debug("refresh begin")
        generation += 1
        let current = generation
        DispatchQueue.main.async {
            self.selected = nil
            self.allowButton.isEnabled = false
            self.statusLabel.stringValue = "正在读取窗口…"
            self.rows.forEach { $0.removeFromSuperview() }
            self.rows = []
        }
        debug("preflight \(CGPreflightScreenCaptureAccess())")
        guard CGPreflightScreenCaptureAccess() else {
            DispatchQueue.main.async {
                self.statusLabel.stringValue = "需要屏幕录制权限"
                emit("error", ["message": "需要屏幕录制权限"])
            }
            return
        }
        do {
            debug("requesting shareable content")
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            debug("got \(content.windows.count) windows")
            let forbidden = ["Terminal", "iTerm2", "Console", "Yukii", "Yukii Control", "window-picker", "preview-panel"]
            let candidates = content.windows
                .filter { window in
                    let app = window.owningApplication?.applicationName ?? ""
                    let title = window.title ?? ""
                    return window.isOnScreen
                        && window.frame.width >= 200
                        && window.frame.height >= 120
                        && !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        && !title.localizedCaseInsensitiveContains("Backstop")
                        && !title.localizedCaseInsensitiveContains("Wallpaper")
                        && app != "Window Server"
                        && app != "Dock"
                        && !forbidden.contains(app)
                        && !title.localizedCaseInsensitiveContains("Yukii · 办公助手")
                        && app != "Yukii Control"
                }
                .sorted {
                    let left = ($0.owningApplication?.applicationName ?? "", $0.title ?? "")
                    let right = ($1.owningApplication?.applicationName ?? "", $1.title ?? "")
                    return left < right
                }
                .prefix(24)
            let list = Array(candidates)
            DispatchQueue.main.async {
                guard current == self.generation else { return }
                self.candidates = list
                self.buildRows()
                self.statusLabel.stringValue = list.isEmpty
                    ? "没有可控制窗口；请先打开目标应用，再点刷新。"
                    : "共 \(list.count) 个窗口；点击缩略图选择。"
            }
            await captureThumbnails(for: list, generation: current)
        } catch {
            DispatchQueue.main.async {
                guard current == self.generation else { return }
                self.statusLabel.stringValue = "无法读取窗口：\(error.localizedDescription)"
                emit("error", ["message": "无法读取窗口：\(error.localizedDescription)"])
            }
        }
    }

    private func buildRows() {
        rows.forEach { $0.removeFromSuperview() }
        rows = []
        for (index, window) in candidates.enumerated() {
            let app = window.owningApplication?.applicationName ?? "未知应用"
            let button = NSButton(
                title: "\(app) — \(window.title ?? "")  [窗口 \(window.windowID)]",
                target: self,
                action: #selector(selectRow(_:))
            )
            button.tag = index
            button.setButtonType(.toggle)
            button.bezelStyle = .regularSquare
            button.alignment = .left
            button.imagePosition = .imageLeading
            button.imageScaling = .scaleProportionallyDown
            button.font = .systemFont(ofSize: 13)
            button.translatesAutoresizingMaskIntoConstraints = false
            button.heightAnchor.constraint(equalToConstant: 112).isActive = true
            stack.addArrangedSubview(button)
            button.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
            rows.append(button)
        }
    }

    private func captureThumbnails(for windows: [SCWindow], generation current: Int) async {
        for (index, window) in windows.enumerated() {
            guard current == generation else { return }
            let filter = SCContentFilter(desktopIndependentWindow: window)
            let configuration = SCStreamConfiguration()
            configuration.width = 180
            configuration.height = 110
            configuration.showsCursor = false
            if let image = try? await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration) {
                DispatchQueue.main.async {
                    guard current == self.generation, index < self.rows.count else { return }
                    self.rows[index].image = NSImage(
                        cgImage: image,
                        size: NSSize(width: 180, height: 110)
                    )
                }
            }
        }
    }

    @objc private func selectRow(_ sender: NSButton) {
        guard sender.tag >= 0, sender.tag < candidates.count else { return }
        selected = sender.tag
        for (index, row) in rows.enumerated() {
            row.state = index == sender.tag ? .on : .off
        }
        let window = candidates[sender.tag]
        let app = window.owningApplication?.applicationName ?? "未知应用"
        statusLabel.stringValue = "已选择：\(app) — \(window.title ?? "")；点击“允许控制”开始。"
        allowButton.isEnabled = true
    }

    @objc private func allowControl() {
        guard let index = selected, index < candidates.count else { return }
        let window = candidates[index]
        let app = window.owningApplication?.applicationName ?? "未知应用"
        let pid = window.owningApplication?.processID ?? 0
        guard pid > 0 else {
            statusLabel.stringValue = "该窗口没有可控制的进程。"
            return
        }
        allowButton.isEnabled = false
        statusLabel.stringValue = "已允许控制，正在打开预览…"
        emit("selected", [
            "window_id": window.windowID,
            "pid": pid,
            "title": window.title ?? "",
            "application": app,
        ])
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { NSApp.terminate(nil) }
    }

    @objc private func cancel() {
        emit("cancelled")
        NSApp.terminate(nil)
    }

    func windowWillClose(_ notification: Notification) {
        emit("cancelled")
        NSApp.terminate(nil)
    }
}

@main
struct WindowPickerMain {
    @MainActor static var retainedController: WindowPickerController?

    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let controller = WindowPickerController()
        retainedController = controller
        app.delegate = controller
        app.run()
    }
}
