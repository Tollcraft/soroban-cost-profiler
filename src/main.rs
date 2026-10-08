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
//! 4. **Format** — serialize the result in the shape `--format` asked for (#215): `.folded`
//!    collapsed stacks for external viewers such as speedscope, a JSON call tree for a program that
//!    walks it, or the raw event stream straight out of stage 1.
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
use clap::{Parser, Subcommand};
use soroban_cost_profiler::aggregator::ProfileAggregator;
use soroban_cost_profiler::formatter::OutputFormatter;
use soroban_cost_profiler::models::{Format, Metric, TraceEvent};
use soroban_cost_profiler::source_map::{SourceMapError, SourceMapper};
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, invoke_function, load_wasm_file,
    parse_module, setup_engine, setup_mock_env,
};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tracing::level_filters::LevelFilter;
use wasmi::{ExternType, Val, ValType};

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

/// `value_parser` for `--instruction-limit`: a positive bound, in the tracer's own `u64`.
///
/// Zero is refused for the opposite reason to `--sample-rate 0`. There, zero silently turns a
/// throttle off and lets the buffer grow; here, `record_step` increments its count *before*
/// comparing, so a ceiling of 0 fails the first boundary and the run stops having profiled nothing
/// — an invocation that looks like a contract that traps immediately. The field is `u64` because
/// `ExecutionTracer::with_instruction_ceiling` takes `u64`, and a raised limit is the point of the
/// flag: the PRD's 100M is a default, not a maximum.
fn parse_positive_u64(s: &str) -> Result<u64, String> {
    let val: u64 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid number"))?;
    if val == 0 {
        Err(String::from("must be greater than 0"))
    } else {
        Ok(val)
    }
}

/// Two shapes: the flat flags profile a contract, and `compare` reads two profiles already on disk.
/// `subcommand_negates_reqs` is what lets the second shape work without `--wasm` — a mode that
/// diffs two `.folded` files cannot sensibly demand a contract to execute — while `--wasm` stays
/// clap-required for the first, so a profiling run that forgot it is still refused with clap's own
/// message rather than a message this file invented.
///
/// `about` is not spelled out here: `#[command(about)]` takes it from `[package.description]` in
/// `Cargo.toml` (#185), which is the reason this doc comment no longer doubles as the help header.
/// `long_about` and `after_help` are literals and not constants because clap's derive reads these
/// attributes as literals; they are the user-facing half of #176, and the tests in this file render
/// the help rather than string-matching the source, so moving the text has to be a content change.
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about,
    long_about = "soroban-cost-profiler traces one exported function of a compiled Soroban contract and says where its cost went.\n\nTwo modes:\n  profile   --wasm <contract.wasm> --fn <export> runs that export under the instrumented engine and writes its profile to --output, in whatever shape --format picks: collapsed stacks (the default), a JSON call tree, or the raw event stream. The name defaults to the format — profile.folded, profile.json, profile.raw — and `-` sends the artifact to stdout instead of a file. `--args 1000,7` passes values to an export that takes parameters. Frames are named from the binary's own DWARF line tables when it has them; a binary built without debug info still profiles, and the run then says so on stderr instead of pretending its `wasm[pc]` frames are source lines.\n  compare   compare <base.folded> <new.folded> reads two profiles already on disk and prints the functions whose cost moved, biggest move first. It runs no contract, so it needs no --wasm.\n\nThe .folded file is the artifact. Open it in speedscope.app, or hand it to flamegraph.pl for a picture; this tool writes text and no SVG. `--format json` is the same tree for a program that walks it, and `--format raw` is the trace before any of it was named or folded. The terminal summary is a glance at the same run, not a second source of truth.\n\nExit codes:\n  0  the run was honoured as asked; a compare that reports a regression still exits 0, because bad news is still an answer\n  1  the invocation could not be honoured as asked: a contract that cannot be read, parsed or linked, an export the module does not have, a contract that trapped, a .folded file that is missing or malformed, or a refused flag\n  2  the input was accepted and the profiler could not finish its own work: a write the machine refused for a reason other than the path, or an engine that would not configure",
    after_help = "Examples:\n  # profile the `call` export\n  soroban-cost-profiler --wasm target/wasm32-unknown-unknown/release/contract.wasm --fn call\n\n  # the same run in memory units, into a named file\n  soroban-cost-profiler --wasm contract.wasm --fn call --metric memory --output memory.folded\n\n  # the call tree as structured data, straight into jq\n  soroban-cost-profiler --wasm contract.wasm --fn call --format json --output -\n\n  # what the engine actually reported: one line per recorded event\n  soroban-cost-profiler --wasm contract.wasm --fn call --format raw --sample-rate 1\n\n  # an export that takes arguments\n  soroban-cost-profiler --wasm contract.wasm --fn transfer --args 1000,7\n\n  # a denser trace: one event every 100 rather than every 1000\n  soroban-cost-profiler --wasm contract.wasm --fn call --sample-rate 100\n\n  # a heavier contract than the default bound allows\n  soroban-cost-profiler --wasm contract.wasm --fn call --instruction-limit 200000000\n\n  # did the change help?\n  soroban-cost-profiler compare before.folded after.folded",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    /// Path to the compiled WASM contract (required, unless `compare` is used)
    //
    // `required = true` is spelled out rather than left to the derive: an `Option` field would
    // otherwise default to *not* required, and then a profiling run that forgot `--wasm` would
    // parse and fail somewhere deep in the pipeline instead of at the flag. The rationale is a
    // comment and not part of the doc comment because clap puts doc comments in `--help`, where
    // "why this is an Option" is not the answer to "what does this flag want".
    #[arg(short, long, required = true)]
    pub wasm: Option<PathBuf>,

    /// Where to write the artifact: `-` means stdout
    ///
    /// This is the artifact the run leaves behind: for `--format folded`, speedscope.app opens it
    /// directly and `flamegraph.pl` turns it into a picture, and `compare` reads files of that same
    /// shape. Omit the flag and the name follows the format — `profile.folded`, `profile.json`,
    /// `profile.raw` — because a JSON tree sitting in a file called `.folded` is a trap for the next
    /// command, which will hand it to `compare` or to a viewer expecting collapsed stacks.
    ///
    /// `-` is a convention and not a file: the artifact goes to stdout on its own, which is what
    /// makes `--format json --output - | jq` work, and the terminal summary then stays silent
    /// because two documents in one stream parse as neither.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Exported function to invoke, e.g. `--fn call`
    ///
    /// Profiling refuses to start without one, and a name the module does not export is an error
    /// that lists the exports it does have.
    ///
    /// The default is hidden because `[default: ]` reads like an accepted empty name, and it is
    /// not: an empty `--fn` is refused at the run, not by clap.
    #[arg(long = "fn", default_value = "", hide_default_value = true)]
    pub fn_name: String,

    /// Arguments for the export, comma-separated: `--args 1000,7`
    ///
    /// A contract's wasm export takes each parameter as an `i64` — the SDK's `Val` is a 64-bit word
    /// at that boundary — so an export declared `(param i64) (param i64)` wants two numbers here, in
    /// the order its signature lists them. The count and the types are checked against that
    /// signature before the call, so a mismatch names the export's real parameters instead of
    /// arriving as `wasmi`'s `encountered an incorrect number of parameters` trap, which cannot say
    /// what it wanted.
    ///
    /// Only integers are accepted, and they are handed over as raw `i64` words. That is enough for an
    /// `extern "C"` export taking `u64`/`i64`, and it is nearly enough for an SDK entry point: the real
    /// Soroban host functions are linked (`src/host.rs`), so a word that already carries a `Val` tag is
    /// decoded into a live object handle. What this flag does not do is tag for you — an SDK `u32`
    /// parameter wants `value << 32 | 4` typed out, and the plain number runs until the guest reads a bad
    /// tag and traps. And an argument the contract expects to *find* in the ledger, rather than receive as
    /// a word, needs the chain state issue 212's `--state` flag would supply, not this one.
    #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
    pub args: Vec<i64>,

    /// Record one trace event every N instructions (must be greater than 0)
    #[arg(long, default_value_t = 1000, value_parser = parse_positive_u32)]
    pub sample_rate: u32,

    /// Stop the run after N traced steps (must be greater than 0)
    ///
    /// The OOM guard on the trace buffer, made configurable: the run stops past this many steps,
    /// the trace recorded up to that point is still written to `--output`, and the exit code is 1
    /// with `Instruction ceiling exceeded` named as the cause. The default is the bound the
    /// profiler has always used, so omitting the flag changes nothing.
    ///
    /// A step is a boundary the engine reports, not a wasm instruction — `wasmi` 2.0 has no
    /// instruction hook, so one step stands for everything run since the boundary before it.
    /// Raising the limit therefore lets a heavy contract finish; lowering it is a way to stop a
    /// runaway run early, but it counts boundaries, so a loop that never calls anything will not
    /// reach it.
    #[arg(long, default_value_t = 100_000_000, value_parser = parse_positive_u64)]
    pub instruction_limit: u64,

    /// Cost metric the counts are written in
    ///
    /// A `.folded` file records no metric of its own, so two files handed to `compare` must come
    /// from runs that agreed on this flag already. `--format json` carries the metric inside the
    /// document, which is the one thing the folded format cannot do; `--format raw` ignores this
    /// flag because a trace event holds its cpu and memory deltas unselected.
    #[arg(long, value_enum, default_value_t = Metric::Cpu)]
    pub metric: Metric,

    /// How the artifact is serialized: collapsed stacks, a JSON tree, or the raw event stream
    ///
    /// `folded` is what every other document here describes and what speedscope.app and
    /// `flamegraph.pl` read. `json` is the same call tree as structured data, with the metric and
    /// all three cost columns on every frame. `raw` is the trace before stages 2 and 3 — one line
    /// per recorded event, no names and no tree — which is the format to reach for when a profile
    /// looks wrong, and the reason it runs no symbolization at all.
    #[arg(long, value_enum, default_value_t = Format::Folded)]
    pub format: Format,

    /// Print the profiler's internal progress on stderr: `-v` stages, `-vv` every call boundary,
    /// `-vvv` every costed step
    //
    // A count, so `-vv` and `-vvv` are one flag rather than three. `conflicts_with` is for the
    // ambiguity, not the arithmetic: `-v --quiet` asks for more and less output at once, and a tool
    // that picked a winner silently would be guessing at a command line it cannot honour.
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Write the artifact and print nothing on stdout
    ///
    /// What is kept, deliberately: the artifact, every `warning:` line about a degraded run, and
    /// every fatal `error:` — quiet means "do not narrate", not "do not report". `compare`'s table
    /// is that mode's whole answer rather than an echo of a file, so it still prints.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Mode to run instead of profiling: see [`Command`]
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The modes that do not execute a contract (#190).
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
    /// Print how each function's cost changed between two `.folded` profiles.
    ///
    /// `baseline` is the "before" and `current` the "after", and the report ranks the functions
    /// whose cost moved, biggest change first, so the headline number a reviewer wants — did this
    /// change make the contract cheaper — is the last line of the table.
    ///
    /// The counts are read exactly as each file wrote them, so the two must come from runs with
    /// the same `--metric`; the files do not record which metric they hold, and this mode cannot
    /// check that. `--sample-rate`, `--metric` and the profiling flags belong to a run and are not
    /// consulted here, which is why `--wasm` next to `compare` is refused rather than ignored.
    Compare {
        /// The `.folded` file to compare against
        baseline: PathBuf,

        /// The `.folded` file to compare to it
        current: PathBuf,
    },
}

