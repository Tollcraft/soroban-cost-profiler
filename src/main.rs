//! Binary entry point for `soroban-cost-profiler`.
//!
//! The profiler is a four-stage pipeline, and `main` is the wiring diagram for it:
//!
//! 1. **Trace** — run the contract's WASM under the instrumented engine and collect
//!    a flat stream of [`TraceEvent`]s (`tracer`).
//! 2. **Symbolize** — turn each event's program counter into a `file:line` frame
//!    (`source_map`).
//! 3. **Aggregate** — fold the flat stream into a [`CallStackNode`] tree carrying
//!    inclusive/exclusive costs (`aggregator`).
//! 4. **Format** — serialize the tree as `.folded` collapsed stacks for external
//!    viewers such as speedscope (`formatter`).
//!
//! Stage 1 is now a real run: `profile` reads `--wasm`, instantiates it, and invokes the export
//! named by `--fn`. Stage 2 is the one that still under-delivers — the mapper resolves DWARF and
//! `name`-section frames, but `wasmi` 2.0 hands its call hook no program counter, so every event
//! arrives at `pc = 0` and every frame reaches Stage 3 as an unresolved `wasm[0]`.
//!
//! Each stage's construction lives in its own function so that wiring the next phase in is a
//! one-line change at the call site, and so the placeholder input each stage needs today has a
//! documented home instead of sitting inline in `main`.
//!
//! [`TraceEvent`]: soroban_cost_profiler::models::TraceEvent
//! [`CallStackNode`]: soroban_cost_profiler::models::CallStackNode
use clap::Parser;
use soroban_cost_profiler::aggregator::ProfileAggregator;
use soroban_cost_profiler::formatter::OutputFormatter;
use soroban_cost_profiler::models::{Metric, TraceEvent};
use soroban_cost_profiler::source_map::SourceMapper;
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, invoke_function, load_wasm_file,
    parse_module, setup_engine, setup_mock_env,
};
use std::io::IsTerminal;
use std::path::PathBuf;
use tracing::warn;
use wasmi::{ExternType, Val};

/// Soroban Cost Profiler
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to the compiled WASM contract
    #[arg(short, long)]
    pub wasm: PathBuf,

    /// Output file path for the .folded stacks
    #[arg(short, long, default_value = "profile.folded")]
    pub output: PathBuf,

    /// Exported function to invoke, e.g. `--fn call` (required)
    #[arg(long = "fn", default_value = "")]
    pub fn_name: String,

    /// Sampling rate
    #[arg(long, default_value_t = 1000)]
    pub sample_rate: u32,

    /// Cost metric the `.folded` counts are written in
    #[arg(long, value_enum, default_value_t = Metric::Cpu)]
    pub metric: Metric,
}

/// How many functions the terminal summary ranks (#181's "top 5").
///
/// A constant and not a flag: the summary is a glance at the run, the `.folded` file is the
/// artifact, and a reader who wants the whole ranking has the file.
const TOP_FUNCTIONS: usize = 5;

/// Stage 1: build a tracer carrying the CLI's sampling rate and the MVP instruction ceiling.
fn initialize_tracer(cli: &Cli) -> ExecutionTracer {
    ExecutionTracer::new().with_sample_rate(cli.sample_rate as u64)
}

/// Stage 2: build the source mapper for the target WASM binary.
///
/// Loading is fallible now that the stage reads DWARF, and the failure is not fatal: a binary
/// without symbols still profiles, it just names frames `wasm[pc]`. The error is logged because
/// it tells the user which build flag to set — a flamegraph of unnamed frames is otherwise
/// indistinguishable from a profiler that is not working.
fn load_source_mapper(wasm_bytes: &[u8]) -> SourceMapper {
    SourceMapper::new(wasm_bytes).unwrap_or_else(|error| {
        warn!("cannot symbolize frames: {error}");
        SourceMapper::unmapped()
    })
}

/// Stage 3: build an empty aggregator.
fn initialize_aggregator() -> ProfileAggregator {
    ProfileAggregator::new()
}

