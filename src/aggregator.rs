use crate::models::{CallStackNode, EventType, SourceFrame, TraceEvent};
use crate::source_map::SourceMapper;
use std::collections::HashMap;

/// Stage 3 of the pipeline: fold the flat [`TraceEvent`] stream into a [`CallStackNode`] tree.
///
/// Raw events are a sequence of boundaries and cost deltas, which no profiler viewer can read:
/// a flamegraph wants a tree where each frame carries what ran inside it (exclusive) and what it
/// caused to run (inclusive). This stage is that reconstruction.
///
/// The fold is deliberately not a map keyed by call path. A run can emit a million sampled `Step`
/// events, and building a path key per event would allocate for every one of them; open frames
/// live on a stack instead and a step only touches the innermost one, so accounting allocates per
/// *call*, not per instruction (`AGENTS.md`'s OOM constraint).
#[derive(Default)]
pub struct ProfileAggregator {
    // No persistent state on purpose: the open-frame stack belongs to one fold, so a single
    // aggregator can be reused across runs without leaking cost from the previous trace.
}

/// Placeholder frame for a WASM boundary the source mapper could not resolve.
///
/// Until Stage 2 resolves program counters (see [`SourceMapper`]), every WASM frame lands here,
/// and today every event carries `pc = 0` (see [`invoke_function`]), so a whole run collapses
/// into one frame. That is the honest output of the input it is given, and `wasm[0]` says so on
/// the flamegraph instead of inventing a name.
///
/// [`invoke_function`]: crate::tracer::invoke_function
fn wasm_frame(pc: usize) -> SourceFrame {
    SourceFrame {
        function_name: format!("wasm[{pc}]"),
        file_path: None,
        line_number: None,
    }
}

/// Frame for a Soroban host call.
///
/// The event says execution crossed into the host at `pc`, and the tracer names no host
/// function, so the call site is the best available label. Host frames stay distinct from the
/// WASM frame at the same `pc` because host cost is measured from the budget and is the accurate
/// part of a trace, while WASM cost is currently one synthetic unit per boundary.
fn host_frame(pc: usize) -> SourceFrame {
    SourceFrame {
        function_name: format!("host[{pc}]"),
        file_path: None,
        line_number: None,
    }
}

/// Frame for the tree returned by an event stream that opened no boundary at all.
///
/// Named separately from `wasm[pc]` so "there was nothing to aggregate" is distinguishable from
/// "everything ran unresolved".
fn empty_frame() -> SourceFrame {
    SourceFrame {
        function_name: "unsymbolized".to_string(),
        file_path: None,
        line_number: None,
    }
}

/// A frame that has been entered but not yet charged or closed.
fn open_node(frame: SourceFrame) -> CallStackNode {
    CallStackNode {
        frame,
        exclusive_cpu: 0,
        inclusive_cpu: 0,
        exclusive_mem: 0,
        inclusive_mem: 0,
        exclusive_hostcalls: 0,
        inclusive_hostcalls: 0,
        children: HashMap::new(),
    }
}

/// Charge a cost delta to the frame that was executing, i.e. the innermost open one.
///
/// Saturating, matching [`ExecutionTracer::record_step`]: an absurd counter should show as an
/// absurd cost rather than roll over into a plausible small one.
///
/// [`ExecutionTracer::record_step`]: crate::tracer::ExecutionTracer::record_step
fn charge(open: &mut [CallStackNode], cpu_cost: u64, mem_cost: u64) {
    if let Some(frame) = open.last_mut() {
        charge_to(frame, cpu_cost, mem_cost);
    }
}

fn charge_to(frame: &mut CallStackNode, cpu_cost: u64, mem_cost: u64) {
    frame.exclusive_cpu = frame.exclusive_cpu.saturating_add(cpu_cost);
    frame.exclusive_mem = frame.exclusive_mem.saturating_add(mem_cost);
}