/// How many functions the terminal summary ranks (#181's "top 5").
///
/// A constant and not a flag: the summary is a glance at the run, the `.folded` file is the
/// artifact, and a reader who wants the whole ranking has the file.
const TOP_FUNCTIONS: usize = 5;

/// Stage 1: build a tracer carrying both CLI bounds — the sampling rate and the instruction ceiling
/// that `--instruction-limit` exists to set.
fn initialize_tracer(cli: &Cli) -> ExecutionTracer {
    ExecutionTracer::new()
        .with_sample_rate(cli.sample_rate as u64)
        .with_instruction_ceiling(cli.instruction_limit)
}

/// Print a degraded-profile warning where the user will actually read it (#186).
///
/// stderr, for two reasons that both bite here: stdout carries the ranked summary that callers pipe
/// into other tools, and the `tracing::warn!` this stage used to write its warning with produced no
/// output at all — the crate installed no subscriber until #214, which is how a profiler running on
/// a stripped binary could claim to have warned the user about it. `warn_user` is also what keeps
/// the sentence independent of `-v`: a user-facing warning must not depend on a verbosity flag.
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

/// Where a run's artifact goes: a file, or stdout when `--output -` asked for it (#215).
///
/// A type rather than a `PathBuf` that might contain `"-"`, because that string would have to be
/// re-checked at three places — the write itself, the write failure's message, and the trap message
/// that tells the user where the partial trace went — and a missed check silently writes a file named
/// `-` into the user's directory while the pipeline behind the pipe waits for a stdout it never gets.
#[derive(Debug, PartialEq, Eq)]
enum Destination {
    Stdout,
    File(PathBuf),
}

impl Destination {
    /// How to call this destination in a message the user reads.
    fn describe(&self) -> String {
        match self {
            Destination::Stdout => String::from("stdout"),
            Destination::File(path) => path.display().to_string(),
        }
    }

    /// "in `halted.folded`" / "on stdout", for the sentence that reports a trapped run.
    ///
    /// The preposition is part of the wording because the file form is quoted verbatim in
    /// `docs/troubleshooting.md` and in the `--instruction-limit` tests; a message that read "is in
    /// stdout" would be the one place this type leaked its own spelling into prose.
    fn located(&self) -> String {
        match self {
            Destination::Stdout => String::from("on stdout"),
            Destination::File(path) => format!("in {}", path.display()),
        }
    }

    /// Write the artifact, turning a refusal into the kind of failure it is (#183's split).
    fn write(&self, artifact: &str, label: &str) -> Result<(), Failure> {
        let result = match self {
            Destination::Stdout => {
                use std::io::Write;
                let handle = std::io::stdout();
                let mut stdout = handle.lock();
                stdout
                    .write_all(artifact.as_bytes())
                    .and_then(|()| stdout.flush())
            }
            Destination::File(path) => std::fs::write(path, artifact),
        };
        result.map_err(|error| {
            let message = format!("failed to write {label} to {}: {error}", self.describe());
            // A path whose parent does not exist is a command line we could never have honoured, so
            // it is input like any other. Every other write failure — permissions, a full disk, a
            // directory in place of a file — says more about the machine than about the invocation,
            // and guessing at those would make the code less trustworthy, not more.
            if error.kind() == std::io::ErrorKind::NotFound {
                Failure::Input(message)
            } else {
                Failure::Internal(message)
            }
        })
    }
}

