# Phase 3: DWARF Source Mapping - Issue Bank

### Issue 0: [Phase 3] SPIKE: Investigate WASM `name` section fallback & `wasm-opt` behavior
* **Tags:** `research`, `architecture`
* **Context:** Developers often strip DWARF sections. We need to know if the lightweight WASM `name` section survives standard `stellar contract build` pipelines, and if downstream `wasm-opt` strips it regardless of `Cargo.toml` settings.
* **Simplified Task:** Build a standard Soroban contract with `debug = "line-tables-only"` using the official CLI. Analyze the `.wasm` file using `wasm-objdump` to see what sections survive. Determine if we can rely on the `name` section as a graceful fallback when DWARF is missing.
* **Why it's independent:** Pure research task before building the DWARF mapper.
* **Acceptance Criteria:** A written summary on the issue detailing which debug sections survive standard builds and how to extract the `name` section if DWARF fails.

### Issue 1: [Phase 3] Add `addr2line` and `gimli` dependencies
* **Tags:** `chore`, `good first issue`
* **Context:** We need external crates to parse the DWARF debug information embedded in the WASM.
* **Simplified Task:** Add `addr2line` and `gimli` to `Cargo.toml`.
* **Why it's independent:** Pure dependency management.
* **Acceptance Criteria:** Dependencies are added and `cargo check` passes.

### Issue 2: [Phase 3] Scaffold the `SourceMapper` state
* **Tags:** `feat`, `good first issue`
* **Context:** The mapper needs to hold the loaded DWARF context in memory.
* **Simplified Task:** Update `SourceMapper` in `src/source_map.rs` to hold an `addr2line::Context` instance. 
* **Why it's independent:** Basic struct definition.
* **Acceptance Criteria:** The struct holds the context and compiles.

### Issue 3: [Phase 3] Implement `load_dwarf_sections`
* **Tags:** `feat`
* **Context:** The DWARF sections (`.debug_line`, `.debug_info`) must be parsed from the raw WASM bytes.
* **Simplified Task:** Write a function that takes `&[u8]` (the WASM bytes), extracts the DWARF sections, and initializes the `addr2line::Context`.
* **Why it's independent:** Isolated to DWARF parsing initialization.
* **Acceptance Criteria:** The function returns a valid `SourceMapper` or an error if DWARF is missing.

### Issue 4: [Phase 3] Implement address resolution signature
* **Tags:** `feat`, `good first issue`
* **Context:** We need a method to translate a WASM Program Counter (PC) to a source frame.
* **Simplified Task:** Create the `pub fn resolve(&self, pc: usize) -> Option<SourceFrame>` method signature. Return `None` for now.
* **Why it's independent:** Sets up the API for the actual mapping logic.
* **Acceptance Criteria:** The method compiles and can be called.

### Issue 5: [Phase 3] Map WASM PC to File Path
* **Tags:** `feat`
* **Context:** The first step of resolution is finding the file where the code lives.
* **Simplified Task:** Update `resolve(pc)` to query the `addr2line` context and extract the file path, storing it in `SourceFrame`.
* **Why it's independent:** Focuses only on extracting the file path string.
* **Acceptance Criteria:** The `SourceFrame` contains the correct file path.

### Issue 6: [Phase 3] Map WASM PC to Line Number
* **Tags:** `feat`
* **Context:** The second step is finding the exact line number.
* **Simplified Task:** Update `resolve(pc)` to extract the line number from the DWARF frame.
* **Why it's independent:** Focuses only on extracting the line number integer.
* **Acceptance Criteria:** The `SourceFrame` contains the correct line number.

### Issue 7: [Phase 3] Map WASM PC to Function Name
* **Tags:** `feat`
* **Context:** The final step is finding the name of the function.
* **Simplified Task:** Update `resolve(pc)` to extract and demangle the Rust function name from the DWARF frame.
* **Why it's independent:** Focuses only on extracting the function name string.
* **Acceptance Criteria:** The `SourceFrame` contains the demangled function name (e.g., `my_contract::swap`).

### Issue 8: [Phase 3] Handle missing DWARF gracefully
* **Tags:** `feat`, `bug`
* **Context:** If a user compiles without debug info, the profiler will panic if not handled.
* **Simplified Task:** Ensure `load_dwarf_sections` returns a highly descriptive, user-friendly error if the `.debug_info` section is missing.
* **Why it's independent:** Error handling logic.
* **Acceptance Criteria:** The error message specifically tells the user to check their compilation flags.