/// Merge a finished frame into its parent, or record it as a top-level frame.
///
/// Children are keyed by function name the way [`CallStackNode`] requires, so the same function
/// reached twice shares one node and their costs add up — which is what a flamegraph shows for a
/// helper called in a loop. A frame whose parent has the *same* name (recursion, or two frames the
/// trace cannot tell apart) folds into that parent rather than nesting beneath it, the way
/// flamegraph consumers collapse repeated stack frames; it also bounds the tree by distinct
/// functions instead of by call depth.
fn close(frame: CallStackNode, open: &mut [CallStackNode], roots: &mut Vec<CallStackNode>) {
    let Some(parent) = open.last_mut() else {
        roots.push(frame);
        return;
    };

    if parent.frame.function_name == frame.frame.function_name {
        merge(parent, frame);
        return;
    }

    let key = frame.frame.function_name.clone();
    match parent.children.get_mut(&key) {
        Some(existing) => merge(existing, frame),
        None => {
            parent.children.insert(key, frame);
        }
    }
}

/// Add one frame's costs and subtree into an existing frame of the same name.
fn merge(into: &mut CallStackNode, from: CallStackNode) {
    into.exclusive_cpu = into.exclusive_cpu.saturating_add(from.exclusive_cpu);
    into.exclusive_mem = into.exclusive_mem.saturating_add(from.exclusive_mem);
    into.exclusive_hostcalls = into.exclusive_hostcalls.saturating_add(from.exclusive_hostcalls);

    for (key, child) in from.children {
        match into.children.get_mut(&key) {
            Some(existing) => merge(existing, child),
            None => {
                into.children.insert(key, child);
            }
        }
    }
}

/// Fill in each node's inclusive cost, returning this subtree's inclusive (cpu, mem).
///
/// Done in one post-order pass after the fold rather than incremented while closing frames:
/// `inclusive` is defined as exclusive cost plus everything the frame caused to run, so deriving
/// it from the finished tree cannot double-count a merged frame, and the invariant
/// `inclusive == exclusive + sum(children.inclusive)` holds at every node by construction.
fn compute_inclusive(node: &mut CallStackNode) -> (u64, u64, u64) {
    let mut cpu = node.exclusive_cpu;
    let mut mem = node.exclusive_mem;
    let mut hostcalls = node.exclusive_hostcalls;

    for child in node.children.values_mut() {
        let (child_cpu, child_mem, child_hostcalls) = compute_inclusive(child);
        cpu = cpu.saturating_add(child_cpu);
        mem = mem.saturating_add(child_mem);
        hostcalls = hostcalls.saturating_add(child_hostcalls);
    }

    node.inclusive_cpu = cpu;
    node.inclusive_mem = mem;
    node.inclusive_hostcalls = hostcalls;
    (cpu, mem, hostcalls)
}

/// Combine top-level frames into the single tree this stage returns.
///
/// One traced run has exactly one host-initiated call, so `roots` normally holds one frame. If a
/// drained tracer is reused across invocations, repeats of the same export merge into the root
/// (their costs add, as for any repeated call) and a differently named root becomes a child of
/// the first — a choice, because [`CallStackNode`] gives the pipeline one frame to return rather
/// than a forest.
fn fold_roots(roots: Vec<CallStackNode>) -> CallStackNode {
    let mut roots = roots.into_iter();

    // The first frame is the run's entry point; later ones are folded beneath it.
    let Some(mut root) = roots.next() else {
        return open_node(empty_frame());
    };

    for frame in roots {
        if frame.frame.function_name == root.frame.function_name {
            merge(&mut root, frame);
        } else {
            let key = frame.frame.function_name.clone();
            match root.children.get_mut(&key) {
                Some(existing) => merge(existing, frame),
                None => {
                    root.children.insert(key, frame);
                }
            }
        }
    }

    root
}

