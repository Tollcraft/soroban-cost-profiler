# Phase 5: CLI & Edge Cases - Issue Bank

### Issue 1: [Phase 5] Add `clap` dependency
* **Context:** We need a robust CLI argument parser.
* **Simplified Task:** Add `clap` with the `derive` feature to `Cargo.toml`.
* **Why it's independent:** Pure dependency management.
* **Acceptance Criteria:** Dependency is added and compiles.

### Issue 2: [Phase 5] Define CLI arguments struct
* **Context:** The profiler needs to accept inputs like the WASM path and the output file path.
* **Simplified Task:** Create a `Cli` struct in `src/main.rs` using `clap::Parser`. Include `--wasm` and `--output` flags.
* **Why it's independent:** Isolated struct definition.
* **Acceptance Criteria:** Running `cargo run -- --help` prints the arguments correctly.

### Issue 3: [Phase 5] Wire up the main pipeline sequence
* **Context:** The `main` function needs to call the modules in the correct order.
* **Simplified Task:** Update `src/main.rs` to sequentially instantiate the tracer, load the DWARF mapping, aggregate the events, and format the output.
* **Why it's independent:** Simple function orchestration.
* **Acceptance Criteria:** The CLI calls the dummy functions without panicking.

### Issue 4: [Phase 5] Implement 100M instruction ceiling
* **Context:** Infinite loops in contracts will cause the tracer to run out of memory.
* **Simplified Task:** Update the `ExecutionTracer` to halt tracing and throw an error if the total instruction count exceeds 100,000,000.
* **Why it's independent:** Isolated boundary check logic inside the tracer.
* **Acceptance Criteria:** A unit test proves the tracer halts at the ceiling.

### Issue 5: [Phase 5] Implement panic flush handler
* **Context:** If a contract panics, `wasmi` will abort, but we still want the flamegraph up to that point.
* **Simplified Task:** Wrap the `wasmi` execution in a match statement. If it returns an `Error` (panic), call `flush_trace()` and continue to aggregation instead of crashing the CLI.
* **Why it's independent:** Isolated error handling in the execution flow.
* **Acceptance Criteria:** The CLI gracefully outputs a partial stack trace when a contract panics.

### Issue 6: [Phase 5] Output to file instead of stdout
* **Context:** The collapsed stack string can be massive and shouldn't just dump to the terminal.
* **Simplified Task:** Write the generated string from `OutputFormatter` to the file specified by the `--output` CLI flag using `std::fs::write`.
* **Why it's independent:** Pure File I/O.
* **Acceptance Criteria:** Running the CLI successfully writes a `.folded` file to disk.

### Issue 7: [Phase 5] Update README with usage examples
* **Context:** Users need to know how to use the finished CLI.
* **Simplified Task:** Update the "Usage" section of `README.md` with examples of running `soroban-cost-profiler --wasm my_contract.wasm --output out.folded`.
* **Why it's independent:** Pure documentation update.
* **Acceptance Criteria:** README is updated.
