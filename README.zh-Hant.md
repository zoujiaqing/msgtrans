# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | **繁體中文** | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | [Bahasa Indonesia](README.id.md) | [العربية](README.ar.md)

MsgTrans 是以 Rust 開發的雙向訊息傳輸函式庫，適用於即時應用程式。以同一套 API 在 TCP、WebSocket 和 QUIC 上傳送單向訊息、發出請求並接收回應，切換傳輸方式，不必重寫應用程式邏輯。適合即時通訊、線上遊戲、裝置遙測與服務間通訊。

## 選擇傳輸方式

| 傳輸方式 | 適用情境 |
|---|---|
| TCP | 原生用戶端直連及服務間通訊，可視需要啟用 TLS。 |
| WebSocket | 適合瀏覽器及支援 WebSocket Upgrade 的 HTTP 閘道。正式環境請使用 WSS。 |
| QUIC | 適合 UDP 可用且網路品質容易變動的環境。雙方協商 `msgtrans/2` 後，伺服器可以先傳送資料，不增加應用層往返。 |

## 2.0 主要改進

- **每條連線獨立實施背壓**：佇列有容量上限，避免對端緩慢時訊息無限制堆積。
- **請求繫結至原始連線**：舊連線延遲送達的回應不會誤完成新連線上的請求。伺服器的 `Responder` 僅能使用一次。
- **寫入確認不等於送達**：寫入成功代表本機傳輸層已接收資料，不保證對端已收到或處理。
- **可驗證的關閉流程**：伺服器的 `shutdown()` 會回報監聽器和工作階段是否在期限內停止。

## 開始使用 Rust

需要 Rust 1.88 或更新版本。

```toml
[dependencies]
msgtrans = "2.0.0"
```

在儲存庫根目錄啟動回音伺服器，再開另一個終端機執行 TCP 用戶端：

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[伺服器與用戶端範例](crates/msgtrans/examples/) · [完整 Rust 指南（英文）](README.md) · [API 參考文件](https://docs.rs/msgtrans)

## Kotlin 和 Swift 原生繫結

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) 透過 UniFFI 為 Kotlin（Android、JVM、Kotlin Multiplatform）和 Swift（iOS、macOS）提供**用戶端 API**。支援 TCP（含 TLS）、WebSocket 和 QUIC，也能回應伺服器主動發送的請求。伺服器 API 僅由 Rust 函式庫提供。FFI 不會單獨發佈到 crates.io，以原生函式庫和產生的繫結程式碼交付。瀏覽器可使用獨立的 [TypeScript WebSocket 用戶端](https://github.com/zoujiaqing/msgtrans-typescript)。

## 從 1.x 升級

[遷移指南](docs/MIGRATION_2.0.md) · [封包格式](docs/WIRE_FORMAT.md)。封包格式維持不變，但 Rust API 和連線規則已更新。預設採用嚴格解碼，不支援未提供 ALPN 的 QUIC 對端，例如 MsgTrans 1.0.10。

採用 Apache-2.0 授權，詳見 [LICENSE](LICENSE)。
