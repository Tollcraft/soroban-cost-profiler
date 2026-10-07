//! Metering probes: does the profiler actually measure cost while WASM runs?
//!
//! `tests/integration.rs` only checks that the pipeline stages can be constructed around
//! an *empty* module. These probes run real instructions through the engine and assert on
//! what the tracer reports about them: fuel really drains, function boundaries really
//! become events, and a run that runs out of fuel still leaves a usable trace behind.
//!
//! # Why the module is hand-assembled
//!
//! The probe needs a module of known cost and non-trivial call structure: an exported
//! function that calls another function twice, each running a bounded loop. Building that
//! with `cargo build --target wasm32-unknown-unknown` would require the main CI job to
//! compile the fixture, which it does not — the WASM build is a separate job whose
//! artifact the test job cannot reach. So the module is encoded as literal bytes below,
//! spelled out section by section rather than generated, so a reader can check each byte
//! against the WASM spec instead of trusting a blob.
//!
//! # Shape of the probe module
//!
//! ```text
//! (module
//!   (func (export "work")  (result i32)  ;; sum += 7, ten times  -> 70
//!     (local i32 i32) ...)
//!   (func (export "probe") (result i32)  ;; work() + work()      -> 140
//!     call 0 call 0 i32.add))
//! ```
//!
//! `WORK_RESULT` and `PROBE_RESULT` pin those numbers, and `probe_returns_expected_values`
//! checks them first: if the encoding ever drifts from what this file claims, that test
//! fails before a cost assertion can be silently skewed.

use soroban_cost_profiler::models::{EventType, TraceEvent};
use soroban_cost_profiler::tracer::{
    ExecutionTracer, ProfilerState, instantiate_module, invoke_function, parse_module,
    setup_engine, setup_mock_env,
};
use wasmi::{Store, Val};

/// `work()` accumulates 7 across ten loop iterations.
const WORK_RESULT: i32 = 70;
/// `probe()` adds two `work()` results together.
const PROBE_RESULT: i32 = 140;

/// A hand-assembled module: a worker with a bounded loop, plus an export that calls it
/// twice.
mod probe_module {
    /// `work()` and `probe()` are both `() -> i32`, so they share one type entry.
    pub const WASM: &[u8] = &[
        // ---- header: magic number, then version 1 ----
        0x00, 0x61, 0x73, 0x6D, // \0asm
        0x01, 0x00, 0x00, 0x00, // version
        // ---- section 1: types ----
        0x01, //   id: type
        0x05, //   payload length
        0x01, //   one type
        0x60, //     func
        0x00, //     no parameters
        0x01, 0x7F, //     one result: i32
        // ---- section 3: functions ----
        0x03, //   id: function
        0x03, //   payload length
        0x02, //   two functions
        0x00, //     func 0 = "work"  (type index 0)
        0x00, //     func 1 = "probe" (type index 0)
        // ---- section 7: exports ----
        0x07, //   id: export
        0x10, //   payload length (16)
        0x02, //   two exports
        0x04, b'w', b'o', b'r', b'k', 0x00, 0x00, //   "work"  -> func 0
        0x05, b'p', b'r', b'o', b'b', b'e', 0x00, 0x01, //   "probe" -> func 1
        // ---- section 10: code ----
        0x0A, //   id: code
        0x2A, //   payload length (42)
        0x02, //   two function bodies
        // ---- body 0: work() ----
        0x20, //   body length (32)
        0x01, 0x02, 0x7F, //   one local group: two i32 (sum, counter)
        0x41, 0x0A, //     i32.const 10
        0x21, 0x01, //     local.set counter
        0x02, 0x40, //     block void
        0x03, 0x40, //       loop void
        0x20, 0x00, //         local.get sum
        0x41, 0x07, //         i32.const 7
        0x6A, //           i32.add
        0x21, 0x00, //         local.set sum
        0x20, 0x01, //         local.get counter
        0x41, 0x01, //         i32.const 1
        0x6B, //           i32.sub
        0x22, 0x01, //         local.tee counter  (leaves counter - 1 on the stack)
        0x0D, 0x00, //         br_if 0            (loop again while non-zero)
        0x0B, //         end loop
        0x0B, //       end block
        0x20, 0x00, //     local.get sum
        0x0B, //   end function
        // ---- body 1: probe() ----
        0x07, //   body length (7)
        0x00, //     no locals
        0x10, 0x00, //     call 0 ("work")
        0x10, 0x00, //     call 0 ("work")
        0x6A, //       i32.add
        0x0B, //   end function
    ];
}

