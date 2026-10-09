package com.lanrhyme.micyou.audio

import android.content.Context
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioRouting
import android.os.Build
import android.os.Handler
import android.os.Looper
import com.lanrhyme.micyou.network.DownlinkAudioMessage
import com.lanrhyme.micyou.network.DownlinkRequestMessage
import com.lanrhyme.micyou.network.DownlinkStateMessage
import com.lanrhyme.micyou.util.Logger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/** 下行失败只处理本会话播放器及输入偏好，不取消录音或共享连接。 */
internal class DownlinkSession(
    private val context: Context,
    parentScope: CoroutineScope,
    sessionId: Long,
    private val recorder: AudioRecord,
    private val send: suspend (DownlinkRequestMessage) -> Unit,
    private val publish: (DesktopAudioStatus) -> Unit
) {
    private val scope = CoroutineScope(parentScope.coroutineContext + SupervisorJob(parentScope.coroutineContext[Job]))
    private val lock = Mutex()
    private val policy = DownlinkPolicy(sessionId)
    private val closed = AtomicBoolean(false)
    private var status = DesktopAudioStatus(phase = DesktopAudioPhase.Checking)
    private var player: DesktopAudioPlayer? = null
    private var preparing: Job? = null
    private var savedInput: AudioDeviceInfo? = null
    private var inputChanged = false
    @Volatile private var inputId = -1
    @Volatile private var inputConfirmed = false
    private var inputListenerInstalled = false
    private var active = false
    private var unsupportedTimedOut = false
    private val dropped = AtomicLong()
    private val handler = Handler(Looper.getMainLooper())
    private var inputListener: AudioRouting.OnRoutingChangedListener? = null

    init {
        publish(status)
        scope.launch {
            delay(2000)
            lock.withLock {
                if (!status.supported && status.phase == DesktopAudioPhase.Checking) {
                    unsupportedTimedOut = true
                    update(status.copy(phase = DesktopAudioPhase.Unsupported, error = "unsupported_peer"))
                }
            }
        }
    }

    fun setEnabled(enabled: Boolean) {
        scope.launch {
            lock.withLock {
                if (closed.get()) return@withLock
                if (!enabled) { disableLocked(""); return@withLock }
                if (policy.enabled) return@withLock
                if (!status.supported || unsupportedTimedOut) {
                    update(status.copy(phase = DesktopAudioPhase.Unsupported, error = "unsupported_peer")); return@withLock
                }
                val request = policy.request(true)
                update(status.copy(enabled = true, phase = DesktopAudioPhase.Connecting, error = ""))
                preparing = scope.launch {
                    try {
                        prepareInput(request)
                        val local = DesktopAudioPlayer(context, scope, { reason -> fail(reason, request) }) { queued, droppedFrames, underruns ->
                            scope.launch {
                                lock.withLock {
                                    if (policy.enabled && request == policy.requestId && !closed.get()) update(status.copy(
                                        queuedMs = queued, droppedFrames = dropped.get() + droppedFrames, underruns = underruns))
                                }
                            }
                        }
                        lock.withLock {
                            if (!policy.enabled || request != policy.requestId || closed.get()) {
                                local.stop(); return@launch
                            }
                            player = local
                        }
                        local.prepare()
                        lock.withLock {
                            if (policy.enabled && request == policy.requestId && !closed.get()) {
                                if (recorder.routedDevice?.id != inputId) throw IllegalStateException("builtin_microphone_required")
                                send(DownlinkRequestMessage(policy.sessionId, request, true))
                            }
                        }
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        Logger.w("DesktopAudioPlayer", "Downlink preparation failed: ${e.message}")
                        scope.launch {
                            lock.withLock {
                                if (request == policy.requestId && !closed.get()) disableLocked(e.message ?: "playback_failed")
                            }
                        }
                    }
                }
            }
        }
    }

    fun state(message: DownlinkStateMessage) {
        scope.launch {
            lock.withLock {
                if (closed.get() || unsupportedTimedOut) return@withLock
                val previousStream = policy.streamId
                if (!policy.accepts(message)) return@withLock
                if (!message.supported) {
                    if (policy.enabled) disableLocked(message.error.ifEmpty { "unsupported_peer" })
                    update(status.copy(supported = false, enabled = false, phase = DesktopAudioPhase.Unsupported,
                        error = message.error.ifEmpty { "unsupported_peer" }))
                    return@withLock
                }
                update(status.copy(supported = true))
                if (!policy.enabled) {
                    update(status.copy(phase = DesktopAudioPhase.Ready, error = "")); return@withLock
                }
                if (message.error.isNotEmpty()) { disableLocked(message.error); return@withLock }
                if (message.active && !policy.validFormat(message)) { disableLocked("invalid_frame"); return@withLock }
                if (policy.streamId != previousStream || !message.active) player?.resetStream()
                active = message.active
                update(status.copy(phase = if (active) DesktopAudioPhase.Playing else DesktopAudioPhase.Connecting))
            }
        }
    }

    fun frame(message: DownlinkAudioMessage) {
        // 控制转换可能触及 native 资源；接收线程宁可丢当前音频，也不等待它。
        if (!lock.tryLock()) { dropped.incrementAndGet(); return }
        try {
            if (closed.get()) return
            when (policy.frame(message)) {
                FrameDecision.Invalid -> fail("invalid_frame", policy.requestId)
                FrameDecision.Drop -> dropped.incrementAndGet()
                FrameDecision.Accept -> if (active) player?.offer(message.pcm) else dropped.incrementAndGet()
            }
        } finally { lock.unlock() }
    }

    private suspend fun prepareInput(request: Long) {
        if (Build.VERSION.SDK_INT < 24) throw IllegalStateException("unsupported_android")
        val manager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val input = manager.getDevices(AudioManager.GET_DEVICES_INPUTS)
            .firstOrNull { it.type == AudioDeviceInfo.TYPE_BUILTIN_MIC }
            ?: throw IllegalStateException("builtin_microphone_required")
        lock.withLock {
            if (!policy.enabled || closed.get()) throw kotlinx.coroutines.CancellationException()
            savedInput = recorder.preferredDevice
            inputId = input.id
            inputChanged = true
            if (!recorder.setPreferredDevice(input)) throw IllegalStateException("builtin_microphone_required")
            val listener = AudioRouting.OnRoutingChangedListener { routing ->
                if (inputConfirmed && routing.routedDevice?.id != input.id) fail("builtin_microphone_required", request)
            }
            inputListener = listener
            recorder.addOnRoutingChangedListener(listener, handler)
            inputListenerInstalled = true
        }
        repeat(50) {
            val confirmed = lock.withLock {
                if (!policy.enabled || closed.get()) throw kotlinx.coroutines.CancellationException()
                val routed = recorder.routedDevice
                (routed?.id == inputId && routed.type == AudioDeviceInfo.TYPE_BUILTIN_MIC).also { inputConfirmed = it }
            }
            if (confirmed) return
            delay(10)
        }
        throw IllegalStateException("builtin_microphone_required")
    }

    private fun fail(reason: String, request: Long) {
        scope.launch { lock.withLock {
            if (!closed.get() && policy.isCurrentRequest(request)) disableLocked(reason)
        } }
    }

    private suspend fun disableLocked(reason: String) {
        preparing?.cancel(); preparing = null
        player?.stop(); player = null; active = false
        restoreInput()
        val wasEnabled = policy.enabled
        val request = policy.request(false)
        update(status.copy(enabled = false, phase = if (reason.isEmpty()) DesktopAudioPhase.Ready else DesktopAudioPhase.Error,
            error = reason, queuedMs = 0))
        if (status.supported && wasEnabled) send(DownlinkRequestMessage(policy.sessionId, request, false))
    }

    private fun restoreInput() {
        inputConfirmed = false
        if (Build.VERSION.SDK_INT >= 24 && inputListenerInstalled) {
            inputListener?.let(recorder::removeOnRoutingChangedListener)
            inputListener = null; inputListenerInstalled = false
        }
        if (Build.VERSION.SDK_INT >= 23 && inputChanged) {
            try {
                if (!recorder.setPreferredDevice(savedInput)) Logger.w("DesktopAudioPlayer", "Could not restore recorder preference")
            } catch (e: Exception) { Logger.w("DesktopAudioPlayer", "Recorder preference restore failed: ${e.message}") }
            inputChanged = false; savedInput = null
        }
    }

    private fun update(value: DesktopAudioStatus) { status = value; publish(value) }

    suspend fun close() {
        if (!closed.compareAndSet(false, true)) return
        lock.withLock {
            preparing?.cancel(); player?.stop(); player = null
            restoreInput()
        }
        scope.cancel()
    }
}
