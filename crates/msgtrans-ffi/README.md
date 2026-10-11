# msgtrans-ffi

[UniFFI](https://mozilla.github.io/uniffi-rs/) bindings for the
[msgtrans](https://github.com/zoujiaqing/msgtrans) client, for Kotlin (Android,
JVM, Kotlin Multiplatform) and Swift (iOS, macOS).

One connection over **TCP** (optionally TLS with SPKI pinning), **WebSocket**
or **QUIC**: send messages and requests, receive events, and answer requests
the server sends. Client only — there is no server surface.

## Toolchain

The workspace is pinned to **Rust 1.99.0** (`rust-toolchain.toml` at the
repository root), with the Android and iOS targets listed there. UniFFI is
0.32.2.

## Building

Run from `crates/msgtrans-ffi`. The shipped libraries use the size-optimised
`release-ffi` profile defined at the workspace root.

| What | Command | Output |
|---|---|---|
| Android | `ANDROID_NDK_HOME=… scripts/build-android.sh` | `dist/android/jniLibs/arm64-v8a/libmsgtrans_ffi.so` |
| iOS | `scripts/build-ios.sh` | `dist/ios/MsgtransFFI.xcframework` (device + Apple-silicon simulator) |
| Bindings | `scripts/gen-bindings.sh` | `bindings/kotlin/…/msgtrans_ffi.kt`, `bindings/swift/MsgtransFFI.swift` + header + modulemap |

Android ships **arm64-v8a only**, API level 24 — the same as the apps'
`abiFilters` and `minSdk`. The library needs only `libc`, `libm` and `libdl`,
and its LOAD segments are 16 KB-aligned, as Google Play requires for Android 15+.
Override the ABI list with `MSGTRANS_FFI_ABIS="arm64-v8a x86_64"` if an
emulator needs it (the target must be installed for 1.99.0).

The Kotlin bindings need **JNA 5.12 or newer** (they use
`com.sun.jna.internal.Cleaner`); on Android use the `@aar` artifact.

## Kotlin

```kotlin
val client = MsgtransClient(
    ClientConfig(
        endpoint = Endpoint.Quic(
            address = "203.0.113.10:8443",
            serverName = "gateway",
            verification = QuicVerification.SpkiPins(listOf("base64-sha256-of-spki=")),
            keepAliveIntervalMs = 15_000uL,
            maxIdleTimeoutMs = 60_000uL,
        ),
        connectTimeoutMs = 10_000uL,
        limits = null,
    )
)

scope.launch {
    while (true) {
        when (val event = client.nextEvent() ?: break) {
            is ClientEvent.Message -> handle(event.message.payload)
            is ClientEvent.Request -> event.request.respond(answer(event.request.payload()))
            is ClientEvent.Disconnected -> scheduleReconnect(event.reason)
            else -> Unit
        }
    }
}

client.connect()
val reply: ByteArray = client.request("ping".toByteArray())
client.send("hello".toByteArray())      // returns once written to the socket
// …
client.shutdown()                         // ends the event loop: nextEvent() returns null
client.close()
```

## Swift

```swift
let client = try MsgtransClient(config: ClientConfig(
    endpoint: .quic(address: "203.0.113.10:8443", serverName: "gateway",
                    verification: .spkiPins(pins: ["base64-sha256-of-spki="]),
                    keepAliveIntervalMs: 15_000, maxIdleTimeoutMs: 60_000),
    connectTimeoutMs: 10_000, limits: nil))

let loop = Task {
    while let event = await client.nextEvent() {
        if case .request(let request) = event {
            _ = try await request.respond(payload: answer(request.payload()))
        }
    }
}
try await client.connect()
let reply = try await client.request(payload: Data("ping".utf8))
// …
try await client.shutdown()   // the only way to end `loop`, see below
```

## Behaviour worth knowing

- **No silent downgrades.** There is no fallback between protocols, and
  configurations that msgtrans would quietly weaken are rejected up front:
  `TcpTls` with no pins (msgtrans would connect in plaintext),
  `QuicVerification.SpkiPins` with an empty list (it would fall back to WebPKI
  roots), and TLS settings on a `ws://` URL (they would be ignored).
- **A pin mismatch is not retryable.** A server rejected by TLS verification
  comes back as `Connection { retryable = false }`; drive reconnects from that
  flag and a wrong pin stops the loop instead of spinning on it.
- **`send` is write-confirmed**: it returns once the bytes reached the socket.
  `sendDetached` returns once queued.
- **`nextEvent` is cancellation-safe**: cancelling a pending call never loses
  an event; the next call receives it.
- **Ending the event loop.** Kotlin can cancel the coroutine. Swift cannot:
  UniFFI's Swift bridge does not propagate task cancellation into Rust, so a
  cancelled task stays suspended in `nextEvent()`. Call `shutdown()` — every
  pending and later `nextEvent()` then returns `nil` once buffered events are
  drained. A loop parked in `nextEvent()` also keeps the client alive, so
  always `shutdown()` a client you are done with.
- **`connect` and `shutdown` take exclusive access** to the connection and wait
  for in-flight sends and requests. `disconnect` does not; a disconnected
  client can `connect` again, a shut-down one cannot.
- `MessageSent` events are a droppable diagnostic, not a delivery ledger.

## Tests

```bash
cargo test -p msgtrans-ffi # Rust surface against real TCP / TLS / WS / QUIC servers
scripts/kotlin-smoke.sh    # generated Kotlin, compiled unchanged, driven over JNA
scripts/swift-smoke.sh     # generated Swift, compiled unchanged, on the macOS host
```

`cargo test` drives every client call from a thread **without** a Tokio
context, which is the situation of a Kotlin coroutine or Swift task polling an
exported future. Both smoke scripts start `examples/test_server` (pinned TCP+TLS
and QUIC on one self-signed certificate, echoing, and sending each client one
request of its own) and check the server received the client's answers.

## How futures cross the boundary

msgtrans runs on a private two-worker Tokio runtime. Exported futures are
polled by the foreign executor, which has no Tokio context, so each poll enters
that runtime first (`src/runtime.rs`). The future stays owned by the caller:
cancelling the coroutine drops the in-flight operation with it, instead of
leaving a detached task whose result nobody reads.

## Versioning and distribution

msgtrans-ffi lives in the msgtrans workspace and is versioned in lockstep with
it: a build reports the msgtrans version it wraps (`msgtransFfiVersion()`). It
always builds against the msgtrans crate beside it, never a crates.io release,
and is not itself published to crates.io — it ships as built libraries plus the
generated bindings.
