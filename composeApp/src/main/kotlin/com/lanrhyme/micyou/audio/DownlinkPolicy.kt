package com.lanrhyme.micyou.audio

import com.lanrhyme.micyou.network.DownlinkAudioMessage
import com.lanrhyme.micyou.network.DownlinkStateMessage

internal const val DOWNLINK_FRAME_BYTES = 3840
internal const val DOWNLINK_QUEUE_FRAMES = 6
internal const val DOWNLINK_PREBUFFER_FRAMES = 3

data class DesktopAudioStatus(
    val supported: Boolean = false,
    val enabled: Boolean = false,
    val supportsPlaybackRouting: Boolean = false,
    val routeState: String = "",
    val routeError: String = "",
    val phase: DesktopAudioPhase = DesktopAudioPhase.Disconnected,
    val error: String = "",
    val queuedMs: Int = 0,
    val droppedFrames: Long = 0,
    val underruns: Long = 0
)

enum class DesktopAudioPhase { Disconnected, Checking, Unsupported, Ready, Connecting, Playing, Paused, Error }

/** 只在会话控制锁下访问；请求与流代际分别拒绝迟到消息。 */
internal class DownlinkPolicy(val sessionId: Long) {
    var requestId: Long = 0
        private set
    var enabled: Boolean = false
        private set
    var desiredEnabled: Boolean = false
        private set
    var streamId: Long = 0
        private set
    private var sequence = -1L

    fun request(enabled: Boolean): Long {
        this.enabled = enabled
        requestId++
        sequence = -1
        return requestId
    }

    fun setDesired(enabled: Boolean) { desiredEnabled = enabled }

    fun accepts(state: DownlinkStateMessage): Boolean {
        if (state.sessionId != sessionId || state.requestId != requestId || state.streamId < streamId) return false
        if (state.active && (!enabled || state.streamId <= 0)) return false
        if (state.streamId > streamId) {
            streamId = state.streamId
            sequence = -1
        }
        return true
    }

    fun validFormat(state: DownlinkStateMessage): Boolean =
        state.sampleRate == 48000 && state.channelCount == 2 && state.audioFormat == 2

    fun isCurrentRequest(request: Long): Boolean = enabled && request == requestId

    fun frame(frame: DownlinkAudioMessage): FrameDecision {
        if (!enabled || streamId <= 0 || frame.sessionId != sessionId || frame.streamId != streamId ||
            frame.sequenceNumber < 0 || frame.sequenceNumber <= sequence) return FrameDecision.Drop
        if (frame.pcm.size != DOWNLINK_FRAME_BYTES) return FrameDecision.Invalid
        sequence = frame.sequenceNumber
        return FrameDecision.Accept
    }
}

internal enum class FrameDecision { Accept, Drop, Invalid }

/** 线上的本地故障只传固定码，不泄漏平台异常文本。 */
internal fun playbackErrorCode(reason: String?): String = when (reason) {
    "invalid_frame", "builtin_microphone_required", "unsupported_android", "unsupported_peer" -> reason
    else -> "playback_failed"
}

/** 包括路由预热静音；AudioTrack 的播放头也会消费这些采样。调用方持 native 锁。 */
internal class PlaybackQueueCounter {
    private var writtenFrames = 0L
    private var headWrap = 0L
    private var previousHead = 0L

    fun recordBytes(bytes: Int) {
        require(bytes >= 0 && bytes % 4 == 0)
        writtenFrames += bytes / 4
    }

    fun queuedMs(playbackHead: Int): Int {
        val head = playbackHead.toLong() and 0xffffffffL
        if (head < previousHead) headWrap += 0x100000000L
        previousHead = head
        return ((writtenFrames - headWrap - head).coerceAtLeast(0) / 48).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()
    }

    fun reset() { writtenFrames = 0; headWrap = 0; previousHead = 0 }
}

/** 所有 native 队列恢复标志与 PCM 一起串行访问，网络线程永不等待播放器。 */
internal class DownlinkFrameQueue {
    private val frames = ArrayDeque<ByteArray>()
    var dropped: Long = 0
        private set
    private var flushRequired = false

    @Synchronized fun offer(frame: ByteArray) {
        if (frames.size == DOWNLINK_QUEUE_FRAMES) {
            frames.removeFirst()
            dropped++
            flushRequired = true
        }
        frames.addLast(frame)
    }

    @Synchronized fun poll(): ByteArray? = if (frames.isEmpty()) null else frames.removeFirst()
    @Synchronized fun size(): Int = frames.size
    @Synchronized fun consumeFlush(): Boolean = flushRequired.also { flushRequired = false }
    @Synchronized fun clear() { frames.clear(); flushRequired = false }
    @Synchronized fun reset() { frames.clear(); flushRequired = true }
}
