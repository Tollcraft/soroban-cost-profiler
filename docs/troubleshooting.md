# Troubleshooting

Every message quoted here is *measured* output of this repository's own binary, not a paraphrase of what
the code seems to say. Build it and run the reproduction line yourself and you should get the same text:

```sh
cargo build --release
./target/release/soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
```

Two of the reproductions need a module the committed fixtures do not contain — one that imports a host
function, whose 52 bytes this repository already commits as test data, and one whose export takes an argument.
Those entries say which module they ran and give its text format, rather than pretending a fixture produced
them.

## What is broken and what is working as designed

Most reports of "the profiler does nothing" are the second thing. `wasmi` 2.0 exposes no instruction-level
hook, so the tracer sees call *boundaries* and nothing between them, and the artifact of a healthy run on a
healthy contract is currently one line of zeros. [README's Limitations](../README.md#limitations) states that
at length; this file is the symptom-by-symptom version of the same findings.

So read the symptom table first, because four of its rows have no fix at all and one more cannot be reached
from any command line:

| What you see | What it means | Entry |
|---|---|---|
| `wasm[0] 0` in the `.folded` file | The run worked; the engine reports no cost per boundary | [Zeros](#the-profile-is-one-line-of-zeros) |
| `no function recorded any exclusive cost (cpu)` | The terminal half of the same fact | [Zeros](#the-profile-is-one-line-of-zeros) |
| Frames named `wasm[0]` though the build has DWARF | The hook hands the tracer no program counter | [Every frame is `wasm[0]`](#every-frame-is-wasm0-even-with-dwarf) |
| `unsymbolized 0` | The trace recorded no boundary at all | [`unsymbolized` frames](#the-file-says-unsymbolized) |
| A warning about `.debug_info` or `name` | Symbol degraded, run fine — pick which of the three | [Unnamed frames](#frames-are-named-by-address-not-source) |
| `failed to instantiate module: cannot find definition for import` | A real `soroban-sdk` contract; blocked on #210 | [Host imports](#a-contract-that-imports-host-functions-does-not-run) |
| `trapped: encountered an incorrect number of parameters` | The export takes arguments; blocked on #211 | [Arguments](#the-export-takes-arguments) |
| `Instruction ceiling exceeded` | The trace-buffer guard tripped at your `--instruction-limit` | [The ceiling](#about-the-100m-instruction-limit) |
| `error: … (os error 2)` and friends | Bad path, bad file, bad flag | [Exit codes](#exit-codes-1-and-2) |

## The profile is one line of zeros

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
no function recorded any exclusive cost (cpu)
$ cat profile.folded
wasm[0] 0
```

**Why.** `invoke_function`'s hook has no instruction hook to hang a count on, so it substitutes one
unit-costed step per boundary (`src/tracer.rs:352-356`). `caller_of_heavy` crosses two boundaries — the call
in and the return out — and at the default `--sample-rate 1000` the accumulator gains 1 per boundary, so it
never reaches the threshold that emits an event. The zero is structural, not a small measurement.

**What to do.** Nothing is broken, and there is no flag that makes the number real. To confirm the tracer is
alive, lower the rate until the boundaries themselves are chargeable:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --sample-rate 1
Top 1 functions by exclusive cost (cpu):
  1. wasm[0]  2
$ cat profile.folded
wasm[0] 2
```

That `2` is the count of boundaries, which is exactly what it is worth: a floor and a shape, not a budget
reading. For the instructions the network will actually charge, use `soroban-budget-assert` (Tier 2).

If `--metric memory` or `--metric hostcalls` gives you the same zero — measured, on the same binary:

```console
$ soroban-cost-profiler --wasm …/dwarf_probe.wasm --fn caller_of_heavy --metric memory
no function recorded any exclusive cost (memory)
$ soroban-cost-profiler --wasm …/dwarf_probe.wasm --fn caller_of_heavy --metric hostcalls
no function recorded any exclusive cost (hostcalls)
```

that is not three separate failures. Memory bytes and host-call counts reach the call tree only through host
frames: the budget deltas read around a call (`record_host_return`, `src/tracer.rs:172-187`) and the
`HostCall` events that open them. No host frame can open while nothing links a host function
([#210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210)), so those two metrics are
structurally empty today rather than merely small.

## Every frame is `wasm[0]`, even with DWARF

**Why.** The call hook is handed the hook variant and nothing else — no callee, no offset — so every event is
recorded at `pc = 0` and the aggregator names it `wasm[0]` (`src/aggregator.rs:29-35`). An internal
instruction pointer would not fix it either: `wasmi` re-encodes wasm bytecode into its own instruction stream
during translation and keeps no table back to the original offsets.

**What to do.** Nothing at the command line. Source mapping is already complete and correct behind this —
`CodeMap` indexes exactly the body-start address space, and `tests/source_map_fixture.rs` checks resolved
addresses against the *text* of the source they came from — so when a real address reaches a frame, the
`file:line` half is waiting for it. Do not spend time on build flags trying to make `wasm[0]` disappear; it
survives every build flag in existence.

## Frames are named by address, not source

Three different failures look alike from the flamegraph and need three different fixes. The run tells you
which one it is, on stderr, and it exits `0` in all three cases: the profile is valid, only its names are
degraded.

### 1. No DWARF and no usable `name` section

```console
$ wasm-opt --strip-debug fixtures/dwarf_probe/dwarf_probe.wasm -o stripped.wasm
$ soroban-cost-profiler --wasm stripped.wasm --fn caller_of_heavy
warning: no `.debug_info` section, so program counters cannot be mapped to Rust source lines. Build the
copy you profile with a profiling profile — `[profile.profiling]` with `inherits = "release"` and
`debug = "line-tables-only"` is enough for `file:line` frames — and profile that artifact rather than the
stripped one (`wasm-opt`, and `stellar contract build`, strip debug info). Keep `debug` out of
`[profile.release]`: that is the profile whose output gets deployed, and mainnet bills for the extra bytes.
The module carries no custom sections at all. Every frame will therefore be named by address, `wasm[pc]`,
and not by source.
```

This is the message the MVP notes call "missing DWARF data". **Fix:** stop profiling the stripped artifact and
build one that kept its line tables:

```toml
[profile.profiling]
inherits = "release"
debug = "line-tables-only" # enough for source mapping, far cheaper than full -g
```

```sh
cargo build --profile profiling --target wasm32-unknown-unknown
```

Note what `--strip-debug` cost here: the spike measurement in
[`spikes/02_wasm_name_section_fallback.md`](spikes/02_wasm_name_section_fallback.md) shows it drops DWARF
*and* the `name` fallback together, so the "just strip it" recipes lose both sources at once.

### 2. `name` present, DWARF absent

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe_no_debug.wasm --fn caller_of_heavy
warning: this artifact has a `name` section but no DWARF line tables, so frames will name functions and
never `file:line`. Build the copy you profile with a profiling profile — `[profile.profiling]` with
`inherits = "release"` and `debug = "line-tables-only"` — and keep `debug` out of `[profile.release]`: that
is the profile whose output gets deployed, and mainnet bills for the extra bytes.
```

**Fix:** the same `[profile.profiling]` build. The distinction matters only in that this artifact is
half-mapped — function names will appear, `file:line` never will — so if you already have a `name` build and
want line numbers, the answer is `debug = "line-tables-only"`, not a different tool.

### 3. DWARF loads and is wrong — no error fires

```console
$ wasm-opt -O0 fixtures/dwarf_probe/dwarf_probe.wasm -o opt0.wasm
$ soroban-cost-profiler --wasm opt0.wasm --fn caller_of_heavy
warning: 100% of the sampled addresses in this binary map to no line or to one another address already
claimed, so its DWARF describes different code from the bytes that ran — typically pre-inlining,
pre-optimization output. The frames below are not wrong about which functions ran, but read their line
numbers with suspicion.
no function recorded any exclusive cost (cpu)
$ echo $?
0
```

**Why.** `wasm-opt` without `-g` rewrites the code section and leaves the old, shorter line tables behind: 855
bytes of `.debug_*` that `SourceMapper::new` finds, that gimli parses, that make `has_debug_info()` return
`true` — and that resolve **0 of 152** probed addresses on that artifact. Nothing is structurally wrong to
fire an error on.

**Fix:** pass `-g` to whatever optimizer you run, or profile the `cargo build --profile profiling` output
directly. `stellar contract optimize` passes no `-g` today, so its output is unmappable by either source.

This warning is the only signal for this case, so a run that warns about *coverage* is telling you the
artifact is broken, not that the run is.

## The file says `unsymbolized`

```console
$ cat na.folded
unsymbolized 0
```

**Why.** `unsymbolized` is the frame the aggregator returns for an event stream that opened no boundary at
all (`src/aggregator.rs:55-61`) — deliberately a different name from `wasm[pc]`, so "nothing to aggregate"
is distinguishable from "everything ran unresolved". You get it when the call never started: the usual cause
is a trap on entry, and the usual trap is arity (see
[Arguments](#the-export-takes-arguments)). The stderr line beside it says which.

**What to do.** Read the error, not the file. A `.folded` of `unsymbolized 0` is not a profile of a cheap
function; it is the shape of a run that did not happen.

## A contract that imports host functions does not run

Not reproducible from the committed fixtures, which import nothing. It *is* reproducible from the 52-byte
module this repository's own e2e test commits as `NEEDS_HOST` (`tests/cli_e2e.rs`), which is
`(module (import "env" "missing" (func)) (func (export "boom") unreachable))` — the shortest module whose
import cannot be linked, standing in for the 622 KB contract build that is full of them:

```console
$ soroban-cost-profiler --wasm needs_host.wasm --fn boom
error: failed to instantiate module: cannot find definition for import (env,missing) with type Func(FuncType { core: FuncType { params: [], results: [] } })
$ echo $?
1
$ ls boom.folded
ls: boom.folded: No such file or directory
```

**Why.** `instantiate_module` links against an empty `wasmi::Linker` (`src/tracer.rs:285`), so a module
importing the Soroban environment interface fails to link before its export is called. This is what a real
`soroban-sdk` build does — its contract imports the host — which is why the README carries the same warning
in an `IMPORTANT` note.

**What to do.** Nothing yet, and note two details so you do not misread the failure: no `.folded` file is
written on this path (nothing ran, so a file would be a profile of a call that was not made), and the exit
code is `1`, not `2` — a module this tool cannot link is bad input, not a broken profiler. Wiring the host
bindings is
[#210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210). Until it lands, profile
pure-computation exports.

## The export takes arguments

Again not a fixture: a module whose single export takes an argument,
`(module (func (export "needs_arg") (param i64) (result i32) local.get 0 drop i32.const 42))`, encoded as
`needs_arg.wasm`:

```console
$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --output na.folded
error: 'needs_arg' trapped: encountered an incorrect number of parameters. The partial trace up to the trap
is in na.folded, and its costs are incomplete because the call never returned.
$ echo $?
1
```

**Why.** The profiler invokes the export with an empty parameter list, so any export that takes arguments
traps on entry. This is the arity trap, not a panic in your contract.

**What to do.** Profile an export that takes none. The trap path still writes the partial trace it earned —
that is #173's requirement that a panicking contract yields a flamegraph up to the point it stopped — which
is why `na.folded` exists here and holds `unsymbolized 0`. Passing real values is
[`--args`, issue 211](https://github.com/Tollcraft/soroban-cost-profiler/issues/211); reading ledger state is
[issue 212](https://github.com/Tollcraft/soroban-cost-profiler/issues/212).

## The export name is wrong

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn nope
error: 'nope' is not an exported function; the module exports caller_of_heavy, compute_heavy_loop, memory_heavy_loop
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm
error: --fn is required; the module exports caller_of_heavy, compute_heavy_loop, memory_heavy_loop
```

**Why and what to do.** Both list the module's real exports, so the fix is in the message. Omitting `--fn`
and passing it empty (`--fn ""`) produce the same second message, and a name the module exports but that is
not a function — an exported memory or table — is refused the same way rather than called. Exit `1`, no file
written. Soroban contracts built with the SDK usually export a single `call` entry that takes arguments, so
read the list before you assume your function name is in it.

## Input that is not a readable module

```console
$ soroban-cost-profiler --wasm missing.wasm --fn caller_of_heavy
error: failed to read missing.wasm: No such file or directory (os error 2)

$ soroban-cost-profiler --wasm notwasm.wasm --fn caller_of_heavy
error: failed to read notwasm.wasm: Invalid WASM signature

$ head -c 845 fixtures/dwarf_probe/dwarf_probe.wasm > truncated.wasm   # the valid artifact, cut in half
$ soroban-cost-profiler --wasm truncated.wasm --fn caller_of_heavy
error: failed to parse WASM module: unexpected end-of-file (at offset 0x2b1)
```

All exit `1`. In order: the path does not exist; the bytes are not a wasm module (a `.wat` text file, an
archive, a `Cargo.toml` — pass the `.wasm` itself, not a build manifest and not a `--target` directory, which
fails as `Is a directory (os error 21)`); the file ended in the middle of
a section, which is a truncated download or a partial write rather than a malformed build, so re-fetch or
rebuild it.

## The output path is wrong, and which exit code it earns

Exit codes are the documented `0` success, `1` "the invocation could not be honoured as asked", `2` "the
input was accepted and the profiler could not finish its own work" (`src/main.rs:463-477` splits these two on
the error kind, so a path you could never have written to is `1` and a machine refusing a write is `2`):

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call --output /nope/dir/x.folded
error: failed to write folded stack to /nope/dir/x.folded: No such file or directory (os error 2)
$ echo $?
1

$ soroban-cost-profiler --wasm contract.wasm --fn call --output ro/x.folded     # ro/ is not writable
error: failed to write folded stack to ro/x.folded: Permission denied (os error 13)
$ echo $?
2

$ soroban-cost-profiler --wasm contract.wasm --fn call --output .
error: failed to write folded stack to .: Is a directory (os error 21)
$ echo $?
2
```

**What to do.** Create the missing parent directory, or point `-o` at a path you can write. Note the run got
all the way to stage 4 before any of these failed, so the contract *did* execute: fixing the path and
re-running costs you a full trace, which for a real contract is the expensive part.

## Refused flags

clap refuses these before anything runs, and the profiler overrides clap's own exit code so a typo'd flag
looks like a bad command line (`1`) and not like a crash:

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call --sample-rate 0
error: invalid value '0' for '--sample-rate <SAMPLE_RATE>': must be greater than 0

$ soroban-cost-profiler --wasm contract.wasm --fn call --metric gas
error: invalid value 'gas' for '--metric <METRIC>'
  [possible values: cpu, memory, hostcalls]

$ soroban-cost-profiler --wasm contract.wasm --fn call --instruction-limit 0
error: invalid value '0' for '--instruction-limit <INSTRUCTION_LIMIT>': must be greater than 0
$ echo $?
1
```

Zero is the case that matters on the first and the last, and it means opposite things, which is why both are
refused rather than honoured. For `--sample-rate`, `record_step` throttles by comparing
`current_step_cost >= sample_rate`, so a rate of `0` would make that true on every step and silently turn
sampling off, buffering one event per step up to the ceiling — the OOM `AGENTS.md` rule 5 exists to prevent.
For `--instruction-limit` the comparison runs the other way: the counter increments *before* it is compared,
so a ceiling of `0` fails the first boundary and the run ends having profiled nothing, which looks exactly
like a contract that traps on its first instruction. Refusing both at the flag means the run never starts
instead of dying later with no explanation.

## `compare` complains

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call compare before.folded after.folded
error: `compare` reads two .folded files and runs no contract, so `--wasm` cannot accompany it.

$ soroban-cost-profiler compare ok.folded .
error: failed to read .: Is a directory (os error 21)

$ soroban-cost-profiler compare junk.folded ok.folded
error: baseline: line 1: expected a non-negative integer cost, got "notanumber"
```

**What to do.** Drop `--wasm` and the profiling flags: `compare` takes two files and nothing else. Name a
`.folded`, not a directory and not a contract — handing it a `.wasm` is the most common form of this failure,
and the error is `failed to read <file>: stream did not contain valid UTF-8`. The parse errors name which
side and which line (`baseline:` or `current:`), because the two files come from different runs and the
broken one is usually the newer.

The silent version of a `compare` problem is a correct table with the wrong story. A `.folded` file records
no metric of its own, so two files from runs that disagreed on `--metric` are diffed exactly as written:
CPU instructions against memory bytes, with the arithmetic intact and the conclusion meaningless. Both sides
have to agree on `--metric` beforehand, and nothing can check that for you. A run that produces no rows but
says `no function's cost changed between the two profiles (1 compared)` is the other non-bug: unchanged cost
is an answer, and the tally line exists so an empty table cannot be mistaken for a broken one. Both of these
exits are `0`.

## About the 100M instruction limit

The MVP constraint (`AGENTS.md` rule 5: a contract can run 100M instructions, so nothing may allocate per
instruction) is enforced as `instruction_ceiling` inside `record_step` (`src/tracer.rs:93-95`), whose error
text is `Instruction ceiling exceeded`. The bound is a flag — `--instruction-limit`, from
[issue 213](https://github.com/Tollcraft/soroban-cost-profiler/issues/213) — so unlike every other ceiling in
this file, this one you can move. When it fires it reaches you through the trap path, so the message has the
shape of a trap and the partial file beside it:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy \
    --output halted.folded --instruction-limit 1
error: 'caller_of_heavy' trapped: Instruction ceiling exceeded. The partial trace up to the trap is in
halted.folded, and its costs are incomplete because the call never returned.
$ echo $?
1
```

Measured on that command (`--instruction-limit 2` on the same fixture exits `0`), and pinned by
`an_instruction_limit_halts_the_run_and_keeps_the_trace_so_far` in `tests/cli_e2e.rs`. Two things the transcript
does not say, and a reader should know before trusting it:

* **The file is not evidence of the halt.** `halted.folded` here holds `wasm[0] 0`, byte-for-byte what a
  completed run of the same export writes, because the guard trips while the call unwinds and `wasmi` still
  reports `ReturningFromWasm`. Only the exit code and this message separate the two, so never read the
  artifact alone as "the call finished".
* **The counter counts boundaries, not instructions.** Its only caller in the live path is the call hook,
  which runs once per boundary (`src/tracer.rs:352-356`), and one host-initiated call gives two of them —
  which is why `1` halts a function that computes a million instructions and `2` lets it finish. A contract
  that loops forever *inside* one function body emits no boundaries, never advances the counter, and is not
  stopped — and `wasmi`'s own fuel is set to `u64::MAX` for the run (`src/main.rs:385-387`), so the engine
  does not stop it either.

So if your symptom is "it hangs" or "my machine ran out of memory", the ceiling message is not the diagnosis,
and `--instruction-limit` is not the lever either:

* **A run that never returns** is a compute-only runaway loop. This is the sharpest edge in the tool, and the
  one place the roadmap's checked "Infinite Loop Protection" box overstates what the current engine can
  deliver — the guard is real for the tracing buffer it was written to protect (a run with many boundaries
  cannot grow the `Vec` unboundedly, and `--instruction-limit` is what bounds it) and inert against a loop
  that stays inside one body. Profile exports that terminate, and prefer the fixture-sized contracts this
  repository tests against. Turning the ceiling into an execution bound needs the instruction hook, which is
  [issue 210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210)'s to land.
* **A very large `.folded` file** is not instruction volume either — event count tracks boundaries — so the
  lever you have is the contract you point at, not `--sample-rate`.

## Exit codes 1 and 2

| Code | Meaning | Examples above |
|---|---|---|
| `0` | The run was honoured as asked. | A zeros profile, a degraded-symbol profile, a `compare` that reports a regression. |
| `1` | The invocation could not be honoured as asked. | Missing or non-wasm or truncated file, unknown export, missing `--fn`, refused flag, unlinked host import, a contract that trapped, a malformed `.folded`, a write to a path whose parent does not exist. |
| `2` | The input was accepted and the profiler could not finish its own work. | A write the machine refused for a reason other than the path (`Permission denied`, `Is a directory`), an engine that would not configure. |

The one worth memorising is the trap rule: **a contract that traps keeps the trace it collected and still
exits `1`.** The contract failed; the profiler did not. `compare` exits `0` even when it prints a regression,
because bad news is still an answer — a CI gate that had to ignore the exit code to read the table would be a
worse tool.
