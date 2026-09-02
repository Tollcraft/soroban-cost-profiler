# 🗺️ Soroban Cost Profiler: Development Roadmap

> **⚠️ CRITICAL RULE:** This document MUST be updated whenever a contribution is made to the repository. If you finish a task, check it off here and update the progress.

## Phase 1: Core Scaffolding & Setup ✅
- [x] Create repository, README, and AGENTS.md instructions.
- [x] Draft PRD and Architecture documents.
- [x] Scaffold initial Rust pipeline modules (`tracer`, `aggregator`, `source_map`, `formatter`).
- [x] Define core data models (`TraceEvent`, `CallStackNode`).
- [x] Setup Tollcraft Org Landing Page.

## Phase 2: Execution Tracing (In Progress 🚧)
- [x] **SPIKE:** Investigate `soroban-env-host` Budget API limitations.
- [x] **WASM Engine Setup:** Import `soroban-env-host` and `wasmi` as dependencies.
- [x] **Fixture Compilation:** Add a `fixtures/dummy-contract` Soroban contract with a `compute_heavy_loop` function and workspace integration.
- [ ] **Tracer Hooks:** Implement the `wasmi` execution hooks in `src/tracer.rs` to intercept instructions.
- [ ] **Instruction Counting:** Accurately measure and record CPU cost and `pc` at every step.
- [ ] **Call/Return Tracking:** Record entry and exit events for WASM function calls.

## Phase 3: DWARF Source Mapping
- [ ] **Add Dependencies:** Add `addr2line` and `gimli` for debug info parsing.
- [ ] **Load DWARF Info:** Parse the `.debug_info` and `.debug_line` sections of the loaded WASM binary in `src/source_map.rs`.
- [ ] **Address Resolution:** Implement the `resolve(pc)` function to translate a WASM Program Counter to a Rust `file:line` frame.

## Phase 4: Aggregation & Formatting
- [ ] **Tree Building:** Implement `ProfileAggregator` to consume the raw `TraceEvent` stream and build a `CallStackNode` tree.
- [ ] **Cost Math:** Calculate `inclusive_cpu` and `exclusive_cpu` correctly during aggregation.
- [ ] **Formatting:** Implement `OutputFormatter` to serialize the tree into the standard `.folded` collapsed stack format.

## Phase 5: CLI & Edge Cases (MVP Completion)
- [ ] **CLI Parsing:** Add `clap` to `src/main.rs` to accept `--wasm`, `--output`, and test arguments.
- [ ] **Panic Handling:** Ensure the aggregator flushes and formats the trace even if the contract panics mid-execution.
- [ ] **Infinite Loop Protection:** Enforce a hard ceiling (e.g. 100M instructions) to halt tracing and prevent OOM crashes.
- [ ] **Documentation:** Update README with usage examples and CLI flag details.