/// The file a run writes when `--output` is not given, named after the format that fills it.
///
/// One name per format because the formats are not interchangeable downstream: `compare` reads
/// collapsed stacks and would report a JSON document as malformed on line 1, and speedscope reads
/// collapsed stacks too. `profile.folded` is the name every document in this repository already
/// quotes, so the default only changes for a reader who asked for a different format.
fn default_artifact_name(format: Format) -> &'static str {
    match format {
        Format::Folded => "profile.folded",
        Format::Json => "profile.json",
        Format::Raw => "profile.raw",
    }
}

/// Resolve `--output` for this run: a path, `-` as stdout, or the name that matches `--format`.
fn artifact_destination(cli: &Cli) -> Destination {
    match &cli.output {
        Some(path) if path == Path::new("-") => Destination::Stdout,
        Some(path) => Destination::File(path.clone()),
        None => Destination::File(PathBuf::from(default_artifact_name(cli.format))),
    }
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

/// What a wasm parameter type is called in a message a user reads.
///
/// `wasmi` gives `ValType` no `Display`, and its own error text for a mismatch is a debug format
/// (`I64`), which is not how anyone writes a contract signature.
fn val_type_name(ty: &ValType) -> &'static str {
    match ty {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        ValType::V128 => "v128",
        ValType::FuncRef => "funcref",
        ValType::ExternRef => "externref",
    }
}

/// `(i64, i64)` for the export's parameter list, `no arguments` for an export that takes none.
fn describe_parameters(params: &[ValType]) -> String {
    if params.is_empty() {
        return String::from("no arguments");
    }
    let types: Vec<&str> = params.iter().map(val_type_name).collect();
    format!(
        "{} {} ({})",
        params.len(),
        if params.len() == 1 {
            "argument"
        } else {
            "arguments"
        },
        types.join(", ")
    )
}

/// Check `--args` against the export's own signature, before the call is made (#211).
///
/// Two failures, because they are two different mistakes. The count is the one a user makes by
/// hand — a forgotten value, or one too many — and the fix is in the message: what the export
/// wants, spelled from its type. The *type* is the one this tool cannot fix: `--args` speaks `i64`
/// only, so an export with an `i32` or `f64` parameter is refused rather than invoked with a
/// value whose width the engine will reject as a trap.
///
/// Checked here rather than left to `wasmi` because the engine's answer is
/// `encountered an incorrect number of parameters` — true, and useless, since it names neither
/// the signature nor what was passed — and because it arrives as a *trap*, which #173's path then
/// reports as a partial profile of a call that was never legal to make.
fn check_target_arguments(fn_name: &str, params: &[ValType], args: &[Val]) -> Result<(), Failure> {
    if params.len() != args.len() {
        let given = if args.is_empty() {
            String::from("--args gave no values")
        } else {
            format!(
                "--args gave {} {}",
                args.len(),
                if args.len() == 1 { "value" } else { "values" }
            )
        };
        return Err(Failure::Input(format!(
            "'{fn_name}' takes {}; {given}. `--args` is one value per parameter, in the order the \
             signature lists them.",
            describe_parameters(params)
        )));
    }
    if let Some((index, ty)) = params
        .iter()
        .enumerate()
        .find(|(_, ty)| !matches!(ty, ValType::I64))
    {
        return Err(Failure::Input(format!(
            "'{fn_name}' takes {}, and `--args` supplies i64 values only: argument {} is {}. \
             A parameter of another width cannot be named from the command line.",
            describe_parameters(params),
            index + 1,
            val_type_name(ty)
        )));
    }
    Ok(())
}

/// Stage 1, executed: instantiate `wasm_bytes`, invoke `fn_name` with `args`, and hand back the
/// trace it produced next to the function's own return values.
///
/// `args` is what `--args` became (#211), and an export that takes no parameters is called with an
/// empty slice — the same shape every run had before the flag existed. It is checked against the
/// export's signature here rather than at the flag, because only the module knows the signature.
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
    args: &[Val],
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
    let export = instance
        .get_func(&store, fn_name)
        .ok_or_else(|| Failure::Input(unknown_export(fn_name, &module)))?;
    let signature = export.ty(&store);
    check_target_arguments(fn_name, signature.params(), args)?;
    let mut results: Vec<Val> = signature
        .results()
        .iter()
        .map(|ty| Val::default_for_ty(*ty))
        .collect();

    // A trap leaves `results` untouched — the function never returned — so the run reports no
    // values and names the trap, while the events recorded up to the trap go to aggregation.
    let trapped = invoke_function(&mut store, &instance, fn_name, args, &mut results)
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

/// Run the whole pipeline for one CLI invocation: write the artifact to `--output` and print the
/// ranked summary to stdout.
///
/// Stage 1 is now a real run of the contract named by `--fn`, so the tree it aggregates is the
/// boundaries that run crossed. The costs in it are still all zero — see [`run_target`]'s note on
/// what the engine hook reports — which is why the folded output names `wasm[0]` and nothing else,
/// and why the summary today usually says that nothing was costed rather than lying with an
/// empty table.
///
/// `--format` chooses the serialization at stage 4, and `raw` chooses a shorter pipeline: it is the
/// stream straight out of stage 1, so stages 2 and 3 do not run for it at all. That is not an
/// optimization but a requirement — [`aggregate`](soroban_cost_profiler::aggregator::ProfileAggregator::aggregate)
/// consumes the event vector, so a raw run that aggregated first could only print the stream by
/// cloning it, which is the per-trace heap growth `AGENTS.md` rule 5 forbids.
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
    //
    // Unreachable from a real command line — `subcommand_negates_reqs` leaves `--wasm` required
    // whenever no subcommand was given — but `Cli` is public and `profile` is called directly by
    // the stage tests, so the type has to be answered rather than assumed.
    let wasm = cli.wasm.as_ref().ok_or_else(|| {
        Failure::Input(String::from(
            "--wasm is required: name the contract to profile, or use `compare <baseline> \
             <current>` to diff two profiles that already exist.",
        ))
    })?;
    let wasm_bytes = load_wasm_file(&wasm.to_string_lossy())
        .map_err(|error| Failure::Input(format!("failed to read {}: {error}", wasm.display())))?;
    let args: Vec<Val> = cli.args.iter().copied().map(Val::I64).collect();
    let run = run_target(&wasm_bytes, &cli.fn_name, initialize_tracer(cli), &args)?;
    let destination = artifact_destination(cli);

    // The artifact, and the one line the terminal adds about it. Two branches because `raw` has no
    // tree to rank: its summary counts what the engine handed over, which is the same news the folded
    // summary carries — did this run produce anything? — told in the units this format has. The word
    // is "events" and not "boundaries" because the two differ: `record_call`/`record_return` emit
    // unconditionally while steps are sampled, so `--sample-rate 1` on this repository's fixture
    // writes four events across two boundaries.
    let (artifact, summary) = if cli.format == Format::Raw {
        (
            OutputFormatter::to_raw_events(&run.events),
            format!(
                "{} trace events written to {}",
                run.events.len(),
                destination.describe()
            ),
        )
    } else {
        // 2. Load DWARF source map. 3. Aggregate events into call tree.
        let mapper = load_source_mapper(&wasm_bytes);
        let mut aggregator = initialize_aggregator();
        let call_tree = aggregator.aggregate(run.events, &mapper);

        // 4. Format and output.
        let artifact = if cli.format == Format::Json {
            OutputFormatter::to_json_tree(&call_tree, &cli.metric)
        } else {
            OutputFormatter::to_collapsed_stack(&call_tree, &cli.metric)
        };
        let ranked = OutputFormatter::top_functions(&call_tree, &cli.metric, TOP_FUNCTIONS);
        let summary =
            OutputFormatter::to_top_summary(&ranked, &cli.metric, std::io::stdout().is_terminal());
        (artifact, summary)
    };
    destination.write(&artifact, cli.format.artifact_label())?;

    if let Some(trap) = run.trapped {
        return Err(Failure::Input(format!(
            "'{fn}' trapped: {trap}. The partial trace up to the trap is {located}, and its costs \
             are incomplete because the call never returned.",
            fn = cli.fn_name,
            located = destination.located()
        )));
    }

    // `--quiet` means "the artifact is the answer" (#214). The summary is a glance at numbers the
    // file already carries, so a caller who asked for silence gets the file and an empty stdout;
    // the warnings and errors above this line are their own sites' output and stay, because quiet
    // is about narration and not about news.
    //
    // `--output -` is the other half of the same rule: stdout already holds the artifact, and a
    // summary printed after it is a second document in a stream that parses as neither. So the
    // stream gets the document and the terminal gets nothing — which is exactly why the flag is
    // worth having, and why the exit code is still the run's answer.
    if cli.quiet || destination == Destination::Stdout {
        return Ok(());
    }

    println!("{summary}");
    Ok(())
}

