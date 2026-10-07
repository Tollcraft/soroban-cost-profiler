use criterion::{Criterion, criterion_group, criterion_main};
use soroban_cost_profiler::source_map::SourceMapper;

const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");

fn bench_address_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("address_resolution");

    let mapper = SourceMapper::new(FIXTURE).unwrap();

    group.bench_function("cached", |b| {
        // pre-warm the cache
        let _ = mapper.resolve(14);
        b.iter(|| {
            let _ = mapper.resolve(std::hint::black_box(14));
        })
    });

    group.bench_function("uncached", |b| {
        // By cycling through 5000 addresses, we exceed the 4096 cache limit,
        // causing it to continually clear and ensuring every lookup is a miss.
        let mut pc = 0;
        b.iter(|| {
            let _ = mapper.resolve(std::hint::black_box(pc));
            pc = (pc + 1) % 5000;
        })
    });

    group.finish();
}

criterion_group!(benches, bench_address_resolution);
criterion_main!(benches);