### Issue 9: [Phase 3] Write dummy unit test for `SourceMapper`
* **Tags:** `test`, `good first issue`
* **Context:** We need to ensure the mapper doesn't panic on random inputs.
* **Simplified Task:** Write a unit test that passes an empty byte array to `SourceMapper::new` and verifies it returns the expected error.
* **Why it's independent:** Isolated testing.
* **Acceptance Criteria:** The test passes.

### Issue 10: [Phase 3] Document `SourceMapper` with inline rustdocs
* **Tags:** `docs`, `good first issue`
* **Context:** The mapping logic is opaque; we need good code-level documentation.
* **Simplified Task:** Add `///` comments to the `SourceMapper` struct and the `resolve` method explaining the input (PC) and output (SourceFrame).
* **Why it's independent:** Pure documentation task.
* **Acceptance Criteria:** `cargo doc` generates the correct method documentation.

### Issue 11: [Phase 3] Create internal `dwarf_mapping.md` doc
* **Tags:** `docs`
* **Context:** DWARF parsing is notoriously complex. We need an internal guide for contributors.
* **Simplified Task:** Create `docs/internals/dwarf_mapping.md` explaining how `addr2line` translates WASM addresses to Rust source code.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** The markdown file exists and explains the DWARF resolution process.


### Issue 22: [Phase 3] Translate `wasmi` PC to static WASM module offset
* **Tags:** `feat`, `architecture`, `bug`
* **Context:** DWARF line tables use code-section-relative addresses, not the raw program counters that `wasmi` produces at runtime.
* **Simplified Task:** Add a translation step in the resolver that maps the dynamic `wasmi` PC back to the static WASM module offset before querying `addr2line`.
* **Why it's independent:** Core correctness fix required before any DWARF mapping works reliably.
* **Acceptance Criteria:** A test confirms a raw `wasmi` PC is correctly offset-adjusted before DWARF resolution.

### Issue 23: [Phase 3] Implement Rust symbol demangling
* **Tags:** `feat`, `good first issue`
* **Simplified Task:** Add `rustc-demangle` to clean up function names in `SourceFrame`.
* **Acceptance Criteria:** Mangled symbol resolves to a clean `my_contract::swap`-style name.

### Issue 24: [Phase 3] Clean up closure and anonymous-type frame names
* **Tags:** `feat`
* **Simplified Task:** Post-process demangled names to collapse nested `{{closure}}` segments.
* **Acceptance Criteria:** Nested closure symbol renders in a readable format.

### Issue 25: [Phase 3] Resolve inlined frames via `addr2line`'s inline iterator
* **Tags:** `feat`, `architecture`
* **Simplified Task:** Update `resolve(pc)` to return `Vec<SourceFrame>` using `addr2line::Context::find_frames`.
* **Acceptance Criteria:** Inlined call site returns multiple frames in order.

### Issue 26: [Phase 3] Implement WASM `name` section fallback parser
* **Tags:** `feat`
* **Simplified Task:** Implement `resolve_from_name_section(pc)` returning function-name-only frames.
* **Acceptance Criteria:** Resolves function names on stripped WASM binaries.

### Issue 27: [Phase 3] Add PC-resolution caching
* **Tags:** `feat`, `test`
* **Simplified Task:** Add an LRU or HashMap cache inside `SourceMapper::resolve`.
* **Acceptance Criteria:** Repeated lookups are significantly faster.

### Issue 28: [Phase 3] Support multiple DWARF contexts keyed by `contract_id`
* **Tags:** `feat`, `architecture`
* **Simplified Task:** Dispatch `resolve(contract_id, pc)` to the correct contract's mapped DWARF table.
* **Acceptance Criteria:** Resolves correctly across multi-contract traces.

### Issue 29: [Phase 3] Integration test: fixture with `line-tables-only` resolves correctly
* **Tags:** `test`
* **Simplified Task:** Write a test asserting a known function's PC resolves to the correct line in the fixture.
* **Acceptance Criteria:** Passes against real compiled fixture.

### Issue 30: [Phase 3] Benchmark address-resolution throughput
* **Tags:** `test`, `chore`
* **Simplified Task:** Add a benchmark for PC resolution (cached vs uncached).
* **Acceptance Criteria:** Reports resolutions/sec.

### Issue 31: [Phase 3] Detect and warn on heavily mangled line mappings
* **Tags:** `feat`
* **Simplified Task:** Compute ratio of PCs mapping to duplicate/`None` lines; emit warning if high.
* **Acceptance Criteria:** Triggers warning on artificially degenerate mappings.

### Issue 32: [Phase 3] Document the fallback resolution order
* **Tags:** `docs`
* **Simplified Task:** Update docs with DWARF -> name section -> function index precedence.
* **Acceptance Criteria:** Fallback chain is clearly diagrammed.

