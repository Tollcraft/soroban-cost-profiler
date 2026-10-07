use crate::models::{EventType, TraceEvent};
use soroban_env_host::{Host, budget::AsBudget};
use tracing::{debug, error, info, trace};

/// Collects costed execution events while a WASM contract runs under `wasmi`.
///
/// The tracer is fed by the engine hooks in [`invoke_function`] and by
/// [`instantiate_module`]; it owns no engine state itself, so it can be cloned,
/// inspected, and drained independently of the run being profiled.
///
/// Two knobs bound the trace's size and cost, both inherited from the PRD's OOM
/// constraints: [`with_sample_rate`] throttles how many `Step` events are emitted,
/// and [`with_instruction_ceiling`] stops a runaway contract from buffering an
/// unbounded number of events.
///
/// [`invoke_function`]: crate::tracer::invoke_function
/// [`instantiate_module`]: crate::tracer::instantiate_module
/// [`with_sample_rate`]: ExecutionTracer::with_sample_rate
/// [`with_instruction_ceiling`]: ExecutionTracer::with_instruction_ceiling
#[derive(Default, Debug, Clone)]
pub struct ExecutionTracer {
    /// Every event emitted so far, in execution order.
    pub events: Vec<TraceEvent>,
    // Accumulated CPU cost required before the next `Step` event is emitted.
    sample_rate: u64,
    // Hard limit on `record_step` calls; exceeding it aborts the trace.
    instruction_ceiling: u64,
    // Steps recorded so far, counted regardless of whether they were sampled.
    instruction_count: u64,
    // Cost carried forward from steps that have not crossed the sample threshold yet.
    current_step_cost: u64,
    current_mem_cost: u64,
    // Budget counters captured at the matching `record_host_call`, so the host return
    // can report the host function's own cost rather than the cumulative total.
    host_snapshot_cpu: u64,
    host_snapshot_mem: u64,
}

impl ExecutionTracer {
    /// Create a tracer with the MVP defaults: sample every ~100 CPU units, and abort
    /// past 100M instructions (the PRD's ceiling for a contract run).
    pub fn new() -> Self {
        Self {
            sample_rate: 100,
            instruction_ceiling: 100_000_000,
            ..Default::default()
        }
    }

    /// Set the CPU cost that must accumulate before a `Step` event is emitted.
    ///
    /// Lower means finer-grained cost attribution and more memory spent on the event
    /// buffer; higher means a smaller trace at the cost of detail.
    pub fn with_sample_rate(mut self, sample_rate: u64) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Set the maximum number of steps accepted before
    /// [`record_step`](crate::tracer::ExecutionTracer::record_step) starts failing.
    ///
    /// This is the protection against an infinite loop inside the profiled contract.
    pub fn with_instruction_ceiling(mut self, ceiling: u64) -> Self {
        self.instruction_ceiling = ceiling;
        self
    }

    /// Account for one executed instruction, emitting a `Step` event once enough cost
    /// has accumulated to cross the sample threshold.
    ///
    /// Costs are carried forward between calls rather than dropped, so the sum of all
    /// emitted `cpu_cost` values equals the total cost passed in (minus whatever is
    /// still below threshold in the buffer at the end of the run). Saturation, not
    /// wrapping, is deliberate: a contract that racks up absurd counters should show a
    /// huge sampled cost, not silently roll over to a small one.
    ///
    /// Returns `Err` after the instruction ceiling is passed, which the caller should
    /// treat as a halt signal — the trace up to that point is still valid and readable
    /// via [`flush_trace`].
    ///
    /// [`flush_trace`]: ExecutionTracer::flush_trace
    pub fn record_step(
        &mut self,
        pc: usize,
        cpu_cost: u64,
        mem_cost: u64,
    ) -> Result<(), &'static str> {
        trace!(
            "Stepping at PC: {}, cpu: {}, mem: {}",
            pc, cpu_cost, mem_cost
        );
        self.instruction_count = self.instruction_count.saturating_add(1);
        if self.instruction_count > self.instruction_ceiling {
            error!("Instruction ceiling exceeded at PC: {}", pc);
            return Err("Instruction ceiling exceeded");
        }

