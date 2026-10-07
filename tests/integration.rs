//! Integration coverage for the profiler's public API seams (#45).
//!
//! `tests/meter_probe.rs` drives the engine hooks by running WASM. This file covers the
//! entry points that run *around* a trace — module and file loading, engine setup, and the
//! tracer's sampling and ceiling knobs — where a mistake currently surfaces as a panic in CI
//! or as a silently empty trace rather than as a test failure. The last test hands a trace
//! through every stage at once, which is the only place the stages' shared types are checked
//! against each other rather than against a unit test's assumption.

use soroban_cost_profiler::aggregator::ProfileAggregator;
use soroban_cost_profiler::formatter::OutputFormatter;
use soroban_cost_profiler::source_map::SourceMapper;
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, load_wasm_file, parse_module, setup_engine,
    setup_mock_env,
};
use wasmi::Store;

#[test]
fn test_fixture_compile_and_trace() {
    let engine = setup_engine();
    let wasm_bytes = b"\0asm\x01\0\0\0"; // Minimal valid empty WASM for test
    let module = parse_module(&engine, wasm_bytes).expect("Failed to parse minimal WASM");
    let state = ProfilerState {
        tracer: ExecutionTracer::new(),
        host: setup_mock_env(),
        last_fuel: 0,
    };
    let mut store = Store::new(&engine, state);
    let _instance =
        instantiate_module(&engine, &mut store, &module).expect("Failed to instantiate");

    // Test the integration tracer flow
    let _ = store.data_mut().tracer.record_step(0, 10, 5);

    let events = store.data_mut().tracer.flush_trace();
    assert_eq!(events.len(), 0); // Didn't hit the default 100 sample rate
}

/// A uniquely-named scratch file in the temp dir, removed when the owning test ends.
///
/// `Drop` instead of cleanup at the end of the body: the assertions can fail, and a leftover
/// file would then decide the outcome of the next run of the same test.
struct ScratchFile(std::path::PathBuf);

impl ScratchFile {
    fn write(tag: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "soroban-cost-profiler-{}-{tag}",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("writing {path:?}: {error}"));
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().expect("temp dir paths are UTF-8")
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn parse_module_reports_an_error_for_bytes_that_are_not_wasm() {
    // A truncated download or a wrong file is the likeliest way to meet this API, and it has
    // to come back as an error the CLI can print rather than a panic that ends the run.
    let engine = setup_engine();

    let error = parse_module(&engine, b"not a wasm module at all")
        .expect_err("arbitrary bytes should not parse as a module");

    assert!(
        !error.to_string().is_empty(),
        "the error should say what was wrong with the input"
    );
}

#[test]
fn load_wasm_file_rejects_a_file_without_the_wasm_magic() {
    let scratch = ScratchFile::write("magic", b"\x7fELF\0\0not wasm");

    let error = load_wasm_file(scratch.path())
        .expect_err("a non-WASM file must not be handed to the engine as if it were one");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("WASM signature"),
        "the error should name the check that failed, got: {error}"
    );
}

