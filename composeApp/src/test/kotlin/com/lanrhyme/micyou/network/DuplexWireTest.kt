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
        assertEquals(ConnectMessage(42, false), decode("legacyConnect").connect)
        assertEquals(ConnectMessage(42, true), decode("duplexConnect").connect)
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

    @Test fun `legacy schema ignores capability flag from new phone`() {
        val old = ProtoBuf.decodeFromByteArray(LegacyWrapper.serializer(), wire("duplexConnect"))
        assertEquals(42L, old.connect!!.sessionId)
    }

    @Test fun `shared routing fixtures preserve capability failure stop and actual restore result`() {
        val connect = MessageWrapper(connect = ConnectMessage(42, supportsDownlink = true, supportsPlaybackRouting = true))
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

    @Test fun `pre routing schema ignores new fields but keeps stop and inactive semantics`() {
        val connect = ProtoBuf.decodeFromByteArray(PreRoutingWrapper.serializer(), wire("routingConnect")).connect!!
        assertEquals(42L, connect.sessionId)
        assertTrue(connect.supportsDownlink)
        val stop = ProtoBuf.decodeFromByteArray(PreRoutingWrapper.serializer(), wire("playbackErrorRequest")).downlinkRequest!!
        assertEquals(42L, stop.sessionId)
        assertEquals(3L, stop.requestId)
        assertFalse(stop.enabled)
        val state = ProtoBuf.decodeFromByteArray(PreRoutingWrapper.serializer(), wire("routingFailedState")).downlinkState!!
        assertEquals(42L, state.sessionId)
        assertEquals(3L, state.requestId)
        assertEquals(2L, state.streamId)
        assertTrue(state.supported)
        assertFalse(state.active)
        assertEquals("playback_failed", state.error)
        assertEquals(48000, state.sampleRate)
        assertEquals(2, state.channelCount)
        assertEquals(2, state.audioFormat)
    }
}

@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class LegacyConnect(@ProtoNumber(1) val sessionId: Long = 0)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class LegacyWrapper(@ProtoNumber(2) val connect: LegacyConnect? = null)

@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class PreRoutingConnect(
    @ProtoNumber(1) val sessionId: Long = 0,
    @ProtoNumber(2) val supportsDownlink: Boolean = false
)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class PreRoutingRequest(
    @ProtoNumber(1) val sessionId: Long = 0,
    @ProtoNumber(2) val requestId: Long = 0,
    @ProtoNumber(3) val enabled: Boolean = false
)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class PreRoutingState(
    @ProtoNumber(1) val sessionId: Long = 0,
    @ProtoNumber(2) val requestId: Long = 0,
    @ProtoNumber(3) val streamId: Long = 0,
    @ProtoNumber(4) val supported: Boolean = false,
    @ProtoNumber(5) val active: Boolean = false,
    @ProtoNumber(6) val error: String = "",
    @ProtoNumber(7) val sampleRate: Int = 0,
    @ProtoNumber(8) val channelCount: Int = 0,
    @ProtoNumber(9) val audioFormat: Int = 0
)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class PreRoutingWrapper(
    @ProtoNumber(2) val connect: PreRoutingConnect? = null,
    @ProtoNumber(8) val downlinkRequest: PreRoutingRequest? = null,
    @ProtoNumber(9) val downlinkState: PreRoutingState? = null
)