        self.current_step_cost = self.current_step_cost.saturating_add(cpu_cost);
        self.current_mem_cost = self.current_mem_cost.saturating_add(mem_cost);
        if self.current_step_cost >= self.sample_rate {
            self.events.push(TraceEvent {
                pc,
                event_type: EventType::Step,
                cpu_cost: self.current_step_cost,
                mem_cost: self.current_mem_cost,
            });
            self.current_step_cost = 0;
            self.current_mem_cost = 0;
        }
        Ok(())
    }

    /// Record entry into a WASM function. Emitted unconditionally: boundaries are the
    /// spine of the call tree the aggregator later rebuilds, so sampling them would
    /// lose frames entirely.
    pub fn record_call(&mut self, pc: usize, cpu_cost: u64, mem_cost: u64) {
        debug!("WASM Call at PC: {}", pc);
        self.events.push(TraceEvent {
            pc,
            event_type: EventType::Call,
            cpu_cost,
            mem_cost,
        });
    }

    /// Record exit from a WASM function, pairing with the nearest [`record_call`].
    ///
    /// [`record_call`]: ExecutionTracer::record_call
    pub fn record_return(&mut self, pc: usize, cpu_cost: u64, mem_cost: u64) {
        debug!("WASM Return at PC: {}", pc);
        self.events.push(TraceEvent {
            pc,
            event_type: EventType::Return,
            cpu_cost,
            mem_cost,
        });
    }

    /// Note that execution is entering the Soroban host, and snapshot the budget.
    ///
    /// The event itself carries zero cost: the host budget is cumulative, so the cost
    /// of the host call is only knowable on return, as the delta from this snapshot in
    /// [`record_host_return`]. Missing this snapshot would attribute every host call
    /// since the start of the run to whichever function returns last.
    ///
    /// Budget reads are `unwrap_or(0)` — a host that cannot report its budget should
    /// degrade into an uncosted trace, not abort the run being profiled.
    ///
    /// [`record_host_return`]: ExecutionTracer::record_host_return
    pub fn record_host_call(&mut self, pc: usize, host: &Host) {
        debug!("Host Call at PC: {}", pc);
        let budget = host.as_budget();
        self.host_snapshot_cpu = budget.get_cpu_insns_consumed().unwrap_or(0);
        self.host_snapshot_mem = budget.get_mem_bytes_consumed().unwrap_or(0);

        self.events.push(TraceEvent {
            pc,
            event_type: EventType::HostCall,
            cpu_cost: 0,
            mem_cost: 0,
        });
    }

    /// Close the host frame opened by [`record_host_call`], charging it the budget it
    /// consumed while we were inside.
    ///
    /// Only the most recent snapshot is kept, which assumes host calls do not nest. If
    /// a host function were to re-enter WASM, the inner return would overwrite the
    /// outer snapshot and the outer cost would be reported against the wrong frame.
    ///
    /// [`record_host_call`]: ExecutionTracer::record_host_call
    pub fn record_host_return(&mut self, pc: usize, host: &Host) {
        debug!("Host Return at PC: {}", pc);
        let budget = host.as_budget();
        let current_cpu = budget.get_cpu_insns_consumed().unwrap_or(0);
        let current_mem = budget.get_mem_bytes_consumed().unwrap_or(0);

        let diff_cpu = current_cpu.saturating_sub(self.host_snapshot_cpu);
        let diff_mem = current_mem.saturating_sub(self.host_snapshot_mem);

        self.events.push(TraceEvent {
            pc,
            event_type: EventType::HostReturn,
            cpu_cost: diff_cpu,
            mem_cost: diff_mem,
        });
    }

    /// Hand the accumulated events over to the next pipeline stage, leaving the tracer
    /// empty.
    ///
    /// Use this when the trace is consumed; it avoids copying what can be up to 100M
    /// events. [`trace`] is the non-destructive alternative for inspection and tests.
    ///
    /// [`trace`]: ExecutionTracer::trace
    pub fn flush_trace(&mut self) -> Vec<TraceEvent> {
        std::mem::take(&mut self.events)
    }

    /// Return a copy of the events recorded so far, keeping them in the buffer.
    ///
    /// Despite the name, this does not drive execution: the run is already being traced
    /// by the engine hooks installed in [`invoke_function`], and this only reads what
    /// they have collected.
    ///
    /// [`invoke_function`]: crate::tracer::invoke_function
    pub fn trace(&mut self) -> Vec<TraceEvent> {
        self.events.clone()
    }
}

/// Read a contract's WASM from disk, rejecting anything that is not a WASM module.
///
/// The four-byte magic check fails fast with a clear error; otherwise `wasmi` would
/// surface a confusing parse failure on whatever file the user pointed us at.
pub fn load_wasm_file(path: &str) -> std::io::Result<Vec<u8>> {
    info!("Loading WASM file from {}", path);
    let bytes = std::fs::read(path)?;
    if bytes.len() < 4 || &bytes[0..4] != b"\0asm" {
        error!("Invalid WASM signature for file: {}", path);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid WASM signature",
        ));
    }
    Ok(bytes)
}

