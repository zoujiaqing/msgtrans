# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | **日本語** | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | [Bahasa Indonesia](README.id.md) | [العربية](README.ar.md)

MsgTrans は、リアルタイムアプリケーション向けの Rust 製双方向メッセージングライブラリです。TCP、WebSocket、QUIC で共通の API を使い、アプリケーションロジックを変更せずに一方向メッセージやリクエスト／レスポンスを扱えます。チャット、オンラインゲーム、デバイスのテレメトリー、サービス間通信に適しています。

## トランスポートの選択

| トランスポート | 用途 |
|---|---|
| TCP | 直接接続できるネイティブクライアントやサービス間通信。必要に応じて TLS を利用できます。 |
| WebSocket | ブラウザーや WebSocket Upgrade に対応した HTTP ゲートウェイ経由の接続。本番環境では WSS を使用します。 |
| QUIC | UDP を利用でき、ネットワーク品質が変動する環境。`msgtrans/2` をネゴシエートした場合、アプリケーション層の往復を増やさずにサーバーから先に送信できます。 |

## 2.0 の主な変更点

- **接続単位のバックプレッシャー**：上限付きキューで、応答の遅い接続へのメッセージの蓄積を抑えます。
- **接続にひも付いたリクエスト**：再接続前の応答が新しい接続のリクエストを完了させることはありません。サーバーの `Responder` は一度だけ使えます。
- **書き込み結果と受信確認を区別**：書き込みの成功は、ローカルのトランスポートがデータを受け付けたことを示します。相手への到達や処理完了を保証するものではありません。
- **終了結果を確認可能**：サーバーの `shutdown()` は期限内にリスナーとセッションが停止したかを報告します。

## Rust で使う

Rust 1.88 以降が必要です。

```toml
[dependencies]
msgtrans = "2.0.0"
```

リポジトリのルートでエコーサーバーを起動し、別のターミナルで TCP クライアントを実行します。

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[サーバー・クライアントのサンプル](crates/msgtrans/examples/) · [Rust 詳細ガイド](README.md) · [API リファレンス](https://docs.rs/msgtrans)

## Kotlin と Swift のネイティブバインディング

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) は UniFFI を使い、Kotlin（Android、JVM、Kotlin Multiplatform）と Swift（iOS、macOS）に **クライアント API** を提供します。TCP（TLS を含む）、WebSocket、QUIC を利用でき、サーバーからのリクエストにも応答できます。サーバー API は Rust 側のみです。FFI は crates.io に単独で公開せず、ネイティブライブラリと生成されたバインディングとして配布します。ブラウザー向けには別途 [TypeScript WebSocket クライアント](https://github.com/zoujiaqing/msgtrans-typescript)があります。

## 1.x からの移行

[移行ガイド](docs/MIGRATION_2.0.md) · [パケット形式](docs/WIRE_FORMAT.md)。パケット形式は変わりませんが、Rust API と接続ルールは変更されています。厳密なデコードが既定で、MsgTrans 1.0.10 など ALPN を提示しない QUIC ピアには対応しません。

Apache-2.0 ライセンス。詳細は [LICENSE](LICENSE) を参照してください。