/// Read one `.folded` artifact for `compare`, naming the file it failed on.
///
/// A read failure is input, whatever the reason: the user pointed at something that is not a
/// profile they can compare, whether it does not exist, is a directory, or is not UTF-8 text.
fn read_folded(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path)
        .map_err(|error| Failure::Input(format!("failed to read {}: {error}", path.display())))
}

/// The `compare` mode (#190): diff two profiles and print the cost moves.
///
/// A subcommand rather than a `--compare <file>` flag on the profiling path because the question
/// "did my change help?" is answered from two files that already exist. As a flag it would have
/// made `--wasm` a required argument of a run that never happens, and the user would have had to
/// name a contract to avoid naming one.
///
/// Both files are read and parsed before anything is printed, so a malformed second file cannot
/// leave half a report on the terminal. The exit code is `0` whatever the numbers say: a found
/// regression is a correct answer, not a failed run, and a CI gate that had to ignore the code to
/// read the table would be a worse tool.
fn compare(baseline: &Path, current: &Path) -> Result<(), Failure> {
    let baseline = read_folded(baseline)?;
    let current = read_folded(current)?;
    let deltas = OutputFormatter::function_deltas(&baseline, &current).map_err(Failure::Input)?;
    println!(
        "{}",
        OutputFormatter::to_compare_report(&deltas, std::io::stdout().is_terminal())
    );
    Ok(())
}

