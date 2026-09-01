# Phase 2: Execution Tracing - Issue Bank

### Issue 0: [Phase 2] SPIKE: Investigate `soroban-env-host` Budget API limitations
* **Tags:** `research`, `architecture`
* **Context:** Most budget is spent in native Host Functions. The public `Budget::get_cost_tracker()` API only gives cumulative per-`CostType` totals, not per-call-site attribution. Internal hooks (`invocation_metering`) exist but are unstable.
* **Simplified Task:** Time-box an investigation (e.g., 2-4 hours). Determine if there is *any* way to intercept per-call-site host costs without forking `rs-soroban-env`. If we must fall back to cumulative totals, document the exact public API methods we will use.
* **Why it's independent:** Pure research task that gates further host-metering implementation.
* **Acceptance Criteria:** A written summary on the issue detailing whether call-site attribution is possible via public APIs, or if we must scope down to cumulative totals for V1.

### Issue 1: [Phase 2] Scaffold the `fixtures/dummy-contract` library
* **Tags:** `chore`, `good first issue`
* **Context:** Before we can test our profiler, we need a compiled Soroban WASM binary to profile.
* **Simplified Task:** Create a standard `no_std` Rust library inside `fixtures/dummy-contract`. 
* **Why it's independent:** It happens entirely inside the `fixtures/` directory and doesn't touch the Rust profiler codebase at all.
* **Acceptance Criteria:** The `fixtures/dummy-contract` folder exists with a basic `Cargo.toml` and `src/lib.rs`.

### Issue 2: [Phase 2] Add `soroban-sdk` to the dummy contract
* **Tags:** `chore`, `good first issue`
* **Context:** The dummy contract needs the Soroban SDK to compile correctly into a valid smart contract.
* **Simplified Task:** Update the fixture's `Cargo.toml` with the `soroban-sdk` dependency and set the release profile to `opt-level = "z"`.
* **Why it's independent:** Purely dependency management for the fixture.
* **Acceptance Criteria:** `Cargo.toml` contains the SDK and release profile.

### Issue 3: [Phase 2] Write a `compute_heavy_loop` test function
* **Tags:** `test`, `good first issue`
* **Context:** We need a function in our dummy contract that uses a lot of CPU instructions so we can measure it.
* **Simplified Task:** Write a smart contract function inside the fixture that executes a large mathematical loop (e.g., adding numbers 10,000 times).
* **Why it's independent:** Writing basic Rust logic without worrying about the profiler.
* **Acceptance Criteria:** The function exists and is exported correctly via the Soroban SDK.

### Issue 4: [Phase 2] Write a `memory_heavy_loop` test function
* **Tags:** `test`, `good first issue`
* **Context:** We need a function that allocates memory to test memory profiling later.
* **Simplified Task:** Write a contract function that dynamically allocates vectors in a loop.
* **Why it's independent:** Another basic Rust logic function isolated to the fixture.
* **Acceptance Criteria:** The function exists and allocates data on the heap using the SDK `Vec`.

### Issue 5: [Phase 2] Create a compile script
* **Tags:** `chore`, `good first issue`
* **Context:** Contributors shouldn't have to remember the exact cargo flags to build a Soroban contract.
* **Simplified Task:** Write a `build.sh` script inside `fixtures/` that runs `cargo build --target wasm32-unknown-unknown --release` on the dummy contract.
* **Why it's independent:** It's just a shell script helper.
* **Acceptance Criteria:** Running `sh build.sh` successfully produces a `.wasm` file.

### Issue 6: [Phase 2] Derive standard traits for Data Models
* **Tags:** `feat`, `good first issue`
* **Context:** We need to be able to clone, debug, and compare our trace events in unit tests.
* **Simplified Task:** Add `#[derive(Debug, Clone, PartialEq)]` to the structs in `src/models.rs`. Write a quick unit test proving equality works.
* **Why it's independent:** Strictly touching struct definitions.
* **Acceptance Criteria:** The traits are derived and `cargo test` passes.

### Issue 7: [Phase 2] Scaffold the `ExecutionTracer` state
* **Tags:** `feat`, `good first issue`
* **Context:** When the WASM engine runs, it will fire millions of events. We need a simple, pre-allocated state machine to store these.
* **Simplified Task:** Add an `events: Vec<TraceEvent>` field to the `ExecutionTracer` struct in `src/tracer.rs` and initialize it in `new()`.
* **Why it's independent:** Basic Rust struct initialization.
* **Acceptance Criteria:** The struct holds the vector and compiles.

### Issue 8: [Phase 2] Implement `record_step` with sampling
* **Tags:** `feat`, `test`
* **Context:** Emitting a `Step` event for all 100M instructions would consume 3.2GB of RAM. Instead, we accumulate cost locally and sample periodically.
* **Simplified Task:** Add `current_step_cost: u64` and `sample_rate: u64` fields to `ExecutionTracer`. In `record_step(&mut self, pc: usize, cpu_cost: u64)`, increment `current_step_cost`. Only push a `Step` event to the vector (and reset the counter) if `current_step_cost >= sample_rate`. Write a unit test proving events are only emitted at the threshold.
* **Why it's independent:** Simple state and math manipulation.
* **Acceptance Criteria:** The method accumulates cost and only pushes an event when the sample rate is hit.