impl ProfileAggregator {
    /// Create an aggregator. It carries no state between folds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a trace into a call tree, resolving each boundary's program counter with `mapper`.
    ///
    /// How each event kind is handled, and why:
    ///
    /// * `Call` charges its own delta to the *caller* — the cost recorded between two events
    ///   accrued before the callee ran — then opens a frame for the callee. The stream's first
    ///   `Call` has no caller, so its delta becomes the entry cost of the frame it opens.
    /// * `Step` charges the innermost open frame, which is the definition of exclusive cost. If
    ///   steps arrive with no frame open (a trace taken partway through a run) the frame is
    ///   opened from the event's `pc` rather than the cost being dropped.
    /// * `HostCall`/`HostReturn` open and close a host frame. They arrive paired, and the tracer
    ///   records zero on entry and the whole budget delta on return, so all host cost lands
    ///   inside the host frame instead of on whichever function happened to close last.
    /// * `Return` charges the frame being closed, then pops it.
    ///
    /// Two input shapes are tolerated rather than treated as errors, because both are reachable
    /// from a real contract. A run that traps mid-call never records its `Return` events, so
    /// whatever frames are still open when the stream ends are folded up the stack and kept —
    /// dropping them would discard exactly the expensive tail a profiler exists to show. An
    /// unmatched `Return` (whose `Call` was already drained by [`flush_trace`]) is ignored instead
    /// of panicking.
    ///
    /// `inclusive_*` is filled in on the returned tree, so a caller can read either half without
    /// recomputing it.
    ///
    /// # Current fidelity
    ///
    /// The tree can only be as deep as the boundaries the engine reports and only as named as
    /// Stage 2 allows. `wasmi` reports no inner WASM-to-WASM calls and every event carries
    /// `pc = 0`, so today's real trace folds into one `wasm[0]` frame holding a single `host[0]`
    /// child with all the measured budget — the contract's own work is attributed to the WASM
    /// frame, and nothing inside it is separated. That is the honest output of the input it is
    /// given, and the unnamed frames say so on the flamegraph instead of inventing names. The
    /// tests below therefore feed synthetic event streams — the shape a deeper trace will produce
    /// once Phases 2 and 3 land — so the fold itself is covered now.
    ///
    /// [`flush_trace`]: crate::tracer::ExecutionTracer::flush_trace
    pub fn aggregate(&mut self, events: Vec<TraceEvent>, mapper: &SourceMapper) -> CallStackNode {
        let mut open: Vec<CallStackNode> = Vec::new();
        let mut roots: Vec<CallStackNode> = Vec::new();

        let wasm_node = |pc| open_node(mapper.resolve(pc).unwrap_or_else(|| wasm_frame(pc)));

        for event in events {
            match event.event_type {
                EventType::Call => {
                    // `wasm_node` is called before the borrow of `open` below ends, so the callee
                    // is built first and only then charged to — or pushed onto — the stack.
                    let mut callee = wasm_node(event.pc);
                    match open.last_mut() {
                        // The delta was spent on the way in, by whoever is calling.
                        Some(caller) => charge_to(caller, event.cpu_cost, event.mem_cost),
                        // Nothing is open yet, so this is the traced run's own entry cost.
                        None => charge_to(&mut callee, event.cpu_cost, event.mem_cost),
                    }
                    open.push(callee);
                }
                EventType::Step => {
                    if open.is_empty() {
                        open.push(wasm_node(event.pc));
                    }
                    charge(&mut open, event.cpu_cost, event.mem_cost);
                }
                EventType::HostCall => {
                    charge(&mut open, event.cpu_cost, event.mem_cost);
                    let mut node = open_node(host_frame(event.pc));
                    node.exclusive_hostcalls = 1;
                    open.push(node);
                }
                // Both return kinds charge the frame that is ending, then hand it to its parent.
                EventType::HostReturn | EventType::Return => {
                    if let Some(mut frame) = open.pop() {
                        charge_to(&mut frame, event.cpu_cost, event.mem_cost);
                        close(frame, &mut open, &mut roots);
                    }
                }
            }
        }

        // A trapped run leaves frames open; fold them into their parents so the partial trace
        // survives instead of being lost with the missing Returns.
        while let Some(frame) = open.pop() {
            close(frame, &mut open, &mut roots);
        }

        let mut tree = fold_roots(roots);
        compute_inclusive(&mut tree);
        tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(pc: usize, event_type: EventType, cpu_cost: u64, mem_cost: u64) -> TraceEvent {
        TraceEvent {
            pc,
            event_type,
            cpu_cost,
            mem_cost,
        }
    }

    /// Stage 2 resolves nothing yet, so every frame in these tests is named by its `pc`.
    fn aggregate(events: Vec<TraceEvent>) -> CallStackNode {
        ProfileAggregator::new().aggregate(events, &SourceMapper::unmapped())
    }

    #[test]
    fn a_call_and_return_pair_becomes_one_frame_holding_every_step_cost() {
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 40, 5),
            event(1, EventType::Step, 60, 5),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[1]");
        assert_eq!(tree.exclusive_cpu, 100);
        assert_eq!(tree.inclusive_cpu, 100);
        assert_eq!(tree.exclusive_mem, 10);
        assert!(tree.children.is_empty());
    }

