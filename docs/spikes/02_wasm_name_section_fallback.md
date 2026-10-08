# SPIKE 02: Does the WASM `name` Section Survive the Soroban Build Pipeline?

## Objective
Issue #141 asks two questions before Phase 3 builds its mapper:

1. When a contract is built the way users build it, do DWARF debug sections survive — and does
   the lightweight `name` custom section survive alongside them?
2. If DWARF is gone, can `name` serve as a graceful fallback for frame attribution?

The answer changes what `SourceMapper` should read, and what the profiler should tell a user who
hands it an unattributable binary.

## Method
No `wasm-objdump`/`wabt` and no `stellar` CLI on the probe machine, so the section data came from
three sources that agree with each other:

* the repository's own `WasmSections` walker (`src/source_map/wasm.rs`), which already retains `name`
  and `.debug_*`;
* a standalone parser for the `name` subsections and the code-section size;
* binaryen itself — `wasm-opt version 133`, `--print-function-map`, and `--print` for the import
  section — as an independent reader of the same bytes.

Toolchain: `rustc 1.98.1 (48a229cea 2026-09-01)`, `wasm32-unknown-unknown`. Every size and count
below is a measurement from that pair, not a spec expectation.

Resolution numbers are not section arithmetic; they are real `addr2line` lookups. A temporary test
built a `SourceMapper` for each binary and swept **every byte offset of the code section** through
`Context::find_location` and `Context::find_frames(..).skip_all_loads()`, counting offsets that
yielded a file, a line, or a function name.

The `stellar contract build`/`optimize` step was reproduced rather than installed: the CLI's
optimizer recipe (`cmd/soroban-cli/src/commands/contract/optimize.rs` upstream) is

```rust
let mut options = OptimizationOptions::new_optimize_for_size_aggressively();
options.converge = true;
options.mvp_features_only();
options.enable_feature(Feature::MutableGlobals);
options.enable_feature(Feature::SignExt);
options.enable_feature(Feature::BulkMemory);
```

— which is `wasm-opt -Oz --converge --enable-mutable-globals --enable-sign-ext --enable-bulk-memory`
and sets no `debuginfo`/`-g` flag. That invocation produces output of identical size to plain
`-Oz` on the probe fixture (1,417 B), which is the evidence the reproduction is faithful.

Binaries measured: `fixtures/dwarf_probe/*.wasm` (three `#![no_std]` functions), and
`fixtures/dummy-contract` built for `wasm32-unknown-unknown` in release with
`debug = "line-tables-only"` (622 KB), before and after the optimizer recipe.

## Findings

### 1. A `cargo build` artifact carries both, and `name` does not need DWARF to exist

| artifact | total | `.debug_*` | `name` | content of `name` |
| --- | --- | --- | --- | --- |
| `dwarf_probe.wasm` (`debug = 1`, `opt-level = "z"`) | 1,690 B | 5 sections, 1,002 B | 98 B | 3 functions |
| `dwarf_probe_no_debug.wasm` (`debug = false`) | 595 B | none | **98 B** | **the same 3 functions** |
| `dummy_contract.wasm` (`debug = "line-tables-only"`) | 622,507 B | 5 sections, 619,318 B (99.5 % of the file) | 1,600 B | 18 functions |

So `rust-lld` emits a `name` section unconditionally, and it survives a build that emits no DWARF
at all. In the 622 KB contract build the debug sections are effectively the file: its whole
code section is 488 bytes.

### 2. Binaryen deletes `name` unless `-g`, and leaves `.debug_*` bytes behind as dead weight

| step applied to `dwarf_probe.wasm` | total | `.debug_*` | `name` | addresses resolving |
| --- | --- | --- | --- | --- |
| (none — as built) | 1,690 B | 1,002 B | yes | 160 of 166 probed, 133 with a line, 4 distinct names |
| `wasm-opt -O0` | 1,424 B | 855 B | **gone** | **0 of 152 probed** |
| `wasm-opt -Oz` | 1,417 B | 855 B | **gone** | **0 of 145 probed** |
| `wasm-opt -O3` | 1,417 B | 855 B | **gone** | **0 of 145 probed** |
| `wasm-opt -Oz -g` | 1,765 B | 1,098 B | yes | 140 of 145 probed, 139 with a line, 4 distinct names |
| `wasm-opt --strip-debug` / `--strip-dwarf` | 489 B | none | gone | not applicable |

Two things follow, and the first is the one that will surprise a user:

* An optimized binary still **loads**. `SourceMapper::new` finds `.debug_info`, gimli parses it,
  `has_debug_info()` returns `true` — and every lookup returns nothing. The addresses in the line
  table refer to the pre-optimization code section, which the optimizer rewrote; the DWARF is
  present and invalid, not absent. `MissingDebugInfo` does not fire, so nothing warns.
  This is Issue #162's degenerate-mapping case, and it is now measured rather than hypothesized.
* `--strip-debug`/`--strip-dwarf` drop DWARF **and** `name` together (489 B of pure code), so
  neither "strip the debug info" recipe leaves a fallback either.

`-g` is what preserves names ("Emit names section", per `wasm-opt --help`) and, with it, resolution
works again: the same 1,002 → 1,098 B of DWARF that survives is usable.

### 3. `stellar contract optimize` passes no `-g`, so deployed artifacts are unmappable by either source

Applying the CLI recipe:

| artifact | total | `.debug_*` | `name` | addresses resolving |
| --- | --- | --- | --- | --- |
| `dummy_contract.wasm` before | 622,507 B | 619,318 B | 1,600 B / 18 | 311 of 489 probed, 296 with a line, 23 distinct names |
| after the recipe | 495,198 B | 493,883 B | **gone** | **0 of 270 probed** |

**Direct answer to the issue's question: no.** The `name` section does *not* survive the standard
`stellar` pipeline — it is dropped by the optimizer pass, not by `Cargo.toml`, and not by anything
the contract author controls. And the DWARF that does survive is stale in exactly the same run.
There is no configuration of the official build pipeline that leaves `name` intact while removing
DWARF, so `name` cannot be the mechanism that rescues a deployed `.wasm`. It rescues a *pre*-build
artifact compiled without debug info — which is a build nobody in this pipeline produces.

The practical guidance for the README and for the error text is therefore: profile
`target/wasm32-unknown-unknown/release/<contract>.wasm` (or add `-g`/`--debuginfo` to any
`wasm-opt` step you run yourself), never the artifact you deployed.

### 4. How to extract the `name` section when DWARF is unavailable

The section is a custom section named exactly `name` (a 4th entry in the same walk
`WasmSections::parse` already does for `.debug_*`; `retain()` already keeps it). Its payload,
as emitted by `rust-lld`, is a sequence of subsections with **no leading version byte** — the
first byte is subsection kind `0`, not a `0` version followed by kind `0`. It is unambiguous only
by sniffing: parse the payload as a chain of `u8 kind` + `uleb128 size` + body and accept the
framing whose kinds are all known and whose bodies tile the payload exactly to the end. Under the
version-byte reading the same bytes claim a kind-`0x11` subsection, which no reader should obey.

Observed layout of the 98-byte payload of `dwarf_probe_no_debug.wasm`:

```
00 11 10 'd''w''a''r''f''_''p''r''o''b''e''.''w''a''s''m      kind 0: module name, "dwarf_probe.wasm"
01 39 03 00 0f 'caller_of_heavy'
           01 11 'memory_heavy_loop'
           02 12 'compute_heavy_loop'                           kind 1: function names, count 3
07 12 01 00 0f '__stack_pointer'                                kind 7: 1 entry
```

The reader needs kind `1` only: `uleb128 count`, then `count` records of
`uleb128 funcidx` + `uleb128 len` + `len` UTF-8 bytes. Three properties of those records decide
Phase 3's design:

* **`funcidx` indexes the whole function index space, imports included.** The 622 KB contract
  imports 4 functions and defines 14; the `name` section has 18 entries, `0..3` naming the
  imports and `4..17` the defined functions (`4: compute_heavy_loop`,
  `5: memory_heavy_loop`). A `pc` → name table therefore has to agree with whatever index space
  Issue #153's offset translation produces, or it will be off by the import count.
* **Names are rustc-mangled v0 symbols**, not source paths:
  `_RNvNtNtCsjLYJdSO2rja_17soroban_env_guest5guest3vec13vec_push_back`. Demangling is mandatory
  for the fallback to be readable, and `addr2line`'s `rustc-demangle` feature — already enabled in
  `Cargo.toml` for #142 — covers it. Plain names appear only for `extern "C"` + `#[no_mangle]`
  functions, where the symbol *is* the export name.
* **Names are module-level, not line-level.** 18 entries for a 488-byte code section: the best `name`
  can do is `compute_heavy_loop` with no `file:line`. `SourceFrame` already models that
  (`name` set, `file`/`line` `None`), which is why #157 is a fallback and not a substitute.

### 5. One incidental hazard: committed fixtures carry the builder's path

DWARF line tables store the compile directory. `fixtures/dwarf_probe/dwarf_probe.wasm` — which
Round 1 committed so tests could `include_bytes!` it — contains
`/Users/allison/Documents/Orgs/Tollcraft/.worktrees/profiler-r1/fixtures/dwarf_probe` and
`src/lib.rs`. Any fixture committed from a developer machine ships that machine's paths, and CI
will bake its own into anything it builds. Worth a line in the contributor docs before Issue #160
adds more fixtures.

## Architectural Decision
1. **Do not count on `name` for deployed binaries.** It is gone before the artifact reaches a
   user, and no `Cargo.toml` setting changes that. #157 stays scoped as a fallback for
   *un-optimized* builds compiled without debug info — a real but secondary case — and it is a
   names-only degradation, never a `file:line` one.
2. **Make the optimized-binary case a first-class diagnostic.** #162's warning should key on
   "sections loaded but nothing resolves", not on "`.debug_info` missing", because the measured
   failure mode of the standard pipeline is the former. A cheap test for it: after the trace, if
   every event resolves to `None` while `has_debug_info()` is `true`, say so and name the
   pre-optimization path.
3. **Recommend the build that works.** Documented guidance is `debug = "line-tables-only"` (or
   `debug = 1`) in the profile *and* no `wasm-opt` step without `-g`. With `-g`, 140 of 145 probed
   offsets in the probe's code section resolve; without it, 0 do.
4. **Frame resolution stays blocked on the engine, not on symbolication.** This spike confirmed
   the DWARF half works on committed fixtures at real offsets, which #145–#148 can now be written
   against; `wasmi` 2.0 still gives the tracer no program counter, so the pipeline cannot produce
   a flamegraph frame until #153 lands a code-section offset.

## Next Steps
* #157 — `name`-section fallback: implement the kind-`1` reader sketched above, demangle through
  `rustc-demangle`, and index by function index space including imports.
* #162 — warn on the degenerate mapping (loaded, resolved nothing) with the pre-optimization path
  in the message.
* #163 — document the fallback precedence this spike established: DWARF at a real offset, then
  `name`, then `wasm[pc]`.
* #153 — `wasmi` pc → code-section offset, keeping the import-count finding in view.
* #160 — fixture integration tests; note the embedded build path there.
