use criterion::{Criterion, criterion_group, criterion_main};
use soroban_cost_profiler::aggregator::ProfileAggregator;
use soroban_cost_profiler::formatter::OutputFormatter;
use soroban_cost_profiler::models::Metric;
use soroban_cost_profiler::source_map::SourceMapper;
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, invoke_function, setup_engine,
    setup_mock_env,
};
use wasmi::{Engine, Func, Instance, Module, Store, Val};

const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");
const EXPORT: &str = "caller_of_heavy";

/// The pieces one invocation of the benchmarked export needs, assembled the way `run_target`
/// assembles them.
struct Run {
    store: Store<ProfilerState>,
    instance: Instance,
    func: Func,
    results: Vec<Val>,
}

/// `metered` decides whether the engine counts fuel — the one thing `setup_engine` turns on that a
/// plain `wasmi` host leaves off — and `sample_rate` is the CLI's `--sample-rate`.
fn setup(metered: bool, sample_rate: u64) -> Run {
    let engine = if metered {
        setup_engine()
    } else {
        Engine::default()
    };
    let module = Module::new(&engine, FIXTURE).expect("the committed fixture compiles");
    let state = ProfilerState {
        tracer: ExecutionTracer::new().with_sample_rate(sample_rate),
        host: setup_mock_env(),
        last_fuel: 0,
    };
    let mut store = Store::new(&engine, state);
    if metered {
        store
            .set_fuel(u64::MAX)
            .expect("a metered engine accepts a fuel reserve");
    }
    let instance = instantiate_module(&engine, &mut store, &module).expect("fixture instantiates");
    let func = instance
        .get_func(&store, EXPORT)
        .expect("the fixture exports the benchmarked function");
    let results = func
        .ty(&store)
        .results()
        .iter()
        .map(|ty| Val::default_for_ty(*ty))
        .collect();
    Run {
        store,
        instance,
        func,
        results,
    }
}

/// The export run by the engine alone: no fuel metering, no call hook, no tracer, no file. This is
/// the denominator every other number here is a multiple of.
fn bench_raw_execution(c: &mut Criterion) {
    let mut run = setup(false, 1000);
    c.bench_function("execution/raw", |b| {
        b.iter(|| {
            run.func
                .call(&mut run.store, &[], &mut run.results)
                .expect("the fixture runs");
            std::hint::black_box(&run.results);
        })
    });
}

/// The profiler's engine configuration with fuel metering on and no hook installed, so what this
/// measures is the cost of the *configuration* rather than of the tracing.
fn bench_metered_execution(c: &mut Criterion) {
    let mut run = setup(true, 1000);
    c.bench_function("execution/metered_no_hook", |b| {
        b.iter(|| {
            run.func
                .call(&mut run.store, &[], &mut run.results)
                .expect("the fixture runs");
            std::hint::black_box(&run.results);
        })
    });
}

/// The execution stage of a profiling run at the CLI's default `--sample-rate`: `invoke_function`
/// installs the call hook, the tracer records the boundaries the engine reports, and the events
/// leave the store the way `run_target` takes them.
fn bench_traced_execution(c: &mut Criterion) {
    let mut run = setup(true, 1000);
    c.bench_function("execution/traced_sample_1000", |b| {
        b.iter(|| {
            invoke_function(&mut run.store, &run.instance, EXPORT, &[], &mut run.results)
                .expect("the fixture runs under the hook");
            let events = run.store.data_mut().tracer.flush_trace();
            std::hint::black_box(events.len());
        })
    });
}

/// The same run at `--sample-rate 1`. A boundary-only hook cannot emit more often than the engine
/// reports boundaries, so this is the measurement that shows whether the flag has any cost to save.
fn bench_traced_dense(c: &mut Criterion) {
    let mut run = setup(true, 1);
    c.bench_function("execution/traced_sample_1", |b| {
        b.iter(|| {
            invoke_function(&mut run.store, &run.instance, EXPORT, &[], &mut run.results)
                .expect("the fixture runs under the hook");
            let events = run.store.data_mut().tracer.flush_trace();
            std::hint::black_box(events.len());
        })
    });
}

/// A whole profiling run minus process start and the file write: hook, events, source map,
/// aggregation and the collapsed-stack text. This is what one `soroban-cost-profiler` invocation
/// is made of, so its ratio to `execution/raw` is the end-to-end multiplier.
fn bench_full_pipeline(c: &mut Criterion) {
    let mut run = setup(true, 1000);
    let mapper = SourceMapper::new(FIXTURE).expect("the fixture carries line tables");
    c.bench_function("pipeline/profile_and_format", |b| {
        b.iter(|| {
            invoke_function(&mut run.store, &run.instance, EXPORT, &[], &mut run.results)
                .expect("the fixture runs under the hook");
            let events = run.store.data_mut().tracer.flush_trace();
            let mut aggregator = ProfileAggregator::new();
            let tree = aggregator.aggregate(events, &mapper);
            let folded = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);
            std::hint::black_box(folded.len());
        })
    });
}

/// The per-boundary bookkeeping on its own, at the same sample rate the hook would use.
fn bench_record_step(c: &mut Criterion) {
    let mut tracer = ExecutionTracer::new().with_sample_rate(100);
    c.bench_function("record_step", |b| {
        b.iter(|| {
            let _ = tracer.record_step(
                std::hint::black_box(1),
                std::hint::black_box(10),
                std::hint::black_box(5),
            );
        })
    });
}

criterion_group!(
    benches,
    bench_raw_execution,
    bench_metered_execution,
    bench_traced_execution,
    bench_traced_dense,
    bench_full_pipeline,
    bench_record_step
);
criterion_main!(benches);
