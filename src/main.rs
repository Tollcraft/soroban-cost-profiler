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

/// `value_parser` for `--sample-rate`: accept a positive count, refuse everything else.
///
/// Zero is the case that matters. `ExecutionTracer::record_step` throttles by comparing
/// `current_step_cost >= sample_rate`, so a rate of 0 makes that true on every instruction and
/// silently turns sampling off — the trace then buffers one event per instruction up to the 100M
/// ceiling, which is the OOM `AGENTS.md` rule 5 exists to prevent. Rejecting it at the flag means the
/// run never starts, rather than starting and dying later with no explanation.
///
/// The two failures get different messages because they are different mistakes: a typo like `abc`
/// needs the offending text quoted back, while `0` is a well-formed number whose meaning is illegal.
fn parse_positive_u32(s: &str) -> Result<u32, String> {
    let val: u32 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid number"))?;
    if val == 0 {
        Err(String::from("must be greater than 0"))
    } else {
        Ok(val)
    }
}

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

    /// Record one trace event every N instructions (must be greater than 0)
    #[arg(long, default_value_t = 1000, value_parser = parse_positive_u32)]
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
/// testable and `main` remains the only place that decides how to report them. A trap mid-call is
/// reported and *not* flushed as a partial trace: that is #173's job, and until it lands the
/// partial trace would hold nothing but zero-cost boundaries anyway.
fn run_target(
    wasm_bytes: &[u8],
    fn_name: &str,
    tracer: ExecutionTracer,
) -> Result<(Vec<TraceEvent>, Vec<Val>), String> {
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

    let values = invoke_function(&mut store, &instance, fn_name, &[], &mut results)
        .map(|()| results)
        .map_err(|error| format!("execution of '{fn_name}' failed: {error}"))?;
    Ok((store.data_mut().tracer.flush_trace(), values))
}

/// Run the whole pipeline for one CLI invocation: write the folded stack to `--output` and print
/// the ranked summary to stdout.
///
/// Stage 1 is now a real run of the contract named by `--fn`, so the tree it aggregates is the
/// boundaries that run crossed. The costs in it are still all zero — see [`run_target`]'s note on
/// what the engine hook reports — which is why the folded output names `wasm[0]` and nothing else,
/// and why the summary today usually says that nothing was costed rather than lying with an
/// empty table.
fn profile(cli: &Cli) -> Result<(), String> {
    // 1. Read the contract and run the target export under the tracer.
    let wasm_bytes = load_wasm_file(&cli.wasm.to_string_lossy())
        .map_err(|error| format!("failed to read {}: {error}", cli.wasm.display()))?;
    let (events, _values) = run_target(&wasm_bytes, &cli.fn_name, initialize_tracer(cli))?;

    // 2. Load DWARF source map
    let mapper = load_source_mapper(&wasm_bytes);

    // 3. Aggregate events into call tree
    let mut aggregator = initialize_aggregator();
    let call_tree = aggregator.aggregate(events, &mapper);

    // 4. Format and output
    let output = OutputFormatter::to_collapsed_stack(&call_tree, &cli.metric);
    std::fs::write(&cli.output, output).map_err(|error| {
        format!(
            "failed to write folded stack to {}: {error}",
            cli.output.display()
        )
    })?;

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
        let (events, values) = run_target(FIXTURE, "caller_of_heavy", tracer()).unwrap();
        assert!(
            matches!(values.as_slice(), [Val::I64(value)] if *value == CALLER_OF_HEAVY),
            "the run must return the value the contract computes, got {values:?}"
        );
        let kinds: Vec<&EventType> = events.iter().map(|event| &event.event_type).collect();
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

    /// Parse argv the way `main` does, so a test fails when the flag itself stops working rather
    /// than when only the helper it calls changes.
    fn parse(args: &[&str]) -> Result<Cli, String> {
        let mut argv = vec![
            "soroban-cost-profiler",
            "--wasm",
            "contract.wasm",
            "--fn",
            "call",
        ];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map_err(|error| error.render().to_string())
    }

    /// #182's "done": `--sample-rate 0` returns a descriptive error. Zero is worth the named
    /// assertion because it is not merely a useless value — it makes the tracer's
    /// `current_step_cost >= sample_rate` test true every instruction, so an unvalidated zero
    /// silently disables sampling and grows the trace toward the 100M ceiling.
    #[test]
    fn a_zero_sample_rate_is_rejected_and_says_why() {
        let error = parse(&["--sample-rate", "0"]).unwrap_err();
        assert!(
            error.contains("--sample-rate")
                && error.contains("'0'")
                && error.contains("greater than 0"),
            "the message must name the flag, the offending value, and the rule: {error}"
        );
    }

    /// A typo and an illegal number are different mistakes, so they must not share a message — a
    /// user who typed `--sample-rate 1000ms` needs "that is not a number", not "must be > 0".
    #[test]
    fn a_non_numeric_sample_rate_is_rejected_as_unparseable() {
        let error = parse(&["--sample-rate", "1000ms"]).unwrap_err();
        assert!(
            error.contains("1000ms") && error.contains("not a valid number"),
            "{error}"
        );
        assert!(
            !error.contains("greater than 0"),
            "an unparseable value must not be reported as a range violation: {error}"
        );
    }

    /// The parser rejects, so it must also accept: a valid rate reaches the field unchanged and an
    /// absent flag still defaults to 1000.
    #[test]
    fn a_positive_sample_rate_reaches_the_tracer() {
        assert_eq!(parse(&["--sample-rate", "42"]).unwrap().sample_rate, 42);
        assert_eq!(parse(&[]).unwrap().sample_rate, 1000);
    }

    /// A negative rate is not a rate. Clap never hands `parse_positive_u32` the token, so the
    /// rejection has to come from clap's own argument matching — asserted as an error rather than a
    /// message, since which of clap's texts applies is its business, not ours.
    #[test]
    fn a_negative_sample_rate_is_rejected_too() {
        assert!(parse(&["--sample-rate", "-1"]).is_err());
    }
}
