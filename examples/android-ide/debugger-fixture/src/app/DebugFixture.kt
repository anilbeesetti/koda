package app

import other.twice
import kotlin.coroutines.*
import java.util.concurrent.CountDownLatch

fun main() {
    val values = intArrayOf(3, 5)
    val answer = twice(values[0])
    println(answer)
    val again = twice(9)
    println(again)
    Values().inspect(values)
    val completed = CountDownLatch(1)
    val action: suspend () -> Unit = {
        val retained = "retained-value"
        suspendCoroutine<Unit> { continuation ->
            Thread { continuation.resume(Unit) }.start()
        }
        println(retained)
    }
    action.startCoroutine(object : Continuation<Unit> {
        override val context = EmptyCoroutineContext
        override fun resumeWith(result: Result<Unit>) { result.getOrThrow(); completed.countDown() }
    })
    completed.await()
    println("ready-for-detach")
    Thread.sleep(30000)
}
