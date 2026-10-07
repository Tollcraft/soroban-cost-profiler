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
//! Stage 2 is still a scaffold (Phase 3 of `ROADMAP.md`), so the mapper it builds resolves
//! nothing and every frame reaches Stage 3 as an unresolved `wasm[pc]`. The four calls that hand
//! data between stages are now live; what is still missing is real input, which Phase 5's CLI
//! supplies.
//!
//! Each stage's construction lives in its own function so that wiring Phase 3 in is a one-line
//! change at the call site, and so the placeholder input each stage needs today has a documented
//! home instead of sitting inline in `main`.
//!
//! [`TraceEvent`]: soroban_cost_profiler::models::TraceEvent
//! [`CallStackNode`]: soroban_cost_profiler::models::CallStackNode
use clap::Parser;
use soroban_cost_profiler::aggregator::ProfileAggregator;
use soroban_cost_profiler::formatter::OutputFormatter;
use soroban_cost_profiler::source_map::SourceMapper;
use soroban_cost_profiler::tracer::ExecutionTracer;
use std::path::PathBuf;
use tracing::warn;

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

    /// Target function to invoke

    #[arg(long = "fn", default_value = "")]
    pub fn_name: String,

    /// Sampling rate
    #[arg(long, default_value_t = 1000)]
    pub sample_rate: u32,
}

/// Stage 1: build a tracer carrying the MVP sampling and instruction-ceiling defaults.
///
/// The engine hooks that feed it are installed by `tracer::invoke_function`, which needs a
/// `Store<ProfilerState>` rather than a bare tracer — that wiring lands with Phase 2's CLI.
fn initialize_tracer() -> ExecutionTracer {
    ExecutionTracer::new()
}

/// Stage 2: build the source mapper for the target WASM binary.
///
/// Loading is fallible now that the stage reads DWARF, and the failure is not fatal: a binary
/// without symbols still profiles, it just names frames `wasm[pc]`. The error is logged because
/// it tells the user which build flag to set — a flamegraph of unnamed frames is otherwise
/// indistinguishable from a profiler that is not working.
///
/// Empty bytes are the placeholder until Phase 5 reads the binary from disk (`load_wasm_file`),
/// so this logs `NotWasm` and continues with an unmapped mapper.
fn load_source_mapper() -> SourceMapper {
    SourceMapper::new(&[]).unwrap_or_else(|error| {
        warn!("cannot symbolize frames: {error}");
        SourceMapper::unmapped()
    })
}

/// Stage 3: build an empty aggregator.
fn initialize_aggregator() -> ProfileAggregator {
    ProfileAggregator::new()
}

/// Run the whole pipeline over an empty trace and return the folded stacks it produces.
///
/// All four stages are now wired in call order, but it is still a dry harness: no WASM is
/// executed, so the tracer flushes nothing and the result is one zero-cost frame. Flag parsing
/// (`--wasm`, `--output`) is Phase 5, which replaces the empty trace with a real run and writes
/// this return value to disk.
fn profile(cli: &Cli) {
    use soroban_cost_profiler::tracer::{ProfilerState, instantiate_module, invoke_function};
    use soroban_env_host::Host;
    // 1. Initialize tracer and execute WASM
    let tracer = initialize_tracer();

    let engine = wasmi::Engine::default();
    let wasm_bytes = std::fs::read(&cli.wasm).unwrap_or_else(|e| {
        tracing::error!("Failed to read WASM file {}: {}", cli.wasm.display(), e);
        std::process::exit(1);
    });

    let module = wasmi::Module::new(&engine, &wasm_bytes[..]).unwrap_or_else(|e| {
        tracing::error!("Failed to parse WASM module: {}", e);
        std::process::exit(1);
    });

    let mut store = wasmi::Store::new(
        &engine,
        ProfilerState {
            tracer,
            host: Host::default(),
            last_fuel: 0,
        },
    );

    let events = match instantiate_module(&engine, &mut store, &module) {
        Ok(instance) => {
            let func_name = if cli.fn_name.is_empty() {
                "test"
            } else {
                &cli.fn_name
            };
            let mut results = vec![wasmi::Val::I32(0); 1];
            match invoke_function(&mut store, &instance, func_name, &[], &mut results) {
                Ok(_) => tracing::info!("WASM execution completed successfully."),
                Err(e) => tracing::error!(
                    "WASM execution trapped/panicked: {}. Flushing partial trace.",
                    e
                ),
            }
            store.into_data().tracer.flush_trace()
        }
        Err(e) => {
            tracing::error!("Failed to instantiate module: {}", e);
            store.into_data().tracer.flush_trace()
        }
    };

    // 2. Load DWARF source map
    let mapper = load_source_mapper();

    // 3. Aggregate events into call tree
    let mut aggregator = initialize_aggregator();
    let call_tree = aggregator.aggregate(events, &mapper);

    // 4. Format and output
    let output = OutputFormatter::to_collapsed_stack(&call_tree);

    if let Err(e) = std::fs::write(&cli.output, output) {
        tracing::error!(
            "Failed to write folded stack to {}: {}",
            cli.output.display(),
            e
        );
    } else {
        tracing::info!(
            "Successfully wrote folded stack to {}",
            cli.output.display()
        );
    }
}

/// Print the MVP notice and run the harness.
fn main() {
    let cli = Cli::parse();
    println!("soroban-cost-profiler MVP (Not yet implemented)");
    profile(&cli);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The harness has no WASM to run yet, so the bar is that the stages hand data to each other
    /// in the documented order and the last one produces a parseable folded stack. An empty trace
    /// is legal input for the whole pipeline: it ends as one zero-cost, unresolved frame.
    #[test]
    fn assembling_the_stages_runs_to_completion() {
        let temp_dir = tempfile::tempdir().unwrap();
        let output_path = temp_dir.path().join("profile.folded");
        let wasm_path = temp_dir.path().join("dummy.wasm");
        std::fs::write(&wasm_path, b"\0asm\x01\x00\x00\x00").unwrap();
        let cli = Cli {
            wasm: wasm_path,
            output: output_path.clone(),
            fn_name: String::new(),
            sample_rate: 1000,
        };
        profile(&cli);

        let collapsed = std::fs::read_to_string(&output_path).unwrap();
        let stacks = OutputFormatter::parse_folded(&collapsed)
            .expect("the pipeline's own output must be valid folded stacks");
        assert_eq!(
            stacks.values().sum::<u64>(),
            0,
            "no WASM ran, so nothing was costed"
        );
    }
}
