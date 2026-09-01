# Phase 2: Execution Tracing - Issue Bank

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

### Issue 8: [Phase 2] Implement `record_step` in `ExecutionTracer`
* **Tags:** `feat`, `test`
* **Context:** The tracer needs to record the cost of individual instructions.
* **Simplified Task:** Add a `record_step(&mut self, pc: usize, cpu_cost: u64)` method to the tracer that pushes a `Step` event to the vector. Write a unit test.
* **Why it's independent:** Simple state manipulation.
* **Acceptance Criteria:** The method works and is covered by a test.

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
