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
}

@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class LegacyConnect(@ProtoNumber(1) val sessionId: Long = 0)
@OptIn(ExperimentalSerializationApi::class)
@Serializable private data class LegacyWrapper(@ProtoNumber(2) val connect: LegacyConnect? = null)
