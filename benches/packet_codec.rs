//! Packet encode/decode throughput baseline.
//!
//! Run with: `cargo bench --bench packet_codec`
//!
//! This is a micro-benchmark of the wire codec only (no I/O). It is meant to
//! catch regressions in `Packet::to_bytes` / `Packet::from_bytes` across a range
//! of payload sizes, not to produce headline network numbers.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use msgtrans::packet::Packet;

const SIZES: &[usize] = &[16, 256, 4096, 65536];

fn encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet_encode");
    for &size in SIZES {
        let packet = Packet::request(42, vec![0u8; size]);
        group.throughput(Throughput::Bytes((16 + size) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &packet, |b, p| {
            b.iter(|| black_box(p.to_bytes()));
        });
    }
    group.finish();
}

fn decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("packet_decode");
    for &size in SIZES {
        let bytes = Packet::request(42, vec![0u8; size]).to_bytes();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &bytes, |b, data| {
            b.iter(|| black_box(Packet::from_bytes(black_box(data.as_ref())).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, encode, decode);
criterion_main!(benches);
