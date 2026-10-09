package com.lanrhyme.micyou.audio

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.AudioRouting
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

/** 每次请求独占一个实例；真实 PCM 只在蓝牙实际路由确认后写入。 */
internal class DesktopAudioPlayer(
    private val context: Context,
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
    @Volatile private var outputId = -1
    private var worker: Job? = null
    private var focusRequest: AudioFocusRequest? = null
    private var focusHeld = false
    private var deviceRegistered = false
    private var receiverRegistered = false
    @Volatile private var routeConfirmed = false
    private var underruns = 0L
    private var streamGeneration = 0L
    private var lastMetricsMs = 0L
    private var lastLogMs = 0L
    private val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build()

    private val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
        if (change != AudioManager.AUDIOFOCUS_GAIN) abort("audio_focus_lost")
    }
    private val deviceCallback = object : AudioDeviceCallback() {
        override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>) {
            if (removedDevices.any { it.id == outputId }) abort("bluetooth_disconnected")
        }
    }
    private val routeListener = AudioRouting.OnRoutingChangedListener { routing ->
        val device = routing.routedDevice
        if (routeConfirmed && (device?.id != outputId || !isBluetoothMedia(device))) abort("bluetooth_route_changed")
    }
    private val noisyReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) abort("bluetooth_disconnected")
        }
    }

    suspend fun prepare() {
        if (Build.VERSION.SDK_INT < 24) throw IllegalStateException("unsupported_android")
        val output = manager.getDevices(AudioManager.GET_DEVICES_OUTPUTS).firstOrNull(::isBluetoothMedia)
            ?: throw IllegalStateException("bluetooth_required")
        synchronized(nativeLock) {
        if (stopped.get()) throw IllegalStateException("playback_cancelled")
        outputId = output.id
        manager.registerAudioDeviceCallback(deviceCallback, handler)
        deviceRegistered = true
        if (Build.VERSION.SDK_INT >= 33) {
            context.registerReceiver(noisyReceiver, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("DEPRECATION")
            context.registerReceiver(noisyReceiver, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        }
        receiverRegistered = true
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
            if (local.state != AudioTrack.STATE_INITIALIZED || !local.setPreferredDevice(output)) {
                throw IllegalStateException("bluetooth_route_failed")
            }
            local.addOnRoutingChangedListener(routeListener, handler)
            local.play()
        }
        val silence = ByteArray(DOWNLINK_FRAME_BYTES)
        var confirmed = false
        for (attempt in 0 until 50) {
            if (stopped.get()) throw IllegalStateException("playback_cancelled")
            synchronized(nativeLock) {
                val routed = local.routedDevice
                if (routed?.id == outputId && isBluetoothMedia(routed)) confirmed = true
                if (!confirmed) writeSilence(local, silence)
            }
            if (confirmed) break
            delay(10)
        }
        if (!confirmed) throw IllegalStateException("bluetooth_route_failed")
        // 暂停时 routedDevice 可能为空；保持播放静音，真实 PCM 到达前仍逐次核对路由。
        synchronized(nativeLock) { routeConfirmed = true }
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
        var routeWaitSince = 0L
        val silence = ByteArray(DOWNLINK_FRAME_BYTES)
        try {
            while (scope.isActive && !stopped.get()) {
                if (queue.consumeFlush()) {
                    synchronized(nativeLock) { flushForPrebuffer(local); generation = streamGeneration }
                    pending = null; offset = 0; primed = false
                }
                if (!primed) {
                    if (queue.size() < DOWNLINK_PREBUFFER_FRAMES) { delay(5); continue }
                    val routed = synchronized(nativeLock) {
                        val device = local.routedDevice
                        if (device?.id == outputId && isBluetoothMedia(device)) {
                            routeConfirmed = true
                            true
                        } else {
                            writeSilence(local, silence)
                            false
                        }
                    }
                    if (!routed) {
                        val now = android.os.SystemClock.elapsedRealtime()
                        if (routeWaitSince == 0L) routeWaitSince = now
                        if (now - routeWaitSince >= 500) throw IllegalStateException("bluetooth_route_failed")
                        delay(5); continue
                    }
                    routeWaitSince = 0
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
                        val routed = local.routedDevice
                        if (routed?.id != outputId || !isBluetoothMedia(routed)) throw IllegalStateException("bluetooth_route_changed")
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
        routeConfirmed = false
        local.pause(); local.flush(); nativeQueue.reset(); local.play()
    }

    private fun writeSilence(local: AudioTrack, silence: ByteArray) {
        val written = local.write(silence, 0, silence.size, AudioTrack.WRITE_NON_BLOCKING)
        if (written < 0 || written % 4 != 0) throw IllegalStateException("playback_failed")
        nativeQueue.recordBytes(written)
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
                try { it.pause(); it.flush(); it.removeOnRoutingChangedListener(routeListener) } catch (_: Exception) { }
                it.release()
            }
            track = null
        }
        if (receiverRegistered) { context.unregisterReceiver(noisyReceiver); receiverRegistered = false }
        if (deviceRegistered) { manager.unregisterAudioDeviceCallback(deviceCallback); deviceRegistered = false }
        if (focusHeld) {
            if (Build.VERSION.SDK_INT >= 26) focusRequest?.let(manager::abandonAudioFocusRequest)
            else { @Suppress("DEPRECATION") manager.abandonAudioFocus(focusListener) }
            focusHeld = false
        }
    }
}

internal fun isBluetoothMedia(device: AudioDeviceInfo?): Boolean = device != null &&
    (device.type == AudioDeviceInfo.TYPE_BLUETOOTH_A2DP ||
        (Build.VERSION.SDK_INT >= 31 && device.type == AudioDeviceInfo.TYPE_BLE_HEADSET))
