# MsgTrans

[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

[English](README.md) | [Français](README.fr-FR.md) | Español | [简体中文](crates/msgtrans/README.zh-CN.md)

MsgTrans es una biblioteca Rust de mensajería bidireccional para aplicaciones en tiempo real. La misma API permite enviar mensajes unidireccionales o intercambiar solicitudes y respuestas mediante TCP, WebSocket o QUIC, sin reescribir la lógica de la aplicación.

## ¿Qué transporte elegir?

| Transporte | Cuándo usarlo |
|---|---|
| TCP, con TLS si es necesario | Comunicaciones entre servicios y clientes nativos en redes donde resulta apropiada una conexión TCP directa. |
| WebSocket | Navegadores y despliegues detrás de una pasarela HTTP que admite la actualización a WebSocket. En producción, la pasarela puede terminar TLS y ofrecer un punto de acceso WSS. |
| QUIC | Clientes nativos en redes variables donde UDP está disponible. Si ambos extremos negocian `msgtrans/2`, el servidor puede enviar el primer mensaje sin una ida y vuelta adicional a nivel de aplicación. |

## Novedades de la versión 2.0

- **Contrapresión por conexión.** Las colas acotadas limitan la acumulación de mensajes si un extremo o un manejador se ralentiza.
- **Solicitudes vinculadas a su conexión.** Una respuesta tardía de la conexión anterior no puede completar una solicitud de la nueva. Cada respuesta se envía mediante un `Responder` de un solo uso.
- **Confirmación de escritura explícita.** Confirma que el transporte local aceptó los datos, no que el extremo remoto los haya recibido o procesado.
- **Cierre verificable.** El servidor puede esperar a que terminen sus sesiones y las negociaciones TLS/TCP y WebSocket en curso; el informe indica si el cierre fue completo.

## Instalación

La biblioteca requiere Rust 1.88 o una versión posterior.

```toml
[dependencies]
msgtrans = "2.0.0"
```

- [Guía completa de Rust y ejemplos](crates/msgtrans/README.md) · [Referencia de la API](https://docs.rs/msgtrans)
- [Migración desde 1.x](docs/MIGRATION_2.0.md) · [Formato de paquetes](docs/WIRE_FORMAT.md)
- [Enlaces de cliente UniFFI para Kotlin y Swift](crates/msgtrans-ffi) · [Cliente TypeScript para WebSocket](https://github.com/zoujiaqing/msgtrans-typescript)

El formato de paquetes no ha cambiado, pero sí la API de Rust y algunas reglas de conexión. La decodificación estricta está activada de forma predeterminada. No se admiten los extremos QUIC antiguos que no anuncian ALPN, incluido MsgTrans 1.0.10.

## Licencia

Apache-2.0. Consulta [LICENSE](LICENSE).
