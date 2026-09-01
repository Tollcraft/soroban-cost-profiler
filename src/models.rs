use std::collections::HashMap;

pub enum EventType {
    Call,
    Return,
    Step,
}

pub struct TraceEvent {
    pub pc: usize, // WASM Program Counter (Instruction Pointer)
    pub event_type: EventType,
    pub cpu_cost: u64, // CPU instructions consumed since last event
    pub mem_cost: u64, // Memory allocated since last event
}

pub struct SourceFrame {
    pub function_name: String,
    pub file_path: Option<String>,
    pub line_number: Option<u32>,
}

pub struct CallStackNode {
    pub frame: SourceFrame,
    pub exclusive_cpu: u64, // CPU cost of this function itself
    pub inclusive_cpu: u64, // CPU cost of this function + all its children
    pub exclusive_mem: u64, // Mem cost of this function itself
    pub inclusive_mem: u64, // Mem cost of this function + all its children
    pub children: HashMap<String, CallStackNode>,
}
