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
    @Serializable private data class OfficialConnect(
        @ProtoNumber(1) val sessionId: Long = 0,
        @ProtoNumber(2) val platform: String = ""
    )

    @Test fun `official platform stays on field 2 and is not a downlink capability`() {
        val official = ProtoBuf.encodeToByteArray(OfficialConnect.serializer(), OfficialConnect(42, "android"))
        val current = ProtoBuf.decodeFromByteArray(ConnectMessage.serializer(), official)
        assertEquals(42L, current.sessionId)
        assertEquals("android", current.platform)
        assertFalse(current.supportsDownlink)
        assertFalse(current.supportsPlaybackRouting)

        val duplex = ProtoBuf.encodeToByteArray(
            ConnectMessage.serializer(),
            ConnectMessage(42, platform = "android", supportsDownlink = true, supportsPlaybackRouting = true)
        )
        assertEquals(OfficialConnect(42, "android"), ProtoBuf.decodeFromByteArray(OfficialConnect.serializer(), duplex))
    }

    @Test fun `failure stop still encodes enabled false and carries the failure cause`() {
        val failed = DownlinkRequestMessage(42, 3, false, "playback_failed")
        val wire = ProtoBuf.encodeToByteArray(DownlinkRequestMessage.serializer(), failed)
        assertEquals(failed, ProtoBuf.decodeFromByteArray(DownlinkRequestMessage.serializer(), wire))
        assertFalse(failed.enabled)
        assertEquals("playback_failed", failed.playbackError)
    }

    @Test fun `restore results carry routing fields without changing inactive state`() {
        val result = DownlinkStateMessage(sessionId = 42, requestId = 3, streamId = 2, supported = true,
            supportsPlaybackRouting = true, routeState = "restore_failed", routeError = "endpoint_missing")
        val wire = ProtoBuf.encodeToByteArray(DownlinkStateMessage.serializer(), result)
        val decoded = ProtoBuf.decodeFromByteArray(DownlinkStateMessage.serializer(), wire)
        assertEquals(result, decoded)
        assertFalse(decoded.active)
        assertEquals("restore_failed", decoded.routeState)
    }
}