/// One metered run: the probe's instance plus the store holding its tracer.
///
/// Each probe builds its own, so no test inherits another test's trace.
struct Probe {
    instance: wasmi::Instance,
    store: Store<ProfilerState>,
}

impl Probe {
    /// Compile and instantiate the probe module with `fuel` units of engine fuel
    /// available.
    ///
    /// Pass `u64::MAX` when the run should complete and only the trace matters; a small
    /// value when the test is about execution stopping early.
    fn with_fuel(fuel: u64) -> Self {
        let engine = setup_engine();
        let module = parse_module(&engine, probe_module::WASM)
            .expect("probe module should compile; the hand-assembled encoding is wrong");
        let state = ProfilerState {
            tracer: ExecutionTracer::new(),
            host: setup_mock_env(),
            last_fuel: 0,
        };
        let mut store = Store::new(&engine, state);
        store
            .set_fuel(fuel)
            .expect("setup_engine() enables fuel metering");
        let instance = instantiate_module(&engine, &mut store, &module)
            .expect("probe module should instantiate; it declares no imports");
        Self { instance, store }
    }

    /// Call export `name` with no arguments and take its single `i32` result.
    ///
    /// Routes through [`invoke_function`] rather than calling the function directly, so
    /// the engine hooks that produce the trace are the ones under test.
    fn call(&mut self, name: &str) -> Result<i32, wasmi::Error> {
        let mut results = [Val::I32(0)];
        invoke_function(&mut self.store, &self.instance, name, &[], &mut results)?;
        match &results[0] {
            Val::I32(value) => Ok(*value),
            other => panic!("both probe exports return i32, got {other:?}"),
        }
    }

    /// Engine fuel still unspent.
    fn fuel_left(&self) -> u64 {
        self.store
            .get_fuel()
            .expect("fuel metering is enabled by setup_engine()")
    }

    /// Replace the tracer, keeping the compiled instance — for tests that need different
    /// sampling or ceiling limits without re-instantiating.
    fn use_tracer(&mut self, tracer: ExecutionTracer) {
        self.store.data_mut().tracer = tracer;
    }

    /// Drain the trace recorded so far.
    fn take_trace(&mut self) -> Vec<TraceEvent> {
        self.store.data_mut().tracer.flush_trace()
    }
}

/// How many events carry `kind`, to keep boundary assertions readable.
fn count(events: &[TraceEvent], kind: EventType) -> usize {
    events
        .iter()
        .filter(|event| event.event_type == kind)
        .count()
}

#[test]
fn probe_returns_expected_values() {
    // Every other assertion here assumes the encoding means "70, then 140". Establish
    // that first, so a byte-level mistake surfaces loudly instead of skewing a cost
    // figure somewhere downstream.
    let mut probe = Probe::with_fuel(u64::MAX);

    assert_eq!(probe.call("work").unwrap(), WORK_RESULT);
    assert_eq!(probe.call("probe").unwrap(), PROBE_RESULT);
}

#[test]
fn fuel_is_drained_by_executed_instructions() {
    // The tracer's premise is that the engine meters instructions. If fuel never moved,
    // every cost figure the profiler reports would be fiction.
    let mut probe = Probe::with_fuel(100_000);
    let before = probe.fuel_left();

    probe.call("probe").unwrap();

    assert!(
        probe.fuel_left() < before,
        "running two 10-iteration loops consumed no fuel"
    );
}

#[test]
fn only_the_outer_invocation_is_recorded_as_a_boundary() {
    // `probe` calls `work` twice, yet the run produces exactly one Call/Return pair:
    // wasmi 2.0's call hook fires for the host-initiated call but not for WASM-internal
    // calls. This is the blocker for Phase 4 aggregation — a call tree cannot be rebuilt
    // from boundaries the engine never reports. Pinned deliberately: when a real
    // per-call hook lands, this test must be rewritten to expect three pairs, not deleted.
    let mut probe = Probe::with_fuel(u64::MAX);

    probe.call("probe").unwrap();
    let events = probe.take_trace();

    assert_eq!(
        count(&events, EventType::Call),
        count(&events, EventType::Return),
        "every call must be matched by a return: {events:?}"
    );
    assert_eq!(count(&events, EventType::Call), 1);
    assert_eq!(events.len(), 2, "unexpected events: {events:?}");
}