### Issue 9: [Phase 2] Implement `record_call` & `record_return`
* **Tags:** `feat`, `test`
* **Context:** The tracer needs to know when functions start and end to build a call stack later.
* **Simplified Task:** Add `record_call` and `record_return` methods to `ExecutionTracer` that push `Call` and `Return` events. Write unit tests.
* **Why it's independent:** Isolated to the tracer's internal state.
* **Acceptance Criteria:** Methods are implemented and tested.

### Issue 10: [Phase 2] Implement `flush_trace`
* **Tags:** `feat`, `test`
* **Context:** If a contract panics, we need to extract the events immediately and clear the buffer.
* **Simplified Task:** Add a `flush_trace(&mut self) -> Vec<TraceEvent>` method that returns the buffered events and empties the internal vector. Write a test.
* **Why it's independent:** Isolated buffer management.
* **Acceptance Criteria:** The buffer is cleared and returned correctly.

### Issue 11: [Phase 2] Add WASM execution dependencies
* **Tags:** `chore`, `good first issue`
* **Context:** We need the core libraries that actually run WebAssembly.
* **Simplified Task:** Add `wasmi` and `soroban-env-host` to the main `Cargo.toml`.
* **Why it's independent:** Pure dependency management.
* **Acceptance Criteria:** `Cargo.toml` is updated and `cargo check` passes.

### Issue 12: [Phase 2] Write a WASM file loader utility
* **Tags:** `feat`, `test`, `good first issue`
* **Context:** We have to load the `.wasm` file from disk into a byte array before executing.
* **Simplified Task:** Write a `pub fn load_wasm_bytes(path: &str) -> std::io::Result<Vec<u8>>` function in `src/tracer.rs` with a unit test.
* **Why it's independent:** Pure File I/O.
* **Acceptance Criteria:** Can successfully read a dummy file from disk in a test.

### Issue 13: [Phase 2] Scaffold the `wasmi` Engine initialization boilerplate
* **Tags:** `feat`
* **Context:** We need to initialize the WASM interpreter environment before running code.
* **Simplified Task:** Create a function `pub fn setup_engine() -> wasmi::Engine` in `src/tracer.rs`. Read the `wasmi` docs and instantiate a default Engine config.
* **Why it's independent:** The contributor doesn't need to hook up contract execution yet; just the engine setup.
* **Acceptance Criteria:** The function returns a valid `wasmi::Engine` instance.

### Issue 14: [Phase 2] Enable instruction metering
* **Tags:** `feat`, `good first issue`
* **Context:** To profile costs, the engine must count instructions.
* **Simplified Task:** Update the `setup_engine()` function to explicitly enable gas/instruction metering in the `wasmi::Config`.
* **Why it's independent:** Isolated configuration toggle.
* **Acceptance Criteria:** The returned engine has metering enabled.

### Issue 15: [Phase 2] Parse a WASM Module
* **Tags:** `feat`
* **Context:** The raw bytes need to be compiled into a module the engine understands.
* **Simplified Task:** Write a function `parse_module(engine: &wasmi::Engine, bytes: &[u8]) -> Result<wasmi::Module, ...>` in `src/tracer.rs`.
* **Why it's independent:** Follows standard `wasmi` documentation.
* **Acceptance Criteria:** Returns a valid module instance.

### Issue 16: [Phase 2] Scaffold the Soroban Host
* **Tags:** `feat`
* **Context:** Soroban contracts require a specific host environment to handle native functions (like crypto).
* **Simplified Task:** Write a dummy function that instantiates a basic `soroban_env_host::Host` object.
* **Why it's independent:** Isolated setup of the Soroban SDK host environment.
* **Acceptance Criteria:** Returns a valid `Host` instance.

### Issue 17: [Phase 2] Document the `TraceEvent` data models
* **Tags:** `docs`, `good first issue`
* **Context:** Future contributors need to understand exactly what data is captured during a trace.
* **Simplified Task:** Add detailed inline Rustdocs (`///`) to the `TraceEvent`, `EventType`, and `CallStackNode` structs in `src/models.rs`.
* **Why it's independent:** Purely adding comments to existing structs.
* **Acceptance Criteria:** `cargo doc` generates clean, readable documentation for the models.

### Issue 18: [Phase 2] Create internal `tracer_architecture.md` doc
* **Context:** Hooking into a WASM engine is complex and needs a dedicated explanation.
* **Simplified Task:** Create a `docs/internals/tracer_architecture.md` file explaining how `wasmi` intercepts instructions and how our buffer handles it.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** The markdown file exists and explains the tracer mechanics.


