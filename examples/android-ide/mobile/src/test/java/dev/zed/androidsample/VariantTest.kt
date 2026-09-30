package dev.zed.androidsample

import dev.zed.greeting.LibraryGreeting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class VariantTest {
    @Test
    fun selectedVariantUsesItsOwnApplicationId() {
        assertTrue(BuildConfig.FLAVOR in setOf("demo", "full"))
        assertEquals("dev.zed.androidsample.${BuildConfig.FLAVOR}", BuildConfig.APPLICATION_ID)
        assertEquals("Hello from the ${BuildConfig.FLAVOR} variant!", Greeting.message(BuildConfig.FLAVOR))
        assertEquals("Android library connected", LibraryGreeting.message())
    }
}
