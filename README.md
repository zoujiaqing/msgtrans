# MsgTrans

[![License](https://img.shields.io/badge/license-Apache-blue.svg)](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

A multi-protocol communication framework for Rust: one interface over TCP,
WebSocket and QUIC.

🌐 [English](crates/msgtrans/README.md) | [简体中文](crates/msgtrans/README.zh-CN.md)

## Repository layout

This repository is a Cargo workspace.

| Path | Contents |
|---|---|
| [`crates/msgtrans`](crates/msgtrans) | The library, published as [`msgtrans`](https://crates.io/crates/msgtrans). Its README is the full guide. |
| [`docs/`](docs) | Architecture, the frozen 2.0 API, migrating from 1.x, and the wire format. |

## Getting started

```toml
[dependencies]
msgtrans = "2.0.0-beta.2"
```

The [crate README](crates/msgtrans/README.md) covers servers, clients and
protocol configuration. The library builds on Rust 1.88 or newer.

## Development

```bash
cargo test --workspace
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the CI gates every change passes.

## License

Apache-2.0 — see [LICENSE](LICENSE).