    #[test]
    fn a_nested_call_becomes_a_child_and_the_parent_is_inclusive_of_it() {
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 10, 0),
            event(2, EventType::Call, 0, 0),
            event(2, EventType::Step, 70, 0),
            event(2, EventType::Return, 0, 0),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.exclusive_cpu, 10, "only the parent's own steps");
        assert_eq!(tree.inclusive_cpu, 80, "its steps plus the callee's");
        assert_eq!(tree.children["wasm[2]"].exclusive_cpu, 70);
        assert_eq!(tree.children["wasm[2]"].inclusive_cpu, 70);
    }

    #[test]
    fn the_cost_of_entering_a_call_lands_on_the_caller() {
        // `cpu_cost` is the delta since the previous event, so what a Call records was spent
        // before the callee ran. Charging it to the callee would move cost into the wrong frame.
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 5, 1),
            event(2, EventType::Call, 7, 2),
            event(2, EventType::Return, 0, 0),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(
            tree.exclusive_cpu, 12,
            "the caller's own steps plus the walk into the callee"
        );
        assert_eq!(tree.exclusive_mem, 3);
        assert_eq!(tree.children["wasm[2]"].exclusive_cpu, 0);
    }

    #[test]
    fn the_entry_cost_of_the_first_call_belongs_to_the_frame_it_opens() {
        // The stream's first boundary arrives outside any frame, so this delta has no caller.
        // Dropping it would under-report every traced run by its own entry cost.
        let tree = aggregate(vec![
            event(1, EventType::Call, 5, 1),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[1]");
        assert_eq!(tree.exclusive_cpu, 5);
        assert_eq!(tree.exclusive_mem, 1);
    }

    #[test]
    fn the_same_function_reached_twice_shares_one_frame_with_pooled_cost() {
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(2, EventType::Call, 0, 0),
            event(2, EventType::Step, 30, 0),
            event(2, EventType::Return, 0, 0),
            event(2, EventType::Call, 0, 0),
            event(2, EventType::Step, 45, 0),
            event(2, EventType::Return, 0, 0),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(
            tree.children.len(),
            1,
            "two visits to the same frame must not print as two flamegraph bars"
        );
        assert_eq!(tree.children["wasm[2]"].exclusive_cpu, 75);
        assert_eq!(tree.inclusive_cpu, 75);
    }

    #[test]
    fn a_self_recursive_function_pools_into_one_frame() {
        // Recursion repeats a name down the stack, not just side by side. Folding a frame into a
        // parent of the same name is what flamegraph consumers do with repeated stack frames, and
        // it keeps a deeply recursive contract from producing a tree as deep as its call stack.
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 30, 0),
            event(1, EventType::Return, 0, 0),
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 45, 0),
            event(1, EventType::Return, 0, 0),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[1]");
        assert!(
            tree.children.is_empty(),
            "the recursion folded into its caller"
        );
        assert_eq!(tree.exclusive_cpu, 75);
        assert_eq!(tree.inclusive_cpu, 75);
    }

    #[test]
    fn host_cost_gets_a_frame_of_its_own_beneath_the_caller() {
        // The tracer records zero on HostCall and the whole budget delta on HostReturn, so this
        // pairing is what keeps measured host cost off whichever WASM frame closed last.
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::HostCall, 0, 0),
            event(1, EventType::HostReturn, 1000, 250),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.children["host[1]"].exclusive_cpu, 1000);
        assert_eq!(tree.children["host[1]"].exclusive_mem, 250);
        assert_eq!(
            tree.exclusive_cpu, 0,
            "the contract frame spent nothing itself"
        );
        assert_eq!(tree.inclusive_cpu, 1000);
    }

    #[test]
    fn a_run_that_traps_mid_call_keeps_its_open_frames() {
        // A trap ends the run without the `Return` events, so this is the whole-trace shape of
        // an out-of-fuel contract: the expensive tail must survive.
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 10, 0),
            event(2, EventType::Call, 0, 0),
            event(2, EventType::Step, 70, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[1]");
        assert_eq!(tree.exclusive_cpu, 10);
        assert_eq!(tree.children["wasm[2]"].exclusive_cpu, 70);
        assert_eq!(
            tree.inclusive_cpu, 80,
            "no cost was lost with the missing returns"
        );
    }

    #[test]
    fn steps_arriving_before_any_boundary_still_get_a_frame() {
        // A trace taken partway through a run starts mid-function. The cost is the point of the
        // exercise, so it opens a frame from the event's own pc rather than being dropped.
        let tree = aggregate(vec![
            event(7, EventType::Step, 20, 3),
            event(7, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[7]");
        assert_eq!(tree.exclusive_cpu, 20);
        assert_eq!(tree.exclusive_mem, 3);
    }

    #[test]
    fn an_unmatched_return_and_an_empty_trace_both_yield_an_empty_root() {
        // A `Return` whose `Call` was already drained by a previous fold has no frame to close.
        for events in [vec![event(1, EventType::Return, 99, 99)], Vec::new()] {
            let tree = aggregate(events);

            assert_eq!(tree.frame.function_name, "unsymbolized");
            assert_eq!(tree.exclusive_cpu, 0);
            assert_eq!(tree.inclusive_cpu, 0);
            assert!(tree.children.is_empty());
        }
    }

    #[test]
    fn boundaries_that_share_a_pc_fold_into_one_frame_without_losing_cost() {
        // This is what today's engine actually delivers: every event carries `pc = 0`, so nested
        // calls are indistinguishable. The depth is wrong until Phase 2, the total is not.
        let tree = aggregate(vec![
            event(0, EventType::Call, 0, 0),
            event(0, EventType::Step, 10, 0),
            event(0, EventType::Call, 0, 0),
            event(0, EventType::Step, 5, 0),
            event(0, EventType::Return, 0, 0),
            event(0, EventType::Return, 0, 0),
        ]);

        assert_eq!(tree.frame.function_name, "wasm[0]");
        assert!(tree.children.is_empty());
        assert_eq!(tree.exclusive_cpu, 15);
    }

    /// Assert the inclusive/exclusive relationship at every node, returning the subtree total so
    /// the sum can be checked against what the parent recorded.
    fn inclusive_total(node: &CallStackNode) -> (u64, u64) {
        let mut cpu = node.exclusive_cpu;
        let mut mem = node.exclusive_mem;

        for child in node.children.values() {
            let (child_cpu, child_mem) = inclusive_total(child);
            cpu += child_cpu;
            mem += child_mem;
        }

        assert_eq!(
            node.inclusive_cpu, cpu,
            "{} must carry its own cost plus its children's",
            node.frame.function_name
        );
        assert_eq!(node.inclusive_mem, mem, "{}", node.frame.function_name);
        (node.inclusive_cpu, node.inclusive_mem)
    }

    #[test]
    fn every_frame_satisfies_inclusive_equalling_exclusive_plus_children() {
        // `formatter` prints exclusive cost while callers read inclusive totals; a tree where a
        // merged or repeated frame double-counted would silently agree with itself.
        let tree = aggregate(vec![
            event(1, EventType::Call, 0, 0),
            event(1, EventType::Step, 10, 2),
            event(2, EventType::Call, 0, 0),
            event(2, EventType::Step, 5, 1),
            event(3, EventType::Call, 0, 0),
            event(3, EventType::Step, 7, 0),
            event(3, EventType::Return, 0, 0),
            event(2, EventType::Return, 0, 0),
            event(1, EventType::HostCall, 0, 0),
            event(1, EventType::HostReturn, 100, 20),
            event(1, EventType::Return, 0, 0),
        ]);

        assert_eq!(inclusive_total(&tree), (122, 23));
        assert_eq!(tree.exclusive_cpu, 10);
        assert_eq!(tree.children["wasm[2]"].inclusive_cpu, 12);
        assert_eq!(tree.children["host[1]"].inclusive_cpu, 100);
    }

    #[test]
    fn folding_the_same_trace_twice_gives_the_same_tree() {
        let nested = || {
            vec![
                event(1, EventType::Call, 0, 0),
                event(1, EventType::Step, 40, 0),
                event(1, EventType::Return, 0, 0),
            ]
        };

        let mapper = SourceMapper::unmapped();
        let mut aggregator = ProfileAggregator::new();
        let first = aggregator.aggregate(nested(), &mapper);
        let second = aggregator.aggregate(nested(), &mapper);

        assert_eq!(first, second, "the fold must retain nothing between runs");

        // A run that costs nothing reports nothing rather than the previous run's cost.
        let empty = aggregator.aggregate(Vec::new(), &mapper);
        assert_eq!(empty.exclusive_cpu, 0);
    }
}
