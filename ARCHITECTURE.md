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

---

## 5. Hard Questions & Known Risks (What Could Break?)

When building this profiler, these are the critical technical risks most likely to cause failure or severe degradation of the developer experience:

### 1. DWARF Mapping Loss in Release Builds
**The Risk:** To get accurate gas/budget costs in Soroban, contracts *must* be profiled using a highly optimized release build (`opt-level = "z"`, `lto = true`). However, aggressive optimizations (inlining, loop unrolling) destroy source-to-instruction mapping. DWARF data will likely point multiple WASM instructions to the same line, or drop lines entirely.
**The Breakage:** The flamegraph will look heavily mangled, attributing huge costs to random lines or pointing to "unknown location".

### 2. Host Function Attribution Limits (The Black Box)
**The Risk:** Soroban smart contracts spend the vast majority of their budget inside native "Host Functions" (e.g., crypto, storage) which our WASM tracer cannot see. The public `soroban_env_host::budget::Budget` API only exposes `get_cost_tracker()`, which provides *cumulative* totals per `CostType` for the entire run, not per-call-site attribution. 
**The Breakage:** If we cannot achieve per-call-site attribution without relying on internal, non-public hooks (`invocation_metering`), the flamegraph might just show massive opaque blocks for host calls. Relying on those internal hooks risks severe upstream brittleness (Risk #4).

### 3. Memory & Tracing Overhead (OOM)
**The Risk:** Tracing every single WASM instruction emits a massive amount of `TraceEvent`s. A contract that takes 100,000,000 instructions to execute will generate 100,000,000 events.
**The Breakage:** Buffering 100M 32-byte events will consume ~3.2GB of RAM (and up to 6.4GB during `Vec` reallocation), which will crash standard CI runners (OOM) and destroy the developer experience. We must rely on boundary tracking (`Call`/`Return`) and periodically sampled `Step` flushing, rather than storing a strict per-instruction event stream.

### 4. Soroban Environment Instability
**The Risk:** We are hooking deeply into `wasmi` or `soroban-env-host` internals.
**The Breakage:** If Stellar updates the Soroban host (e.g., swapping `wasmi` for `wasmtime`, altering the cost model logic, or changing the host function dispatch ABI), our internal tracing hooks will break completely and require a rewrite.

### 5. The Stripped WASM Precondition
**The Risk:** To map line numbers without manual instrumentation, we rely on DWARF info. But Soroban's standard workflow strips debug info to minimize on-chain binary size. If `stellar contract build` runs downstream `wasm-opt` passes, it may strip DWARF regardless of Cargo configuration.
**The Breakage:** If a stripped binary is profiled, the source mapper will gracefully degrade to raw WASM function indices (or the WASM `name` section, if preserved) rather than failing entirely. Developers must manually configure `debug = "line-tables-only"`. 
*CRITICAL:* This precondition is additive to Risk #1. Adding `line-tables-only` does *not* prevent LTO and aggressive inlining from mangling the line outputs. It just guarantees the tables exist.

---

## 6. Edge Cases & Blind Spots

### 1. Cross-Contract Calls (Multi-WASM Execution)
If Contract A invokes Contract B, the execution engine switches to a completely different WASM binary. 
**The Edge Case:** Our source mapper must be aware of the active `contract_id` and load/unload the correct DWARF debug info dynamically. If we just assume all WASM instructions map to Contract A's debug info, the flamegraph for Contract B's execution will map to random, incorrect lines in Contract A's source code.

### 2. Panics and Traps
If a contract panics (e.g., an `unwrap()` fails or it hits an out-of-bounds array access), the WASM engine abruptly halts.
**The Edge Case:** We cannot wait for a clean "shutdown" event to build our tree. The profiler must be capable of flushing the current call stack and rendering the flamegraph up to the exact instruction of the panic, which is critical for debugging *why* a costly contract failed.

### 3. Infinite Loops & Budget Exhaustion
If a contract test enters an infinite loop, a standard test runner will eventually time out. 
**The Edge Case:** The profiler will generate trace events endlessly, leading to an Out-Of-Memory (OOM) crash before the timeout. The tracer must enforce a hard execution limit (e.g., halting gracefully when it hits the Soroban network max of 100,000,000 instructions) and flush the trace.

### 4. Rust Closures and Anonymous Types
**The Edge Case:** Rust heavily utilizes closures, which compile down to deeply nested, anonymous types (e.g., `my_contract::foo::{{closure}}`). DWARF mappers often struggle to present these cleanly, leading to ugly, unreadable frame names in the flamegraph that frustrate developers.

---

## 7. What is Overengineered? (MVP Simplifications)

If we need to cut scope for a v1 MVP, these architectural decisions might be overengineered:

### 1. Embedded SVG Generation (`inferno`)
* **The Overengineering:** Bundling `inferno` to natively render SVG flamegraphs from the CLI adds significant dependency bloat and formatting logic. 
* **The Simplification:** We could just dump a `.folded` stack trace text file and tell developers to drag-and-drop it into `speedscope.app` (or pipe it to the external `flamegraph` CLI tool).

### 2. Streaming Aggregation vs. Boundary/Sampled Emission
* **The Overengineering:** Originally, we considered streaming events directly into the `ProfileAggregator` on-the-fly to avoid Out-Of-Memory (OOM) crashes. However, tree-walking and HashMap lookups on every single instruction (up to 100M times) would create a massive CPU bottleneck on the hottest path, slowing execution to a crawl and destroying the developer experience.
* **The Simplification:** Instead of streaming *or* blindly buffering every single instruction (which would consume ~3.2GB of RAM for 100M 32-byte events and OOM CI runners), we use **Boundary & Sampled Emission**. We strictly emit `Call` and `Return` events at function boundaries. For raw instructions, the hot-path simply increments a local counter and only pushes a `Step` event periodically (e.g., every 1,000 instructions) as a backstop for tight loops. This sampling interval will be tunable via a CLI flag. 
*(Note: A hard 100M instruction ceiling is strictly retained to prevent unbounded time execution during infinite loops).*

### 3. Custom DWARF Parsing
* **The Overengineering:** Using `gimli` to manually crawl the WASM DWARF sections and build a bespoke source-mapper is a massive undertaking with extreme edge cases.
* **The Simplification:** We should rely entirely on the high-level `addr2line` crate (which wraps `gimli`) or even literally shell out to the `llvm-addr2line` CLI tool for v1 to resolve addresses, rather than building a custom parser.
