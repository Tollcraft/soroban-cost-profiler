# Architecture & Technical Design

## 1. Tech Stack

* **Language:** Rust (to integrate natively with the Soroban SDK and Tollcraft ecosystem).
* **WASM Execution / Tracing:** 
  * `soroban-env-host` (to provide the standard Soroban host functions).
  * `wasmi` (the WASM interpreter used by Soroban; we will need to inject an execution tracer/metering hook here to count instructions).
* **Source Mapping:**
  * `gimli` or `addr2line`: To parse DWARF debug data (`.debug_line`, `.debug_info`) from the WASM binary and translate raw WASM Program Counters (PC) to Rust source file and line numbers.
* **Flamegraph Generation:**
  * `inferno`: A Rust port of Brendan Gregg's FlameGraph tools. Used to fold stacks and generate SVG flamegraphs directly from the CLI.

---

## 2. System Architecture

The profiler is designed as a pipeline that takes a WASM binary and a test scenario, executes it while tracing, translates the raw trace, and renders the output.

### 2.1. Components

1. **CLI Frontend (`src/main.rs`)**
   * Parses CLI arguments.
   * Accepts the path to a compiled WASM contract and a mock environment setup / test payload.
   
2. **Execution Tracer (`src/tracer.rs`)**
   * Wraps the `soroban-env-host` execution.
   * Instruments the WASM engine (`wasmi`) to emit a `TraceEvent` for every function call/return and specific instruction blocks, tracking the cumulative CPU/memory cost at each step.
   
3. **Source Mapper (`src/source_map.rs`)**
   * Reads the WASM binary and extracts DWARF debug sections.
   * Converts a WASM instruction pointer (PC) into a human-readable frame (`Function`, `File`, `Line`).
   
4. **Profile Aggregator (`src/aggregator.rs`)**
   * Consumes the stream of `TraceEvent`s.
   * Maintains a logical call stack during execution.
   * Aggregates inclusive and exclusive costs for every node in the call tree.
   
5. **Output Formatter (`src/formatter.rs`)**
   * Walks the aggregated call tree and formats it into a "collapsed stack" string.
   * Passes the collapsed stack to `inferno` to generate the final `profile.svg`.

---

## 3. Data Models

### 3.1. `TraceEvent`
Emitted by the Execution Tracer during the WASM run.
```rust
pub enum EventType {
    Call,
    Return,
    Step,
}

pub struct TraceEvent {
    pub pc: usize,              // WASM Program Counter (Instruction Pointer)
    pub event_type: EventType,  
    pub cpu_cost: u64,          // CPU instructions consumed since last event
    pub mem_cost: u64,          // Memory allocated since last event
}
```

### 3.2. `SourceFrame`
The resolved human-readable location of a WASM instruction.
```rust
pub struct SourceFrame {
    pub function_name: String, // e.g., "my_contract::swap"
    pub file_path: String,     // e.g., "src/lib.rs"
    pub line_number: u32,      // e.g., 42
}
```

### 3.3. `CallStackNode`
Used by the Aggregator to build the final execution tree.
```rust
pub struct CallStackNode {
    pub frame: SourceFrame,
    pub exclusive_cpu: u64,    // CPU cost of this function itself
    pub inclusive_cpu: u64,    // CPU cost of this function + all its children
    pub exclusive_mem: u64,    // Mem cost of this function itself
    pub inclusive_mem: u64,    // Mem cost of this function + all its children
    pub children: HashMap<String, CallStackNode>,
}
```

---

## 4. Control Flow (The Pipeline)

1. **Initialize:** The user runs `cargo run -p soroban-cost-profiler -- --wasm ./contract.wasm`.
2. **Setup:** The tool loads the WASM and parses the DWARF sections using `gimli`.
3. **Execute:** The tool instantiates the Soroban Host with a tracing-enabled WASM engine.
4. **Trace:** As the contract runs, `TraceEvent`s are pushed to the Aggregator.
5. **Aggregate:** 
   * On `Call`, the Aggregator pushes a new frame to its internal stack.
   * On `Step`, it adds `cpu_cost` to the current frame's `exclusive_cpu`.
   * On `Return`, it pops the frame, adding its totals to its parent's `inclusive_cpu`.
6. **Resolve:** The Aggregator resolves all recorded WASM PCs to `SourceFrame`s using the Source Mapper.
7. **Render:** The Aggregator produces folded stacks (e.g., `main;swap;calculate_fee 5000`) and passes them to `inferno` to create `flamegraph.svg`.
