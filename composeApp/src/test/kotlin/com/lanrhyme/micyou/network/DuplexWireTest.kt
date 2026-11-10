package com.lanrhyme.micyou.network

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.protobuf.ProtoBuf
import kotlinx.serialization.protobuf.ProtoNumber
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalSerializationApi::class)
class DuplexWireTest {
    private val fixtures = javaClass.getResourceAsStream("/duplex-wire.json")!!.bufferedReader().use {
        Json.parseToJsonElement(it.readText()).jsonObject
    }
    private fun wire(name: String): ByteArray = fixtures.getValue(name).jsonPrimitive.content
        .chunked(2).map { it.toInt(16).toByte() }.toByteArray()
    private fun decode(name: String): MessageWrapper = ProtoBuf.decodeFromByteArray(MessageWrapper.serializer(), wire(name))

    @Test fun `shared Rust fixtures decode and encode exact protobuf bytes`() {
        assertEquals(ConnectMessage(42), decode("legacyConnect").connect)
        assertEquals(ConnectMessage(42, supportsDownlink = true), decode("duplexConnect").connect)
        assertEquals(DownlinkRequestMessage(42, 1, true), decode("enableRequest").downlinkRequest)
        assertTrue(decode("enableRequest").hasControlMessage())
        val state = decode("activeState").downlinkState!!
        assertEquals(42L, state.sessionId); assertEquals(1L, state.requestId); assertEquals(2L, state.streamId)
        assertTrue(state.supported); assertTrue(state.active)
        assertEquals(48000, state.sampleRate); assertEquals(2, state.channelCount); assertEquals(2, state.audioFormat)
        val audio = decode("audioWireSample").downlinkAudio!!
        assertEquals(7L, audio.sequenceNumber)
        assertArrayEquals(byteArrayOf(0, -128, -1, 127, 0, 0, 0, 64), audio.pcm)
        for (name in fixtures.keys) assertArrayEquals(name, wire(name),
            ProtoBuf.encodeToByteArray(MessageWrapper.serializer(), decode(name)))
    }

    @Test fun `official connect keeps session and platform and ignores duplex flags`() {
        val official = OfficialWrapper(
            connect = OfficialConnect(sessionId = 42, platform = "android")
        )
        val bytes = ProtoBuf.encodeToByteArray(OfficialWrapper.serializer(), official)
        val current = ProtoBuf.decodeFromByteArray(MessageWrapper.serializer(), bytes).connect!!
        assertEquals(42L, current.sessionId)
        assertEquals("android", current.platform)
        assertFalse(current.supportsDownlink)
        assertFalse(current.supportsPlaybackRouting)

        val duplex = MessageWrapper(
            connect = ConnectMessage(42, platform = "android", supportsDownlink = true, supportsPlaybackRouting = true)
        )
        val duplexBytes = ProtoBuf.encodeToByteArray(MessageWrapper.serializer(), duplex)
        val seen = ProtoBuf.decodeFromByteArray(OfficialWrapper.serializer(), duplexBytes).connect!!
        assertEquals(42L, seen.sessionId)
        assertEquals("android", seen.platform)
    }

    @Test fun `official stats occupies field 8 and is not a downlink request`() {
        val official = OfficialWrapper(stats = OfficialStats(rttMs = 12, jitterMs = 1.5f, packetLoss = 0.25f, latencyMs = 40))
        val bytes = ProtoBuf.encodeToByteArray(OfficialWrapper.serializer(), official)
        val current = ProtoBuf.decodeFromByteArray(MessageWrapper.serializer(), bytes)
        assertEquals(StatsMessage(12, 1.5f, 0.25f, 40), current.stats)
        assertNull(current.downlinkRequest)
        assertNull(current.downlinkState)
        assertNull(current.downlinkAudio)

        val downlink = decode("enableRequest")
        val officialView = ProtoBuf.decodeFromByteArray(OfficialWrapper.serializer(), wire("enableRequest"))
        assertNull(officialView.stats)
        assertNotNull(downlink.downlinkRequest)
    }

    @Test fun `shared routing fixtures preserve capability failure stop and actual restore result`() {
        val connect = MessageWrapper(
            connect = ConnectMessage(42, supportsDownlink = true, supportsPlaybackRouting = true)
        )
        val request = MessageWrapper(downlinkRequest = DownlinkRequestMessage(42, 3, false, "playback_failed"))
        val state = MessageWrapper(downlinkState = DownlinkStateMessage(
            sessionId = 42, requestId = 3, streamId = 2, supported = true, active = false,
            error = "playback_failed", sampleRate = 48000, channelCount = 2, audioFormat = 2,
            supportsPlaybackRouting = true, routeState = "restore_failed", routeError = "route_restore_failed"))
        for ((name, expected) in listOf("routingConnect" to connect, "playbackErrorRequest" to request,
            "routingFailedState" to state)) {
            assertEquals(name, expected, decode(name))
            assertArrayEquals(name, wire(name), ProtoBuf.encodeToByteArray(MessageWrapper.serializer(), expected))
        }
    }
}

@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class OfficialConnect(
    @ProtoNumber(1) val sessionId: Long = 0,
    @ProtoNumber(2) val platform: String = ""
)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class OfficialStats(
    @ProtoNumber(1) val rttMs: Int = 0,
    @ProtoNumber(2) val jitterMs: Float = 0f,
    @ProtoNumber(3) val packetLoss: Float = 0f,
    @ProtoNumber(4) val latencyMs: Int = 0
)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class OfficialWrapper(
    @ProtoNumber(2) val connect: OfficialConnect? = null,
    @ProtoNumber(8) val stats: OfficialStats? = null
)
