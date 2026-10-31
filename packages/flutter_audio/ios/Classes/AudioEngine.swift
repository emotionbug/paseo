import AVFoundation
import Foundation

// Adapted from packages/expo-two-way-audio/ios/AudioEngine.swift.
// Output only: native PCM16 playback and AVAudioSession ownership with the
// playback category, so microphone permission is never requested. Flutter gets
// dataPlayedBack acknowledgements instead of estimating completion in Dart.
final class AudioEngine {
    private var engine = AVAudioEngine()
    private var player = AVAudioPlayerNode()
    private let format = AVAudioFormat(standardFormatWithSampleRate: 16000, channels: 1)!
    private var configured = false
    private var active = false
    private var externalPlayback = false
    private var generation = 0
    private struct Chunk { let id: String; let buffer: AVAudioPCMBuffer }
    private var pending: [Chunk] = []
    private var observers: [NSObjectProtocol] = []
    private let emit: ([String: Any]) -> Void

    init(emit: @escaping ([String: Any]) -> Void) {
        self.emit = emit
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] note in self?.interrupted(note) })
        observers.append(center.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: .main) { [weak self] _ in self?.resetMediaServices() })
        observers.append(center.addObserver(forName: .AVAudioEngineConfigurationChange, object: nil, queue: .main) { [weak self] note in
            guard let self = self, let changed = note.object as? AVAudioEngine, changed === self.engine, self.active, !self.engine.isRunning else { return }
            // Route and configuration changes can invalidate all scheduled buffers.
            // Never report them as heard; cancel them and rebuild at the new format.
            self.reconfigure()
        })
    }
    deinit { for observer in observers { NotificationCenter.default.removeObserver(observer) } }
    private func event(_ type: String, _ fields: [String: Any] = [:]) { emit(fields.merging(["type": type]) { _, new in new }) }
    private func activate() throws {
        if !active {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .spokenAudio, options: [])
            try session.setActive(true)
            active = true
            event("audioFocus", ["state": "gained"])
        }
        if !configured {
            do { try configure() } catch { destroyGraph(); throw error }
        }
        if !engine.isRunning { try engine.start() }
    }
    private func configure() throws {
        engine.attach(player)
        // The mixer converts 16 kHz mono to the hardware output format.
        engine.connect(player, to: engine.mainMixerNode, format: format)
        engine.mainMixerNode.installTap(onBus: 0, bufferSize: 2048, format: nil) { [weak self] buffer, _ in
            guard let samples = buffer.floatChannelData?[0] else { return }
            let level = Self.volume(samples, Int(buffer.frameLength))
            self?.event("outputVolume", ["level": level])
        }
        configured = true
        engine.prepare()
    }
    func enqueue(id: String, data: Data) throws {
        guard !data.isEmpty, data.count % 2 == 0, !pending.contains(where: { $0.id == id }),
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(data.count / 2)) else {
            throw NSError(domain: "PaseoAudio", code: 3, userInfo: [NSLocalizedDescriptionKey: "Invalid PCM16 data or duplicate playback id"])
        }
        buffer.frameLength = buffer.frameCapacity
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            let source = raw.bindMemory(to: UInt8.self)
            let dest = buffer.floatChannelData![0]
            for index in 0..<Int(buffer.frameLength) {
                let bits = UInt16(source[index * 2]) | UInt16(source[index * 2 + 1]) << 8
                dest[index] = Float(Int16(bitPattern: bits)) / 32768
            }
        }
        try activate()
        pending.append(Chunk(id: id, buffer: buffer))
        if pending.count == 1 { scheduleFirst() }
    }
    private func scheduleFirst() {
        guard let chunk = pending.first else { return }
        let ticket = generation
        player.scheduleBuffer(chunk.buffer, completionCallbackType: .dataPlayedBack) { [weak self] _ in
            DispatchQueue.main.async {
                guard let self = self, self.generation == ticket, self.pending.first?.id == chunk.id else { return }
                self.pending.removeFirst()
                self.event("playbackEnded", ["id": chunk.id, "cancelled": false])
                if self.pending.isEmpty {
                    self.event("outputVolume", ["level": 0.0])
                    do { try self.releaseSession() } catch { self.event("error", ["error": error.localizedDescription]) }
                } else { self.scheduleFirst() }
            }
        }
        if !player.isPlaying { player.play() }
        event("playbackStarted", ["id": chunk.id])
    }
    func cancelPlayback() throws {
        generation += 1
        player.stop()
        let cancelled = pending
        pending.removeAll(keepingCapacity: true)
        for chunk in cancelled { event("playbackEnded", ["id": chunk.id, "cancelled": true]) }
        event("outputVolume", ["level": 0.0])
        try releaseSession()
    }
    func beginEncodedPlayback() throws { try activate(); externalPlayback = true }
    func endEncodedPlayback() throws { externalPlayback = false; try releaseSession() }
    private func releaseSession() throws {
        guard active, !externalPlayback, pending.isEmpty else { return }
        engine.pause()
        let session = AVAudioSession.sharedInstance()
        try session.setActive(false, options: .notifyOthersOnDeactivation)
        active = false
        try session.setCategory(.ambient, options: .mixWithOthers)
        event("audioFocus", ["state": "released"])
    }
    private func interrupted(_ note: Notification) {
        guard let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw) else { return }
        if type == .began {
            do { try cancelPlayback() } catch { event("error", ["error": error.localizedDescription]) }
            active = false
            event("audioFocus", ["state": "lost"])
            event("interruption", ["state": "began"])
        } else {
            let options = AVAudioSession.InterruptionOptions(rawValue: note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0)
            event("interruption", ["state": options.contains(.shouldResume) ? "ended" : "blocked"])
        }
    }
    private func reconfigure() {
        do {
            try cancelPlayback()
        } catch { event("error", ["error": error.localizedDescription]); event("interruption", ["state": "blocked"]) }
        destroyGraph()
    }
    private func resetMediaServices() {
        active = false
        reconfigure()
    }
    private func destroyGraph() {
        engine.stop()
        if configured { engine.mainMixerNode.removeTap(onBus: 0) }
        configured = false
        engine = AVAudioEngine()
        player = AVAudioPlayerNode()
    }
    func dispose() throws {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
        externalPlayback = false
        defer { destroyGraph() }
        try cancelPlayback()
    }
    private static func volume(_ samples: UnsafePointer<Float>, _ count: Int) -> Float {
        guard count > 0 else { return 0 }
        var squares: Float = 0
        for index in 0..<count { squares += samples[index] * samples[index] }
        let db = 20 * log10(max(sqrt(squares / Float(count)), 1e-5))
        return pow(max(0, min(1, (db + 80) / 80)), 2)
    }
}
