package dev.tapwing.driver

import maestro_android.MaestroAndroid
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ViewHierarchyRequestTest {

    @Test
    fun protoDefaultIsWireCompatibleAndSkipsWaitForIdleOnlyWhenRequested() {
        val defaultReq = MaestroAndroid.ViewHierarchyRequest.getDefaultInstance()
        assertFalse(defaultReq.skipWaitForIdle)
        assertEquals(0, defaultReq.serializedSize)

        val decodedEmpty = MaestroAndroid.ViewHierarchyRequest.parseFrom(ByteArray(0))
        assertFalse(decodedEmpty.skipWaitForIdle)

        val fastReq = MaestroAndroid.ViewHierarchyRequest.newBuilder().setSkipWaitForIdle(true).build()
        val roundTripped = MaestroAndroid.ViewHierarchyRequest.parseFrom(fastReq.toByteArray())
        assertTrue(roundTripped.skipWaitForIdle)
    }

    @Test
    fun refreshAccessibilityCacheAlwaysFlushesServiceInfoEvenWhenSkippingWaitForIdle() {
        val events = mutableListOf<String>()
        refreshAccessibilityCacheWith(
            waitForIdle = true,
            waitIdle = { events.add("waitForIdle") },
            clearServiceInfo = {
                events.add("clearServiceInfo")
                throw NullPointerException("expected on null serviceInfo")
            },
        )
        assertEquals(listOf("waitForIdle", "clearServiceInfo"), events)

        events.clear()
        val skipReq = MaestroAndroid.ViewHierarchyRequest.newBuilder().setSkipWaitForIdle(true).build()
        refreshAccessibilityCacheWith(
            waitForIdle = !skipReq.skipWaitForIdle,
            waitIdle = { events.add("waitForIdle") },
            clearServiceInfo = {
                events.add("clearServiceInfo")
                throw NullPointerException("expected on null serviceInfo")
            },
        )
        assertEquals(listOf("clearServiceInfo"), events)
    }
}