/// Describe a `--fn` the module cannot run by listing the exports it does have.
///
/// The list is the point. Without it `--fn compute_heavy` and a forgotten `--fn` both read as
/// the same dead end, and the user has no way to tell a typo from a missing flag without
/// reaching for `wasm-objdump`.
fn unknown_export(fn_name: &str, module: &wasmi::Module) -> String {
    let mut functions: Vec<&str> = module
        .exports()
        .filter(|export| matches!(export.ty(), ExternType::Func(_)))
        .map(|export| export.name())
        .collect();
    functions.sort_unstable();
    let names = functions.join(", ");
    if fn_name.is_empty() {
        format!("--fn is required; the module exports {names}")
    } else {
        format!("'{fn_name}' is not an exported function; the module exports {names}")
    }
}

/// What one traced run produced: the boundaries it crossed, the values it returned, and the trap
/// that ended it if it did not finish.
///
/// `trapped` is a field rather than an `Err` because the two outcomes it separates need different
/// handling downstream: the trace has to reach aggregation (#173's requirement that a panicking
/// contract still yields a flamegraph up to that point), while the failure still has to reach the
/// user and the exit status. Returning `Err` would discard the trace, and returning `Ok` with no
/// trap marker would report a half-executed contract as a complete profile.
#[derive(Debug)]
struct TargetRun {
    events: Vec<TraceEvent>,
    /// The callee's own return values, empty when the run did not reach its end.
    ///
    /// The CLI itself has no use for them — a `.folded` file is the deliverable — but they are the
    /// only evidence that distinguishes a completed run from one that never started (see
    /// [`run_target`]), so the tests read them and the field has to exist outside `cfg(test)`.
    #[allow(
        dead_code,
        reason = "read by the stage-1 tests as the proof that the export executed"
    )]
    values: Vec<Val>,
    trapped: Option<String>,
}

/// Stage 1, executed: instantiate `wasm_bytes`, invoke `fn_name`, and hand back the trace it
/// produced next to the function's own return values.
///
/// The values are part of this function's contract because the trace cannot prove a run happened.
/// `wasmi` 2.0's call hook reports no program counter and no fuel, so every boundary it records
/// costs 0, and an aborted run writes a `.folded` file byte-identical to a completed one. Whoever
/// needs to know whether the contract actually executed reads the results — which is what the
/// tests below assert.
///
/// Every failure returns a message rather than calling `process::exit`, so the failure paths stay
/// testable and `main` remains the only place that decides how to report them. The two ways a run
/// fails are kept apart on purpose:
///
/// - **Nothing ran** (unparsable bytes, unknown export, instantiation error) is an `Err` and yields
///   no trace. Instantiation executes the module's start section and host imports, and the traced
///   function never begins, so any file written from that path would be a profile of a call that
///   was not made.
/// - **It ran and trapped** is an `Ok` carrying the partial trace plus [`TargetRun::trapped`],
///   because the boundaries crossed before the panic are exactly the data #173 asks to keep. The
///   trap is still reported — see [`profile`], which writes the file and then fails the
///   invocation, so a truncated profile never looks like a finished one.
fn run_target(
    wasm_bytes: &[u8],
    fn_name: &str,
    tracer: ExecutionTracer,
) -> Result<TargetRun, String> {
    let engine = setup_engine();
    let module = parse_module(&engine, wasm_bytes)
        .map_err(|error| format!("failed to parse WASM module: {error}"))?;
    if !matches!(module.get_export(fn_name), Some(ExternType::Func(_))) {
        return Err(unknown_export(fn_name, &module));
    }

    let state = ProfilerState {
        tracer,
        host: setup_mock_env(),
        last_fuel: 0,
    };
    let mut store = wasmi::Store::new(&engine, state);
    store
        .set_fuel(u64::MAX)
        .map_err(|error| format!("failed to enable fuel metering: {error}"))?;

    let instance = instantiate_module(&engine, &mut store, &module)
        .map_err(|error| format!("failed to instantiate module: {error}"))?;

    // Sized and typed from the signature: a contract returning `u64` gets an `I64` slot, and a
    // void one runs on an empty buffer.
    let mut results: Vec<Val> = instance
        .get_func(&store, fn_name)
        .ok_or_else(|| unknown_export(fn_name, &module))?
        .ty(&store)
        .results()
        .iter()
        .map(|ty| Val::default_for_ty(*ty))
        .collect();

    // A trap leaves `results` untouched — the function never returned — so the run reports no
    // values and names the trap, while the events recorded up to the trap go to aggregation.
    let trapped = invoke_function(&mut store, &instance, fn_name, &[], &mut results)
        .err()
        .map(|error| error.to_string());
    let values = if trapped.is_some() {
        Vec::new()
    } else {
        results
    };
    Ok(TargetRun {
        events: store.into_data().tracer.flush_trace(),
        values,
        trapped,
    })
}

