# MsgTrans

[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文（台灣）](README.zh-TW.md) | **Français** | [日本語](README.ja-JP.md) | [한국어](README.ko-KR.md) | [Tiếng Việt](README.vi-VN.md) | [Español](README.es-ES.md)

MsgTrans est une bibliothèque Rust de messagerie bidirectionnelle pour les applications temps réel. Une même API permet d’envoyer des messages à sens unique ou d’échanger des requêtes et des réponses sur TCP, WebSocket ou QUIC, sans réécrire la logique applicative.

## Quel transport choisir ?

| Transport | Cas d’usage |
|---|---|
| TCP, avec TLS si nécessaire | Communications entre services et clients natifs sur des réseaux où une connexion TCP directe est adaptée. |
| WebSocket | Navigateurs et déploiements derrière une passerelle HTTP qui accepte la mise à niveau WebSocket. En production, une passerelle peut terminer TLS pour exposer un point d’accès WSS. |
| QUIC | Clients natifs sur des réseaux variables où UDP est disponible. Lorsque les deux pairs négocient `msgtrans/2`, le serveur peut envoyer le premier message sans aller-retour applicatif supplémentaire. |

## Ce qu’apporte la version 2.0

- **Contre-pression par connexion.** Des files bornées limitent l’accumulation de messages lorsqu’un pair ou un gestionnaire ralentit.
- **Requêtes liées à leur connexion.** Une réponse tardive de l’ancienne connexion ne peut pas terminer une requête de la nouvelle. Chaque réponse passe par un `Responder` à usage unique.
- **Confirmation d’écriture explicite.** Une écriture confirmée indique que le transport local a accepté les données, pas que le pair les a reçues ni traitées.
- **Arrêt vérifiable.** Le serveur peut attendre la fermeture de ses sessions et la fin des négociations TLS/TCP et WebSocket ; le rapport indique si l’arrêt est complet.

## Installation

La bibliothèque nécessite Rust 1.88 ou une version ultérieure.

```toml
[dependencies]
msgtrans = "2.0.0"
```

Depuis la racine du dépôt, démarrez le serveur d’écho puis lancez le client TCP dans un second terminal :

```sh
cargo run -p msgtrans --example echo_server
```

```sh
cargo run -p msgtrans --example echo_client_tcp
```

- [Guide Rust complet et exemples](README.md) · [Référence de l’API](https://docs.rs/msgtrans)

## Liaisons natives Kotlin et Swift

[`msgtrans-ffi`](crates/msgtrans-ffi/README.md) expose l’**API cliente** via UniFFI pour Kotlin (Android, JVM, Kotlin Multiplatform) et Swift (iOS, macOS). Les liaisons prennent en charge TCP (avec TLS), WebSocket et QUIC, y compris les réponses aux requêtes initiées par le serveur. L’API serveur reste dans la bibliothèque Rust. Les bibliothèques natives et les liaisons générées sont distribuées ensemble, et non via un paquet crates.io distinct. Pour les navigateurs, le [client TypeScript pour WebSocket](https://github.com/zoujiaqing/msgtrans-typescript) est disponible séparément.

## Migration depuis 1.x

- [Migration depuis 1.x](docs/MIGRATION_2.0.md) · [Format des paquets](docs/WIRE_FORMAT.md)

Le format des paquets n’a pas changé, mais l’API Rust et certaines règles de connexion ont évolué. Le décodage strict est désormais le comportement par défaut. Les anciens pairs QUIC qui ne proposent pas d’ALPN, dont MsgTrans 1.0.10, ne sont pas pris en charge.

## Licence

Apache-2.0. Voir [LICENSE](LICENSE).
