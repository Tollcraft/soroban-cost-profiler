# Phase 5: CLI & Edge Cases - Issue Bank

### Issue 1: [Phase 5] Add `clap` dependency
* **Tags:** `chore`, `good first issue`
* **Context:** We need a robust CLI argument parser.
* **Simplified Task:** Add `clap` with the `derive` feature to `Cargo.toml`.
* **Why it's independent:** Pure dependency management.
* **Acceptance Criteria:** Dependency is added and compiles.

### Issue 2: [Phase 5] Define CLI arguments struct
* **Tags:** `feat`, `good first issue`
* **Context:** The profiler needs to accept inputs like the WASM path, output file path, and a tunable sampling rate.
* **Simplified Task:** Create a `Cli` struct in `src/main.rs` using `clap::Parser`. Include `--wasm`, `--output`, `--fn` (to specify the target function to invoke), and a `--sample-rate` flag (default: 1000).
* **Why it's independent:** Isolated struct definition.
* **Acceptance Criteria:** Running `cargo run -- --help` prints all arguments correctly.

### Issue 3: [Phase 5] Wire up the main pipeline sequence
* **Tags:** `feat`
* **Context:** The `main` function needs to call the modules in the correct order.
* **Simplified Task:** Update `src/main.rs` to sequentially instantiate the tracer, load the DWARF mapping, aggregate the events, and format the output.
* **Why it's independent:** Simple function orchestration.
* **Acceptance Criteria:** The CLI calls the dummy functions without panicking.

### Issue 4: [Phase 5] Implement 100M instruction ceiling
* **Tags:** `feat`, `security`, `bug`
* **Context:** Infinite loops in contracts will cause the tracer to run out of memory.
* **Simplified Task:** Update the `ExecutionTracer` to halt tracing and throw an error if the total instruction count exceeds 100,000,000.
* **Why it's independent:** Isolated boundary check logic inside the tracer.
* **Acceptance Criteria:** A unit test proves the tracer halts at the ceiling.

### Issue 5: [Phase 5] Implement panic flush handler
* **Tags:** `feat`, `bug`
* **Context:** If a contract panics, `wasmi` will abort, but we still want the flamegraph up to that point.
* **Simplified Task:** Wrap the `wasmi` execution in a match statement. If it returns an `Error` (panic), call `flush_trace()` and continue to aggregation instead of crashing the CLI.
* **Why it's independent:** Isolated error handling in the execution flow.
* **Acceptance Criteria:** The CLI gracefully outputs a partial stack trace when a contract panics.

### Issue 6: [Phase 5] Output to file instead of stdout
* **Tags:** `feat`
* **Context:** The collapsed stack string can be massive and shouldn't just dump to the terminal.
* **Simplified Task:** Write the generated string from `OutputFormatter` to the file specified by the `--output` CLI flag using `std::fs::write`.
* **Why it's independent:** Pure File I/O.
* **Acceptance Criteria:** Running the CLI successfully writes a `.folded` file to disk.

### Issue 7: [Phase 5] Update README with usage examples
* **Tags:** `docs`, `good first issue`
* **Context:** Users need to know how to use the finished CLI.
* **Simplified Task:** Update the "Usage" section of `README.md` with examples of running `soroban-cost-profiler --wasm my_contract.wasm --output out.folded`.
* **Why it's independent:** Pure documentation update.
* **Acceptance Criteria:** README is updated.

### Issue 8: [Phase 5] Write the `--help` string documentation
* **Tags:** `docs`, `good first issue`
* **Context:** The CLI tool needs user-friendly help text when running `--help`.
* **Simplified Task:** Add `#[clap(about = "...")]` and `#[clap(help = "...")]` attributes to the `Cli` struct in `src/main.rs`.
* **Why it's independent:** Just writing user-facing text strings.
* **Acceptance Criteria:** The CLI prints a beautifully formatted, helpful explanation of the tool.

