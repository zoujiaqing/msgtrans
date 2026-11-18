# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | **한국어** | [Tiếng Việt](README.vi-VN.md)

MsgTrans는 실시간 애플리케이션을 위한 Rust 양방향 메시징 라이브러리입니다. 하나의 API로 TCP, WebSocket, QUIC에서 단방향 메시지와 요청·응답을 주고받을 수 있어, 전송 방식을 바꿔도 애플리케이션 로직을 다시 작성할 필요가 없습니다. 채팅, 온라인 게임, 장치 텔레메트리, 서비스 간 통신에 적합합니다.

## 전송 방식 선택

| 전송 방식 | 적합한 환경 |
|---|---|
| TCP | 직접 연결할 수 있는 네이티브 클라이언트와 서비스 간 통신. 필요에 따라 TLS를 사용할 수 있습니다. |
| WebSocket | 브라우저 및 WebSocket Upgrade를 지원하는 HTTP 게이트웨이. 운영 환경에서는 WSS를 사용하세요. |
| QUIC | UDP를 사용할 수 있고 네트워크 품질이 변하는 환경. `msgtrans/2`를 협상하면 애플리케이션 계층의 추가 왕복 없이 서버가 먼저 데이터를 보낼 수 있습니다. |

## 2.0의 주요 변경 사항

- **연결별 백프레셔**: 큐의 크기를 제한해 느린 연결에 메시지가 계속 쌓이지 않도록 합니다.
- **연결에 귀속되는 요청**: 이전 연결의 지연 응답이 새 연결의 요청을 완료하지 못합니다. 서버의 `Responder`는 한 번만 사용할 수 있습니다.
- **쓰기 성공과 수신 확인 구분**: 쓰기 성공은 로컬 전송 계층이 데이터를 받아들였다는 뜻이며, 상대방의 수신이나 처리 완료를 보장하지 않습니다.
- **확인 가능한 종료**: 서버의 `shutdown()`은 정해진 시간 내에 리스너와 세션이 종료되었는지 보고합니다.

## Rust에서 시작하기

Rust 1.88 이상이 필요합니다.

```toml
[dependencies]
msgtrans = "2.0.0"
```

저장소 루트에서 에코 서버를 시작한 뒤, 별도 터미널에서 TCP 클라이언트를 실행하세요.

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[서버 및 클라이언트 예제](crates/msgtrans/examples/) · [Rust 상세 가이드](README.md) · [API 레퍼런스](https://docs.rs/msgtrans)

## Kotlin 및 Swift 네이티브 바인딩

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md)는 UniFFI를 통해 Kotlin(Android, JVM, Kotlin Multiplatform)과 Swift(iOS, macOS)에 **클라이언트 API**를 제공합니다. TCP(TLS 포함), WebSocket, QUIC를 지원하며 서버가 시작한 요청에도 응답할 수 있습니다. 서버 API는 Rust에서만 사용할 수 있습니다. FFI는 crates.io에 별도로 게시하지 않으며 네이티브 라이브러리와 생성된 바인딩으로 배포합니다. 브라우저에서는 별도의 [TypeScript WebSocket 클라이언트](https://github.com/zoujiaqing/msgtrans-typescript)를 사용할 수 있습니다.

## 1.x에서 업그레이드

[마이그레이션 가이드](docs/MIGRATION_2.0.md) · [패킷 형식](docs/WIRE_FORMAT.md). 패킷 형식은 동일하지만 Rust API와 연결 규칙은 변경되었습니다. 엄격한 디코딩이 기본이며, MsgTrans 1.0.10처럼 ALPN을 제시하지 않는 QUIC 피어는 지원하지 않습니다.

Apache-2.0 라이선스. 자세한 내용은 [LICENSE](LICENSE)를 참조하세요.