#[test]
fn load_wasm_file_reports_a_missing_path_rather_than_panicking() {
    let missing = std::env::temp_dir().join(format!(
        "soroban-cost-profiler-{}-definitely-absent",
        std::process::id()
    ));

    let error = load_wasm_file(missing.to_str().unwrap())
        .expect_err("reading a path that does not exist should not succeed");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn the_engine_is_built_with_fuel_accounting_enabled() {
    // Every measurement the profiler makes rests on the engine being constructed with
    // `consume_fuel(true)`. If that config line is dropped, `set_fuel` starts failing and
    // the numbers mean nothing — so it is asserted here rather than noticed in a benchmark.
    let engine = setup_engine();
    let state = ProfilerState {
        tracer: ExecutionTracer::new(),
        host: setup_mock_env(),
        last_fuel: 0,
    };
    let mut store = Store::new(&engine, state);

    store
        .set_fuel(10_000)
        .expect("setup_engine() should enable fuel metering");

    assert_eq!(store.get_fuel().expect("fuel metering is enabled"), 10_000);
}

#[test]
fn sampling_can_be_turned_off_for_finer_attribution() {
    // The default 100-unit rate exists to bound memory use. The knob only earns its keep if
    // lowering it genuinely emits per-step events, which is what Phase 4's cost attribution
    // needs for short functions.
    let mut tracer = ExecutionTracer::new().with_sample_rate(1);

    tracer.record_step(7, 1, 0).expect("well under the ceiling");

    let events = tracer.flush_trace();
    assert_eq!(events.len(), 1, "a rate of 1 should emit every step");
    assert_eq!(events[0].pc, 7);
}

#[test]
fn the_instruction_ceiling_errors_at_the_configured_step() {
    // `invoke_function` discards this `Err` today (pinned by
    // `the_instruction_ceiling_does_not_stop_execution` in tests/meter_probe.rs), so the
    // tracer's own contract is the only place the guard can be verified until that is fixed.
    let mut tracer = ExecutionTracer::new()
        .with_sample_rate(u64::MAX)
        .with_instruction_ceiling(2);

    tracer
        .record_step(0, 1, 0)
        .expect("step 1 is inside the ceiling");
    tracer
        .record_step(1, 1, 0)
        .expect("step 2 is exactly at the ceiling");

    let error = tracer
        .record_step(2, 1, 0)
        .expect_err("step 3 passes the ceiling and should be refused");
    assert!(error.contains("ceiling"), "got: {error}");

    assert!(
        tracer.trace().is_empty(),
        "sampling was disabled, so nothing should have been emitted"
    );
}

#[test]
fn step_costs_accumulate_across_samples_instead_of_being_dropped() {
    // The documented invariant: the emitted `cpu_cost` values sum to the total cost passed
    // in. Without it a profile would quietly understate every function whose steps fall
    // between two samples.
    let mut tracer = ExecutionTracer::new(); // default sample rate: 100

    for _ in 0..3 {
        tracer
            .record_step(0, 40, 3)
            .expect("well under the ceiling");
    }

    let events = tracer.flush_trace();
    assert_eq!(
        events.len(),
        1,
        "120 units of cost should cross the 100-unit threshold exactly once"
    );
    assert_eq!(
        events[0].cpu_cost, 120,
        "the whole accumulated cost is emitted, not the threshold"
    );
    assert_eq!(
        events[0].mem_cost, 9,
        "memory accumulates on the same schedule"
    );

    // A fresh accumulation starts from zero after an emit, so the next sample needs its own
    // full threshold.
    tracer.record_step(0, 5, 0).expect("well under the ceiling");
    assert!(
        tracer.trace().is_empty(),
        "5 units alone is below the threshold"
    );
    tracer
        .record_step(0, 95, 0)
        .expect("well under the ceiling");
    assert_eq!(
        tracer.flush_trace()[0].cpu_cost,
        100,
        "the two steps after the reset should cross together"
    );
}

/// Stage 1 → 3 → 4, with nothing stubbed between them.
///
/// Each stage's unit tests hand it input in the shape it expects; this pins that the shapes
/// actually agree, which is where a pipeline breaks silently — a tree the formatter cannot walk,
/// or events the aggregator reads as a different boundary kind, both cost nothing and report
/// nothing.
#[test]
fn a_traced_run_reaches_the_viewer_as_folded_stacks() {
    let mut tracer = ExecutionTracer::new().with_sample_rate(1);
    tracer.record_call(0, 0, 0);
    let _ = tracer.record_step(0, 40, 7);
    // A nested call: the engine reports no inner boundaries and no program counters, so this is
    // what every real contract trace looks like today.
    tracer.record_call(0, 0, 0);
    let _ = tracer.record_step(0, 60, 3);
    tracer.record_return(0, 0, 0);
    tracer.record_return(0, 0, 0);

    let events = tracer.flush_trace();
    let mut aggregator = ProfileAggregator::new();
    let tree = aggregator.aggregate(events, &SourceMapper::unmapped());
    let folded = OutputFormatter::to_collapsed_stack(&tree, &soroban_cost_profiler::models::Metric::Cpu);

    // Asserted through the parser rather than as a string: `CallStackNode`'s children are a
    // `HashMap`, so line order is not stable across runs.
    let stacks = OutputFormatter::parse_folded(&folded)
        .unwrap_or_else(|error| panic!("the pipeline emitted unreadable stacks: {error}"));

    assert_eq!(
        stacks.len(),
        1,
        "unresolved boundaries all fold into one frame, got: {stacks:?}"
    );
    assert_eq!(
        stacks.values().sum::<u64>(),
        100,
        "the whole traced cost survives the trip through the pipeline"
    );
}