### Issue 9: [Phase 5] Create a troubleshooting guide
* **Tags:** `docs`
* **Context:** Users will inevitably hit panics or OOM ceilings. They need a guide to resolve them.
* **Simplified Task:** Create `docs/troubleshooting.md` explaining why the "100M instruction limit reached" or "Missing DWARF data" errors occur and how to fix them.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** Guide exists with solutions for common edge cases.

### Issue 10: [Phase 5] Write an End-to-End Tutorial
* **Tags:** `docs`
* **Context:** Users need a step-by-step guide to actually view their flamegraphs.
* **Simplified Task:** Create `docs/tutorial.md` walking a user through compiling a contract, running the profiler, and dropping the `.folded` file into Speedscope.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** A complete, easy-to-follow tutorial exists.


### Issue 24: [Phase 5] Wire `--fn` to actually invoke the target contract function
* **Tags:** `feat`
* **Simplified Task:** Parse `--fn` and use it to invoke the matching export.
* **Acceptance Criteria:** CLI successfully executes the named function.

### Issue 25: [Phase 5] Add `--metric cpu|memory|hostcalls` CLI flag
* **Tags:** `feat`
* **Simplified Task:** Add `--metric` flag to `Cli` and thread into the formatter.
* **Acceptance Criteria:** Produces `.folded` files based on selected dimension.

### Issue 26: [Phase 5] Print a colorized top-N hottest-functions summary
* **Tags:** `feat`
* **Simplified Task:** Print top 5 functions by exclusive cost to stdout.
* **Acceptance Criteria:** Terminal shows readable ranked list.

### Issue 27: [Phase 5] Validate numeric flags
* **Tags:** `feat`, `bug`, `good first issue`
* **Simplified Task:** Reject non-positive `--sample-rate` values.
* **Acceptance Criteria:** Flag 0 returns descriptive error.

### Issue 28: [Phase 5] Standardize CLI exit codes
* **Tags:** `feat`, `chore`
* **Simplified Task:** Define exit code conventions (0: success, 1: input error, 2: internal error).
* **Acceptance Criteria:** Different errors return distinct codes.

### Issue 29: [Phase 5] End-to-end CLI integration test against the fixture
* **Tags:** `test`
* **Simplified Task:** Run built CLI binary against `fixtures/dummy-contract`.
* **Acceptance Criteria:** Produces valid `.folded` file natively.

### Issue 30: [Phase 5] Add `--version` and richer `--help` metadata
* **Tags:** `feat`, `good first issue`
* **Simplified Task:** Add clap command metadata from `Cargo.toml`.
* **Acceptance Criteria:** `--version` works.

### Issue 31: [Phase 5] Warn when input WASM lacks debug info
* **Tags:** `feat`
* **Simplified Task:** Detect degraded SourceMapper state and print a CLI warning.
* **Acceptance Criteria:** Stripped WASM triggers profile warning.

### Issue 32: [Phase 5] Create `CONTRIBUTING.md`
* **Tags:** `docs`, `good first issue`
* **Simplified Task:** Write `CONTRIBUTING.md` covering workflow guidelines.
* **Acceptance Criteria:** Document covers setup and PR expectations.

### Issue 33: [Phase 5] Benchmark end-to-end profiler overhead vs. raw execution
* **Tags:** `test`, `chore`
* **Simplified Task:** Compare wall-clock time of CLI vs raw `cargo test` execution.
* **Acceptance Criteria:** Reports a concrete multiplier overhead.

### Issue 34: [Phase 5] Add a "Limitations" section to the README
* **Tags:** `docs`, `good first issue`
* **Simplified Task:** Summarize host-function and DWARF mapping risks in README.
* **Acceptance Criteria:** Sets expectations about known tool ceilings.

### Issue 35: [Phase 5] Support `--compare` for diffing two profile runs
* **Tags:** `feat`, `architecture`
* **Context:** Optimization Engineers need to prove a code change actually reduced cost, but the tool only profiles single runs.
* **Simplified Task:** Add a `soroban-cost-profiler compare base.folded new.folded` subcommand that parses two outputs and prints the cost delta.
* **Why it's independent:** Entirely new CLI mode for the Auditor/Optimization persona.
* **Acceptance Criteria:** CLI outputs a readable diff of function cost changes.

