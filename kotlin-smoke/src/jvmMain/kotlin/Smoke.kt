import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import uniffi.msgtrans_ffi.*

/**
 * Drives the generated bindings end to end:
 * connect, request, send/echo, answering a server-initiated request,
 * pin rejection, disconnect, and cancelling a suspended nextEvent().
 */
fun main(args: Array<String>) = runBlocking {
    val (tcpPort, quicPort, pin) = args
    println("bindings ${msgtransFfiVersion()}")

    val quic = Endpoint.Quic(
        address = "127.0.0.1:$quicPort",
        serverName = "localhost",
        verification = QuicVerification.SpkiPins(listOf(pin)),
        keepAliveIntervalMs = 5_000uL,
        maxIdleTimeoutMs = 30_000uL,
    )
    val tcpTls = Endpoint.Tcp(
        address = "127.0.0.1:$tcpPort",
        tls = TcpTls(spkiPins = listOf(pin), serverName = "localhost"),
    )

    for ((name, endpoint) in listOf("QUIC" to quic, "TCP+TLS" to tcpTls)) {
        exercise(name, endpoint)
    }

    // A server whose key does not match the pin must be refused.
    val wrongPin = Endpoint.Quic(
        address = "127.0.0.1:$quicPort",
        serverName = "localhost",
        verification = QuicVerification.SpkiPins(listOf("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")),
        keepAliveIntervalMs = null,
        maxIdleTimeoutMs = null,
    )
    MsgtransClient(ClientConfig(wrongPin, 5_000uL, null)).use { client ->
        try {
            client.connect()
            error("QUIC connect with a wrong pin succeeded")
        } catch (e: MsgtransException) {
            println("[QUIC wrong pin] refused as expected: ${e::class.simpleName}: ${e.message}")
        }
    }

    // An empty pin list is a configuration error, never a silent fallback.
    try {
        MsgtransClient(ClientConfig(Endpoint.Tcp("127.0.0.1:1", TcpTls(emptyList(), null)), 5_000uL, null))
        error("empty pin list was accepted")
    } catch (e: MsgtransException.Configuration) {
        println("[config] empty pins rejected: field=${e.field}")
    }

    println("SMOKE OK")
}

suspend fun exercise(name: String, endpoint: Endpoint) = coroutineScope {
    MsgtransClient(ClientConfig(endpoint, 5_000uL, null)).use { client ->
        val events = Channel<ClientEvent>(Channel.UNLIMITED)
        val collector = launch(Dispatchers.Default) {
            while (true) {
                when (val event = client.nextEvent() ?: break) {
                    is ClientEvent.Request -> {
                        val outcome = event.request.respond("client-answers".toByteArray())
                        println("[$name] answered server request '${String(event.request.payload())}': $outcome")
                    }
                    is ClientEvent.MessageSent -> Unit
                    else -> events.send(event)
                }
            }
        }

        client.connect()
        val connected = withTimeout(5_000) { events.receive() }
        check(connected is ClientEvent.Connected) { "expected Connected, got $connected" }
        println("[$name] connected via ${connected.info.protocol} to ${connected.info.peerAddress}")

        val response = client.request("ping".toByteArray())
        check(String(response) == "ping") { "bad response ${String(response)}" }
        println("[$name] request -> ${String(response)}")

        client.send("hello".toByteArray())
        val echo = withTimeout(5_000) { events.receive() }
        check(echo is ClientEvent.Message && String(echo.message.payload) == "hello") { "bad echo $echo" }
        println("[$name] echo -> ${String(echo.message.payload)}")

        // Give the server's own request time to arrive and be answered.
        delay(500)

        client.disconnect()
        val closed = withTimeout(5_000) { events.receive() }
        check(closed is ClientEvent.Disconnected) { "expected Disconnected, got $closed" }
        println("[$name] disconnected: ${closed.reason}")

        // The collector is suspended inside nextEvent(): cancelling it must
        // cancel the Rust future cleanly rather than hang or crash.
        collector.cancelAndJoin()
        println("[$name] collector cancelled while suspended in nextEvent()")
    }
}