/// Build a `wasmi` engine with fuel metering enabled.
///
/// Fuel is what makes costing possible at all: with `consume_fuel` off, the engine
/// runs without counting instructions and there is nothing to attribute to a frame.
pub fn setup_engine() -> wasmi::Engine {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    wasmi::Engine::new(&config)
}

/// Compile WASM bytes into a `wasmi` module ready for instantiation.
///
/// Validation happens here, so a malformed but magic-numbered file fails at this step
/// rather than mid-trace.
pub fn parse_module(
    engine: &wasmi::Engine,
    wasm_bytes: &[u8],
) -> Result<wasmi::Module, wasmi::Error> {
    wasmi::Module::new(engine, wasm_bytes)
}

/// Provide a host for traces that never touch chain state.
///
/// A default `Host` carries its own budget, which is what [`record_host_call`] and
/// [`record_host_return`] read; profiling pure-computation contracts therefore needs
/// no ledger setup.
///
/// [`record_host_call`]: ExecutionTracer::record_host_call
/// [`record_host_return`]: ExecutionTracer::record_host_return
pub fn setup_mock_env() -> Host {
    Host::default()
}

/// The `wasmi` store state the profiler needs reachable from inside engine callbacks.
pub struct ProfilerState {
    pub tracer: ExecutionTracer,
    pub host: Host,
    /// Reserved for fuel-delta step metering. The call hooks installed by
    /// [`invoke_function`] cannot read remaining fuel, so nothing populates this yet.
    ///
    /// [`invoke_function`]: crate::tracer::invoke_function
    pub last_fuel: u64,
}

/// Instantiate a module and run its start function, with no host imports registered.
///
/// The linker is intentionally empty: the fixture contracts are pure computation, and
/// any contract that imports a Soroban host function will fail here until the real host
/// bindings are wired up.
#[tracing::instrument(skip(engine, store, module))]
pub fn instantiate_module(
    engine: &wasmi::Engine,
    store: &mut wasmi::Store<ProfilerState>,
    module: &wasmi::Module,
) -> Result<wasmi::Instance, wasmi::Error> {
    info!("Instantiating WASM module");
    let linker = <wasmi::Linker<ProfilerState>>::new(engine);
    linker.instantiate_and_start(store, module)
}

