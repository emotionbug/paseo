package app.paseo.flutter_audio

// Adapted from packages/expo-two-way-audio/android/.../AudioEngine.kt.
// Output only: media audio focus and PCM16 playback. Playback completion is
// based on rendered frames rather than a JS duration timer. No audio capture.
import android.annotation.SuppressLint
import android.content.Context
import android.media.*
import android.os.Build
import android.os.Handler
import android.os.Looper
import java.util.ArrayDeque
import java.util.concurrent.Executors
import kotlin.math.*

@SuppressLint("NewApi")
internal class AudioEngine(context: Context, private val emit: (Map<String, Any>) -> Unit) {
    private val manager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val handler = Handler(Looper.getMainLooper())
    private val playbackExecutor = Executors.newSingleThreadExecutor()
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()
    private var focus: AudioFocusRequest? = null
    private var focusHeld = false
    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (focusHeld) {
            event("audioFocus", "state" to if (change == AudioManager.AUDIOFOCUS_GAIN) "gained" else "lost")
            if (change < 0) {
                cancelPlayback()
                abandonFocus()
                event("interruption", "state" to "blocked")
            }
        }
    }
    private var disposed = false
    private var externalPlayback = false
    private var backgrounded = false
    private var currentStarted = false
    private data class Chunk(val id: String, val bytes: ByteArray)
    private val queue = ArrayDeque<Chunk>()
    private var current: Chunk? = null
    private var track: AudioTrack? = null
    private var playbackGeneration = 0
    private fun event(type: String, vararg fields: Pair<String, Any>) = emit(mapOf("type" to type, *fields))
    private fun acquireSession(): Boolean {
        if (focusHeld) return true
        val result = if (Build.VERSION.SDK_INT >= 26) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
                .setAudioAttributes(attributes)
                .setOnAudioFocusChangeListener(focusListener, handler).build()
            focus = request
            manager.requestAudioFocus(request)
        } else {
            @Suppress("DEPRECATION")
            manager.requestAudioFocus(focusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE)
        }
        val granted = result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        focusHeld = granted
        event("audioFocus", "state" to if (granted) "gained" else "blocked")
        if (!granted) abandonFocus()
        return granted
    }
    private fun abandonFocus() {
        if (Build.VERSION.SDK_INT >= 26) focus?.let { manager.abandonAudioFocusRequest(it) }
        else {
            @Suppress("DEPRECATION")
            manager.abandonAudioFocus(focusListener)
        }
        focus = null
        focusHeld = false
    }
    fun beginEncodedPlayback() {
        if (!acquireSession()) {
            releaseSession()
            error("Audio focus denied")
        }
        externalPlayback = true
    }
    fun endEncodedPlayback() {
        externalPlayback = false
        releaseSession()
    }
    private fun releaseSession() {
        if (externalPlayback || current != null || queue.isNotEmpty() || !focusHeld) return
        abandonFocus()
        event("audioFocus", "state" to "released")
    }
    fun pauseForBackground() {
        if (backgrounded || disposed) return
        backgrounded = true
        track?.let { if (it.playState == AudioTrack.PLAYSTATE_PLAYING) it.pause() }
        event("playbackPaused")
    }
    fun resumeFromBackground() {
        if (!backgrounded || disposed) return
        backgrounded = false
        try {
            val output = track
            val chunk = current
            if (output != null && chunk != null && output.state == AudioTrack.STATE_INITIALIZED) startOutput(output, chunk)
            else if (chunk == null) playNext()
            event("playbackResumed")
        } catch (error: Exception) {
            failPlayback(error.message ?: error.toString())
            event("interruption", "state" to "blocked")
        }
    }
    fun enqueue(id: String, bytes: ByteArray) {
        require(bytes.isNotEmpty() && bytes.size % 2 == 0) { "Expected PCM16 mono bytes" }
        require(current?.id != id && queue.none { it.id == id }) { "Duplicate playback id" }
        queue.addLast(Chunk(id, bytes))
        if (current == null && !backgrounded) playNext()
    }
    private fun playNext() {
        if (backgrounded) return
        if (queue.isEmpty()) { releaseSession(); return }
        val chunk = queue.removeFirst()
        current = chunk
        currentStarted = false
        if (!acquireSession()) { failPlayback("Audio focus denied"); return }
        val generation = playbackGeneration
        // Static buffers provide an exact playback-head boundary, including the final sample.
        // The allocation contains exactly the queued chunk, not an unbounded stream buffer.
        try {
            val output = AudioTrack.Builder()
                .setAudioAttributes(attributes)
                .setAudioFormat(AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(16000).setChannelMask(AudioFormat.CHANNEL_OUT_MONO).build())
                .setTransferMode(AudioTrack.MODE_STATIC).setBufferSizeInBytes(max(2, chunk.bytes.size)).build()
            track = output
            playbackExecutor.execute {
                try {
                    val written = output.write(chunk.bytes, 0, chunk.bytes.size)
                    check(written == chunk.bytes.size) { "AudioTrack write failed: $written/${chunk.bytes.size}" }
                    handler.post {
                        if (generation != playbackGeneration || track !== output) return@post
                        if (!backgrounded) startOutput(output, chunk)
                        monitor(output, chunk, generation)
                    }
                } catch (error: Exception) {
                    handler.post { if (generation == playbackGeneration) failPlayback(error.message ?: error.toString()) }
                }
            }
        } catch (error: Exception) { failPlayback(error.message ?: error.toString()) }
    }
    private fun startOutput(output: AudioTrack, chunk: Chunk) {
        output.play()
        if (!currentStarted) {
            currentStarted = true
            event("playbackStarted", "id" to chunk.id)
        }
        event("outputVolume", "level" to volume(chunk.bytes))
    }
    private fun monitor(output: AudioTrack, chunk: Chunk, generation: Int) {
        if (generation != playbackGeneration || track !== output) return
        if ((output.playbackHeadPosition.toLong() and 0xffffffffL) >= chunk.bytes.size / 2L) {
            output.stop(); output.release(); track = null; current = null
            event("playbackEnded", "id" to chunk.id, "cancelled" to false)
            event("outputVolume", "level" to 0.0)
            playNext()
        } else handler.postDelayed({ monitor(output, chunk, generation) }, 10)
    }
    private fun failPlayback(message: String) {
        event("error", "error" to message)
        cancelPlayback()
    }
    fun cancelPlayback() {
        playbackGeneration++
        track?.let { output ->
            try { if (output.playState == AudioTrack.PLAYSTATE_PLAYING) output.pause() } finally {
                // Release after any in-flight native write has returned.
                playbackExecutor.execute { output.release() }
            }
        }
        track = null
        current?.let { event("playbackEnded", "id" to it.id, "cancelled" to true) }
        current = null
        while (queue.isNotEmpty()) event("playbackEnded", "id" to queue.removeFirst().id, "cancelled" to true)
        event("outputVolume", "level" to 0.0)
        releaseSession()
    }
    fun dispose() {
        if (disposed) return
        disposed = true
        externalPlayback = false
        cancelPlayback()
        abandonFocus()
        playbackExecutor.shutdown()
    }
    private fun volume(bytes: ByteArray): Double {
        var squares = 0.0
        var index = 0
        while (index + 1 < bytes.size) {
            val sample = ((bytes[index].toInt() and 255) or (bytes[index + 1].toInt() shl 8)).toShort() / 32768.0
            squares += sample * sample
            index += 2
        }
        if (index == 0) return 0.0
        val db = 20 * log10(max(sqrt(squares / (index / 2)), 1e-5))
        return ((db + 80) / 80).coerceIn(0.0, 1.0).pow(2)
    }
}
