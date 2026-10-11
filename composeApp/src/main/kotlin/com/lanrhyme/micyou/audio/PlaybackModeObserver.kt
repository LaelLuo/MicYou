package com.lanrhyme.micyou.audio

import android.content.Context
import android.media.AudioManager
import android.os.Build
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean

/** 仅观测系统通信模式，不申请电话权限，也不修改路由或音频模式。 */
internal class PlaybackModeObserver(context: Context, scope: CoroutineScope) {
    private val manager = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val observation = PlaybackModeObservation(scope, readMode = { manager.mode },
        subscribe = if (Build.VERSION.SDK_INT >= 31) ({ notify -> Api31.register(manager, notify) }) else null)

    fun isPaused(): Boolean = shouldPauseForAudioMode(manager.mode)
    fun start(changed: () -> Unit, failed: () -> Unit) = observation.start(changed, failed)
    fun stop() = observation.stop()

    @android.annotation.TargetApi(31)
    private object Api31 {
        fun register(manager: AudioManager, notify: () -> Unit): () -> Unit {
            val listener = AudioManager.OnModeChangedListener { notify() }
            manager.addOnModeChangedListener(Executor { it.run() }, listener)
            return { manager.removeOnModeChangedListener(listener) }
        }
    }
}

/** 生命周期独立于Android，可核验轮询取消与移除后迟到的监听事件。 */
internal class PlaybackModeObservation(
    private val scope: CoroutineScope,
    private val readMode: () -> Int,
    private val subscribe: ((() -> Unit) -> (() -> Unit))? = null
) {
    private var poll: Job? = null
    private var removeListener: (() -> Unit)? = null
    private var observing: AtomicBoolean? = null

    fun start(changed: () -> Unit, failed: () -> Unit) {
        stop()
        val alive = AtomicBoolean(true)
        observing = alive
        val report = { if (alive.get()) { readMode(); changed() } }
        if (subscribe != null) {
            removeListener = subscribe.invoke {
                try { report() }
                catch (_: Exception) { if (alive.get()) failed() }
            }
        } else {
            poll = scope.launch {
                try {
                    while (isActive && alive.get()) {
                        delay(500)
                        report()
                    }
                } catch (e: CancellationException) {
                    throw e
                } catch (_: Exception) {
                    if (alive.get()) failed()
                }
            }
        }
        report()
    }

    fun stop() {
        observing?.set(false)
        observing = null
        poll?.cancel(); poll = null
        try { removeListener?.invoke() }
        catch (e: Exception) { com.lanrhyme.micyou.util.Logger.w("PlaybackModeObserver", "Mode listener removal failed: ${e.message}") }
        finally { removeListener = null }
    }

}

// 未知模式也不推定安全；平台未发布模式的中断仍由系统决定。
internal fun shouldPauseForAudioMode(mode: Int): Boolean = mode != AudioManager.MODE_NORMAL
