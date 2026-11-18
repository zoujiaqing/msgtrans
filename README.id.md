# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | **Bahasa Indonesia** | [العربية](README.ar.md)

MsgTrans adalah pustaka Rust untuk pertukaran pesan dua arah dalam aplikasi waktu nyata. API yang sama mendukung pesan satu arah serta permintaan dan respons melalui TCP, WebSocket, atau QUIC, tanpa perlu menulis ulang logika aplikasi. Cocok untuk aplikasi obrolan, gim daring, telemetri, dan komunikasi antarlayanan.

## Memilih transport

| Transport | Kapan digunakan |
|---|---|
| TCP | Koneksi langsung untuk klien native atau antar layanan, dengan TLS bila diperlukan. |
| WebSocket | Peramban dan gateway HTTP yang mendukung upgrade ke WebSocket. Gunakan WSS di lingkungan produksi. |
| QUIC | Klien native pada jaringan dengan kualitas yang berubah-ubah dan mendukung UDP. Jika kedua pihak menegosiasikan `msgtrans/2`, server dapat mengirim data lebih dulu tanpa tambahan satu putaran komunikasi pada lapisan aplikasi. |

## Yang baru di 2.0

- **Pengendalian aliran per koneksi:** antrean berkapasitas terbatas mencegah pesan menumpuk tanpa batas ketika pihak lain lambat.
- **Permintaan terikat pada koneksinya:** respons yang terlambat dari koneksi lama tidak dapat menyelesaikan permintaan pada koneksi baru. `Responder` di sisi server hanya dapat digunakan sekali.
- **Konfirmasi penulisan bukan konfirmasi pengiriman:** konfirmasi ini hanya berarti transport lokal telah menerima data, bukan bahwa pihak lain sudah menerima atau memprosesnya.
- **Penghentian yang bisa diverifikasi:** `shutdown()` melaporkan apakah listener dan sesi server berhenti dalam batas waktu.

## Memulai dengan Rust

Memerlukan Rust 1.88 atau versi yang lebih baru.

```toml
[dependencies]
msgtrans = "2.0.0"
```

Dari direktori utama repositori, jalankan server echo, lalu jalankan klien TCP di terminal lain:

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[Panduan Rust lengkap](README.md) · [Referensi API](https://docs.rs/msgtrans)

## Binding native untuk Kotlin dan Swift

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) menyediakan **API klien** melalui UniFFI untuk Kotlin (Android, JVM, Kotlin Multiplatform) dan Swift (iOS, macOS). Binding ini mendukung TCP (termasuk TLS), WebSocket, dan QUIC, serta dapat menjawab permintaan yang dimulai oleh server. API server hanya tersedia di pustaka Rust. FFI didistribusikan sebagai pustaka native beserta binding yang dihasilkan, bukan sebagai paket terpisah di crates.io. Untuk peramban, tersedia [klien WebSocket TypeScript](https://github.com/zoujiaqing/msgtrans-typescript) yang terpisah.

## Migrasi dari 1.x

[Panduan migrasi](docs/MIGRATION_2.0.md) · [Format paket](docs/WIRE_FORMAT.md). Format paket tidak berubah, tetapi API Rust dan aturan koneksi telah berubah. Dekode ketat kini menjadi perilaku bawaan. Peer QUIC yang tidak menawarkan ALPN, termasuk MsgTrans 1.0.10, tidak didukung.

Lisensi Apache-2.0. Lihat [LICENSE](LICENSE).
