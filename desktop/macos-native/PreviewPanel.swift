// Yukii 临时实时预览/控制面板：SCStream 显示受控窗口，提供暂停/继续/停止。
// 这是原生控制助手，不是最终 Chromium 147 产品 UI；stdout 只输出 JSON 事件。
import AppKit
import CoreImage
import CoreMedia
import CoreVideo
import ScreenCaptureKit

struct PanelFailure: Error { let message: String }
func failure(_ message: String) -> PanelFailure { PanelFailure(message: message) }

private let outputQueue = DispatchQueue(label: "yukii.preview.output")
func emit(_ event: String, _ extra: [String: Any] = [:]) {
    var payload = extra
    payload["event"] = event
    guard let data = try? JSONSerialization.data(withJSONObject: payload) else { return }
    var line = data
    line.append(0x0a)
    outputQueue.sync { FileHandle.standardOutput.write(line) }
}

struct Arguments {
    let windowID: UInt32
    let pid: Int32
    let title: String
}

func parseArguments() -> Arguments? {
    var windowID: UInt32?
    var pid: Int32?
    var title = "受控窗口"
    var arguments = CommandLine.arguments.dropFirst().makeIterator()
    while let flag = arguments.next() {
        switch flag {
        case "--window-id": windowID = arguments.next().flatMap { UInt32($0) }
        case "--pid": pid = arguments.next().flatMap { Int32($0) }
        case "--title": title = arguments.next() ?? title
        default: break
        }
    }
    guard let windowID, let pid else { return nil }
    return Arguments(windowID: windowID, pid: pid, title: title)
}

final class PreviewController: NSObject, NSApplicationDelegate, SCStreamOutput, SCStreamDelegate, NSWindowDelegate {
    private let arguments: Arguments
    private var panel: NSWindow!
    private let imageView = NSImageView()
    private let statusLabel = NSTextField(labelWithString: "准备预览")
    private var pauseButton: NSButton!
    private var stopButton: NSButton!
    private var stream: SCStream?
    private var paused = false
    private let ciContext = CIContext(options: [.cacheIntermediates: false])
    private let frameQueue = DispatchQueue(label: "yukii.preview.frames")

    init(arguments: Arguments) {
        self.arguments = arguments
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildPanel()
        readCommands()
        Task { await startStream() }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        if let stream { stream.stopCapture { _ in } }
    }

    private func buildPanel() {
        let rect = NSRect(x: 180, y: 160, width: 1000, height: 740)
        panel = NSWindow(
            contentRect: rect,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        panel.title = "Yukii Control"
        panel.isReleasedWhenClosed = false
        panel.delegate = self
        let content = NSView(frame: NSRect(origin: .zero, size: rect.size))
        panel.contentView = content

        statusLabel.frame = NSRect(x: 20, y: 700, width: 960, height: 24)
        statusLabel.font = .systemFont(ofSize: 15, weight: .medium)
        statusLabel.lineBreakMode = .byTruncatingTail
        statusLabel.autoresizingMask = [.width, .minYMargin]

        imageView.frame = NSRect(x: 20, y: 70, width: 960, height: 620)
        imageView.imageScaling = .scaleProportionallyUpOrDown
        imageView.wantsLayer = true
        imageView.layer?.backgroundColor = NSColor.black.cgColor
        imageView.layer?.cornerRadius = 8
        imageView.autoresizingMask = [.width, .height]

        pauseButton = NSButton(title: "暂停", target: self, action: #selector(togglePause))
        pauseButton.frame = NSRect(x: 20, y: 20, width: 120, height: 32)
        pauseButton.autoresizingMask = [.maxYMargin]
        stopButton = NSButton(title: "停止", target: self, action: #selector(stop))
        stopButton.frame = NSRect(x: 150, y: 20, width: 120, height: 32)
        stopButton.autoresizingMask = [.maxYMargin]

        content.addSubview(statusLabel)
        content.addSubview(imageView)
        content.addSubview(pauseButton)
        content.addSubview(stopButton)
        panel.orderFrontRegardless()
    }

    private func startStream() async {
        guard CGPreflightScreenCaptureAccess() else {
            reportError("需要屏幕录制权限")
            return
        }
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            guard let target = content.windows.first(where: {
                $0.windowID == arguments.windowID && $0.owningApplication?.processID == arguments.pid
            }) else {
                reportError("目标窗口已不可用")
                return
            }
            let filter = SCContentFilter(desktopIndependentWindow: target)
            let configuration = SCStreamConfiguration()
            let scale = min(1.0, 960.0 / max(target.frame.width, 1))
            configuration.width = max(1, Int(target.frame.width * scale))
            configuration.height = max(1, Int(target.frame.height * scale))
            configuration.showsCursor = false
            configuration.minimumFrameInterval = CMTime(value: 1, timescale: 10)
            configuration.pixelFormat = kCVPixelFormatType_32BGRA
            configuration.queueDepth = 3
            let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: frameQueue)
            try await stream.startCapture()
            self.stream = stream
            setStatus("正在预览 · \(Int(target.frame.width))×\(Int(target.frame.height))")
            emit("ready", ["width": configuration.width, "height": configuration.height])
        } catch {
            reportError("无法启动预览：\(error.localizedDescription)")
        }
    }

    private func setStatus(_ text: String) {
        DispatchQueue.main.async { self.statusLabel.stringValue = text }
    }

    private func reportError(_ message: String) {
        setStatus(message)
        emit("error", ["message": message])
    }

    @objc private func togglePause() {
        paused.toggle()
        pauseButton.title = paused ? "继续" : "暂停"
        statusLabel.stringValue = paused ? "已暂停：新的桌面动作会被拒绝" : "正在预览"
        emit(paused ? "paused" : "resumed")
    }

    @objc private func stop() {
        emit("stopped")
        statusLabel.stringValue = "已停止"
        pauseButton.isEnabled = false
        stopButton.isEnabled = false
        stream?.stopCapture { _ in }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { NSApp.terminate(nil) }
    }

    private func readCommands() {
        DispatchQueue.global(qos: .utility).async {
            while let line = readLine(strippingNewline: true) {
                guard let data = line.data(using: .utf8),
                      let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let command = object["command"] as? String else { continue }
                let text = object["text"] as? String
                DispatchQueue.main.async { self.handle(command: command, text: text) }
            }
            DispatchQueue.main.async {
                if self.stream != nil { emit("closed") }
                NSApp.terminate(nil)
            }
        }
    }

    private func handle(command: String, text: String?) {
        switch command {
        case "stop": stop()
        case "pause": if !paused { togglePause() }
        case "resume": if paused { togglePause() }
        case "status": if let text { statusLabel.stringValue = text }
        case "close": NSApp.terminate(nil)
        default: break
        }
    }

    func windowWillClose(_ notification: Notification) {
        emit("closed")
        NSApp.terminate(nil)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let image = CIImage(cvPixelBuffer: pixelBuffer)
        guard let cgImage = ciContext.createCGImage(image, from: image.extent) else { return }
        DispatchQueue.main.async {
            self.imageView.image = NSImage(cgImage: cgImage, size: NSSize(width: cgImage.width, height: cgImage.height))
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        DispatchQueue.main.async {
            self.reportError("预览已停止：\(error.localizedDescription)")
        }
    }
}

@main
struct PreviewPanelMain {
    static func main() {
        guard let arguments = parseArguments() else {
            FileHandle.standardError.write(Data("usage: preview-panel --window-id <id> --pid <pid> [--title <text>]\n".utf8))
            exit(2)
        }
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let controller = PreviewController(arguments: arguments)
        app.delegate = controller
        app.run()
    }
}
