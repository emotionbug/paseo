import Flutter

public final class PaseoFlutterAudioPlugin: NSObject, FlutterPlugin, FlutterStreamHandler {
    private var engine: AudioEngine?
    private var sink: FlutterEventSink?
    public static func register(with registrar: FlutterPluginRegistrar) {
        let plugin = PaseoFlutterAudioPlugin()
        registrar.addMethodCallDelegate(plugin, channel: FlutterMethodChannel(name: "app.paseo/audio", binaryMessenger: registrar.messenger()))
        FlutterEventChannel(name: "app.paseo/audio/events", binaryMessenger: registrar.messenger()).setStreamHandler(plugin)
    }
    public func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink) -> FlutterError? { sink = events; return nil }
    public func onCancel(withArguments arguments: Any?) -> FlutterError? { sink = nil; return nil }
    private func send(_ event: [String: Any]) {
        DispatchQueue.main.async { [weak self] in self?.sink?(event) }
    }
    public func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        let args = call.arguments as? [String: Any] ?? [:]
        do {
            if call.method == "initialize" {
                if engine == nil { engine = AudioEngine { [weak self] event in self?.send(event) } }
                result(nil); return
            }
            guard let audio = engine else { result(FlutterError(code: "not_initialized", message: "Initialize audio first", details: nil)); return }
            switch call.method {
            case "enqueuePlayback":
                guard let id = args["id"] as? String, let data = args["pcm16"] as? FlutterStandardTypedData, args["sampleRate"] as? Int == 16000 else {
                    result(FlutterError(code: "invalid_pcm", message: "Expected id and PCM16 mono 16000 Hz bytes", details: nil)); return
                }
                try audio.enqueue(id: id, data: data.data)
            case "beginEncodedPlayback": try audio.beginEncodedPlayback()
            case "endEncodedPlayback": try audio.endEncodedPlayback()
            case "cancelPlayback": try audio.cancelPlayback()
            case "dispose": try audio.dispose(); engine = nil
            default: result(FlutterMethodNotImplemented); return
            }
            result(nil)
        } catch { result(FlutterError(code: "audio_error", message: error.localizedDescription, details: nil)) }
    }
    public func detachFromEngine(for registrar: FlutterPluginRegistrar) {
        try? engine?.dispose()
        engine = nil
        sink = nil
    }
}
