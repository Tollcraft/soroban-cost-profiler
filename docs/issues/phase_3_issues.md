# Phase 3: DWARF Source Mapping - Issue Bank

### Issue 1: [Phase 3] Add `addr2line` and `gimli` dependencies
* **Context:** We need external crates to parse the DWARF debug information embedded in the WASM.
* **Simplified Task:** Add `addr2line` and `gimli` to `Cargo.toml`.
* **Why it's independent:** Pure dependency management.
* **Acceptance Criteria:** Dependencies are added and `cargo check` passes.

### Issue 2: [Phase 3] Scaffold the `SourceMapper` state
* **Context:** The mapper needs to hold the loaded DWARF context in memory.
* **Simplified Task:** Update `SourceMapper` in `src/source_map.rs` to hold an `addr2line::Context` instance. 
* **Why it's independent:** Basic struct definition.
* **Acceptance Criteria:** The struct holds the context and compiles.

### Issue 3: [Phase 3] Implement `load_dwarf_sections`
* **Context:** The DWARF sections (`.debug_line`, `.debug_info`) must be parsed from the raw WASM bytes.
* **Simplified Task:** Write a function that takes `&[u8]` (the WASM bytes), extracts the DWARF sections, and initializes the `addr2line::Context`.
* **Why it's independent:** Isolated to DWARF parsing initialization.
* **Acceptance Criteria:** The function returns a valid `SourceMapper` or an error if DWARF is missing.

### Issue 4: [Phase 3] Implement address resolution signature
* **Context:** We need a method to translate a WASM Program Counter (PC) to a source frame.
* **Simplified Task:** Create the `pub fn resolve(&self, pc: usize) -> Option<SourceFrame>` method signature. Return `None` for now.
* **Why it's independent:** Sets up the API for the actual mapping logic.
* **Acceptance Criteria:** The method compiles and can be called.

### Issue 5: [Phase 3] Map WASM PC to File Path
* **Context:** The first step of resolution is finding the file where the code lives.
* **Simplified Task:** Update `resolve(pc)` to query the `addr2line` context and extract the file path, storing it in `SourceFrame`.
* **Why it's independent:** Focuses only on extracting the file path string.
* **Acceptance Criteria:** The `SourceFrame` contains the correct file path.

### Issue 6: [Phase 3] Map WASM PC to Line Number
* **Context:** The second step is finding the exact line number.
* **Simplified Task:** Update `resolve(pc)` to extract the line number from the DWARF frame.
* **Why it's independent:** Focuses only on extracting the line number integer.
* **Acceptance Criteria:** The `SourceFrame` contains the correct line number.

### Issue 7: [Phase 3] Map WASM PC to Function Name
* **Context:** The final step is finding the name of the function.
* **Simplified Task:** Update `resolve(pc)` to extract and demangle the Rust function name from the DWARF frame.
* **Why it's independent:** Focuses only on extracting the function name string.
* **Acceptance Criteria:** The `SourceFrame` contains the demangled function name (e.g., `my_contract::swap`).

### Issue 8: [Phase 3] Handle missing DWARF gracefully
* **Context:** If a user compiles without debug info, the profiler will panic if not handled.
* **Simplified Task:** Ensure `load_dwarf_sections` returns a highly descriptive, user-friendly error if the `.debug_info` section is missing.
* **Why it's independent:** Error handling logic.
* **Acceptance Criteria:** The error message specifically tells the user to check their compilation flags.

### Issue 9: [Phase 3] Write dummy unit test for `SourceMapper`
* **Context:** We need to ensure the mapper doesn't panic on random inputs.
* **Simplified Task:** Write a unit test that passes an empty byte array to `SourceMapper::new` and verifies it returns the expected error.
* **Why it's independent:** Isolated testing.
* **Acceptance Criteria:** The test passes.
