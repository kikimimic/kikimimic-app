// A C interface to SpeechAnalyzer for kikimimic-asr-apple. Each handle is one
// lane: audio goes in with kkm_speech_push, and the results the analyzer has
// produced since the last call come out of kkm_speech_drain.

import AVFoundation
import Foundation
import Speech

public typealias KkmHeard = @convention(c) (
    UnsafeMutableRawPointer?, Bool, UnsafePointer<CChar>, Int64, Int64
) -> Void

private struct BridgeError: Error, CustomStringConvertible {
    let description: String
}

private final class Box<T>: @unchecked Sendable {
    var value: Result<T, Error>?
}

/// Runs `body` to completion from a thread outside Swift concurrency (the
/// core's ASR thread), which is free to block.
private func blocking<T>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
    let done = DispatchSemaphore(value: 0)
    let box = Box<T>()
    Task.detached {
        do { box.value = .success(try await body()) } catch { box.value = .failure(error) }
        done.signal()
    }
    done.wait()
    return try box.value!.get()
}

private func message(_ error: Error) -> UnsafeMutablePointer<CChar> {
    strdup(String(describing: error))!
}

private let source = AVAudioFormat(
    commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!

@available(macOS 26, *)
private final class Lane: @unchecked Sendable {
    private let lock = NSLock()
    private var heard: [(Bool, String, Int64, Int64)] = []
    private var failure: String?

    private let analyzer: SpeechAnalyzer
    private let input: AsyncStream<AnalyzerInput>.Continuation
    private let converter: AVAudioConverter?
    private let target: AVAudioFormat
    private var reader: Task<Void, Never>!
    private var pushed: Int64 = 0

    init(locale code: String) throws {
        let (transcriber, target) = try blocking { () async throws -> (SpeechTranscriber, AVAudioFormat) in
            guard
                let locale = await SpeechTranscriber.supportedLocale(
                    equivalentTo: Locale(identifier: code))
            else { throw BridgeError(description: "macOS speech recognition does not support \(code)") }
            let transcriber = SpeechTranscriber(locale: locale, preset: .progressiveTranscription)
            // ponytail: the first use of a language downloads its assets here,
            // blocking the lane until done; surface progress if users notice.
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try await request.downloadAndInstall()
            }
            let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
            return (transcriber, format ?? source)
        }
        self.target = target
        converter = target == source ? nil : AVAudioConverter(from: source, to: target)
        let (stream, input) = AsyncStream<AnalyzerInput>.makeStream()
        self.input = input
        analyzer = SpeechAnalyzer(modules: [transcriber])
        let analyzer = analyzer
        try blocking { try await analyzer.start(inputSequence: stream) }
        reader = Task.detached { [self] in
            do {
                for try await r in transcriber.results {
                    let text = String(r.text.characters)
                    let ms = { (t: CMTime) in Int64((t.seconds * 1000).rounded()) }
                    record(r.isFinal, text, ms(r.range.start), ms(r.range.end))
                }
            } catch {
                fail(String(describing: error))
            }
        }
    }

    private func record(_ final: Bool, _ text: String, _ start: Int64, _ end: Int64) {
        lock.lock()
        heard.append((final, text, start, end))
        lock.unlock()
    }

    private func fail(_ why: String) {
        lock.lock()
        failure = why
        lock.unlock()
    }

    func push(_ samples: UnsafePointer<Float>, _ count: Int) {
        guard count > 0, let buffer = AVAudioPCMBuffer(pcmFormat: source, frameCapacity: AVAudioFrameCount(count))
        else { return }
        buffer.frameLength = AVAudioFrameCount(count)
        buffer.floatChannelData![0].update(from: samples, count: count)
        let start = CMTime(value: pushed, timescale: 16_000)
        pushed += Int64(count)
        guard let converted = convert(buffer) else { return }
        input.yield(AnalyzerInput(buffer: converted, bufferStartTime: start))
    }

    private func convert(_ buffer: AVAudioPCMBuffer) -> AVAudioPCMBuffer? {
        guard let converter else { return buffer }
        let capacity = AVAudioFrameCount(
            (Double(buffer.frameLength) * target.sampleRate / source.sampleRate).rounded(.up)) + 1
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return nil }
        var given = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if given {
                status.pointee = .noDataNow
                return nil
            }
            given = true
            status.pointee = .haveData
            return buffer
        }
        if let error {
            fail(error.localizedDescription)
            return nil
        }
        return out
    }

    /// Reports what was heard since the last call, then the error that
    /// stopped the analyzer, if any.
    func drain(_ ctx: UnsafeMutableRawPointer?, _ cb: KkmHeard) -> String? {
        lock.lock()
        let taken = heard
        heard.removeAll()
        let failure = failure
        lock.unlock()
        for (final, text, start, end) in taken {
            text.withCString { cb(ctx, final, $0, start, end) }
        }
        return failure
    }

    func finish() throws {
        input.finish()
        let analyzer = analyzer
        try blocking { try await analyzer.finalizeAndFinishThroughEndOfInput() }
        let reader = reader!
        _ = try? blocking { await reader.value }
    }

    func cancel() {
        input.finish()
        let analyzer = analyzer
        _ = try? blocking { await analyzer.cancelAndFinishNow() }
        reader.cancel()
    }
}

@_cdecl("kkm_speech_available")
public func kkm_speech_available() -> Bool {
    if #available(macOS 26, *) { return SpeechTranscriber.isAvailable }
    return false
}

@_cdecl("kkm_speech_string_free")
public func kkm_speech_string_free(_ s: UnsafeMutablePointer<CChar>?) {
    free(s)
}

@_cdecl("kkm_speech_open")
public func kkm_speech_open(
    _ locale: UnsafePointer<CChar>, _ error: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
) -> UnsafeMutableRawPointer? {
    guard #available(macOS 26, *) else {
        error.pointee = strdup("macOS speech recognition needs macOS 26 or later")
        return nil
    }
    do {
        let lane = try Lane(locale: String(cString: locale))
        return Unmanaged.passRetained(lane).toOpaque()
    } catch let e {
        error.pointee = message(e)
        return nil
    }
}

@available(macOS 26, *)
private func lane(_ h: UnsafeMutableRawPointer) -> Lane {
    Unmanaged<Lane>.fromOpaque(h).takeUnretainedValue()
}

@_cdecl("kkm_speech_push")
public func kkm_speech_push(_ h: UnsafeMutableRawPointer, _ samples: UnsafePointer<Float>, _ count: Int) {
    guard #available(macOS 26, *) else { return }
    lane(h).push(samples, count)
}

@_cdecl("kkm_speech_drain")
public func kkm_speech_drain(
    _ h: UnsafeMutableRawPointer, _ ctx: UnsafeMutableRawPointer?, _ cb: KkmHeard
) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26, *) else { return nil }
    return lane(h).drain(ctx, cb).map { strdup($0)! }
}

/// Finalizes everything pushed; results stay drainable until close.
@_cdecl("kkm_speech_finish")
public func kkm_speech_finish(_ h: UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26, *) else { return nil }
    do {
        try lane(h).finish()
        return nil
    } catch let e {
        return message(e)
    }
}

@_cdecl("kkm_speech_close")
public func kkm_speech_close(_ h: UnsafeMutableRawPointer, _ finished: Bool) {
    guard #available(macOS 26, *) else { return }
    let lane = Unmanaged<Lane>.fromOpaque(h).takeRetainedValue()
    if !finished { lane.cancel() }
}
