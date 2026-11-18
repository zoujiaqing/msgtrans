# MsgTrans

[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

[English](README.md) | Français | [Español](README.es-ES.md) | [简体中文](crates/msgtrans/README.zh-CN.md)

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

- [Guide Rust complet et exemples](crates/msgtrans/README.md) · [Référence de l’API](https://docs.rs/msgtrans)
- [Migration depuis 1.x](docs/MIGRATION_2.0.md) · [Format des paquets](docs/WIRE_FORMAT.md)
- [Liaisons client UniFFI pour Kotlin et Swift](crates/msgtrans-ffi) · [Client TypeScript pour WebSocket](https://github.com/zoujiaqing/msgtrans-typescript)

Le format des paquets n’a pas changé, mais l’API Rust et certaines règles de connexion ont évolué. Le décodage strict est désormais le comportement par défaut. Les anciens pairs QUIC qui ne proposent pas d’ALPN, dont MsgTrans 1.0.10, ne sont pas pris en charge.

## Licence

Apache-2.0. Voir [LICENSE](LICENSE).
