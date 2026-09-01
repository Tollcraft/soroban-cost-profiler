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
