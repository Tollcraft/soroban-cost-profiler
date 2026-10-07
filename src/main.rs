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
use soroban_cost_profiler::source_map::{SourceMapError, SourceMapper};
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, invoke_function, load_wasm_file,
    parse_module, setup_engine, setup_mock_env,
};
use std::io::IsTerminal;
use std::path::PathBuf;
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

/// Print a degraded-profile warning where the user will actually read it (#186).
///
/// stderr, for two reasons that both bite here: stdout carries the ranked summary that callers pipe
/// into other tools, and nothing in this crate installs a `tracing` subscriber — so the
/// `tracing::warn!` this stage used to write its warning with produced no output at all, which is
/// how a profiler running on a stripped binary could claim to have warned the user about it.
fn warn_user(message: &str) {
    eprintln!("warning: {message}");
}

/// What to tell the user about a binary this stage could not name at all (#186).
///
/// The error already carries the diagnosis — [`SourceMapError::MissingDebugInfo`]'s `Display` names
/// the build flag and lists the sections that *were* present — so this only adds the consequence
/// the user is about to see in the file, because "no `.debug_info` section" does not obviously mean
/// "every frame in your flamegraph is an address".
fn unmapped_warning(error: &SourceMapError) -> String {
    format!(
        "{error} Every frame will therefore be named by address, `wasm[pc]`, and not by source."
    )
}

/// The warnings a loaded mapper's own state calls for, in the order the user should read them.
///
/// Computed separately from printing so a test can assert what a given binary earns. Two of the
/// three degradations are distinguishable only from outside — DWARF and the `name` section fail
/// independently and need different sentences — and the third is the mapper's own measurement,
/// already phrased, arriving through [`SourceMapper::warning`].
fn symbolization_warnings(mapper: &SourceMapper) -> Vec<String> {
    let mut warnings = Vec::new();
    if !mapper.has_debug_info() && mapper.names_functions() {
        warnings.push(String::from(
            "this artifact has a `name` section but no DWARF line tables, so frames will name \
             functions and never `file:line`. Build the copy you profile with a profiling profile \
             — `[profile.profiling]` with `inherits = \"release\"` and `debug = \
             \"line-tables-only\"` — and keep `debug` out of `[profile.release]`: that is the \
             profile whose output gets deployed, and mainnet bills for the extra bytes.",
        ));
    }
    warnings.extend(mapper.warning().map(String::from));
    warnings
}

/// Stage 2: build the source mapper for the target WASM binary, and say out loud what it cannot name.
///
/// Loading is fallible, and the failure is not fatal: a binary without symbols still profiles, it
/// just names frames by address. What #186 adds is that the *degradation* has to be reported, not
/// merely survived — a flamegraph of unnamed frames is otherwise indistinguishable from a profiler
/// that is not working, and the three ways a binary is unnamed are three different fixes.
fn load_source_mapper(wasm_bytes: &[u8]) -> SourceMapper {
    let mapper = match SourceMapper::new(wasm_bytes) {
        Ok(mapper) => mapper,
        Err(error) => {
            warn_user(&unmapped_warning(&error));
            return SourceMapper::unmapped();
        }
    };
    for warning in symbolization_warnings(&mapper) {
        warn_user(&warning);
    }
    mapper
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

/// How a failure reaches the user: the message on stderr and the process exit code (#183).
///
/// The two kinds are not stylistic. A script that wraps the profiler can retry or report a broken
/// *tool* differently from a bad *command line*, and once both exit 1 it cannot tell them apart at
/// all — so the distinction the issue asks for lives in the type, and every error site has to
/// choose a side rather than default to one.
///
/// `1` for anything the user handed us, `2` for anything we could not do about it. clap's own
/// failure code is also `2`, which would make a typo'd flag look like a crash, so `main` overrides
/// it — see [`clap_exit_code`].
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// The invocation could not be honoured as given: an unreadable or unparsable contract, an
    /// export that does not exist, a module this tool cannot link, a contract that trapped, an
    /// output path that cannot exist.
    Input(String),
    /// The input was accepted and the run began, but the profiler could not finish its own work —
    /// the engine refused to configure, or the artifact could not be written for a reason that has
    /// nothing to do with the command line.
    Internal(String),
}

impl Failure {
    /// The process exit code for this kind: 1 for input, 2 for internal.
    fn code(&self) -> i32 {
        match self {
            Self::Input(_) => 1,
            Self::Internal(_) => 2,
        }
    }

