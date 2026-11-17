import AppKit
import AVFoundation

// Read-only player: the original WAVs and transcript are never rewritten.
final class Player: NSObject, NSApplicationDelegate {
    var audio: AVAudioPlayer?
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 650, height: 240), styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let slider = NSSlider(value: 0, minValue: 0, maxValue: 1, target: nil, action: nil)
    let status = NSTextField(labelWithString: "")
    var timer: Timer?
    var currentLane = ""
    // Optional args preserve the original `directory seconds` invocation.
    var stateURL: URL? {
        CommandLine.arguments.count > 3 ? URL(fileURLWithPath: CommandLine.arguments[3]) : nil
    }
    var autoplay: Bool { CommandLine.arguments.count > 4 && CommandLine.arguments[4] == "autoplay" }
    func report() {
        guard let url = stateURL else { return }
        let value: [String: Any] = ["position_ms": Int((audio?.currentTime ?? 0) * 1000),
                                  "playing": audio?.isPlaying ?? false, "lane": currentLane]
        if let bytes = try? JSONSerialization.data(withJSONObject: value) {
            try? bytes.write(to: url, options: .atomic)
        }
    }
    func applicationDidFinishLaunching(_ notification: Notification) {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 12
        stack.frame = NSRect(x: 20, y: 20, width: 610, height: 200)
        stack.autoresizingMask = [.width, .height]
        window.contentView?.addSubview(stack)
        let dir = URL(fileURLWithPath: CommandLine.arguments[1])
        let lanes = NSSegmentedControl(labels: ["mix", "mic", "speaker"], trackingMode: .selectOne, target: self, action: #selector(lane(_:)))
        for (i, name) in ["mix", "mic", "speaker"].enumerated() {
            lanes.setEnabled(FileManager.default.fileExists(atPath: dir.appendingPathComponent(name + ".wav").path), forSegment: i)
        }
        stack.addArrangedSubview(lanes)
        stack.addArrangedSubview(NSButton(title: "Play / Pause", target: self, action: #selector(toggle)))
        slider.target = self
        slider.action = #selector(seek)
        stack.addArrangedSubview(slider)
        stack.addArrangedSubview(status)
        let requested = CommandLine.arguments.count > 5 ? CommandLine.arguments[5] : "mix"
        if let i = ["mix", "mic", "speaker"].firstIndex(of: requested), lanes.isEnabled(forSegment: i) {
            lanes.selectedSegment = i
        } else {
            for i in 0..<3 where lanes.isEnabled(forSegment: i) {
                lanes.selectedSegment = i
                break
            }
        }
        if lanes.selectedSegment >= 0 { lane(lanes) }
        if autoplay { audio?.play() }
        report()
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            guard let self = self, let audio = self.audio else { return }
            self.slider.doubleValue = audio.currentTime
            self.status.stringValue = String(format: "%.1f / %.1f s", audio.currentTime, audio.duration)
            self.report()
        }
        window.title = "Recording playback"
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
    @objc func lane(_ sender: NSSegmentedControl) {
        let position = audio?.currentTime ?? Double(CommandLine.arguments[2]) ?? 0
        let wasPlaying = audio?.isPlaying ?? false
        audio?.stop()
        do {
            currentLane = ["mix", "mic", "speaker"][sender.selectedSegment]
            let url = URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent(currentLane + ".wav")
            audio = try AVAudioPlayer(contentsOf: url)
            slider.maxValue = audio!.duration
            audio!.currentTime = min(max(0, position), audio!.duration)
            if wasPlaying { audio?.play() }
        } catch {
            audio = nil
            status.stringValue = "Cannot open audio: \(error.localizedDescription)"
        }
        report()
    }
    @objc func toggle() { guard let audio = audio else { return }; if audio.isPlaying { audio.pause() } else { audio.play() }; report() }
    @objc func seek() { audio?.currentTime = slider.doubleValue; report() }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
let app = NSApplication.shared
let delegate = Player()
app.setActivationPolicy(.regular)
app.delegate = delegate
app.run()
