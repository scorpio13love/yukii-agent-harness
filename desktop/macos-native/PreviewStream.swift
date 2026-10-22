// 无窗口 SCStream 预览：只输出 JPEG 帧 JSON 行，不显示 AppKit 窗口。
// 帧事件：{"event":"ready",...} / {"event":"frame","data":"<jpeg base64>","width":...}
import AppKit
import CoreImage
import CoreMedia
import CoreVideo
import ScreenCaptureKit

private let outputQueue = DispatchQueue(label: "yukii.preview.stream.output")
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
}

func parseArguments() -> Arguments? {
    var windowID: UInt32?
    var pid: Int32?
    var arguments = CommandLine.arguments.dropFirst().makeIterator()
    while let flag = arguments.next() {
        switch flag {
        case "--window-id": windowID = arguments.next().flatMap { UInt32($0) }
        case "--pid": pid = arguments.next().flatMap { Int32($0) }
        default: break
        }
    }
    guard let windowID, let pid else { return nil }
    return Arguments(windowID: windowID, pid: pid)
}

final class PreviewStreamController: NSObject, NSApplicationDelegate, SCStreamOutput, SCStreamDelegate {
    private let arguments: Arguments
    private var stream: SCStream?
    private let ciContext = CIContext(options: [.cacheIntermediates: false])
    private let frameQueue = DispatchQueue(label: "yukii.preview.stream.frames")

    init(arguments: Arguments) {
        self.arguments = arguments
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        Task { await start() }
        DispatchQueue.global(qos: .utility).async {
            while readLine(strippingNewline: true) != nil {}
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let stream { stream.stopCapture { _ in } }
    }

    private func start() async {
        guard CGPreflightScreenCaptureAccess() else {
            emit("error", ["message": "screen_capture_permission_required"])
            DispatchQueue.main.async { NSApp.terminate(nil) }
            return
        }
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            guard let target = content.windows.first(where: {
                $0.windowID == arguments.windowID && $0.owningApplication?.processID == arguments.pid
            }) else {
                emit("error", ["message": "target_window_missing"])
                DispatchQueue.main.async { NSApp.terminate(nil) }
                return
            }
            let filter = SCContentFilter(desktopIndependentWindow: target)
            let configuration = SCStreamConfiguration()
            let scale = min(1.0, 1280.0 / max(target.frame.width, 1))
            configuration.width = max(1, Int(target.frame.width * scale))
            configuration.height = max(1, Int(target.frame.height * scale))
            configuration.showsCursor = false
            configuration.ignoreShadowsSingleWindow = true
            configuration.minimumFrameInterval = CMTime(value: 1, timescale: 15)
            configuration.queueDepth = 5
            let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: frameQueue)
            try await stream.startCapture()
            self.stream = stream
            emit("ready", ["width": configuration.width, "height": configuration.height])
        } catch {
            emit("error", ["message": "preview_stream_failed"])
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let image = CIImage(cvPixelBuffer: pixelBuffer)
        guard let cgImage = ciContext.createCGImage(image, from: image.extent) else { return }
        let representation = NSBitmapImageRep(cgImage: cgImage)
        guard let jpeg = representation.representation(using: .jpeg, properties: [.compressionFactor: 0.6]),
              jpeg.count <= 2 * 1024 * 1024 else { return }
        emit("frame", [
            "data": jpeg.base64EncodedString(),
            "width": cgImage.width,
            "height": cgImage.height,
        ])
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        emit("error", ["message": "preview_stream_stopped"])
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }
}

@main
struct PreviewStreamMain {
    @MainActor static var retainedController: PreviewStreamController?

    static func main() {
        guard let arguments = parseArguments() else {
            FileHandle.standardError.write(Data("usage: preview-stream --window-id <id> --pid <pid>\n".utf8))
            exit(2)
        }
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let controller = PreviewStreamController(arguments: arguments)
        retainedController = controller
        app.delegate = controller
        app.run()
    }
}
