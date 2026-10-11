package com.lanrhyme.micyou.audio

import com.lanrhyme.micyou.network.ConnectMessage
import com.lanrhyme.micyou.network.DownlinkRequestMessage
import com.lanrhyme.micyou.network.DownlinkStateMessage
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.protobuf.ProtoBuf
import kotlinx.serialization.protobuf.ProtoNumber
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalSerializationApi::class)
class PlaybackProtocolTest {
    @Serializable private data class LegacyConnect(
        @ProtoNumber(1) val sessionId: Long = 0,
        @ProtoNumber(2) val supportsDownlink: Boolean = false
    )
    @Serializable private data class LegacyRequest(
        @ProtoNumber(1) val sessionId: Long = 0,
        @ProtoNumber(2) val requestId: Long = 0,
        @ProtoNumber(3) val enabled: Boolean = false
    )
    @Serializable private data class LegacyState(
        @ProtoNumber(1) val sessionId: Long = 0,
        @ProtoNumber(2) val requestId: Long = 0,
        @ProtoNumber(3) val streamId: Long = 0,
        @ProtoNumber(4) val supported: Boolean = false,
        @ProtoNumber(5) val active: Boolean = false
    )

    @Test fun `old peers keep existing fields and never advertise automatic routing`() {
        val old = ProtoBuf.encodeToByteArray(LegacyConnect.serializer(), LegacyConnect(42, true))
        val current = ProtoBuf.decodeFromByteArray(ConnectMessage.serializer(), old)
        assertTrue(current.supportsDownlink)
        assertFalse(current.supportsPlaybackRouting)
        val new = ProtoBuf.encodeToByteArray(ConnectMessage.serializer(), ConnectMessage(42, true, true))
        assertEquals(LegacyConnect(42, true), ProtoBuf.decodeFromByteArray(LegacyConnect.serializer(), new))
        val oldState = ProtoBuf.encodeToByteArray(LegacyState.serializer(), LegacyState(42, 1, 2, true, true))
        val state = ProtoBuf.decodeFromByteArray(DownlinkStateMessage.serializer(), oldState)
        assertFalse(state.supportsPlaybackRouting)
        assertEquals("", state.routeState)
        assertEquals("", state.routeError)
    }

    @Test fun `failure stop remains stop to old peers while new peers receive failure cause`() {
        val failed = DownlinkRequestMessage(42, 3, false, "playback_failed")
        val wire = ProtoBuf.encodeToByteArray(DownlinkRequestMessage.serializer(), failed)
        assertEquals(LegacyRequest(42, 3, false), ProtoBuf.decodeFromByteArray(LegacyRequest.serializer(), wire))
        assertEquals(failed, ProtoBuf.decodeFromByteArray(DownlinkRequestMessage.serializer(), wire))
    }

    @Test fun `actual restore results carry generation and do not alter old active fields`() {
        val result = DownlinkStateMessage(sessionId = 42, requestId = 3, streamId = 2, supported = true,
            supportsPlaybackRouting = true, routeState = "restore_failed", routeError = "endpoint_missing")
        val wire = ProtoBuf.encodeToByteArray(DownlinkStateMessage.serializer(), result)
        val old = ProtoBuf.decodeFromByteArray(LegacyState.serializer(), wire)
        assertEquals(LegacyState(42, 3, 2, true, false), old)
        assertEquals(result, ProtoBuf.decodeFromByteArray(DownlinkStateMessage.serializer(), wire))
    }
}