/// Run the whole pipeline for one CLI invocation: write the folded stack to `--output` and print
/// the ranked summary to stdout.
///
/// Stage 1 is now a real run of the contract named by `--fn`, so the tree it aggregates is the
/// boundaries that run crossed. The costs in it are still all zero — see [`run_target`]'s note on
/// what the engine hook reports — which is why the folded output names `wasm[0]` and nothing else,
/// and why the summary today usually says that nothing was costed rather than lying with an
/// empty table.
///
/// A contract that traps mid-call is #173's case: the partial trace is aggregated and written,
/// because the frames it crossed before the panic are the profile the user came for, and the
/// invocation then fails with the trap. The order matters in both directions. Failing before the
/// write loses the data; succeeding after it leaves a truncated profile indistinguishable from a
/// complete one, which is how a profiler reports a contract that never finished. The summary is
/// skipped on that path — ranking five zero-cost frames of a run that stopped early is noise next
/// to the message that says it stopped early.
fn profile(cli: &Cli) -> Result<(), String> {
    // 1. Read the contract and run the target export under the tracer.
    let wasm_bytes = load_wasm_file(&cli.wasm.to_string_lossy())
        .map_err(|error| format!("failed to read {}: {error}", cli.wasm.display()))?;
    let run = run_target(&wasm_bytes, &cli.fn_name, initialize_tracer(cli))?;

    // 2. Load DWARF source map
    let mapper = load_source_mapper(&wasm_bytes);

    // 3. Aggregate events into call tree
    let mut aggregator = initialize_aggregator();
    let call_tree = aggregator.aggregate(run.events, &mapper);

    // 4. Format and output
    let output = OutputFormatter::to_collapsed_stack(&call_tree, &cli.metric);
    std::fs::write(&cli.output, output).map_err(|error| {
        format!(
            "failed to write folded stack to {}: {error}",
            cli.output.display()
        )
    })?;

    if let Some(trap) = run.trapped {
        return Err(format!(
            "'{fn}' trapped: {trap}. The partial trace up to the trap is in {path}, and its costs \
             are incomplete because the call never returned.",
            fn = cli.fn_name,
            path = cli.output.display()
        ));
    }

    let ranked = OutputFormatter::top_functions(&call_tree, &cli.metric, TOP_FUNCTIONS);
    println!(
        "{}",
        OutputFormatter::to_top_summary(&ranked, &cli.metric, std::io::stdout().is_terminal())
    );
    Ok(())
}