#[test]
fn a_pure_computation_run_produces_no_host_or_step_events() {
    // These contracts never enter the Soroban host, so no host-boundary events should
    // appear. The step assertion pins the current sampling reality: `invoke_function`
    // charges one unit per boundary, which cannot reach the tracer's default 100-unit
    // sample rate over so few boundaries, so a short run emits no Step events at all.
    let mut probe = Probe::with_fuel(u64::MAX);

    probe.call("probe").unwrap();
    let events = probe.take_trace();

    assert_eq!(count(&events, EventType::HostCall), 0);
    assert_eq!(count(&events, EventType::HostReturn), 0);
    assert_eq!(count(&events, EventType::Step), 0);
}

/// Fuel a full `probe()` run spends, measured rather than hard-coded so the figure tracks
/// whatever `wasmi`'s current metering costs.
fn probe_fuel_cost() -> u64 {
    let mut probe = Probe::with_fuel(u64::MAX);
    let before = probe.fuel_left();

    probe.call("probe").unwrap();

    before - probe.fuel_left()
}

#[test]
fn fuel_consumption_grows_with_work_done() {
    // The point of a *meter* is that more instructions cost more. `probe` runs the same
    // loop twice, so it must draw strictly more fuel than a single `work` call — and stay
    // inside a sane band, since the outer call itself carries fixed overhead.
    let single = {
        let mut probe = Probe::with_fuel(u64::MAX);
        let before = probe.fuel_left();
        probe.call("work").unwrap();
        before - probe.fuel_left()
    };
    let doubled = probe_fuel_cost();

    assert!(
        doubled > single,
        "two loop runs ({doubled}) should cost more than one ({single})"
    );
    assert!(
        doubled < single * 3,
        "two loop runs ({doubled}) costing 3x a single run ({single}) means the meter is \
         counting something other than the work"
    );
}

#[test]
fn an_out_of_fuel_run_leaves_a_partial_trace() {
    // Contracts trap on fuel routinely. Whatever executed before the trap is still real
    // work, so the trace has to survive the failure rather than be discarded with it.
    //
    // One unit short of the measured full cost guarantees the run enters (and therefore
    // records its boundary) before it traps, rather than failing at the door with an empty
    // trace.
    let full = probe_fuel_cost();
    let mut probe = Probe::with_fuel(full - 1);

    let outcome = probe.call("probe");
    let events = probe.take_trace();

    let error = outcome.expect_err(&format!(
        "{} fuel is one unit short of the {full} a full run costs, so it should trap",
        full - 1
    ));
    // The trap code, not the message: `Display` for this error is `wasmi`'s to reword, and a
    // substring match on "fuel" would also pass if an unrelated error happened to mention it.
    assert_eq!(
        error.as_trap_code(),
        Some(wasmi::TrapCode::OutOfFuel),
        "expected an out-of-fuel trap, got: {error}"
    );
    assert_eq!(
        count(&events, EventType::Call),
        1,
        "the boundary crossed before the trap should still be recorded: {events:?}"
    );
}

#[test]
fn an_unknown_export_is_reported_not_panicked() {
    // A typo'd or absent export is user-facing input, so it must come back as an error
    // naming the function rather than a panic.
    let mut probe = Probe::with_fuel(u64::MAX);

    let error = probe
        .call("does_not_exist")
        .expect_err("a missing export should not resolve");

    assert!(
        error.to_string().contains("does_not_exist"),
        "the error should name the missing function, got: {error}"
    );
}

#[test]
fn the_instruction_ceiling_stops_execution() {
    // The error is now surfaced and execution halts.
    let mut probe = Probe::with_fuel(u64::MAX);
    probe.use_tracer(ExecutionTracer::new().with_instruction_ceiling(1));

    let outcome = probe.call("probe");

    assert!(
        outcome.is_err(),
        "execution should trap when ceiling is reached"
    );
}
