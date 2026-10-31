package app.paseo.flutter_audio

import android.app.Activity
import android.app.Application
import android.os.Bundle
import android.content.Context
import android.os.Handler
import android.os.Looper
import io.flutter.embedding.engine.plugins.FlutterPlugin
import io.flutter.embedding.engine.plugins.activity.ActivityAware
import io.flutter.embedding.engine.plugins.activity.ActivityPluginBinding
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel

class PaseoFlutterAudioPlugin : FlutterPlugin, ActivityAware, MethodChannel.MethodCallHandler,
    EventChannel.StreamHandler, Application.ActivityLifecycleCallbacks {
    private lateinit var context: Context
    private lateinit var methods: MethodChannel
    private lateinit var events: EventChannel
    private val main = Handler(Looper.getMainLooper())
    private var sink: EventChannel.EventSink? = null
    private var engine: AudioEngine? = null
    private var activity: Activity? = null
    override fun onAttachedToEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        context = binding.applicationContext
        methods = MethodChannel(binding.binaryMessenger, "app.paseo/audio")
        events = EventChannel(binding.binaryMessenger, "app.paseo/audio/events")
        methods.setMethodCallHandler(this)
        events.setStreamHandler(this)
        (context as Application).registerActivityLifecycleCallbacks(this)
    }
    override fun onListen(arguments: Any?, events: EventChannel.EventSink) { sink = events }
    override fun onCancel(arguments: Any?) { sink = null }
    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        try {
            if (call.method == "initialize") {
                if (engine == null) engine = AudioEngine(context) { event -> main.post { sink?.success(event) } }
                result.success(null)
                return
            }
            val audio = engine ?: throw IllegalStateException("Initialize audio first")
            when (call.method) {
                "enqueuePlayback" -> {
                    require(call.argument<Int>("sampleRate") == 16000) { "Native PCM playback requires 16000 Hz" }
                    audio.enqueue(requireNotNull(call.argument<String>("id")), requireNotNull(call.argument<ByteArray>("pcm16")))
                }
                "beginEncodedPlayback" -> audio.beginEncodedPlayback()
                "endEncodedPlayback" -> audio.endEncodedPlayback()
                "cancelPlayback" -> audio.cancelPlayback()
                "dispose" -> { audio.dispose(); engine = null }
                else -> { result.notImplemented(); return }
            }
            result.success(null)
        } catch (error: Exception) { result.error("audio_error", error.message, null) }
    }
    override fun onAttachedToActivity(binding: ActivityPluginBinding) { activity = binding.activity }
    override fun onDetachedFromActivityForConfigChanges() { activity = null }
    override fun onReattachedToActivityForConfigChanges(binding: ActivityPluginBinding) = onAttachedToActivity(binding)
    override fun onDetachedFromActivity() {
        activity = null
        engine?.cancelPlayback()
    }
    override fun onActivityPaused(activity: Activity) {
        if (activity === this.activity) engine?.pauseForBackground()
    }
    override fun onActivityResumed(activity: Activity) {
        if (activity === this.activity) engine?.resumeFromBackground()
    }
    override fun onActivityCreated(activity: Activity, state: Bundle?) {}
    override fun onActivityStarted(activity: Activity) {}
    override fun onActivityStopped(activity: Activity) {}
    override fun onActivitySaveInstanceState(activity: Activity, state: Bundle) {}
    override fun onActivityDestroyed(activity: Activity) {}
    override fun onDetachedFromEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        (context as Application).unregisterActivityLifecycleCallbacks(this)
        activity = null
        engine?.dispose(); engine = null; sink = null
        methods.setMethodCallHandler(null)
        events.setStreamHandler(null)
    }
}
