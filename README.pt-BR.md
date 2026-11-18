# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | **Português (Brasil)** | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | [Bahasa Indonesia](README.id.md) | [العربية](README.ar.md)

MsgTrans é uma biblioteca Rust para troca bidirecional de mensagens em aplicações em tempo real. A mesma API permite enviar mensagens unidirecionais e trocar solicitações e respostas por TCP, WebSocket ou QUIC, sem reescrever a lógica da aplicação. É indicada para chats, jogos on-line, telemetria e comunicação entre serviços.

## Qual transporte escolher?

| Transporte | Quando usar |
|---|---|
| TCP | Conexões diretas de clientes nativos ou entre serviços, com TLS quando necessário. |
| WebSocket | Navegadores e gateways HTTP que aceitam o upgrade para WebSocket. Em produção, use WSS. |
| QUIC | Clientes nativos em redes de qualidade variável que permitem UDP. Quando os dois lados negociam `msgtrans/2`, o servidor pode enviar dados primeiro, sem uma ida e volta adicional na camada de aplicação. |

## Novidades na versão 2.0

- **Controle de fluxo por conexão:** filas com capacidade limitada evitam que mensagens se acumulem indefinidamente quando a outra ponta está lenta.
- **Solicitações vinculadas à conexão:** uma resposta atrasada da conexão anterior não pode concluir uma solicitação da nova conexão. O `Responder` no servidor só pode ser usado uma vez.
- **Confirmação de escrita não é confirmação de entrega:** indica que o transporte local aceitou os dados, não que o destinatário os recebeu ou processou.
- **Encerramento verificável:** `shutdown()` informa se os listeners e as sessões do servidor foram encerrados dentro do prazo.

## Início rápido com Rust

Requer Rust 1.88 ou posterior.

```toml
[dependencies]
msgtrans = "2.0.0"
```

Na raiz do repositório, inicie o servidor de eco e execute o cliente TCP em outro terminal:

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[Guia completo de Rust](README.md) · [Referência da API](https://docs.rs/msgtrans)

## Bindings nativos para Kotlin e Swift

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) fornece a **API de cliente** via UniFFI para Kotlin (Android, JVM e Kotlin Multiplatform) e Swift (iOS e macOS). Os bindings oferecem TCP (incluindo TLS), WebSocket e QUIC, além de permitir respostas a solicitações iniciadas pelo servidor. A API de servidor está disponível apenas na biblioteca Rust. A FFI é distribuída como bibliotecas nativas e bindings gerados, não como um pacote separado no crates.io. Para navegadores, há um [cliente WebSocket em TypeScript](https://github.com/zoujiaqing/msgtrans-typescript) separado.

## Migração da versão 1.x

[Guia de migração](docs/MIGRATION_2.0.md) · [Formato dos pacotes](docs/WIRE_FORMAT.md). O formato dos pacotes não mudou, mas a API Rust e as regras de conexão mudaram. A decodificação estrita é o padrão. Conexões QUIC com pares que não anunciam ALPN, como o MsgTrans 1.0.10, não são compatíveis.

Licença Apache-2.0. Consulte [LICENSE](LICENSE).
