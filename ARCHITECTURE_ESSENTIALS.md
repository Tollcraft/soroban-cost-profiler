# Architecture Essentials

**Goal:** A quick-reference outline of critical architectural decisions for the `soroban-cost-profiler`.

## 1. Core Tech Stack
* **Language:** Rust
* **Execution & Tracing:** `wasmi` (engine) + `soroban-env-host`
* **Source Mapping:** `gimli` / `addr2line` (parsing DWARF `.debug_info` & `.debug_line`)
* **Visualization:** `inferno` (SVG flamegraph generation)

## 2. The Profiling Pipeline
1. **Tracer:** Intercepts `wasmi` execution to emit `TraceEvent`s at every instruction or function boundary.
2. **Aggregator:** Accumulates events into a logical call stack tree, computing inclusive/exclusive costs.
3. **Source Mapper:** Translates raw WASM program counters (PCs) in the tree to human-readable Rust source lines using DWARF.
4. **Formatter:** Serializes the resolved tree into a collapsed stack format and renders an SVG via `inferno`.

## 3. Key Data Models
* **`TraceEvent`**: Raw data from the engine. `(pc, cost_delta, event_type)`
* **`CallStackNode`**: Aggregated tree node. `(inclusive_cost, exclusive_cost, children_nodes)`
* **`SourceFrame`**: Resolved code location. `(function_name, file_path, line_number)`

## 4. Critical Constraints & Decisions
* **Zero-Instrumentation:** The profiler must work on standard compiled WASM (with debug info). The user must *not* need to add tracing macros to their smart contract code.
* **Cost Metric:** Primary metric is Soroban CPU instructions, but the architecture must support memory allocations as a secondary dimension.
* **Deterministic Output:** Tracing must be fully deterministic based on the WASM execution rules of the Soroban environment.
* **Standard Formats:** Output must be compatible with standard folded-stack formats to allow users to use custom renderers (e.g., speedscope) if they prefer them over the built-in SVG generator.

## 5. Hard Questions (What Could Break?)
* **DWARF vs. Optimizations:** Soroban contracts must be profiled in `--release` to get accurate costs, but release optimizations (inlining/LTO) destroy source mapping. The flamegraph might point to inaccurate or "unknown" lines.
* **Host Function Blindspots:** Most costs occur inside native Host Functions (like crypto hashing). The WASM tracer cannot see inside these native calls, resulting in massive opaque blocks in the flamegraph.
* **Tracing Overhead (OOM):** Emitting an event for every WASM instruction can generate millions of events per second. If not aggregated synchronously on-the-fly, the profiler will run out of memory.
* **Upstream Breakage:** Hooking into `wasmi` or `soroban-env-host` internals means any major upstream engine update by Stellar will break the profiler.

## 6. Edge Cases & Blind Spots
* **Cross-Contract Calls:** If Contract A calls Contract B, the execution context switches to a new WASM binary. The source mapper must dynamically switch DWARF tables, or else it will map Contract B's instructions to random lines in Contract A.
* **Panics & Abrupt Halts:** If the contract panics mid-execution, the profiler must gracefully flush and render the incomplete tree rather than losing the entire trace.
* **Infinite Loops:** Unbounded loops will generate infinite trace events and OOM the profiler. We must enforce a hard instruction ceiling (e.g., network max) to halt and flush.

## 7. What is Overengineered? (MVP Cuts)
* **Embedded SVG Rendering:** Compiling `inferno` into our CLI is unnecessary bloat. v1 should just output a `.folded` text file for users to drop into `speedscope.app`.
* **Streaming Aggregation vs Sampled Emission:** Streaming every event into the aggregator on-the-fly would destroy performance via 100M hash-map lookups. Conversely, buffering 100M 32-byte events would consume ~3.2GB of RAM and OOM CI runners. **The V1 Cut:** We use Boundary/Sampled tracing. We only emit events on `Call`/`Return`, and accumulate instruction costs locally, flushing a `Step` event only at a configurable sampling interval (e.g., every 1,000 instructions). The hard 100M instruction ceiling is retained to halt infinite loops.
* **Custom DWARF Parsing:** Manually parsing DWARF sections is prone to edge cases. We should lean on the high-level `addr2line` crate instead of writing custom `gimli` logic.
