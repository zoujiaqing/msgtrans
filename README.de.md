# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | **Deutsch** | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | [Bahasa Indonesia](README.id.md) | [العربية](README.ar.md)

MsgTrans ist eine Rust-Bibliothek für bidirektionalen Nachrichtenaustausch in Echtzeitanwendungen. Dieselbe API unterstützt Einwegnachrichten sowie Anfragen und Antworten über TCP, WebSocket und QUIC. Die Anwendungslogik bleibt beim Wechsel des Transports unverändert. Typische Einsatzbereiche sind Chats, Onlinespiele, Telemetrie und die Kommunikation zwischen Diensten.

## Den passenden Transport wählen

| Transport | Einsatzbereich |
|---|---|
| TCP | Direkte Verbindungen für native Clients und zwischen Diensten, bei Bedarf mit TLS. |
| WebSocket | Browser und HTTP-Gateways, die ein WebSocket-Upgrade unterstützen. Im Produktivbetrieb WSS verwenden. |
| QUIC | Native Clients in wechselhaften Netzen, in denen UDP verfügbar ist. Wenn beide Seiten `msgtrans/2` aushandeln, kann der Server ohne zusätzlichen Roundtrip auf Anwendungsebene zuerst senden. |

## Was ist neu in 2.0?

- **Flusskontrolle je Verbindung:** Begrenzte Warteschlangen verhindern, dass sich bei langsamen Gegenstellen unbegrenzt Nachrichten ansammeln.
- **Anfragen sind an ihre Verbindung gebunden:** Eine verspätete Antwort aus der alten Verbindung kann keine Anfrage der neuen Verbindung abschließen. Der `Responder` des Servers ist nur einmal verwendbar.
- **Schreibbestätigung ist keine Zustellbestätigung:** Sie bestätigt, dass der lokale Transport die Daten angenommen hat, nicht dass die Gegenseite sie empfangen oder verarbeitet hat.
- **Überprüfbares Herunterfahren:** `shutdown()` meldet, ob Listener und Sitzungen innerhalb der Frist beendet wurden.

## Schnellstart mit Rust

Rust 1.88 oder neuer ist erforderlich.

```toml
[dependencies]
msgtrans = "2.0.0"
```

Im Stammverzeichnis des Repositorys den Echo-Server starten und den TCP-Client in einem zweiten Terminal ausführen:

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[Vollständige Rust-Anleitung](README.md) · [API-Referenz](https://docs.rs/msgtrans)

## Native Bindings für Kotlin und Swift

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) stellt mit UniFFI die **Client-API** für Kotlin (Android, JVM, Kotlin Multiplatform) und Swift (iOS, macOS) bereit. Die Bindings unterstützen TCP (einschließlich TLS), WebSocket und QUIC und können vom Server initiierte Anfragen beantworten. Die Server-API bleibt in Rust. Die FFI wird als native Bibliothek mit generierten Bindings ausgeliefert, nicht als separates Paket auf crates.io. Für Browser gibt es einen separaten [TypeScript-WebSocket-Client](https://github.com/zoujiaqing/msgtrans-typescript).

## Umstieg von 1.x

[Migrationsleitfaden](docs/MIGRATION_2.0.md) · [Paketformat](docs/WIRE_FORMAT.md). Das Paketformat ist unverändert, die Rust-API und die Verbindungsregeln haben sich jedoch geändert. Strikte Dekodierung ist jetzt Standard. QUIC-Gegenstellen ohne ALPN-Angebot, etwa MsgTrans 1.0.10, werden nicht unterstützt.

Apache-2.0-Lizenz. Siehe [LICENSE](LICENSE).
