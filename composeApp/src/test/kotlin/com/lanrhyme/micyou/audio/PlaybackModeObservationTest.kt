package com.lanrhyme.micyou.audio

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class PlaybackModeObservationTest {
    @Test fun `legacy mode poll reads immediately then every half second and stops with intent`() = runBlocking {
        var reads = 0
        var notifications = 0
        var mode = 0
        val observed = mutableListOf<Boolean>()
        val observation = PlaybackModeObservation(this, readMode = { reads++; mode })
        observation.start(changed = { notifications++; observed.add(shouldPauseForAudioMode(mode)) },
            failed = { fail("Unexpected mode observation failure") })
        assertEquals(1, reads)
        mode = 3
        delay(550)
        assertEquals(listOf(false, true), observed)
        observation.stop()
        val stoppedReads = reads
        val stoppedNotifications = notifications
        mode = 0
        delay(550)
        assertEquals(stoppedReads, reads)
        assertEquals(stoppedNotifications, notifications)
    }

    @Test fun `listener receives initial mode and ignores late events after replacement or stop`() = runBlocking {
        val callbacks = mutableListOf<() -> Unit>()
        var removed = 0
        var notifications = 0
        val observation = PlaybackModeObservation(this, { 0 }, subscribe = { notify ->
            callbacks.add(notify)
            val unsubscribe: () -> Unit = { removed++ }
            unsubscribe
        })
        observation.start({ notifications++ }, { fail("Unexpected failure") })
        assertEquals(1, notifications)
        callbacks[0]()
        assertEquals(2, notifications)
        observation.start({ notifications++ }, { fail("Unexpected failure") })
        assertEquals(1, removed)
        assertEquals(3, notifications)
        callbacks[0]()
        assertEquals(3, notifications)
        callbacks[1]()
        assertEquals(4, notifications)
        observation.stop()
        assertEquals(2, removed)
        callbacks[1]()
        assertEquals(4, notifications)
    }

    @Test fun `failed legacy reads notify failure rather than silently dropping monitoring`() = runBlocking {
        var reads = 0
        var failures = 0
        val observation = PlaybackModeObservation(this, {
            if (++reads > 1) error("Mode service unavailable")
            0
        })
        observation.start({}, { failures++ })
        delay(550)
        assertEquals(1, failures)
        observation.stop()
    }
}
