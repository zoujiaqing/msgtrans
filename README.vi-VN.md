# MsgTrans

[English](README.md) | [简体中文](README.zh-Hans.md) | [繁體中文](README.zh-Hant.md) | [Français](README.fr-FR.md) | [Deutsch](README.de.md) | [Español](README.es.md) | [Українська](README.uk.md) | [Русский](README.ru.md) | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | **Tiếng Việt**

MsgTrans là thư viện Rust cung cấp khả năng truyền tin hai chiều cho các ứng dụng thời gian thực. Một API dùng chung cho TCP, WebSocket và QUIC hỗ trợ cả tin nhắn một chiều lẫn mô hình yêu cầu/phản hồi, không cần viết lại logic ứng dụng khi đổi phương thức truyền tải. Phù hợp cho ứng dụng trò chuyện, trò chơi trực tuyến, thu thập dữ liệu thiết bị và giao tiếp giữa các dịch vụ.

## Chọn phương thức truyền tải

| Phương thức | Trường hợp sử dụng |
|---|---|
| TCP | Kết nối trực tiếp cho ứng dụng native hoặc giữa các dịch vụ. Có thể bật TLS khi cần. |
| WebSocket | Trình duyệt hoặc cổng HTTP hỗ trợ WebSocket Upgrade. Dùng WSS trong môi trường sản xuất. |
| QUIC | Mạng có chất lượng thay đổi và cho phép UDP. Khi hai bên thương lượng `msgtrans/2`, máy chủ có thể gửi trước mà không cần thêm một lượt khứ hồi ở tầng ứng dụng. |

## Điểm mới trong 2.0

- **Điều tiết lưu lượng theo kết nối**: hàng đợi có giới hạn giúp tránh tích tụ tin nhắn khi phía bên kia xử lý chậm.
- **Yêu cầu gắn với kết nối**: phản hồi trễ từ kết nối cũ không thể hoàn tất yêu cầu trên kết nối mới. `Responder` ở phía máy chủ chỉ dùng được một lần.
- **Xác nhận ghi không phải xác nhận đã nhận**: ghi thành công chỉ cho biết lớp truyền tải cục bộ đã tiếp nhận dữ liệu, không bảo đảm phía bên kia đã nhận hay xử lý.
- **Kiểm tra được kết quả dừng**: `shutdown()` báo cáo liệu các bộ lắng nghe và phiên của máy chủ đã dừng trong thời hạn hay chưa.

## Bắt đầu với Rust

Yêu cầu Rust 1.88 trở lên.

```toml
[dependencies]
msgtrans = "2.0.0"
```

Từ thư mục gốc của kho mã, khởi động máy chủ echo rồi chạy máy khách TCP trong một cửa sổ dòng lệnh khác:

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

[Ví dụ máy chủ và máy khách](crates/msgtrans/examples/) · [Hướng dẫn Rust đầy đủ](README.md) · [Tham chiếu API](https://docs.rs/msgtrans)

## Liên kết native cho Kotlin và Swift

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) dùng UniFFI để cung cấp **API máy khách** cho Kotlin (Android, JVM, Kotlin Multiplatform) và Swift (iOS, macOS). Các liên kết hỗ trợ TCP (kể cả TLS), WebSocket và QUIC, đồng thời có thể trả lời yêu cầu do máy chủ gửi. API máy chủ chỉ có trong thư viện Rust. FFI không phát hành riêng trên crates.io mà được phân phối dưới dạng thư viện native và mã liên kết được tạo tự động. Trình duyệt có thể dùng [máy khách WebSocket TypeScript](https://github.com/zoujiaqing/msgtrans-typescript) riêng.

## Nâng cấp từ 1.x

[Hướng dẫn chuyển đổi](docs/MIGRATION_2.0.md) · [Định dạng gói tin](docs/WIRE_FORMAT.md). Định dạng gói tin không đổi, nhưng API Rust và quy tắc kết nối đã thay đổi. Chế độ giải mã nghiêm ngặt được bật mặc định; không hỗ trợ đối tác QUIC không đề xuất ALPN, bao gồm MsgTrans 1.0.10.

Giấy phép Apache-2.0. Xem [LICENSE](LICENSE).