/// Run an exported function while recording the boundaries it crosses.
///
/// A call hook is installed on the store before the call, and `wasmi` runs it at the
/// boundaries it actually sees: one `CallingWasm`/`ReturningFromWasm` pair for the call this
/// function starts, plus a `CallingHost`/`ReturningFromHost` pair for every host function the
/// contract invokes. Results are written into `results`, which the caller must size to the
/// function's return arity.
///
/// Three consequences, all traceable to what `wasmi` 2.0 exposes to a call hook:
///
/// * **WASM-to-WASM calls are invisible.** The engine fires the WASM half of the hook only
///   where a host-initiated call enters it, not for calls made from inside running wasm. A
///   contract that calls five helpers still produces exactly one `Call` and one `Return`
///   event, so the trace names the entry point but not the call tree beneath it — which is
///   why [`ProfileAggregator::aggregate`] cannot be written against these events alone.
///   `only_the_outer_invocation_is_recorded_as_a_boundary` in `tests/meter_probe.rs` pins
///   this, and says what to change rather than delete when a per-call hook exists.
/// * Every event is recorded at `pc = 0`. A call hook is given no program counter — `wasmi` 2.0's
///   only execution hook passes the hook *variant* and nothing else, no callee and no offset — so the
///   cost of a whole call lands on its single boundary. Even an internal instruction pointer would
///   not help: the engine re-encodes wasm bytecode into its own instruction stream during
///   translation and keeps no table back to the original offsets, so the finest address a tracer
///   could ever be handed is a function body's start. That is the space [`CodeMap`] indexes, and
///   #153 is the translation into it; see [`SourceMapper`] for what real offsets would unlock, and
///   why DWARF alone is not enough.
/// * One synthetic step of cost 1 is recorded per boundary rather than per instruction,
///   because there is no instruction-level hook. CPU cost therefore under-reports work done
///   inside a function body; the host-budget deltas in [`record_host_return`] are the
///   accurate part.
///
/// [`ProfileAggregator::aggregate`]: crate::aggregator::ProfileAggregator::aggregate
/// [`CodeMap`]: crate::source_map::CodeMap
/// [`SourceMapper`]: crate::source_map::SourceMapper
/// [`record_host_return`]: ExecutionTracer::record_host_return
#[tracing::instrument(skip(store, instance, params, results))]
pub fn invoke_function(
    store: &mut wasmi::Store<ProfilerState>,
    instance: &wasmi::Instance,
    func_name: &str,
    params: &[wasmi::Val],
    results: &mut [wasmi::Val],
) -> Result<(), wasmi::Error> {
    info!("Invoking function: {}", func_name);

    // Record the boundaries the engine reports: this call's entry and exit, plus each host
    // call in between. Calls the contract makes to its own functions do not reach here.
    store.call_hook(|state: &mut ProfilerState, hook_type| {
        // Remaining fuel is not reachable from inside this callback, so boundaries are
        // recorded as events and costed from the host budget instead of from fuel.
        match hook_type {
            wasmi::CallHook::CallingWasm => {
                state.tracer.record_call(0, 0, 0);
            }
            wasmi::CallHook::ReturningFromWasm => {
                state.tracer.record_return(0, 0, 0);
            }
            wasmi::CallHook::CallingHost => {
                state.tracer.record_host_call(0, &state.host);
            }
            wasmi::CallHook::ReturningFromHost => {
                state.tracer.record_host_return(0, &state.host);
            }
        }
        // `wasmi` 2.0.0 has no instruction hook, so a single unit-costed step stands in
        // for the instructions run since the last boundary.
        if let Err(e) = state.tracer.record_step(0, 1, 0) {
            return Err(wasmi::Error::new(e.to_string()));
        }
        Ok(())
    });

    let func = instance.get_func(&mut *store, func_name).ok_or_else(|| {
        error!("Function '{}' not found", func_name);
        wasmi::Error::new(format!("Function '{}' not found", func_name))
    })?;

    func.call(store, params, results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_step_sampling() {
        let mut tracer = ExecutionTracer::new().with_sample_rate(100);

        let _ = tracer.record_step(1, 50, 10);
        assert!(tracer.events.is_empty());

        let _ = tracer.record_step(2, 60, 20);
        assert_eq!(tracer.events.len(), 1);
        assert_eq!(tracer.events[0].pc, 2);
        assert_eq!(tracer.events[0].cpu_cost, 110);
        assert_eq!(tracer.events[0].mem_cost, 30);
        assert_eq!(tracer.events[0].event_type, EventType::Step);

        let _ = tracer.record_step(3, 40, 0);
        assert_eq!(tracer.events.len(), 1);
    }

    #[test]
    fn test_record_call_and_return() {
        let mut tracer = ExecutionTracer::new().with_sample_rate(100);

        tracer.record_call(1, 10, 5);
        tracer.record_return(2, 20, 10);

        assert_eq!(tracer.events.len(), 2);
        assert_eq!(tracer.events[0].event_type, EventType::Call);
        assert_eq!(tracer.events[0].cpu_cost, 10);
        assert_eq!(tracer.events[0].mem_cost, 5);
        assert_eq!(tracer.events[1].event_type, EventType::Return);
        assert_eq!(tracer.events[1].cpu_cost, 20);
        assert_eq!(tracer.events[1].mem_cost, 10);
    }

    #[test]
    fn test_record_host_call_and_return() {
        let mut tracer = ExecutionTracer::new().with_sample_rate(100);
        let host = setup_mock_env();

        tracer.record_host_call(1, &host);

        let _ = host.as_budget().charge(
            soroban_env_host::xdr::ContractCostType::WasmInsnExec,
            Some(100),
        );

        tracer.record_host_return(2, &host);

        assert_eq!(tracer.events.len(), 2);
        assert_eq!(tracer.events[0].event_type, EventType::HostCall);
        assert_eq!(tracer.events[1].event_type, EventType::HostReturn);
        assert_eq!(tracer.events[1].cpu_cost, 0);
        assert_eq!(tracer.events[1].mem_cost, 0);
    }

    #[test]
    fn test_instruction_ceiling() {
        let mut tracer = ExecutionTracer::new().with_instruction_ceiling(2);
        assert!(tracer.record_step(1, 10, 0).is_ok());
        assert!(tracer.record_step(2, 10, 0).is_ok());
        assert!(tracer.record_step(3, 10, 0).is_err());
    }
}

#[cfg(test)]
mod recursive_tests {
    use super::*;

    #[test]
    fn test_recursive_function_calls() {
        let mut tracer = ExecutionTracer::new().with_sample_rate(100);
        let depth = 1000;

        for i in 0..depth {
            tracer.record_call(i, 5, 2);
        }

        for i in (0..depth).rev() {
            tracer.record_return(i, 5, 2);
        }

        assert_eq!(tracer.events.len(), 2000);
        assert_eq!(tracer.events[0].event_type, EventType::Call);
        assert_eq!(tracer.events[1999].event_type, EventType::Return);
    }
}
