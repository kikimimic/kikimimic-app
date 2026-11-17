import AppKit
import AVFoundation

// Read-only player: the original WAVs and transcript are never rewritten.
final class Player: NSObject, NSApplicationDelegate {
    var audio: AVAudioPlayer?
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 650, height: 240), styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let slider = NSSlider(value: 0, minValue: 0, maxValue: 1, target: nil, action: nil)
    let status = NSTextField(labelWithString: "")
    var timer: Timer?
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
        for i in 0..<3 where lanes.isEnabled(forSegment: i) {
            lanes.selectedSegment = i
            lane(lanes)
            break
        }
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
            guard let self = self, let audio = self.audio else { return }
            self.slider.doubleValue = audio.currentTime
            self.status.stringValue = String(format: "%.1f / %.1f s", audio.currentTime, audio.duration)
        }
        window.title = "Recording playback"
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
    @objc func lane(_ sender: NSSegmentedControl) {
        let position = audio?.currentTime ?? Double(CommandLine.arguments[2]) ?? 0
        audio?.stop()
        do {
            let url = URL(fileURLWithPath: CommandLine.arguments[1]).appendingPathComponent(["mix", "mic", "speaker"][sender.selectedSegment] + ".wav")
            audio = try AVAudioPlayer(contentsOf: url)
            slider.maxValue = audio!.duration
            audio!.currentTime = min(max(0, position), audio!.duration)
        } catch {
            audio = nil
            status.stringValue = "Cannot open audio: \(error.localizedDescription)"
        }
    }
    @objc func toggle() { guard let audio = audio else { return }; if audio.isPlaying { audio.pause() } else { audio.play() } }
    @objc func seek() { audio?.currentTime = slider.doubleValue }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}
let app = NSApplication.shared
let delegate = Player()
app.setActivationPolicy(.regular)
app.delegate = delegate
app.run()
