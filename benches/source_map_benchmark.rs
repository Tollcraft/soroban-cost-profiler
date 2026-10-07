use criterion::{Criterion, criterion_group, criterion_main};
use soroban_cost_profiler::source_map::SourceMapper;

const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");

fn bench_address_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("address_resolution");

    group.bench_function("cached", |b| {
        let mapper = SourceMapper::new(FIXTURE).unwrap();
        // pre-warm the cache
        let _ = mapper.resolve(14);
        b.iter(|| {
            let _ = mapper.resolve(std::hint::black_box(14));
        })
    });

    group.bench_function("uncached", |b| {
        b.iter_batched(
            || SourceMapper::new(FIXTURE).unwrap(),
            |mapper| {
                let _ = mapper.resolve(std::hint::black_box(14));
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_address_resolution);
criterion_main!(benches);
