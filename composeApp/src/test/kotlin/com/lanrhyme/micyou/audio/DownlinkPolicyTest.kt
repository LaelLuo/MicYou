package com.lanrhyme.micyou.audio

import android.media.AudioManager
import com.lanrhyme.micyou.network.DownlinkAudioMessage
import com.lanrhyme.micyou.network.DownlinkStateMessage
import org.junit.Assert.*
import org.junit.Test

class DownlinkPolicyTest {
    @Test fun `communication and unknown modes pause without changing user intent`() {
        assertFalse(shouldPauseForAudioMode(AudioManager.MODE_NORMAL))
        assertTrue(shouldPauseForAudioMode(AudioManager.MODE_RINGTONE))
        assertTrue(shouldPauseForAudioMode(AudioManager.MODE_IN_CALL))
        assertTrue(shouldPauseForAudioMode(AudioManager.MODE_IN_COMMUNICATION))
        assertTrue(shouldPauseForAudioMode(999))
        val gate = DownlinkPolicy(42)
        gate.setDesired(true)
        val request = gate.request(true)
        assertTrue(gate.desiredEnabled)
        assertTrue(gate.isCurrentRequest(request))
    }

    @Test fun `fatal stop keeps intent while stopping frames and requiring a new request to retry`() {
        val gate = DownlinkPolicy(42)
        gate.setDesired(true)
        val original = gate.request(true)
        assertTrue(gate.accepts(active()))
        val failed = gate.request(false)
        assertTrue(gate.desiredEnabled)
        assertFalse(gate.enabled)
        assertFalse(gate.isCurrentRequest(original))
        assertEquals(FrameDecision.Drop, gate.frame(frame(100)))
        assertTrue(gate.accepts(active(request = failed).copy(active = false)))
        val retry = gate.request(true)
        assertTrue(gate.isCurrentRequest(retry))
        assertFalse(gate.accepts(active(request = failed).copy(active = false, routeState = "restored")))
        assertTrue(gate.accepts(active(request = retry, stream = 3)))
        gate.setDesired(false)
        gate.request(false)
        assertFalse(gate.desiredEnabled)
        assertFalse(gate.isCurrentRequest(retry))
    }

    @Test fun `playback errors use controlled codes rather than platform exception messages`() {
        assertEquals("playback_failed", playbackErrorCode("native exception with sensitive detail"))
        assertEquals("playback_failed", playbackErrorCode(null))
        assertEquals("builtin_microphone_required", playbackErrorCode("builtin_microphone_required"))
        assertEquals("invalid_frame", playbackErrorCode("invalid_frame"))
    }

    private fun active(request: Long = 1, stream: Long = 2) = DownlinkStateMessage(
        sessionId = 42, requestId = request, streamId = stream, supported = true, active = true,
        sampleRate = 48000, channelCount = 2, audioFormat = 2)
    private fun frame(sequence: Long, session: Long = 42, stream: Long = 2, size: Int = DOWNLINK_FRAME_BYTES) =
        DownlinkAudioMessage(session, stream, sequence, ByteArray(size))

    @Test fun `late active after disable cannot reopen playback`() {
        val gate = DownlinkPolicy(42)
        assertEquals(1L, gate.request(true))
        assertTrue(gate.accepts(active()))
        assertEquals(2L, gate.request(false))
        assertFalse(gate.accepts(active()))
        assertFalse(gate.accepts(active(request = 2)))
        assertEquals(FrameDecision.Drop, gate.frame(frame(1)))
    }

    @Test fun `source generations never move backwards including stopped states`() {
        val gate = DownlinkPolicy(42)
        gate.request(true)
        assertTrue(gate.accepts(active(stream = 3)))
        assertFalse(gate.accepts(active(stream = 2)))
        assertFalse(gate.accepts(active(stream = 2).copy(active = false, error = "capture_failed")))
        assertEquals(FrameDecision.Drop, gate.frame(frame(100, stream = 2)))
        assertEquals(FrameDecision.Accept, gate.frame(frame(1, stream = 3)))
    }

    @Test fun `frame validation distinguishes stale frames from invalid current payload`() {
        val gate = DownlinkPolicy(42)
        gate.request(true); gate.accepts(active())
        assertEquals(FrameDecision.Drop, gate.frame(frame(0, session = 41)))
        assertEquals(FrameDecision.Drop, gate.frame(frame(-1)))
        assertEquals(FrameDecision.Invalid, gate.frame(frame(0, size = 8)))
        assertEquals(FrameDecision.Invalid, gate.frame(frame(0, size = 3841)))
        assertEquals(FrameDecision.Invalid, gate.frame(frame(0, size = 1024 * 1024)))
        assertEquals(FrameDecision.Accept, gate.frame(frame(0)))
        assertEquals(FrameDecision.Drop, gate.frame(frame(0)))
        assertEquals(FrameDecision.Accept, gate.frame(frame(10)))
        assertEquals(FrameDecision.Drop, gate.frame(frame(9)))
    }

    @Test fun `format is fixed PCM16 stereo 48k`() {
        val gate = DownlinkPolicy(42)
        assertTrue(gate.validFormat(active()))
        assertFalse(gate.validFormat(active().copy(sampleRate = 44100)))
        assertFalse(gate.validFormat(active().copy(channelCount = 1)))
        assertFalse(gate.validFormat(active().copy(audioFormat = 4)))
    }

    @Test fun `overflow keeps newest six frames and requests native flush`() {
        val queue = DownlinkFrameQueue()
        repeat(100) { queue.offer(byteArrayOf(it.toByte())) }
        assertEquals(6, queue.size())
        assertEquals(94L, queue.dropped)
        assertTrue(queue.consumeFlush())
        assertFalse(queue.consumeFlush())
        assertEquals(94, queue.poll()!![0].toInt())
        queue.reset()
        assertEquals(0, queue.size())
        assertTrue(queue.consumeFlush())
        assertNull(queue.poll())
    }

    @Test fun `reconnect begins closed with independent request and sequence state`() {
        val first = DownlinkPolicy(42)
        first.request(true); first.accepts(active()); first.frame(frame(500))
        val second = DownlinkPolicy(43)
        assertFalse(second.enabled)
        assertFalse(second.accepts(active()))
        assertEquals(FrameDecision.Drop, second.frame(frame(501)))
    }

    @Test fun `delayed routing and focus failures cannot disable a later request`() {
        val gate = DownlinkPolicy(42)
        val first = gate.request(true)
        assertTrue(gate.isCurrentRequest(first))
        gate.request(false)
        assertFalse(gate.isCurrentRequest(first))
        val second = gate.request(true)
        assertFalse(gate.isCurrentRequest(first))
        assertTrue(gate.isCurrentRequest(second))
    }

    @Test fun `native queue counts all written PCM and resets after flush`() {
        val counter = PlaybackQueueCounter()
        counter.recordBytes(3840) // 静音路由预热。
        counter.recordBytes(3840) // 首帧真实 PCM。
        assertEquals(20, counter.queuedMs(960))
        assertEquals(0, counter.queuedMs(1920))
        counter.reset()
        assertEquals(0, counter.queuedMs(0))
        counter.recordBytes(1920)
        assertEquals(10, counter.queuedMs(0))
    }
}