/// Run the pipeline, reporting any failure on stderr instead of leaving the user a silent exit.
///
/// Every message goes through `eprintln!` rather than `tracing` because nothing in this crate
/// installs a subscriber: a `tracing::error!` on a fatal path writes nowhere, which is how a CLI
/// that had never run its WASM still managed to print a plausible-looking empty profile.
fn main() {
    let cli = Cli::parse();
    if let Err(error) = profile(&cli) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_cost_profiler::models::EventType;

    const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");

    /// `caller_of_heavy` is `compute_heavy_loop() + memory_heavy_loop()`: the first sums
    /// `3 * i` for `i < 1000`, the second sums `7 * i` for `i < 64`.
    const CALLER_OF_HEAVY: i64 = 1_512_612;

    fn cli(output: PathBuf, wasm: PathBuf, fn_name: &str) -> Cli {
        Cli {
            wasm,
            output,
            fn_name: fn_name.into(),
            sample_rate: 1000,
            metric: Metric::Cpu,
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dwarf_probe/dwarf_probe.wasm")
    }

    fn tracer() -> ExecutionTracer {
        ExecutionTracer::new().with_sample_rate(1000)
    }

    /// The issue's "done" state: the named export really executes. The return value is the only
    /// evidence that distinguishes a completed run from one that never started, so this asserts
    /// the number the contract computes rather than the shape of the trace.
    #[test]
    fn the_named_export_is_invoked_and_its_result_returned() {
        let run = run_target(FIXTURE, "caller_of_heavy", tracer()).unwrap();
        assert!(
            matches!(run.values.as_slice(), [Val::I64(value)] if *value == CALLER_OF_HEAVY),
            "the run must return the value the contract computes, got {:?}",
            run.values
        );
        assert!(run.trapped.is_none(), "a completed run reports no trap");
        let kinds: Vec<&EventType> = run.events.iter().map(|event| &event.event_type).collect();
        assert!(
            kinds.contains(&&EventType::Call) && kinds.contains(&&EventType::Return),
            "the run must cross both boundaries, not just report a value: {kinds:?}"
        );
    }

    #[test]
    fn an_unknown_fn_names_the_functions_the_module_does_export() {
        let error = run_target(FIXTURE, "compute_heavy", tracer()).unwrap_err();
        assert!(
            error.contains("caller_of_heavy") && error.contains("memory_heavy_loop"),
            "{error}"
        );
    }

    /// The harness used to answer an absent `--fn` by running a `"test"` export no fixture has,
    /// which failed silently. Not knowing which export to profile is not a runnable default.
    #[test]
    fn an_empty_fn_name_is_an_error_rather_than_a_guess() {
        let error = run_target(FIXTURE, "", tracer()).unwrap_err();
        assert!(error.starts_with("--fn is required"), "{error}");
    }

    #[test]
    fn a_missing_wasm_file_fails_instead_of_profiling_nothing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cli = cli(
            temp_dir.path().join("profile.folded"),
            fixture().parent().unwrap().join("nope.wasm"),
            "caller_of_heavy",
        );
        let error = profile(&cli).unwrap_err();
        assert!(error.contains("nope.wasm"), "{error}");
        assert!(!temp_dir.path().join("profile.folded").exists());
    }

    /// The stages hand data to each other in the documented order, and a real run now reaches the
    /// formatter: the file is written and parses as folded stacks.
    #[test]
    fn assembling_the_stages_runs_to_completion() {
        let temp_dir = tempfile::tempdir().unwrap();
        let output_path = temp_dir.path().join("profile.folded");
        profile(&cli(output_path.clone(), fixture(), "caller_of_heavy")).unwrap();

        let collapsed = std::fs::read_to_string(&output_path).unwrap();
        let stacks = OutputFormatter::parse_folded(&collapsed)
            .expect("the pipeline's own output must be valid folded stacks");
        assert!(!stacks.is_empty(), "the run must produce a frame");
    }

    /// `(module (func (export "boom") unreachable))` — the smallest contract that traps.
    ///
    /// Written as section bytes rather than built by a toolchain, the same way `source_map.rs`'s
    /// tests synthesize modules: a fixture that needs `wasm32-unknown-unknown` to exist cannot run
    /// in `cargo test` on a machine without it, and #173 is about the trap path, not about what
    /// traps. Bytes, in order: 8-byte header; type section (one `() -> {}` function type); function
    /// section (function 0 has type 0); export section (`"boom"` = func 0); code section (body of
    /// `unreachable` + `end`).
    const BOOM: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x04\x01\x60\x00\x00\x03\x02\x01\x00\x07\x08\x01\x04boom\x00\x00\x0a\x05\x01\x03\x00\x00\x0b";

    /// The #173 requirement at the stage-1 boundary: a trap keeps the boundaries it crossed and
    /// says it trapped, instead of either discarding the trace or reporting a complete run.
    ///
    /// The last assertion is a measurement, not an expectation. `wasmi`'s call hook fires
    /// `ReturningFromWasm` while the trap unwinds, so a call that never returned still closes its
    /// frame in the trace. That is why the trap cannot live in the event stream alone — a profile
    /// built from this trace is structurally indistinguishable from one where the call finished, so
    /// `trapped` has to travel beside the events and be reported by the CLI.
    #[test]
    fn a_trapping_contract_keeps_its_partial_trace_and_reports_the_trap() {
        let run = run_target(BOOM, "boom", tracer()).unwrap();
        assert!(
            run.trapped.is_some(),
            "the run must record that it did not finish"
        );
        assert!(run.values.is_empty(), "a trapped call returns no values");
        let kinds: Vec<&EventType> = run.events.iter().map(|event| &event.event_type).collect();
        assert!(
            kinds.contains(&&EventType::Call),
            "the call that trapped was still crossed, so its boundary belongs in the trace: {kinds:?}"
        );
        assert!(
            kinds.contains(&&EventType::Return),
            "measured behavior: the hook emits a Return while the trap unwinds, so the trace \
             alone cannot tell a truncated call from a finished one: {kinds:?}"
        );
    }

    /// #173's "done": the CLI outputs a partial stack when a contract panics. The file is what the
    /// user asked for, so it is written *before* the failure is reported — and the failure is still
    /// reported, because a truncated profile that exits 0 is indistinguishable from a finished one.
    #[test]
    fn a_trapped_run_writes_a_parseable_partial_profile_and_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let output_path = temp_dir.path().join("boom.folded");
        let temp_wasm = temp_dir.path().join("boom.wasm");
        std::fs::write(&temp_wasm, BOOM).unwrap();

        let error = profile(&cli(output_path.clone(), temp_wasm, "boom")).unwrap_err();
        assert!(
            error.contains("trapped") && error.contains("boom.folded"),
            "the message must name the failure and the file that holds the partial trace: {error}"
        );
        let collapsed = std::fs::read_to_string(&output_path).unwrap();
        let stacks = OutputFormatter::parse_folded(&collapsed)
            .expect("a partial trace must still be a valid folded stack file");
        assert!(!stacks.is_empty(), "the partial run must produce a frame");
    }

    /// The other half of the split: a module that cannot be instantiated never runs the target
    /// export, so a profile of it would be a profile of a call that was never made.
    ///
    /// `NEEDS_HOST` is `(module (import "env" "missing" (func)) (func (export "boom") unreachable))`
    /// — it parses fine and then fails to instantiate, because `instantiate_module` links against an
    /// empty `Linker`. Truncating bytes instead would have tested the parse path, which is a
    /// different branch of the same rule.
    #[test]
    fn a_module_that_fails_to_instantiate_writes_no_profile() {
        const NEEDS_HOST: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x04\x01\x60\x00\x00\x02\x0f\x01\x03env\x07missing\x00\x00\x03\x02\x01\x00\x07\x08\x01\x04boom\x00\x01\x0a\x05\x01\x03\x00\x00\x0b";

        let temp_dir = tempfile::tempdir().unwrap();
        let output_path = temp_dir.path().join("broken.folded");
        let temp_wasm = temp_dir.path().join("broken.wasm");
        std::fs::write(&temp_wasm, NEEDS_HOST).unwrap();

        let error = profile(&cli(output_path.clone(), temp_wasm, "boom")).unwrap_err();
        assert!(
            error.contains("instantiate"),
            "the failure must say the module never ran, not just that something went wrong: {error}"
        );
        assert!(
            !output_path.exists(),
            "a run that never started must not leave a profile behind"
        );
    }
}
