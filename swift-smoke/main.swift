// Swift smoke test for the generated bindings, run on the macOS host by
// scripts/swift-smoke.sh against examples/test_server.
import Foundation

@main
struct Smoke {
    static func main() async throws {
        setvbuf(stdout, nil, _IONBF, 0)  // progress must survive a hang
        let args = CommandLine.arguments
        let (quicPort, pin) = (args[1], args[2])
        print("bindings \(msgtransFfiVersion())")

        let client = try MsgtransClient(config: ClientConfig(
            endpoint: .quic(
                address: "127.0.0.1:\(quicPort)",
                serverName: "localhost",
                verification: .spkiPins(pins: [pin]),
                keepAliveIntervalMs: 5_000,
                maxIdleTimeoutMs: 30_000
            ),
            connectTimeoutMs: 5_000,
            limits: nil
        ))

        // Answer the server's own request, and forward everything else.
        let (events, sink) = AsyncStream<ClientEvent>.makeStream()
        let collector = Task {
            while let event = await client.nextEvent() {
                if case .request(let request) = event {
                    let outcome = try await request.respond(payload: Data("client-answers".utf8))
                    print("[QUIC] answered server request '\(String(decoding: request.payload(), as: UTF8.self))': \(outcome)")
                } else if case .messageSent = event {
                    continue
                } else {
                    sink.yield(event)
                }
            }
        }
        var it = events.makeAsyncIterator()

        try await client.connect()
        guard case .connected(let info) = await it.next() else { fatalError("expected connected") }
        print("[QUIC] connected via \(info.protocol) to \(info.peerAddress)")

        let response = try await client.request(payload: Data("ping".utf8))
        precondition(String(decoding: response, as: UTF8.self) == "ping")
        print("[QUIC] request -> ping")

        _ = try await client.send(payload: Data("hello".utf8))
        guard case .message(let message) = await it.next(),
              String(decoding: message.payload, as: UTF8.self) == "hello" else { fatalError("bad echo") }
        print("[QUIC] echo -> hello")

        try await Task.sleep(nanoseconds: 500_000_000)
        try await client.disconnect()
        guard case .disconnected = await it.next() else { fatalError("expected disconnected") }
        print("[QUIC] disconnected")

        // Swift task cancellation does not reach a pending nextEvent(); the
        // way to end the loop is shutdown(), which makes it return nil.
        try await client.shutdown()
        _ = await collector.result
        print("[QUIC] event loop ended by shutdown()")

        do {
            _ = try MsgtransClient(config: ClientConfig(
                endpoint: .tcp(address: "127.0.0.1:1", tls: TcpTls(spkiPins: [], serverName: nil)),
                connectTimeoutMs: 5_000, limits: nil))
            fatalError("empty pin list was accepted")
        } catch MsgtransError.Configuration(let field, _) {
            print("[config] empty pins rejected: field=\(field)")
        }
        print("SWIFT SMOKE OK")
    }
}