/// Dispatch the invocation to the mode it named.
///
/// `--wasm` beside `compare` is refused instead of ignored. Either flag on its own says what to do;
/// both together say two things, and the only honest answers are "run the contract and ignore the
/// files" or "read the files and ignore the contract" — the tool should not pick one silently.
/// `--args` is the same shape (#211): values only mean something to a call, and `compare` makes none.
fn run(cli: &Cli) -> Result<(), Failure> {
    match &cli.command {
        Some(Command::Compare { baseline, current }) => {
            if cli.wasm.is_some() {
                return Err(Failure::Input(String::from(
                    "`compare` reads two .folded files and runs no contract, so `--wasm` cannot \
                     accompany it.",
                )));
            }
            if !cli.args.is_empty() {
                return Err(Failure::Input(String::from(
                    "`compare` reads two .folded files and runs no contract, so `--args` cannot \
                     accompany it.",
                )));
            }
            compare(baseline, current)
        }
        None => profile(cli),
    }
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

/// The `tracing` level the verbosity flags select (#214).
///
/// The default is `WARN`, and the honest description of that setting is *silent*: after #198 and
/// #186 every message a user is meant to read leaves through `warn_user` or `main`'s `eprintln!`,
/// and the crate has no `warn!` or `error!` record left. That is the point — a subscriber that
/// prints the profiler's module paths over the top of the sentences it already chose would be a
/// regression, which is why the three remaining fatal-path `error!` sites in `tracer.rs` are
/// `debug!` here rather than a second copy of the CLI's text.
///
/// The steps up are what makes the ten-odd internal records this crate has ever written reachable:
/// `INFO` names the stages a run passes through, `DEBUG` adds every call boundary the engine
/// reports, `TRACE` adds the costed step recorded at each one. `--quiet` is the single notch below
/// the default, and `main` gives it the conventional Unix second meaning — see [`Cli::quiet`].
fn log_level(quiet: bool, verbosity: u8) -> LevelFilter {
    if quiet {
        LevelFilter::ERROR
    } else {
        match verbosity {
            0 => LevelFilter::WARN,
            1 => LevelFilter::INFO,
            2 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        }
    }
}

/// Install the subscriber this crate's `tracing` calls have been writing into since they were
/// written (#214), before the first stage runs.
///
/// stderr, and that is the load-bearing part: stdout is a contract here. The `.folded` artifact is
/// a file, but the ranked summary is a stream callers pipe, and #184's tests and
/// `docs/troubleshooting.md` both rest on the two staying separate — a subscriber writing records
/// to stdout would break them silently, on the runs that use `-v` and nothing else.
///
/// No `env_filter`: the level comes from argv, so `RUST_LOG` cannot change what a CI run prints
/// behind a reviewer's back, and the `matchers`/`regex-automata` machinery it would pull in buys
/// nothing a `-v` count does not already cover.
fn init_logging(cli: &Cli) {
    tracing_subscriber::fmt()
        .with_max_level(log_level(cli.quiet, cli.verbose))
        .with_writer(std::io::stderr)
        .init();
}

/// Run the pipeline, reporting any failure on stderr and exiting with the code its kind maps to.
///
/// Every message goes through `eprintln!` rather than `tracing` because a `tracing::error!` needs a
/// subscriber to reach anyone, and for the whole of this crate's history until #214 there was none —
/// which is how a CLI that had never run its WASM still managed to print a plausible-looking empty
/// profile. `init_logging` now installs one, at a level chosen so the two channels stay disjoint:
/// records are what `-v` asks for, `error:` lines are what a failure always says.
///
/// Two exit codes, one message shape. A bad invocation — unreadable contract, unknown export, a
/// contract that trapped, an unreadable or malformed `.folded` file — is `1`; a profiler that could
/// not finish its own work is `2`. `--help` and `--version` print and exit `0`, and so does a
/// `compare` that found a regression: the report is the result, not a failure.
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
    // Installed after parsing and before anything logs: a record emitted while `Cli::try_parse`
    // was still running would go to whatever level the *previous* command line asked for, and a
    // record emitted before `init_logging` goes nowhere at all — which is the bug #214 is about.
    init_logging(&cli);
    if let Err(failure) = run(&cli) {
        eprintln!("error: {}", failure.message());
        std::process::exit(failure.code());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
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

    /// `(module (func (export "needs_arg") (param i64) (result i64) local.get 0 i64.const 2 i64.add))`.
    ///
    /// #211's subject at 46 bytes: an export whose signature has a parameter, hand-assembled for the
    /// same reason `BOOM` and `NEEDS_HOST` are (`source_map.rs` set the precedent) — the committed
    /// fixtures have no parameterized export, and a `wasm32` build is not available to the test job.
    /// The body returns `argument + 2`, so the return value says whether the value passed in actually
    /// arrived.
    const NEEDS_ARG: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x06\x01\x60\x01\x7e\x01\x7e\x03\x02\x01\x00\x07\x0d\x01\x09\x6e\x65\x65\x64\x73\x5f\x61\x72\x67\x00\x00\x0a\x09\x01\x07\x00\x20\x00\x42\x02\x7c\x0b";

    /// The same shape with `(param i32) (result i32)`, for the refusal that is about *width* rather
    /// than count: `--args` can name an `i64` and nothing else.
    const NEEDS_I32: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x06\x01\x60\x01\x7f\x01\x7f\x03\x02\x01\x00\x07\x0d\x01\x09\x6e\x65\x65\x64\x73\x5f\x69\x33\x32\x00\x00\x0a\x06\x01\x04\x00\x20\x00\x0b";

    fn cli(output: PathBuf, wasm: PathBuf, fn_name: &str) -> Cli {
        Cli {
            wasm: Some(wasm),
            output: Some(output),
            fn_name: fn_name.into(),
            args: Vec::new(),
            sample_rate: 1000,
            instruction_limit: 100_000_000,
            metric: Metric::Cpu,
            format: Format::Folded,
            verbose: 0,
            quiet: false,
            command: None,
        }
    }

    /// A `compare` invocation over two files that are already on disk.
    fn compare_cli(baseline: PathBuf, current: PathBuf) -> Cli {
        Cli {
            wasm: None,
            output: Some(PathBuf::from("unused.folded")),
            fn_name: String::new(),
            args: Vec::new(),
            sample_rate: 1000,
            instruction_limit: 100_000_000,
            metric: Metric::Cpu,
            format: Format::Folded,
            verbose: 0,
            quiet: false,
            command: Some(Command::Compare { baseline, current }),
        }
    }

    /// Write a tiny `.folded` artifact and hand back its path.
    fn folded_file(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
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
        let run = run_target(FIXTURE, "caller_of_heavy", tracer(), &[]).unwrap();
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

    /// #211's "done" line: an export that takes parameters runs, when the parameters are given.
    ///
    /// The assertion is on the **returned number**, not the exit code or the trace, because `40 + 2`
    /// can only come out of a body that read its argument. A test that stopped at "exit 0" would also
    /// pass if the value were dropped and the engine handed the function a zero.
    #[test]
    fn the_argument_reaches_the_contract_and_the_answer_comes_back() {
        let run = run_target(NEEDS_ARG, "needs_arg", tracer(), &[Val::I64(40)]).unwrap();
        assert!(run.trapped.is_none(), "{:?}", run.trapped);
        assert!(
            matches!(run.values.as_slice(), [Val::I64(value)] if *value == 42),
            "needs_arg(40) is 40 + 2, got {:?}",
            run.values
        );
    }

    /// The failure this issue was filed about, and the one the docs quote: no `--args` against a
    /// one-parameter export. It used to be `wasmi`'s trap, which arrived as a *profile* of a call that
    /// was never legal (`encountered an incorrect number of parameters`, exit 1, and a partial
    /// `.folded` beside it). Now the signature check refuses it before the call and names the
    /// parameter type the module actually declares.
    #[test]
    fn a_missing_argument_is_refused_with_the_exports_signature() {
        let error = input_failure(run_target(NEEDS_ARG, "needs_arg", tracer(), &[]).unwrap_err());
        assert!(
            error.contains("'needs_arg' takes 1 argument (i64)")
                && error.contains("--args gave no values"),
            "{error}"
        );
        assert!(
            !error.contains("incorrect number of parameters"),
            "the engine's trap text must not be what the user is left with: {error}"
        );
    }

    #[test]
    fn an_extra_argument_is_refused_the_same_way() {
        let args = [Val::I64(1), Val::I64(2)];
        let error = input_failure(run_target(NEEDS_ARG, "needs_arg", tracer(), &args).unwrap_err());
        assert!(
            error.contains("takes 1 argument (i64)") && error.contains("--args gave 2 values"),
            "{error}"
        );
    }

    /// The other direction: an export that takes nothing, given something. The plural agreement is
    /// asserted because this is prose a person reads to decide what to retype.
    #[test]
    fn an_export_with_no_parameters_says_so_rather_than_the_count_it_wanted() {
        let error = input_failure(
            run_target(FIXTURE, "caller_of_heavy", tracer(), &[Val::I64(1)]).unwrap_err(),
        );
        assert!(
            error.contains("takes no arguments") && error.contains("--args gave 1 value"),
            "{error}"
        );
    }

    /// `--args` speaks `i64` only, so a parameter of another width is a refusal rather than a trap.
    /// Naming the position and the type is the whole point: `(param i32)` is invisible in a binary the
    /// user cannot read, and `wasmi`'s own complaint is a type mismatch inside a trap.
    #[test]
    fn a_parameter_of_another_width_is_named_by_position_and_type() {
        let error = input_failure(
            run_target(NEEDS_I32, "needs_i32", tracer(), &[Val::I64(1)]).unwrap_err(),
        );
        assert!(
            error.contains("takes 1 argument (i32)")
                && error.contains("argument 1 is i32")
                && error.contains("i64 values only"),
            "{error}"
        );
    }

    /// The flag itself: `--args 1000,7` is a list, `-1` is a value rather than a flag, and the flag
    /// may be repeated. Through `parse`, so the test fails if the *command line* stops working rather
    /// than if only a helper changes.
    #[test]
    fn the_args_flag_is_a_comma_separated_list_of_64_bit_values() {
        assert_eq!(parse(&["--args", "1000,7"]).unwrap().args, vec![1000, 7]);
        assert_eq!(
            parse(&["--args", "-1,2"]).unwrap().args,
            vec![-1, 2],
            "a negative amount is a value, not a flag"
        );
        assert_eq!(
            parse(&["--args", "1", "--args", "2,3"]).unwrap().args,
            vec![1, 2, 3]
        );
        assert!(
            parse(&[]).unwrap().args.is_empty(),
            "omitting the flag changes nothing"
        );

        let text = clap_error(&["--args", "abc"]).render().to_string();
        assert!(
            text.contains("--args"),
            "the refusal names the flag: {text}"
        );
    }

    #[test]
    fn an_unknown_fn_names_the_functions_the_module_does_export() {
        let error = input_failure(run_target(FIXTURE, "compute_heavy", tracer(), &[]).unwrap_err());
        assert!(
            error.contains("caller_of_heavy") && error.contains("memory_heavy_loop"),
            "{error}"
        );
    }

    /// The harness used to answer an absent `--fn` by running a `"test"` export no fixture has,
    /// which failed silently. Not knowing which export to profile is not a runnable default.
    #[test]
    fn an_empty_fn_name_is_an_error_rather_than_a_guess() {
        let error = input_failure(run_target(FIXTURE, "", tracer(), &[]).unwrap_err());
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

    /// The help as a user sees it, rendered rather than read from the source.
    ///
    /// The attributes are compile-time data, so a test that string-matched this file would still
    /// pass if clap stopped rendering them. These call the same renderer `main`'s error path uses.
    fn long_help() -> String {
        Cli::command().render_long_help().to_string()
    }

    fn short_help() -> String {
        Cli::command().render_help().to_string()
    }

    /// #185: the header line and the author are package metadata, not strings living twice.
    ///
    /// Before this, `Cargo.toml` had no `description`, so `#[command(about)]` — the wiring the issue
    /// asks for — resolved to an empty string and the `Cli` doc comment was standing in for it by
    /// accident. The absence check is what keeps that arrangement from silently coming back.
    #[test]
    fn the_help_header_and_author_come_from_cargo_toml() {
        let help = short_help();
        assert!(
            help.contains(env!("CARGO_PKG_DESCRIPTION")),
            "the `-h` header must be `[package.description]`"
        );
        assert!(
            !help.contains("Soroban Cost Profiler"),
            "the old doc-comment header must not still be what renders"
        );
        assert!(
            !env!("CARGO_PKG_AUTHORS").is_empty(),
            "`#[command(author)]` would otherwise advertise an author field that says nothing"
        );
    }

    /// #185's "done" clause: `--version` works, and the number it prints is the crate's own.
    #[test]
    fn the_version_flag_reports_the_package_and_its_cargo_version() {
        let rendered = Cli::command().render_version().to_string();
        assert_eq!(
            rendered.trim(),
            format!("soroban-cost-profiler {}", env!("CARGO_PKG_VERSION")),
            "`--version` is the crate name and Cargo's version, and nothing else"
        );
        assert_eq!(clap_exit_code(&clap_error(&["--version"])), 0);
    }

    /// #183's table, printed at last: a caller that scripts against the CLI needs the codes without
    /// having to read this file first, which is what "not printed by `--help` yet" left open.
    #[test]
    fn the_long_help_prints_the_exit_code_table() {
        let help = long_help();
        for line in [
            "Exit codes:",
            "0  the run was honoured as asked",
            "1  the invocation could not be honoured as asked",
            "2  the input was accepted and the profiler could not finish its own work",
        ] {
            assert!(help.contains(line), "the help must carry {line:?}");
        }
        assert!(
            !short_help().contains("Exit codes"),
            "`-h` is the summary; the table is the `--help` half of the split clap makes for this"
        );
    }

    /// The two questions a first run actually asks — which mode do I want, and what do I do with the
    /// file — answered in the help rather than in review comments.
    #[test]
    fn the_long_help_names_both_modes_and_where_the_output_goes() {
        let help = long_help();
        for phrase in [
            "--wasm <contract.wasm>",
            "compare <base.folded>",
            "speedscope.app",
            "flamegraph.pl",
            "no SVG",
            "without debug info",
        ] {
            assert!(
                help.contains(phrase),
                "the help must say something about {phrase:?}"
            );
        }
    }

    /// An example that does not parse is worse than no example, because the reader trusts it.
    ///
    /// The lines are read back off the rendered help, so the test cannot pass while the printed text
    /// and the accepted flags drift apart — which is exactly what a hardcoded argv list in here would
    /// have hidden. Indentation is the selector: the `long_about` header also starts with the
    /// program's name, at column 0, and it is prose rather than a command line.
    #[test]
    fn every_example_in_the_help_parses() {
        let examples: Vec<Vec<String>> = long_help()
            .lines()
            .filter_map(|line| line.strip_prefix("  soroban-cost-profiler "))
            .map(|rest| rest.split_whitespace().map(String::from).collect())
            .filter(|argv: &Vec<String>| !argv.is_empty())
            .collect();

        assert!(
            examples.len() >= 4,
            "expected the documented examples in the help, parsed {examples:?}"
        );
        for argv in &examples {
            let parsed = Cli::try_parse_from(
                std::iter::once("soroban-cost-profiler").chain(argv.iter().map(String::as_str)),
            );
            assert!(
                parsed.is_ok(),
                "example `{}` does not parse: {}",
                argv.join(" "),
                parsed.unwrap_err()
            );
        }
        assert!(
            examples
                .iter()
                .any(|argv| argv.first().is_some_and(|arg| arg == "compare")),
            "the help documents one mode while the code has two"
        );
    }

    /// `[default: ]` on `--fn` reads as though an empty export name were accepted. It is not — the
    /// run refuses it — and an empty default is an artifact of how the flag is parsed, not a value
    /// worth advertising.
    ///
    /// `--output` is the one flag whose default this test cannot check as clap prints it, because it
    /// has none to print: the name depends on `--format`, so it is computed at the run and stated in
    /// the flag's own help text. The second half of this test is what keeps that honest — the three
    /// names have to be readable from the help, not only from this source file.
    #[test]
    fn the_short_help_hides_defaults_that_mean_nothing() {
        let help = short_help();
        assert!(
            !help.contains("[default: ]"),
            "an empty default must not be presented as if it were one"
        );
        for real in ["[default: 1000]", "[default: cpu]", "[default: folded]"] {
            assert!(
                help.contains(real),
                "a real default must still show: {real}"
            );
        }

        let long = long_help();
        for name in ["profile.folded", "profile.json", "profile.raw"] {
            assert!(
                long.contains(name),
                "`--output` has no printed default, so the help itself has to name the file each \
                 format writes: missing {name}"
            );
        }
    }

    /// Every format writes the file that matches its name, and none of them reuses another's.
    #[test]
    fn each_format_defaults_to_the_file_named_after_it() {
        for (format, name) in [
            (Format::Folded, "profile.folded"),
            (Format::Json, "profile.json"),
            (Format::Raw, "profile.raw"),
        ] {
            let mut cli = cli(PathBuf::new(), fixture(), "caller_of_heavy");
            cli.output = None;
            cli.format = format;
            assert_eq!(
                artifact_destination(&cli),
                Destination::File(PathBuf::from(name)),
                "a run with no `--output` has to write {name}"
            );
        }
    }

    /// `-` is stdout and not a file called `-` in the current directory.
    ///
    /// The whole of the difference is which stream the bytes reach: a pipeline waiting on stdout
    /// hangs, and the user finds a file named `-` they did not ask for.
    #[test]
    fn a_dash_output_is_stdout_and_not_a_filename() {
        for path in ["-", "./-"] {
            let mut cli = cli(PathBuf::new(), fixture(), "caller_of_heavy");
            cli.output = Some(PathBuf::from(path));
            assert_eq!(
                artifact_destination(&cli),
                if path == "-" {
                    Destination::Stdout
                } else {
                    Destination::File(PathBuf::from(path))
                },
                "`--output {path}` has to be read as {}",
                if path == "-" {
                    "stdout"
                } else {
                    "a relative file named `-`"
                }
            );
        }
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

    /// A refused write names the document it could not produce (#215).
    ///
    /// Three formats now share one write path, and one message that says "folded stack" while the
    /// run was asked for JSON sends the reader to the wrong entry of `docs/troubleshooting.md`, which
    /// quotes this sentence. The folded wording is checked byte-for-byte for the same reason.
    #[test]
    fn a_refused_write_names_the_format_it_was_attempting() {
        let dir = tempfile::tempdir().unwrap();
        let unwritable = dir.path().to_path_buf();
        for (format, label) in [
            (Format::Folded, "failed to write folded stack to"),
            (Format::Json, "failed to write JSON call tree to"),
            (Format::Raw, "failed to write raw event stream to"),
        ] {
            let mut cli = cli(unwritable.clone(), fixture(), "caller_of_heavy");
            cli.format = format;
            let failure = profile(&cli).unwrap_err();
            assert_eq!(failure.code(), 2, "{label}: {}", failure.message());
            assert!(
                failure.message().contains(label),
                "the message has to name {label:?}, got {}",
                failure.message()
            );
        }
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

    /// #213 exposes a bound the tracer has always enforced, so "done" has two halves: the flag has
    /// to parse, and the run has to honour it. This is the first half — an accepted value reaches
    /// the field unchanged, and an absent flag keeps the number every existing run used.
    #[test]
    fn an_instruction_limit_reaches_the_cli_and_keeps_its_default() {
        assert_eq!(
            parse(&["--instruction-limit", "200000000"])
                .unwrap()
                .instruction_limit,
            200_000_000
        );
        assert_eq!(parse(&[]).unwrap().instruction_limit, 100_000_000);
    }

    /// Zero is refused, and not because it is a useless bound: `record_step` increments its counter
    /// *before* comparing, so a ceiling of 0 fails the first boundary and the run ends having
    /// profiled nothing. The message is the same rule as `--sample-rate 0` for the same reason —
    /// both are better refused here than explained several stages later.
    #[test]
    fn a_zero_instruction_limit_is_rejected_and_says_why() {
        let error = parse(&["--instruction-limit", "0"]).unwrap_err();
        assert!(
            error.contains("--instruction-limit")
                && error.contains("'0'")
                && error.contains("greater than 0"),
            "the message must name the flag, the offending value, and the rule: {error}"
        );
    }

    /// The field's type is the tracer's, and a number too large for a `u32` is still a number a
    /// `u64` can hold: a bound past `u32::MAX` is accepted rather than read as an overflow, which is
    /// why this flag has its own parser instead of reusing `--sample-rate`'s.
    #[test]
    fn an_instruction_limit_beyond_u32_is_accepted() {
        assert_eq!(
            parse(&["--instruction-limit", "5000000000"])
                .unwrap()
                .instruction_limit,
            5_000_000_000
        );
    }

    /// Past the type's own range the text is refused as what it is: `u64::MAX + 1` is not "too
    /// large" for a bound, it is not a number the field can hold, and `record_step` saturates rather
    /// than wrapping, so a ceiling that cannot be represented has no meaning to be given.
    #[test]
    fn an_instruction_limit_too_large_for_u64_is_unparseable() {
        let error = parse(&["--instruction-limit", "18446744073709551616"]).unwrap_err();
        assert!(
            error.contains("not a valid number") && !error.contains("greater than 0"),
            "{error}"
        );
    }

    /// The second half of "done", measured rather than asserted: the same export that completes at
    /// the default ceiling stops at a ceiling of 1, the halt arrives as #183's input failure, and the
    /// boundaries already crossed survive in the artifact — #173's rule applied to the profiler's own
    /// guard rather than to a contract trap.
    #[test]
    fn a_low_instruction_limit_halts_the_run_and_keeps_the_trace_so_far() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("halted.folded");
        let mut cli = cli(output.clone(), fixture(), "caller_of_heavy");
        cli.instruction_limit = 1;

        let error = input_failure(profile(&cli).unwrap_err());
        assert!(error.contains("Instruction ceiling exceeded"), "{error}");
        let artifact = std::fs::read_to_string(&output)
            .expect("the trace up to the halt is the profile the user came for");
        assert!(
            !artifact.is_empty(),
            "a halted run must still write: {artifact:?}"
        );
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
        let run = run_target(BOOM, "boom", tracer(), &[]).unwrap();
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

    /// #190's structural "done": two profiles on disk, no contract in sight. This is the case
    /// `subcommand_negates_reqs` exists for — a runtime check in `profile` would have made
    /// `--wasm` optional to clap, and then a profiling run that forgot it would parse and fail
    /// somewhere deep in the pipeline instead of at the flag.
    #[test]
    fn compare_is_a_mode_that_needs_no_contract() {
        let cli = Cli::try_parse_from([
            "soroban-cost-profiler",
            "compare",
            "base.folded",
            "new.folded",
        ])
        .expect("`compare` names its two files and nothing else");

        assert_eq!(
            cli.command,
            Some(Command::Compare {
                baseline: PathBuf::from("base.folded"),
                current: PathBuf::from("new.folded"),
            })
        );
        assert!(cli.wasm.is_none(), "the negated flag stays unset");
    }

    /// The negation is conditional, and this is the half that makes it safe: with no subcommand,
    /// `--wasm` is still required and clap says so in its own words, listing what it did not get.
    /// #183 maps that refusal to exit `1` like every other command line to fix.
    #[test]
    fn profiling_still_cannot_run_without_a_contract() {
        let error = Cli::try_parse_from(["soroban-cost-profiler", "--fn", "call"])
            .expect_err("`--wasm` is required when no subcommand was named");
        let rendered = error.render().to_string();
        assert!(rendered.contains("--wasm"), "{rendered}");
        assert_eq!(clap_exit_code(&error), 1);
    }

    /// The default path still profiles: an absent subcommand must not become a third mode that
    /// quietly does nothing.
    #[test]
    fn no_subcommand_still_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("profile.folded");
        run(&cli(output.clone(), fixture(), "caller_of_heavy")).unwrap();
        assert!(output.exists(), "the `.folded` artifact is the proof");
    }

    /// A regression the report finds is the answer, not a failed run, so `compare` exits `0`
    /// whatever the numbers say. A tool that exited non-zero on "your change made it dearer" would
    /// force a CI gate to ignore the exit code in order to read the table — and then the code stops
    /// meaning anything to anyone, including #183.
    #[test]
    fn a_found_regression_is_a_successful_run() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let current = folded_file(&dir, "new.folded", "caller_of_heavy 400\n");

        assert!(run(&compare_cli(baseline, current)).is_ok());
    }

    /// Both files are read and parsed before the first row prints, and a bad line names its file:
    /// `parse_folded` counts from the start of whatever string it was handed, so "line 1" on its own
    /// would leave the reader choosing between two candidates.
    #[test]
    fn a_malformed_profile_names_its_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let broken = folded_file(&dir, "new.folded", "this is not a folded line\n");

        let error = input_failure(run(&compare_cli(baseline, broken)).unwrap_err());
        assert!(error.contains("current:"), "{error}");
        assert!(error.contains("line 1"), "{error}");
    }

    /// An absent file is #183's input error, with the path in the message — the second file is the
    /// one a mistype lands on, because the first is already on screen from the run that made it.
    #[test]
    fn an_absent_profile_is_named_rather_than_mysterious() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let missing = dir.path().join("nope.folded");

        let error = input_failure(run(&compare_cli(baseline, missing)).unwrap_err());
        assert!(error.contains("nope.folded"), "{error}");
    }

    /// `--wasm` beside `compare` states two intentions at once, and whichever answer the tool picked
    /// it would pick silently: the files are what was asked about, the contract is what was named.
    /// Refused rather than guessed, and still an input error — it is the command line to fix.
    #[test]
    fn a_contract_beside_compare_is_refused_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let current = folded_file(&dir, "new.folded", "caller_of_heavy 90\n");
        let cli = Cli {
            wasm: Some(fixture()),
            ..compare_cli(baseline, current)
        };

        let error = input_failure(run(&cli).unwrap_err());
        assert!(error.contains("--wasm"), "{error}");
    }

    /// #211's flag joins that refusal for the same reason: values are for a call, and `compare` makes
    /// none. A flag the mode cannot honour has to say so, because `--args 1000,7 compare a b` reads like
    /// an instruction and would otherwise be discarded without a word.
    #[test]
    fn arguments_beside_compare_are_refused_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let current = folded_file(&dir, "new.folded", "caller_of_heavy 90\n");
        let cli = Cli {
            args: vec![1000, 7],
            ..compare_cli(baseline, current)
        };

        let error = input_failure(run(&cli).unwrap_err());
        assert!(
            error.contains("--args") && error.contains("compare"),
            "{error}"
        );
    }

    /// Stage 4's own output is what `compare` consumes: two runs of the same contract differ by
    /// nothing, and that property is what makes a real change readable. Today both profiles are the
    /// single zero-cost frame `wasmi` hands the tracer, so this pins the composition — file written,
    /// parsed back, no phantom moves — rather than interesting numbers.
    #[test]
    fn two_runs_of_the_same_contract_compare_to_nothing_moved() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.folded");
        let second = dir.path().join("second.folded");
        profile(&cli(first.clone(), fixture(), "caller_of_heavy")).unwrap();
        profile(&cli(second.clone(), fixture(), "caller_of_heavy")).unwrap();

        let deltas = OutputFormatter::function_deltas(
            &std::fs::read_to_string(&first).unwrap(),
            &std::fs::read_to_string(&second).unwrap(),
        )
        .unwrap();
        assert!(!deltas.is_empty(), "the runs produced frames to compare");
        assert!(
            deltas.iter().all(|delta| delta.delta() == 0),
            "two identical runs must report no moves: {deltas:?}"
        );
    }

    /// #214's first half: the flags exist and carry what the user typed. `-v` is a *count*, so
    /// `-vvv` is one argument repeated rather than three flags, and the honest reading of the
    /// default is that nothing was asked for — `verbose: 0`, `quiet: false`.
    #[test]
    fn the_verbosity_flags_reach_the_cli_as_a_count_and_a_switch() {
        assert_eq!(
            parse(&[]).unwrap().verbose,
            0,
            "the default narrates nothing"
        );
        assert!(!parse(&[]).unwrap().quiet);
        assert_eq!(parse(&["-v"]).unwrap().verbose, 1);
        assert_eq!(
            parse(&["--verbose", "--verbose"]).unwrap().verbose,
            2,
            "the long form counts too, so a script can spell it out"
        );
        assert_eq!(parse(&["-vvv"]).unwrap().verbose, 3);
        assert!(parse(&["--quiet"]).unwrap().quiet);
        assert!(parse(&["-q"]).unwrap().quiet);
    }

    /// The mapping the doc comments and the README both describe, asserted as a table because the
    /// two ends of it are load-bearing: `WARN` is the default precisely so a plain run prints no
    /// records at all, and a `u8` count has to top out at `TRACE` rather than overflow or wrap.
    #[test]
    fn each_verbosity_notch_selects_the_documented_level() {
        for (verbosity, level) in [
            (0, LevelFilter::WARN),
            (1, LevelFilter::INFO),
            (2, LevelFilter::DEBUG),
            (3, LevelFilter::TRACE),
        ] {
            assert_eq!(
                log_level(false, verbosity),
                level,
                "the {verbosity}-`v` notch must select {level}"
            );
        }
        assert_eq!(
            log_level(false, 99),
            LevelFilter::TRACE,
            "past the deepest notch the count saturates the meaning, not the level"
        );
        assert_eq!(log_level(true, 0), LevelFilter::ERROR);
    }

    /// `-v --quiet` states two intentions about output at once, and clap refuses rather than
    /// picking: a tool that silently won the argument would leave the user reading a transcript
    /// that their own command line did not ask for. Same rule as `--wasm` beside `compare`.
    #[test]
    fn verbose_and_quiet_are_refused_together() {
        let error = parse(&["-v", "--quiet"]).unwrap_err();
        assert!(
            error.contains("--verbose") && error.contains("--quiet"),
            "the refusal must name both flags the user typed: {error}"
        );
    }

    /// `--quiet` is a suppression of narration, not of the run: the artifact is the answer, and an
    /// invocation that prints nothing by leaving the file unwritten would be a different tool.
    #[test]
    fn quiet_runs_still_write_the_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("quiet.folded");
        let mut cli = cli(output.clone(), fixture(), "caller_of_heavy");
        cli.quiet = true;

        profile(&cli).unwrap();
        let stacks = OutputFormatter::parse_folded(&std::fs::read_to_string(&output).unwrap())
            .expect("a quiet run writes the same artifact a loud one does");
        assert!(!stacks.is_empty());
    }

    /// `compare`'s table is the mode's whole answer rather than an echo of a file, so quiet may not
    /// take it away — and this is the assertion that keeps the flag from being implemented as a
    /// blanket "print nothing" at the top of `run`.
    #[test]
    fn quiet_does_not_silence_compare() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = folded_file(&dir, "base.folded", "caller_of_heavy 100\n");
        let current = folded_file(&dir, "new.folded", "caller_of_heavy 90\n");
        let cli = Cli {
            quiet: true,
            ..compare_cli(baseline, current)
        };
        assert!(run(&cli).is_ok());
    }

    /// The ordering that makes `--quiet` safe on the failure path: `profile` returns early *after*
    /// the artifact is written and *after* a trap is raised, so silence costs a caller its summary
    /// and nothing else. A quiet run of a trapping contract is still exit `1` with #173's message.
    #[test]
    fn quiet_does_not_swallow_a_trap() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("boom.folded");
        let temp_wasm = dir.path().join("boom.wasm");
        std::fs::write(&temp_wasm, BOOM).unwrap();
        let cli = Cli {
            quiet: true,
            ..cli(output.clone(), temp_wasm, "boom")
        };

        let error = input_failure(profile(&cli).unwrap_err());
        assert!(
            error.contains("trapped") && error.contains("boom.folded"),
            "{error}"
        );
        assert!(output.exists(), "and the partial trace is still written");
    }

    /// The two flags are part of the interface a user reads before typing anything, and clap's help
    /// is generated from the attributes — so the shorts, not just the longs, have to appear.
    #[test]
    fn the_short_help_lists_both_output_flags_with_their_shorts() {
        let help = short_help();
        for entry in ["-v, --verbose", "-q, --quiet"] {
            assert!(
                help.contains(entry),
                "the help must advertise {entry:?}:\n{help}"
            );
        }
    }
}
