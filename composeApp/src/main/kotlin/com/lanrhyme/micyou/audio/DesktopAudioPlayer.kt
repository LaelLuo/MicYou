package com.lanrhyme.micyou.audio

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.util.concurrent.atomic.AtomicBoolean

/** 每次请求独占一个实例；MEDIA 输出由 Android 系统选择，普通路由变化不终止会话。 */
internal class DesktopAudioPlayer(
    context: Context,
    private val scope: CoroutineScope,
    private val fail: (String) -> Unit,
    private val metrics: (Int, Long, Long) -> Unit
) {
    private val manager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val handler = Handler(Looper.getMainLooper())
    private val queue = DownlinkFrameQueue()
    private val nativeQueue = PlaybackQueueCounter()
    private val stopped = AtomicBoolean(false)
    private val nativeLock = Any()
    private var track: AudioTrack? = null
    private var worker: Job? = null
    private var focusRequest: AudioFocusRequest? = null
    private var focusHeld = false
    private var underruns = 0L
    private var streamGeneration = 0L
    private var lastMetricsMs = 0L
    private var lastLogMs = 0L
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build()

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (shouldStopForAudioFocus(change)) abort("audio_focus_lost")
    }

    suspend fun prepare() {
        if (Build.VERSION.SDK_INT < 24) throw IllegalStateException("unsupported_android")
        synchronized(nativeLock) {
            if (stopped.get()) throw IllegalStateException("playback_cancelled")
            val focus = if (Build.VERSION.SDK_INT >= 26) {
                AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN).setAudioAttributes(attributes)
                    .setOnAudioFocusChangeListener(focusListener, handler).build().also { focusRequest = it }
                    .let(manager::requestAudioFocus)
            } else {
                @Suppress("DEPRECATION")
                manager.requestAudioFocus(focusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN)
            }
            if (focus != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) throw IllegalStateException("audio_focus_denied")
            focusHeld = true
        }
        val format = android.media.AudioFormat.Builder().setSampleRate(48000)
            .setEncoding(android.media.AudioFormat.ENCODING_PCM_16BIT)
            .setChannelMask(android.media.AudioFormat.CHANNEL_OUT_STEREO).build()
        val minimum = AudioTrack.getMinBufferSize(48000, android.media.AudioFormat.CHANNEL_OUT_STEREO,
            android.media.AudioFormat.ENCODING_PCM_16BIT)
        if (minimum <= 0) throw IllegalStateException("playback_failed")
        val local = AudioTrack.Builder().setAudioAttributes(attributes).setAudioFormat(format)
            .setTransferMode(AudioTrack.MODE_STREAM).setBufferSizeInBytes(maxOf(minimum, DOWNLINK_FRAME_BYTES * 3)).build()
        synchronized(nativeLock) {
            if (stopped.get()) { local.release(); throw IllegalStateException("playback_cancelled") }
            track = local
            if (local.state != AudioTrack.STATE_INITIALIZED) throw IllegalStateException("playback_failed")
        }
        worker = scope.launch(Dispatchers.IO) { playbackLoop(local) }
    }

    fun offer(pcm: ByteArray) { if (!stopped.get()) queue.offer(pcm) }

    fun resetStream() = synchronized(nativeLock) {
        streamGeneration++
        queue.reset()
        track?.let { flushForPrebuffer(it) }
    }

    private suspend fun playbackLoop(local: AudioTrack) {
        var primed = false
        var pending: ByteArray? = null
        var offset = 0
        var emptySince = 0L
        var generation = synchronized(nativeLock) { streamGeneration }
        try {
            while (scope.isActive && !stopped.get()) {
                if (queue.consumeFlush()) {
                    synchronized(nativeLock) { flushForPrebuffer(local); generation = streamGeneration }
                    pending = null; offset = 0; primed = false
                }
                if (!primed) {
                    if (queue.size() < DOWNLINK_PREBUFFER_FRAMES) { delay(5); continue }
                    synchronized(nativeLock) { if (!stopped.get()) local.play() }
                    primed = true
                }
                if (pending == null) { pending = queue.poll(); offset = 0 }
                val pcm = pending
                if (pcm == null) {
                    val now = android.os.SystemClock.elapsedRealtime()
                    if (emptySince == 0L) emptySince = now
                    if (now - emptySince >= 60) {
                        synchronized(nativeLock) { flushForPrebuffer(local) }
                        primed = false; underruns++; emptySince = 0
                        publishMetrics(0)
                    }
                    delay(5); continue
                }
                emptySince = 0
                val written = synchronized(nativeLock) {
                    if (stopped.get() || generation != streamGeneration) 0 else {
                        local.write(pcm, offset, pcm.size - offset, AudioTrack.WRITE_NON_BLOCKING).also {
                            if (it < 0 || it % 4 != 0) throw IllegalStateException("playback_failed")
                            nativeQueue.recordBytes(it)
                        }
                    }
                }
                if (written < 0 || written % 4 != 0) throw IllegalStateException("playback_failed")
                offset += written
                if (offset == pcm.size) pending = null
                val nativeMs = synchronized(nativeLock) {
                    nativeQueue.queuedMs(local.playbackHeadPosition)
                }
                publishMetrics(nativeMs, pending?.let { it.size - offset } ?: 0)
                if (written == 0) delay(2)
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            abort(e.message ?: "playback_failed")
        }
    }

    private fun flushForPrebuffer(local: AudioTrack) {
        local.pause(); local.flush(); nativeQueue.reset()
    }

    private fun publishMetrics(nativeMs: Int, pendingBytes: Int = 0) {
        val now = android.os.SystemClock.elapsedRealtime()
        val queued = queue.size() * 20 + nativeMs + pendingBytes / 192
        if (now - lastMetricsMs >= 250) {
            lastMetricsMs = now
            metrics(queued, queue.dropped, underruns)
        }
        if (now - lastLogMs >= 5000) {
            lastLogMs = now
            com.lanrhyme.micyou.util.Logger.d("DesktopAudioPlayer",
                "queuedMs=$queued droppedFrames=${queue.dropped} underruns=$underruns")
        }
    }

    private fun abort(reason: String) {
        if (!stopped.get()) { stop(); fail(reason) }
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        worker?.cancel()
        queue.clear()
        synchronized(nativeLock) {
            track?.let {
                try { it.pause(); it.flush() } catch (_: Exception) { }
                it.release()
            }
            track = null
        }
        if (focusHeld) {
            if (Build.VERSION.SDK_INT >= 26) focusRequest?.let(manager::abandonAudioFocusRequest)
            else { @Suppress("DEPRECATION") manager.abandonAudioFocus(focusListener) }
            focusHeld = false
        }
    }
}

internal fun shouldStopForAudioFocus(change: Int): Boolean =
    change == AudioManager.AUDIOFOCUS_LOSS || change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT ||
        change == AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK
