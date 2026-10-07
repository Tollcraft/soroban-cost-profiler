# 🗺️ Soroban Cost Profiler: Development Roadmap

> **⚠️ CRITICAL RULE:** This document MUST be updated whenever a contribution is made to the repository. If you finish a task, check it off here and update the progress.

## Phase 1: Core Scaffolding & Setup ✅
- [x] Create repository, README, and AGENTS.md instructions.
- [x] Draft PRD and Architecture documents.
- [x] Scaffold initial Rust pipeline modules (`tracer`, `aggregator`, `source_map`, `formatter`).
- [x] Define core data models (`TraceEvent`, `CallStackNode`).
- [x] Setup Tollcraft Org Landing Page.
- [x] Implement interactive demo animations for Linter and Assert tabs.
- [x] Implement Light/Dark mode toggle button in the navbar.

## Phase 2: Execution Tracing (In Progress 🚧)
- [x] **SPIKE:** Investigate `soroban-env-host` Budget API limitations.
- [x] **WASM Engine Setup:** Import `soroban-env-host` and `wasmi` as dependencies.
- [x] **Fixture Compilation:** Add a `fixtures/dummy-contract` Soroban contract with a `compute_heavy_loop` function and workspace integration.
- [x] **Fixture Documentation:** Add README and doc comments to dummy-contract fixture.
- [x] **Fixture Script:** Add `fixtures/build.sh` compile script.
- [x] **Memory Fixture:** Add `memory_heavy_loop` to dummy contract.
- [x] **Tracer State:** Scaffold `ExecutionTracer` state and `TraceEvent` structures.
- [x] **Instruction Metering:** Enable fuel consumption in the `wasmi` engine setup.
- [x] **WASM Parser:** Implement a WASM file loader and `wasmi` module parser.
- [x] **Host Setup:** Scaffold the native `soroban_env_host::Host` proxy for the tracer.
- [x] **Trace Data Models:** Write detailed doc comments for `TraceEvent` structures.
- [x] **Architecture Docs:** Create the internal `tracer_architecture.md` document explaining the sampling mechanics.
- [x] **Mock Host:** Scaffold `MockHost` struct for the environment.
- [x] **Module Instantiation:** Implement `instantiate_module` and link imports.
- [x] **Function Invocation:** Implement `invoke_function` for named exports.
- [x] **Memory Cost:** Track `mem_cost` alongside `cpu_cost`.
- [x] **Host Boundaries:** Distinguish host call boundaries from WASM boundaries.
- [x] **Tracer Hooks:** Implement the `wasmi` execution hooks in `src/tracer.rs` to intercept instructions.
- [x] **Instruction Counting:** Accurately measure and record CPU cost and `pc` at every step.
- [x] **Call/Return Tracking:** Record entry and exit events for the WASM call the profiler
  initiates. Not the whole call tree: `wasmi` 2.0 reports no inner WASM-to-WASM calls, so the
  entry/exit pair is the outer one only (pinned by
  `only_the_outer_invocation_is_recorded_as_a_boundary`, and documented on `invoke_function`).
- [x] **Document Hook Reality:** `invoke_function`'s rustdoc claimed every WASM/host entry and
  exit becomes an event, which contradicted both the engine and our own probes. Rewritten to
  state which boundaries fire, that host boundaries nest correctly while WASM ones do not, and
  why every event is recorded at `pc = 0`.

## Phase 3: DWARF Source Mapping
- [x] **SPIKE: `name` section fallback & `wasm-opt` behavior (#141):** answered with measurements
  in `docs/spikes/02_wasm_name_section_fallback.md`, and the answer is "no, not for deployed
  artifacts". `rust-lld` emits a `name` section unconditionally — it is present in a
  `debug = false` build (98 B, all three probe functions, 0 DWARF bytes) — and binaryen deletes it
  unless `-g` is passed. `stellar contract optimize` never passes `-g` (its recipe is
  `new_optimize_for_size_aggressively()` + `converge` + MVP features + MutableGlobals/SignExt/
  BulkMemory, reproduced on `wasm-opt 133`), so the deployed `.wasm` has neither names nor usable
  DWARF. Worse: its five `.debug_*` sections *survive* (493,883 of 495,198 bytes) and are stale,
  so `SourceMapper::new` succeeds, `has_debug_info()` is `true`, and 0 of 270 probed code offsets
  resolve — the degenerate mapping #162 has to warn about, now measured rather than hypothesized.
  Adding `-g` restores both (1,765 B fixture, 140 of 145 offsets resolve, 139 with a line), which
  makes the user-facing guidance "profile the pre-optimization artifact, or optimize with `-g`".
  For #157 the section is readable with the walk we already do: kind `1` records are
  `funcidx` + length + bytes, `funcidx` spans the whole function index space including the
  contract's 4 imports (18 entries for 4 imports + 14 defined functions), and the names are
  rustc-mangled v0 symbols, so `rustc-demangle` — already enabled for #142 — is mandatory.
  Names are function-level only: no `file:line`, ever.
- [x] **Add Dependencies (#142):** `addr2line 0.25.1` and `gimli 0.32.3` are both direct
  dependencies now, and neither is a new download — both were already in `Cargo.lock` via
  `backtrace`. `addr2line` builds with `default-features = false, features = ["std",
  "rustc-demangle"]`, which keeps `cpp_demangle`, `object` and `memmap2` out of the tree.
  `gimli` has to be named on its own rather than reached through `addr2line::gimli`: addr2line's
  `endian-reader` is what provides `EndianRcSlice` — the owned reader a mapper that outlives the
  caller's buffer needs. The footprint is two small pure-Rust crates entering `Cargo.lock`
  (`stable_deref_trait`, for `Rc<[u8]>: CloneStableDeref`, and `fallible-iterator`); `std` is not
  optional, since without it `EndianRcSlice` does not implement `gimli::Reader` at all.
- [x] **Enable Debug Info for the Fixture (#143 prerequisite):** the root manifest gained
  `[profile.release.package.dummy-contract] debug = "line-tables-only"`, so `fixtures/build.sh`
  now emits `.debug_abbrev`, `.debug_info`, `.debug_line`, `.debug_ranges` and `.debug_str`.
  Measured on the current toolchain: the artifact goes from 3.1 KB to **622,507 bytes**, of which
  ~619 KB are those five sections — a `std`-linked build symbolizes every inlined dependency, not
  just the contract's own functions. That is affordable for a profiling input and not something to
  deploy, which is why the fixture stays out of git (CI's `build-fixture` job builds it) and why
  `fixtures/dwarf_probe/` exists: the same three functions in a `#![no_std]` crate with
  `panic = "abort"` and `debug = 1` are **1,690 bytes with real DWARF**, small enough to commit and
  to `include_bytes!` from a unit test. What survives `wasm-opt` and the `stellar` pipeline is
  #141's question, answered at the top of this phase and in
  `docs/spikes/02_wasm_name_section_fallback.md`.
- [x] **Load DWARF Info (#143, #144, #149, #150):** `SourceMapper::new(&[u8])` now returns
  `Result<Self, SourceMapError>` and holds the built `addr2line::Context`; `unmapped()` is the
  explicit no-symbols path and `has_debug_info()` tells the two apart. The WASM section table is
  walked by hand — section id, payload length, custom-section name — and nothing below that is
  hand-written: the retained bytes go to `gimli::Dwarf::load` and `addr2line::Context::from_dwarf`,
  which is what keeps `AGENTS.md`'s "no custom DWARF parsing" rule satisfiable. Only `name` and
  `.debug_*` are copied out; `producers`, `target_features` and Soroban's `contractspecv0` are
  skipped. Missing `.debug_info` is `MissingDebugInfo`, whose message names
  `debug = "line-tables-only"` and lists the sections that *were* present so a user can see the
  `name` fallback; #157 later made a *readable* `name` section load rather than error, so reaching
  this variant now means neither symbol source is usable. Unreadable DWARF is `UnreadableDwarf` carrying gimli's reason; neither is a
  panic. Covered by nine unit tests plus two doctests against both committed fixtures, and verified
  against the real 622 KB Soroban build.
- [x] **Address Resolution (#145–#148):** `resolve(pc)` now queries the `addr2line` context and returns resolved `SourceFrame`s — the file path (#146), the line number (#147) and the demangled function name (#148) all come from one `find_frames` lookup, whose first frame at a leaf address names the inlined function rather than the enclosing one. It resolves against the committed fixture: 160 of its 166 code-section addresses yield a frame, and `14` yields `<u64>::wrapping_add` in core rather than the `_RNv…` symbol DWARF stores, which is `rustc-demangle` doing #148's job. A name is what makes a frame (`CallStackNode`'s children are keyed by it); the location fields stay independent, because address `2` is a prologue with a name and no location at all and `61`–`71` and `75`–`89` have a file with no line. `usize::MAX` is answered with an empty stack rather than a panic — `addr2line` probes `[address, address + 1)` and overflows on it.
  **This is the mapper half only.** End-to-end attribution still waits on an address: `wasmi` 2.0 gives a call hook no program counter and has no instruction hook, so every event the tracer records is at `pc = 0` (see `invoke_function`'s docs and `only_the_outer_invocation_is_recorded_as_a_boundary`), and even a real engine offset would need translation into code-section-relative form, which is the address space these line tables are written against — #153 built that translation and measured that the engine retains nothing to feed it. Five unit tests plus one doctest cover the new behavior; #156 turned this into a `Vec<SourceFrame>` inline stack and #157 added the `name`-section fallback beneath it, while #158 (caching, which would make this `&mut self`) remains open.
- [x] **Address Translation (#153):** `CodeMap` is the bridge between a position in the `.wasm` file and the space `addr2line` indexes, built in the same section walk that finds the DWARF. The base is **measured against both committed fixtures, not copied from a spec reading**: DWARF address `0` is the code section's function-count byte, so in `dwarf_probe.wasm` (payload at file offset `111`) the three function bodies are code-relative `2..16`, `18..157` and `158..165` — and framing them from the wasm and resolving them from DWARF name `caller_of_heavy`, `memory_heavy_loop` and `compute_heavy_loop` respectively. An off-by-one base would put those addresses on a size prefix and resolve to nothing, which is why the check is cross-source rather than arithmetic. `SourceMapper::resolve_file_offset` composes the translation with the lookup; `function_at` separates "inside the section, belongs to no instruction" (`0`, `1`, `16`, `17`) from "not in the section at all", the distinction #162's warning needs. Building the map is best effort: a function list that overruns its section yields `code_map() == None` and still loads, and the walk is bounded by the section's bytes rather than by its declared count, so a 12-byte module cannot be made to allocate for 2^64-1 functions (`AGENTS.md`'s OOM rule, applied to reading a binary). **The engine half is not implementable and now says why:** `wasmi` 2.0's only execution hook, `Store::call_hook`, receives the hook variant and nothing else — no callee, no offset — and the engine re-encodes wasm bytecode into its own instruction stream during translation while retaining no table back to the original offsets, so a runtime instruction position is not merely hidden, it does not exist. The finest granularity reachable is a function body, and `CodeMap::bodies` is what a per-call hook would need. Seven unit tests plus two doctest examples cover it; `invoke_function`'s docs and `docs/internals/dwarf_mapping.md` record the engine's limits.
- [x] **Source Mapping Documentation (#151, #152):** `docs/internals/dwarf_mapping.md` is the contributor guide to Stage 2 — why DWARF in a `.wasm` file is just a pile of custom sections and where the hand-written parsing stops (`WasmSections::parse` walks the section table; `gimli::Dwarf::load` and `addr2line::Context::from_dwarf` do the rest, which is what keeps `AGENTS.md`'s "no custom DWARF parsing" satisfiable); that the line tables are **code-section-relative**, so a translation bug shows up as an empty flamegraph rather than a failing test; the measured per-address table above, including the two shapes that make `SourceFrame`'s fields what they are (`2` has a name and no location, `61`–`71`/`75`–`89` have a file and no line, and `column` is absent from the struct precisely because address `158` reports `Some(0)`); innermost-frame semantics, which #156 then replaced with the full inline stack; the `{:#}` demangling contract and why a `_R…` reaching a frame is a bug; and how to build a mappable binary, with the `stellar contract optimize` trap #141 measured. It corrects two numbers that had drifted with the rebuilt fixture: the file-without-line addresses (previously quoted from an ad-hoc `/tmp` build) and the fact that every name in the fixture is `#[no_mangle] extern "C"`, so its only mangled symbol comes from inlined core. #151 needed no diff — `SourceMapper` and `resolve` have carried `///` docs since #50 and #166 respectively, and `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` is the check that keeps them honest; the struct's docs now point at this file.
- [x] **Closure Frame Names (#155, with #154's evidence):** `collapse_closures` rewrites rustc's anonymous closure segments on the way into a `SourceFrame`, so `outer::{closure#0}::{closure#1}` renders as `outer::[closure#0]`. The spellings come from measurement rather than from the issue text: a probe crate whose `outer` holds a closure that holds a closure yields `{closure#0}` at `debug = 2`/`opt-level = 1` (each closure gets its own function) and `{closure_env#0}` at `line-tables-only`/`opt-level = 3` (the body inlines away and only the environment type survives, inside another function's generic arguments). `{{closure}}` -- the form #155 names -- appears in neither build, because v0 encodes a closure structurally (`...13closure_probe5outer0E...`) and `rustc-demangle` renders it as `{closure#N}`; it is handled anyway for the legacy symbols #157 will read out of a `name` section. A nested run collapses to one marker because `CallStackNode`'s children are keyed by `function_name` and a stack of markers describes one frame; the index is kept when a closure stands alone because two sibling closures in one function are different work, and flattening them pools their cost with nothing in the trace to explain it. Six unit tests cover it, including the whole measured frame byte-for-byte and a sweep asserting every name the committed fixture yields passes through unchanged. #154 (demangling) needed no diff and is closed with evidence: `rustc-demangle` has been enabled since #142, #148's test pins the v0 -> `<u64>::wrapping_add` pair, and the same sweep over the real 622 KB build shows 23 `DW_LANG_Rust` names with zero `_R` -- rustc writes readable strings into DWARF, so what stays mangled on a real artifact lives in the `name` section, which is #157's deliverable and #163's precedence question. #162's premise narrows the same way: the degenerate case worth warning about is the measured stale-DWARF 0-of-270 build, not mangling.
- [x] **Inline Stack Frames (#156):** `resolve` and `resolve_file_offset` return `Vec<SourceFrame>` — the whole inline stack, innermost frame first, in the order `addr2line` walks it — instead of only the frame at the leaf. The motive is measurement, not aesthetics: sweeping `fixtures/dwarf_probe`'s 166 code-section addresses gives **170 frames over 160 resolved addresses**, because 10 of them are inlined call sites — `14` (`<u64>::wrapping_add` inside `caller_of_heavy`) and `101`..=`109`, nine consecutive bytes of `memory_heavy_loop`'s inlined `wrapping_add` — and the depth distribution is `{0: 6, 1: 150, 2: 10}`. Nothing is deeper than two, which is what a three-line fixture should produce; a third level would mean the walk was reading past the function that owns the address, so the test pins the exact vector rather than a count. Dropping a nameless frame now ends only that frame, not the address: previously the innermost-only rule meant one unnamed inline cost the whole lookup its attribution even when `caller_of_heavy` sat beneath it and had a name. The aggregator deliberately keys each `CallStackNode` on `stack[0]` and does **not** fold inline depth into tree depth — the engine reports one call boundary for the wasm function, so a tree level for an inlined `core` function would invent a boundary the trace never contains, and `inclusive == exclusive + sum(children)` would then describe frames that were never entered. What the stack is *for* is Stage 4: an `.folded` line wants the inline chain as stack levels, and #156 makes that data available without changing what today's output means. Five new tests (three in `src/source_map.rs`, two in `src/aggregator.rs`) plus rewritten doctests cover it, and `docs/internals/dwarf_mapping.md` records the distribution and the aggregator's reasoning.
- [x] **Function-Name Fallback (#157):** `resolve_from_name_section(pc)` reads the wasm `name` section's function-names subsection and answers with one frame — `function_name` set, `file_path` and `line_number` `None` — for any address inside a defined function's body. That is what "resolves function names on stripped WASM binaries" means: the 595-byte `debug = false` fixture now maps all three functions, and `SourceMapper::new` loads it instead of returning `MissingDebugInfo`. `resolve` consults it only when no DWARF is loaded, so the order is DWARF → `name` → the aggregator's `wasm[pc]`, which #163 writes up. The layout came out of #141's spike rather than a spec reading: a chain of `u8 kind` + `uleb128 size` + body with **no leading version byte**, of which only kind `1` is kept, its records being `funcidx` + `len` + UTF-8 bytes. Two measurements decide the design. `funcidx` counts the whole function index space, **imports first**, while `CodeMap::bodies` is the defined-function list — the 622 KB contract imports 4 functions and defines 14, for 18 entries — so the import count is read from the import section in the same walk, and a module whose import list does not parse yields no names at all rather than names aligned on a guess: charging a contract function's cost to a host import's name is a wrong flamegraph where an unnamed one is only an unhelpful one. The stored symbols are also raw (`_RNvMs7_NtCsknUcikIyyBm_4core3numy12wrapping_add`), so the fallback runs the same `rustc-demangle` + `collapse_closures` pipeline #148 and #155 put behind DWARF names. Both fixtures carry a kind-`7` subsection (`__stack_pointer`, a global) that is skipped rather than rejected, because unknown kinds belong to proposals this stage never reads. The strongest check is `dwarf_and_names_agree_on_the_function_that_owns_every_address`: the two committed fixtures are the same three functions built twice, so the index table and gimli's line tables are independent readers — and across all 166 code-section addresses the outermost DWARF frame and the name-section entry name the same function, including the ten inlined call sites where DWARF gives two frames and `name` gives one. Neither fixture has an import section, which is why the offset is pinned by synthetic modules (two imports, names starting at index `2`) instead of by a fixture. Both declared counts are bounded by the bytes that carry them, so a subsection claiming 2^32 names costs one failed read and no allocation — `AGENTS.md`'s OOM rule applied to reading a binary, as in #153. `MissingDebugInfo` now means neither source is usable, and its message says so when a `name` section was present but unreadable. Ten new unit tests plus the rewritten `new` doctest; `docs/internals/dwarf_mapping.md` gains "How #157 reads it".

## Phase 4: Aggregation & Formatting
- [x] **Tree Building:** Implement `ProfileAggregator` to consume the raw `TraceEvent` stream and build a `CallStackNode` tree (#52). Open frames live on a stack, so accounting allocates per call rather than per instruction (`AGENTS.md`'s OOM constraint); a frame that meets another of the same name pools into it, the way flamegraph consumers collapse repeated stack frames. Trapped runs keep their still-open frames and an unmatched `Return` is ignored, so a partial trace still renders.
- [x] **Cost Math:** Exclusive cost accumulates on the innermost open frame; a `Call`'s delta is charged to its caller (or, for the stream's first boundary, to the frame it opens); host cost lands in its own frame because the tracer records zero on entry and the whole budget delta on return. `inclusive_*` is filled by one post-order pass over the finished tree, so `inclusive == exclusive + sum(children.inclusive)` holds at every node by construction — asserted for every frame in a mixed WASM/host/recursion trace.
- [x] **Formatting:** Implement `OutputFormatter` to serialize the tree into the standard `.folded` collapsed stack format — written with #44/#53 and tested there; what was missing was an input, which Stage 3 now provides, so the call is live in `profile()` rather than commented out.
- [x] **Pipeline Wiring:** `src/main.rs`'s `profile()` now runs all four stages in order instead of commenting the last two out, and `tests/integration.rs` hands a traced run through tracer → aggregator → formatter, reading the result back with `parse_folded`. The tree is a placeholder until Phases 2 and 3 give it real boundaries and names, so the end-to-end output is one unresolved `wasm[0]` frame holding the total.
- [x] **Differential Comparison:** Diff two `.folded` artifacts into `<stack> <baseline> <current>` lines for `flamegraph.pl --diff`, with the red/blue/neutral classification tested in `src/formatter.rs` and `tests/differential.rs` (#44). Rendering stays an external step: `AGENTS.md` cuts SVG/inferno from the MVP.

## Phase 5: CLI & Edge Cases (MVP Completion)
- [ ] **CLI Parsing:** Add `clap` to `src/main.rs` to accept `--wasm`, `--output`, and test arguments.
- [ ] **Panic Handling:** Ensure the aggregator flushes and formats the trace even if the contract panics mid-execution.
- [x] **Infinite Loop Protection:** Enforce a hard ceiling (e.g. 100M instructions) to halt tracing and prevent OOM crashes.
- [ ] **Documentation:** Update README with usage examples and CLI flag details.
- [x] **FAQ:** Design and implement FAQ section for GitHub Pages (`docs/index.html`).

## Tooling & Agent Setup
- [x] Install `agentic-awesome-skills` to `.agents/` for enhanced AI workflows.
- [x] Install official Anthropic skills and plugins from `claude-plugins-official`.
- [x] Generate `INSTALLED_SKILLS.md` catalog detailing all loaded agents and plugins.
- [x] Install `frontend-design` (anthropics/skills) and `design-taste-frontend` (leonxlnx/taste-skill) UI/UX skills for site audits.

## Website Polish (Landing Page Audit) ✅
- [x] **Fix marquee full-width bug:** Missing `</div>` caused `.marq-wrap` to nest inside `.wrap.hero__grid` and render as a 649px grid column instead of a full-width band.
- [x] **Light-theme contrast:** Override `--cyan`/`--magenta`/`--faint`/`--t2` with darker variants; white text on primary CTA; visible ghost-button border; visible `flame--4` bar.
- [x] **Dark-theme contrast:** Deepen `--violet` for primary CTA (AA 4.5:1) and lighten `--faint` for small mono labels.
- [x] **Responsive nav:** Tighten `.nav__links` gap to fix overflow at ~768px; add `scroll-padding-top` for anchored sections.
- [x] **Polish:** Add inline SVG favicon, `color-scheme` for native scrollbars, remove dead CSS (`.term__flame-block`, `.card`), clean duplicate rule, swap visible em-dashes for commas/parens.
- [x] **Social sharing:** Add branded 1200x630 Open Graph image (`docs/og-image.png`) plus `og:image`/`twitter:card` (summary_large_image) meta tags with alt text.
- [x] **Favicon:** Replace the inline SVG favicon with the Tollcraft org profile picture (`docs/favicon.png`, downloaded from GitHub avatars and converted to PNG), plus an `apple-touch-icon` link.

## Phase 6: Code Quality & Refactoring ✅
- [x] Refactor and modularize complex logic in `src/tracer.rs` (#64)
- [x] Review and optimize performance/allocations in `src/formatter.rs` (#63)
- [x] Improve inline documentation and comments in `src/aggregator.rs` (#62)
- [x] Add comprehensive unit tests for `src/lib.rs` (#61)
- [x] Add comprehensive unit tests for `src/formatter.rs` (#53)
- [x] Improve inline documentation and comments in `src/tracer.rs` (#54)
- [x] Add comprehensive unit tests for `fixtures/dummy-contract/src/lib.rs` (#57)
- [x] Improve inline documentation and comments in `src/main.rs` (#58)
- [x] Add comprehensive unit tests for `src/models.rs` (#49) — the derived semantics Phase 4 depends on: distinct `EventType` variants, field-by-field `TraceEvent` equality, `SourceFrame`'s independent `Option` location fields, and `CallStackNode` children keyed by function name.
- [x] Add comprehensive unit tests for `tests/integration.rs` (#45) — the seams around a run: `parse_module`/`load_wasm_file` error paths, fuel metering actually enabled, and the sampling and ceiling knobs.
- [x] Refactor and modularize complex logic in `src/main.rs` (#48) — each pipeline stage now constructed by its own function behind a `profile()` harness, which is also the first test to execute the binary's code at all.
- [x] Improve inline documentation and comments in `src/source_map.rs` (#50) — the stage's contract, what Phase 3 will hold, and the facts checked against real builds (section names, `from_sections` needs no `object`, code-section-relative addresses, the fixture currently shipping no DWARF). Documentation plus two tests pinning that construction tolerates a debug-info-free binary and that an unattributable `pc` yields no frame; `resolve()` itself stays blocked on the engine giving the tracer a real offset.

- [x] Implement and refactor `src/aggregator.rs` (#52) — `aggregate()` was `unimplemented!()`; it now folds the flat event stream into the tree Phase 4 needs, which is also what the two open Phase 4 boxes above describe. 13 unit tests cover cost attribution, nesting, host frames, pooling, trapped runs, and the inclusive invariant.

### Blocked on unimplemented code
The quality-issue bank (#45-#60) was generated per file, but several targets are still
scaffolds, so their ask has nothing to act on yet. Revisit after the phase that
implements the file:

- `src/source_map.rs` (#60 refactor) — a documented 17-line stub until Phase 3 implements
  `resolve()`; #50 documented it rather than restructuring it, because there is no logic yet to
  restructure.
- `src/models.rs` (#59 perf) — derive-only data structures; no loops or clones to remove,
  and the issue forbids changing the public API.
- `src/lib.rs` (#51 perf) — module declarations only.

### Not applicable as written
Two bank items ask for something that would make the code worse, so they are recorded here
rather than "solved" with a cosmetic diff:

- `fixtures/dummy-contract/src/lib.rs` (#47 perf) — the deliberately expensive loops *are*
  the fixture: `compute_heavy_loop` and `memory_heavy_loop` exist to produce measurable cost
  for the profiler. Optimizing them would remove the signal every trace test depends on, and
  #57 pinned their exact arithmetic.
- `tests/integration.rs` (#55 perf) — a test file with no loop, clone, or allocation to
  remove. #45 expanded it; there is nothing for a performance pass to do to it.

## Metering Probes (`tests/meter_probe.rs`)
- [x] Create the probe suite — `tests/meter_probe.rs` did not exist, which is why #46 and #56 had nothing to act on.
- [x] Refactor and modularize complex logic in `tests/meter_probe.rs` (#56) — shared `Probe` harness, one job per test, WASM encoding isolated in `mod probe_module`.
- [x] Improve inline documentation and comments in `tests/meter_probe.rs` (#46) — byte-level WASM annotations and the reason behind every assertion.

### Findings the probes surfaced, both blocking Phase 4
- **Internal WASM calls are not traced.** `wasmi` 2.0's `Store::call_hook` fires only for the
  host-initiated call: `probe()` calling `work()` twice yields one Call/Return pair, not three.
  Phase 4's call tree cannot be rebuilt from boundaries the engine never reports. Pinned by
  `only_the_outer_invocation_is_recorded_as_a_boundary`.
- **The instruction ceiling cannot halt a run.** `invoke_function` discards the `Err` returned by
  `record_step` once the ceiling is passed, so a runaway contract runs to completion. Pinned by
  `the_instruction_ceiling_does_not_stop_execution`.
- **A trapped run keeps its trace.** An out-of-fuel contract still yields the boundaries crossed
  before the trap, which is what Phase 5's panic handling needs.
