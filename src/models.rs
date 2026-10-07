use std::collections::HashMap;

/// Represents the kind of WASM execution event intercepted by the tracer.
///
/// * `Call`: Indicates that execution has crossed a function boundary into a new frame.
/// * `Return`: Indicates that the current function frame has exited.
/// * `Step`: Indicates that a WASM instruction (or sequence of instructions) was executed.
/// * `HostCall`: A call from WASM to a host-provided function (e.g., Soroban environment).
/// * `HostReturn`: Return from a host-provided function back to WASM.
#[derive(Debug, Clone, PartialEq)]
pub enum EventType {
    Call,
    Return,
    Step,
    HostCall,
    HostReturn,
}

/// A snapshot of execution state emitted by the `ExecutionTracer`.
///
/// This model captures the program counter and associated costs for a specific `EventType`.
/// `cpu_cost` and `mem_cost` represent the delta (accumulated cost) since the last event
/// was emitted.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceEvent {
    /// Where in the wasm this event happened, in the address space `addr2line` indexes: an offset
    /// into the code section's payload, where address `0` is the function-count byte. Not a file
    /// offset and not a linear-memory address — [`crate::source_map::CodeMap`] translates those.
    /// `wasmi` 2.0 gives its call hook no instruction pointer, so every event the tracer records is
    /// `0`, which is a real address that belongs to no instruction.
    pub pc: usize,
    pub event_type: EventType,
    pub cpu_cost: u64, // CPU cost consumed since last event
    pub mem_cost: u64, // Memory allocated since last event
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceFrame {
    /// What ran, as `addr2line` demangled it with rustc's anonymous closure segments rewritten to
    /// `[closure]`/`[closure#N]` (see `source_map::collapse_closures`). This is the key
    /// `CallStackNode::children` pools frames by, so two names that differ only in `{closure}`
    /// noise would split one function's cost across two frames.
    pub function_name: String,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallStackNode {
    pub frame: SourceFrame,
    pub exclusive_cpu: u64, // CPU cost of this function itself
    pub inclusive_cpu: u64, // CPU cost of this function + all its children
    pub exclusive_mem: u64, // Mem cost of this function itself
    pub inclusive_mem: u64,
    pub exclusive_hostcalls: u64,
    pub inclusive_hostcalls: u64, // Mem cost of this function + all its children
    pub children: HashMap<String, CallStackNode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equality() {
        let event1 = TraceEvent {
            pc: 10,
            event_type: EventType::Call,
            cpu_cost: 100,
            mem_cost: 200,
        };
        let event2 = event1.clone();
        assert_eq!(event1, event2);
    }

    /// The five boundary kinds the tracer distinguishes, listed once so the tests below
    /// cover all of them.
    const ALL_EVENT_TYPES: [EventType; 5] = [
        EventType::Call,
        EventType::Return,
        EventType::Step,
        EventType::HostCall,
        EventType::HostReturn,
    ];

    #[test]
    fn every_event_type_is_a_distinct_value() {
        // The aggregator will select on this enum, so two variants comparing equal would
        // merge, say, a Call with a Step and corrupt the tree built from them.
        for (index, left) in ALL_EVENT_TYPES.iter().enumerate() {
            for right in &ALL_EVENT_TYPES[index + 1..] {
                assert_ne!(left, right, "{left:?} and {right:?} must not compare equal");
            }
        }
    }

    #[test]
    fn event_types_survive_a_clone() {
        for kind in &ALL_EVENT_TYPES {
            assert_eq!(
                *kind,
                Clone::clone(kind),
                "cloning {kind:?} changed the variant"
            );
        }
    }

    #[test]
    fn trace_event_equality_compares_every_field() {
        let base = TraceEvent {
            pc: 10,
            event_type: EventType::Call,
            cpu_cost: 100,
            mem_cost: 200,
        };

        let mut other = base.clone();
        other.pc = 11;
        assert_ne!(base, other, "pc is part of an event's identity");

        let mut other = base.clone();
        other.event_type = EventType::Step;
        assert_ne!(
            base, other,
            "the boundary kind distinguishes otherwise identical events"
        );

        let mut other = base.clone();
        other.cpu_cost = 101;
        assert_ne!(base, other, "a different CPU delta is a different event");

        let mut other = base.clone();
        other.mem_cost = 201;
        assert_ne!(base, other, "a different memory delta is a different event");
    }

    #[test]
    fn a_zero_cost_is_representable() {
        // Costs are deltas, so "this step cost nothing" is a real statement the tracer emits
        // and must be able to distinguish from a step that cost something.
        let free = TraceEvent {
            pc: 1,
            event_type: EventType::Step,
            cpu_cost: 0,
            mem_cost: 0,
        };

        assert_eq!(free.cpu_cost, 0);
        assert_ne!(
            free,
            TraceEvent {
                cpu_cost: 1,
                ..free.clone()
            }
        );
    }

    #[test]
    fn source_frame_location_fields_are_independent() {
        let resolved = SourceFrame {
            function_name: "compute_heavy_loop".to_string(),
            file_path: Some("src/lib.rs".to_string()),
            line_number: Some(12),
        };

        // Phase 3 fills these in from different DWARF sections, so each `None` has to be a
        // state of its own rather than collapsing into "unresolved".
        assert_ne!(
            resolved,
            SourceFrame {
                file_path: None,
                ..resolved.clone()
            }
        );
        assert_ne!(
            resolved,
            SourceFrame {
                line_number: None,
                ..resolved.clone()
            }
        );
        assert_eq!(
            SourceFrame {
                function_name: "main".to_string(),
                file_path: None,
                line_number: None,
            },
            SourceFrame {
                function_name: "main".to_string(),
                file_path: None,
                line_number: None,
            },
            "two unresolved frames of the same function should be equal"
        );
    }

    /// A node with `cost` exclusive CPU (mirrored into inclusive) and the given children,
    /// keyed by their function names the way the aggregator keys them.
    fn node(name: &str, cost: u64, children: Vec<CallStackNode>) -> CallStackNode {
        CallStackNode {
            frame: SourceFrame {
                function_name: name.to_string(),
                file_path: None,
                line_number: None,
            },
            exclusive_cpu: cost,
            inclusive_cpu: cost,
            exclusive_mem: 0,
            inclusive_mem: 0,
            exclusive_hostcalls: 0,
            inclusive_hostcalls: 0,
            children: children
                .into_iter()
                .map(|child| (child.frame.function_name.clone(), child))
                .collect(),
        }
    }

    #[test]
    fn exclusive_and_inclusive_costs_are_separate_fields() {
        // `formatter` emits the exclusive cost of a stack while callers read the inclusive
        // subtree total; a single field serving both purposes would silently agree with
        // itself no matter how wrong aggregation went.
        let caller = CallStackNode {
            inclusive_cpu: 100,
            ..node("main", 10, Vec::new())
        };

        assert_ne!(caller, node("main", 10, Vec::new()));
        assert_eq!(caller.exclusive_cpu, 10);
        assert_eq!(caller.inclusive_cpu, 100);
    }

    #[test]
    fn children_are_keyed_by_function_name() {
        let child = node("callee", 5, Vec::new());
        let left = node("main", 1, vec![child.clone()]);

        assert_eq!(left.children.len(), 1);
        assert!(left.children.contains_key("callee"));

        // The same subtree filed under a different key is a different tree, because the key
        // is what the consumer walks.
        let mut right = node("main", 1, Vec::new());
        let mut renamed = child;
        renamed.frame.function_name = "renamed".to_string();
        right.children.insert("renamed".to_string(), renamed);

        assert_ne!(left, right, "a child's key is part of the tree's identity");
    }

    #[test]
    fn cloning_a_nested_tree_deep_copies_it() {
        // Aggregation clones subtrees while it folds the event stream; a `Clone` that shared
        // structure would leak cost from one run into another.
        let original = node("main", 10, vec![node("callee", 5, Vec::new())]);
        let mut copy = original.clone();

        copy.children.get_mut("callee").unwrap().exclusive_cpu = 999;

        assert_eq!(original.children["callee"].exclusive_cpu, 5);
        assert_ne!(original, copy);
    }
}


#[derive(clap::ValueEnum, Clone, Debug, PartialEq)]
pub enum Metric {
    Cpu,
    Memory,
    Hostcalls,
}