### Issue 21: [Phase 2] Scaffold the Mock Host Environment
* **Tags:** `feat`, `architecture`
* **Context:** We cannot link or instantiate the Soroban WASM module without the imported host functions (log, storage, crypto) actually existing in our mock `soroban_env_host::Host`.
* **Simplified Task:** Scaffold a complete mock `Host` object that wires up the necessary import stubs so the WASM engine has something to link against.
* **Why it's independent:** Foundational dependency for module instantiation.
* **Acceptance Criteria:** A mock `Host` can be created and passed to the WASM engine without linking errors for standard SDK imports.

### Issue 22: [Phase 2] Implement `instantiate_module` linking Soroban host imports
* **Tags:** `feat`
* **Context:** A parsed `wasmi::Module` isn't runnable until its imports are linked to concrete implementations.
* **Simplified Task:** Write `instantiate_module` that links the module's imports against the scaffolded mock host (Issue 21).
* **Why it's independent:** Builds directly on the mock host scaffolding.
* **Acceptance Criteria:** A dummy contract module instantiates successfully.

### Issue 23: [Phase 2] Implement `invoke_function` to call a named contract export
* **Tags:** `feat`
* **Simplified Task:** Write `invoke_function(instance, store, fn_name, args) -> Result<...>` that looks up the export and calls it.
* **Why it's independent:** Pure `wasmi` invocation logic.
* **Acceptance Criteria:** Calling `compute_heavy_loop` on the fixture executes and returns successfully.

### Issue 24: [Phase 2] Track `mem_cost` during tracing
* **Tags:** `feat`, `test`
* **Simplified Task:** Extend `record_step`/`record_call` to accumulate a `mem_cost` delta (bytes allocated).
* **Acceptance Criteria:** A unit test proves `mem_cost` accumulates independently of `cpu_cost`.

### Issue 25: [Phase 2] Distinguish Host Function boundaries from WASM call boundaries
* **Tags:** `feat`, `architecture`
* **Simplified Task:** Add a `HostCall`/`HostReturn` variant to `EventType` and emit it when a WASM import trampoline is entered/exited.
* **Acceptance Criteria:** A test shows a `HostCall` event is emitted on a mock log function.

### Issue 26: [Phase 2] Wire `Budget::get_cost_tracker()` snapshots into host-boundary events
* **Tags:** `feat`
* **Simplified Task:** At each `HostReturn` event, snapshot the diff in `get_cost_tracker()` totals since the last snapshot.
* **Acceptance Criteria:** Diffed cost values are attached to `HostReturn` events.

### Issue 27: [Phase 2] Build a minimal mock ledger/env setup helper
* **Tags:** `feat`, `test`
* **Simplified Task:** Write a `setup_mock_env()` helper in test-support that configures a minimal ledger snapshot.
* **Acceptance Criteria:** Produces a `Host` that can successfully invoke fixture functions.

### Issue 28: [Phase 2] Enforce an instruction ceiling inside the tracer's hot loop
* **Tags:** `feat`, `security`
* **Simplified Task:** Add a `max_instructions: u64` field to `ExecutionTracer`; in `record_step`, halt once exceeded.
* **Acceptance Criteria:** Tracer halts once the configured ceiling is exceeded.

### Issue 29: [Phase 2] Integration test: fixture compile → tracer → raw event stream
* **Tags:** `test`
* **Simplified Task:** Write an integration test running `fixtures/dummy-contract` through the full tracer pipeline.
* **Acceptance Criteria:** Test runs end-to-end and produces trace events without panicking.

### Issue 30: [Phase 2] Benchmark tracer overhead
* **Tags:** `test`, `chore`
* **Simplified Task:** Add a `criterion` benchmark measuring events/sec throughput of `record_step`.
* **Acceptance Criteria:** Running `cargo bench` produces throughput numbers.

### Issue 31: [Phase 2] Stress-test recursive function calls
* **Tags:** `test`
* **Simplified Task:** Add `recursive_fibonacci` to dummy contract and assert stack bookkeeping.
* **Acceptance Criteria:** Test passes for depth 20 without stack corruption.

### Issue 32: [Phase 2] Reject malformed / non-Soroban WASM binaries gracefully
* **Tags:** `feat`, `bug`
* **Simplified Task:** Add validation returning descriptive errors for invalid WASM.
* **Acceptance Criteria:** Feeding a garbage byte array returns `Result::Err`.

### Issue 33: [Phase 2] Document Host vs. WASM call-boundary tracing
* **Tags:** `docs`
* **Simplified Task:** Create `docs/internals/host_function_boundary.md` explaining the `HostCall` limits.
* **Acceptance Criteria:** Doc explains why host costs are opaque blocks.

### Issue 34: [Phase 2] Add internal `tracing` crate logging
* **Tags:** `chore`, `good first issue`
* **Simplified Task:** Instrument key tracer methods with `debug!`/`warn!` spans.
* **Acceptance Criteria:** `RUST_LOG=debug` shows tracer logs.

### Issue 35: [Phase 2] CI job to build the WASM fixture as a cached artifact
* **Tags:** `chore`
* **Simplified Task:** Add a GitHub Actions job that runs `fixtures/build.sh` and caches it.
* **Acceptance Criteria:** Workflow produces a downloadable `.wasm` artifact.