    /// The message to print, without the `error: ` prefix `main` adds.
    fn message(&self) -> &str {
        match self {
            Self::Input(message) | Self::Internal(message) => message,
        }
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
) -> Result<TargetRun, Failure> {
    let engine = setup_engine();
    let module = parse_module(&engine, wasm_bytes)
        .map_err(|error| Failure::Input(format!("failed to parse WASM module: {error}")))?;
    if !matches!(module.get_export(fn_name), Some(ExternType::Func(_))) {
        return Err(Failure::Input(unknown_export(fn_name, &module)));
    }

    let state = ProfilerState {
        tracer,
        host: setup_mock_env(),
        last_fuel: 0,
    };
    let mut store = wasmi::Store::new(&engine, state);
    // The one failure here is "this engine was built without the config the run needs", which is
    // ours and not the user's, so it is the only `Internal` in this function.
    store
        .set_fuel(u64::MAX)
        .map_err(|error| Failure::Internal(format!("failed to enable fuel metering: {error}")))?;

    // A module that needs imports this tool does not link is bad *input*, not a broken profiler:
    // the message says what never ran, and a different contract would run fine.
    let instance = instantiate_module(&engine, &mut store, &module)
        .map_err(|error| Failure::Input(format!("failed to instantiate module: {error}")))?;

    // Sized and typed from the signature: a contract returning `u64` gets an `I64` slot, and a
    // void one runs on an empty buffer.
    let mut results: Vec<Val> = instance
        .get_func(&store, fn_name)
        .ok_or_else(|| Failure::Input(unknown_export(fn_name, &module)))?
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
fn profile(cli: &Cli) -> Result<(), Failure> {
    // 1. Read the contract and run the target export under the tracer.
    let wasm_bytes = load_wasm_file(&cli.wasm.to_string_lossy()).map_err(|error| {
        Failure::Input(format!("failed to read {}: {error}", cli.wasm.display()))
    })?;
    let run = run_target(&wasm_bytes, &cli.fn_name, initialize_tracer(cli))?;

    // 2. Load DWARF source map
    let mapper = load_source_mapper(&wasm_bytes);

    // 3. Aggregate events into call tree
    let mut aggregator = initialize_aggregator();
    let call_tree = aggregator.aggregate(run.events, &mapper);

    // 4. Format and output
    let output = OutputFormatter::to_collapsed_stack(&call_tree, &cli.metric);
    std::fs::write(&cli.output, output).map_err(|error| {
        let message = format!(
            "failed to write folded stack to {}: {error}",
            cli.output.display()
        );
        // A path whose parent does not exist is a command line we could never have honoured, so
        // it is input like any other. Every other write failure — permissions, a full disk, a
        // directory in place of a file — says more about the machine than about the invocation,
        // and guessing at those would make the code less trustworthy, not more.
        if error.kind() == std::io::ErrorKind::NotFound {
            Failure::Input(message)
        } else {
            Failure::Internal(message)
        }
    })?;

    if let Some(trap) = run.trapped {
        return Err(Failure::Input(format!(
            "'{fn}' trapped: {trap}. The partial trace up to the trap is in {path}, and its costs \
             are incomplete because the call never returned.",
            fn = cli.fn_name,
            path = cli.output.display()
        )));
    }

    let ranked = OutputFormatter::top_functions(&call_tree, &cli.metric, TOP_FUNCTIONS);
    println!(
        "{}",
        OutputFormatter::to_top_summary(&ranked, &cli.metric, std::io::stdout().is_terminal())
    );
    Ok(())
}

/// The exit code for a clap error (#183).
///
/// clap answers `--help` and `--version` by returning an error that prints to stdout — those are
/// successes that stop early, and they exit 0. Everything else it rejects is a command line the
/// user has to fix, so it exits 1. clap's built-in code for that is 2, and this crate reserves 2
/// for its own failures, so the CLI decides the code instead of inheriting the library default.
fn clap_exit_code(error: &clap::Error) -> i32 {
    use clap::error::ErrorKind;
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
        _ => 1,
    }
}

/// Run the pipeline, reporting any failure on stderr and exiting with the code its kind maps to.
///
/// Every message goes through `eprintln!` rather than `tracing` because nothing in this crate
/// installs a subscriber: a `tracing::error!` on a fatal path writes nowhere, which is how a CLI
/// that had never run its WASM still managed to print a plausible-looking empty profile.
///
/// Two exit codes, one message shape. A bad invocation — unreadable contract, unknown export, a
/// contract that trapped — is `1`; a profiler that could not finish its own work is `2`. `--help`
/// and `--version` print and exit `0`.
fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // `print()` routes help and `--version` to stdout and refusals to stderr, so the
            // message keeps clap's own formatting and only the code is ours.
            let _ = error.print();
            std::process::exit(clap_exit_code(&error));
        }
    };
    if let Err(failure) = profile(&cli) {
        eprintln!("error: {}", failure.message());
        std::process::exit(failure.code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_cost_profiler::models::EventType;

    const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");

    /// The same three contract functions built with `debug = false`: a `name` section, no line
    /// tables. #186's middle case, and the one that has to be told apart from a fully stripped
    /// binary because the user's fix differs.
    const NO_DEBUG_PROBE: &[u8] =
        include_bytes!("../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");

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
        let error = input_failure(run_target(FIXTURE, "compute_heavy", tracer()).unwrap_err());
        assert!(
            error.contains("caller_of_heavy") && error.contains("memory_heavy_loop"),
            "{error}"
        );
    }

    /// The harness used to answer an absent `--fn` by running a `"test"` export no fixture has,
    /// which failed silently. Not knowing which export to profile is not a runnable default.
    #[test]
    fn an_empty_fn_name_is_an_error_rather_than_a_guess() {
        let error = input_failure(run_target(FIXTURE, "", tracer()).unwrap_err());
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
        let error = input_failure(profile(&cli).unwrap_err());
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

    /// Assert that a failure is the kind the user caused — #183's exit 1 — and hand back the
    /// message so the test can pin the text as well as the code.
    ///
    /// The code is asserted rather than the variant, because the code is what a wrapping script
    /// actually sees; which enum arm produced it is this file's business.
    fn input_failure(failure: Failure) -> String {
        assert_eq!(
            failure.code(),
            1,
            "an input failure must exit 1, got {}: {}",
            failure.code(),
            failure.message()
        );
        failure.message().to_string()
    }

    /// The `clap::Error` a command line produces, so `--help`, `--version` and the refusals can all
    /// be run through [`clap_exit_code`] without spawning the binary (#184 owns that).
    fn clap_error(args: &[&str]) -> clap::Error {
        let mut argv = vec![
            "soroban-cost-profiler",
            "--wasm",
            "contract.wasm",
            "--fn",
            "call",
        ];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
            .expect_err("every command line passed here is a refusal or a print")
    }

    /// #183's "done" for the two codes a script has to tell apart.
    ///
    /// `--help` and `--version` are clap *errors* that print to stdout, and a tool that exits
    /// non-zero after doing exactly what was asked is broken for any caller that captured its
    /// output. Everything clap refuses is a command line to fix, so it is 1 like the rest of the
    /// input failures — clap's own default there is 2, which this crate reserves for itself.
    #[test]
    fn help_and_version_are_successes_and_refusals_are_input_errors() {
        for args in [&["--help"][..], &["--version"]] {
            assert_eq!(clap_exit_code(&clap_error(args)), 0, "{args:?}");
        }
        for args in [
            &["--sample-rate", "0"][..],
            &["--metric", "gas"],
            &["--wot"],
        ] {
            assert_eq!(clap_exit_code(&clap_error(args)), 1, "{args:?}");
        }
        // A missing required flag is the same kind of mistake as an invalid one.
        let missing = Cli::try_parse_from(["soroban-cost-profiler"])
            .expect_err("--wasm is required, so this must be refused");
        assert_eq!(clap_exit_code(&missing), 1);
    }

    /// The other side of the write path, and the reason the code is not simply hardcoded to 1: a
    /// path with no parent directory is a command line that could never be honoured, while a path
    /// that exists but cannot be written says something about the machine.
    #[test]
    fn an_output_path_that_cannot_exist_is_an_input_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let error = input_failure(
            profile(&cli(
                temp_dir.path().join("no-such-dir/profile.folded"),
                fixture(),
                "caller_of_heavy",
            ))
            .unwrap_err(),
        );
        assert!(error.contains("no-such-dir"), "{error}");
    }

    /// `--output` pointing at a directory is not a path the user can fix by re-typing the same
    /// thing, and it is not a rejected invocation either — the run happened and only the artifact
    /// failed. That is #183's `2`, and this is the reachable case for it.
    #[test]
    fn an_unwritable_output_path_is_an_internal_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let failure = profile(&cli(
            temp_dir.path().to_path_buf(),
            fixture(),
            "caller_of_heavy",
        ))
        .unwrap_err();
        assert_eq!(
            failure.code(),
            2,
            "a write the machine refused must exit 2: {}",
            failure.message()
        );
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

        let error =
            input_failure(profile(&cli(output_path.clone(), temp_wasm, "boom")).unwrap_err());
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

        let error =
            input_failure(profile(&cli(output_path.clone(), temp_wasm, "boom")).unwrap_err());
        assert!(
            error.contains("instantiate"),
            "the failure must say the module never ran, not just that something went wrong: {error}"
        );
        assert!(
            !output_path.exists(),
            "a run that never started must not leave a profile behind"
        );
    }

    /// Load a mapper the tests expect to succeed. `unwrap`/`expect` would need `SourceMapper: Debug`,
    /// and the type holds a `gimli` parse context that has no useful debug format.
    fn loaded_mapper(bytes: &[u8]) -> SourceMapper {
        SourceMapper::new(bytes)
            .unwrap_or_else(|error| panic!("expected this binary to load: {error}"))
    }

    /// #186's "done": a stripped binary triggers a profile warning. The binary this crate can
    /// neither name nor line is the one case where surviving the error silently was indistinguishable
    /// from working, so the message has to name both the consequence and the fix.
    #[test]
    fn a_binary_with_no_symbols_at_all_warns_that_frames_will_be_addresses() {
        let error = SourceMapper::new(BOOM)
            .err()
            .expect("a hand-assembled module carries neither DWARF nor a `name` section");
        let warning = unmapped_warning(&error);
        assert!(
            warning.contains("wasm[pc]") && warning.contains("line-tables-only"),
            "the warning must say what the user will see and how to stop it: {warning}"
        );
    }

    /// The middle case #186 exists for: #157's fallback loads happily and resolves *something*, so
    /// no error fires and the old code said nothing at all — while the flamegraph the user gets
    /// names functions and never a line. A run that works less than the user asked for is not a run
    /// with nothing to report.
    #[test]
    fn a_name_section_only_binary_warns_about_missing_lines() {
        let mapper = loaded_mapper(NO_DEBUG_PROBE);
        assert!(mapper.names_functions() && !mapper.has_debug_info());
        let warnings = symbolization_warnings(&mapper);
        assert_eq!(
            warnings.len(),
            1,
            "one degradation, one warning: {warnings:?}"
        );
        assert!(
            warnings[0].contains("file:line"),
            "the warning must name what is missing: {}",
            warnings[0]
        );
    }

    /// The other direction: a properly built artifact earns no warning, which is what makes the
    /// other two tests mean anything. This also pins that #162's degenerate-mapping ratio does not
    /// fire on the committed fixture, so the warning this PR adds cannot become background noise.
    #[test]
    fn a_fully_symbolized_binary_earns_no_warning() {
        let mapper = loaded_mapper(FIXTURE);
        assert!(
            mapper.has_debug_info(),
            "the fixture is the symbolized case"
        );
        assert!(
            symbolization_warnings(&mapper).is_empty(),
            "a properly built artifact must stay quiet"
        );
    }

    /// The fallback mapper is what a run continues with *after* the error above was printed, so
    /// warnings computed from its state must not repeat that message. Two sentences saying one
    /// thing is how users start ignoring all of them.
    #[test]
    fn the_unnamed_fallback_adds_no_second_warning() {
        assert!(
            symbolization_warnings(&SourceMapper::unmapped()).is_empty(),
            "the already-stripped case is reported by `unmapped_warning`, not twice"
        );
    }

    /// Neither message may tell the user to put debug info in the profile whose output gets
    /// deployed. `SourceMapError`'s own text used to read `[profile.release] debug =
    /// "line-tables-only"`, which the README marks as a deployment-cost hazard, and a warning is
    /// instructions — so this is asserted, not stylistic.
    #[test]
    fn no_warning_asks_for_debug_info_in_the_deployed_profile() {
        let stripped = unmapped_warning(
            &SourceMapper::new(BOOM)
                .err()
                .expect("a hand-assembled module carries neither DWARF nor a `name` section"),
        );
        let name_only = symbolization_warnings(&loaded_mapper(NO_DEBUG_PROBE)).join(" ");
        for message in [&stripped, &name_only] {
            assert!(
                !message.contains("[profile.release] debug"),
                "the advice must name a profiling profile, never the release one: {message}"
            );
        }
    }
}
