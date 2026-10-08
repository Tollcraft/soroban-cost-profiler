# Troubleshooting

Every message quoted here is *measured* output of this repository's own binary, not a paraphrase of what
the code seems to say. Build it and run the reproduction line yourself and you should get the same text:

```sh
cargo build --release
./target/release/soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
```

Four of the reproductions need a module the committed fixtures do not contain — one that imports a host
function, one whose export takes an `i64`, one whose export takes an `i32`, and one whose only work is a
ledger read. All four are hand-assembled
test data in this repository (`NEEDS_HOST`, `NEEDS_ARG`, `NEEDS_I32` and `READS_LEDGER` in `src/main.rs`, at
52, 46, 43 and 55 bytes), and the entries below say which one they ran and give its text format, rather than
pretending a fixture produced them.

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
| `failed to instantiate module: cannot find definition for import` | An import outside the Soroban host interface — the host's own 199 functions link | [Unlinked imports](#an-import-the-profiler-does-not-link) |
| A `… trapped: wasm unreachable instruction executed` error on an SDK build | A `--args` word the guest could not read as a `Val`, or a host function that needed chain state | [Arguments](#the-export-takes-arguments) |
| `takes 1 argument (i64); --args gave no values` | The export takes parameters; pass them with `--args` | [Arguments](#the-export-takes-arguments) |
| `… trapped: host function 'x.3' failed: HostError: Error(Context, InternalError)` | A ledger read with no ledger — the run needs `--state` | [The contract reads the ledger](#the-contract-reads-the-ledger) |
| `Instruction ceiling exceeded` | The trace-buffer guard tripped at your `--instruction-limit` | [The ceiling](#about-the-100m-instruction-limit) |
| No records on stderr when you want them | The default level prints none; `-v` installs the transcript | [What the profiler is doing](#i-want-to-see-what-the-profiler-is-doing) |
| `--quiet` prints nothing on stdout and exits `0` | The flag's whole job: the artifact is the answer | [`--quiet`](#--quiet-printed-nothing-is-that-a-failure) |
| `--output -` prints the profile and the summary vanishes | The artifact took stdout, which the summary also wanted | [`--output -`](#--output-printed-the-profile-and-none-of-the-summary) |
| `error: … (os error 2)` and friends | Bad path, bad file, bad flag | [Exit codes](#exit-codes-1-and-2) |

## The profile is one line of zeros

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
no function recorded any exclusive cost (cpu)
$ cat profile.folded
wasm[0] 0
```

**Why.** `invoke_function`'s hook has no instruction hook to hang a count on, so it substitutes one
unit-costed step per boundary (`src/tracer.rs:357-360`). `caller_of_heavy` crosses two boundaries — the call
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
frames: the budget deltas read around a call (`record_host_return`, `src/tracer.rs:176-191`) and the
`HostCall` events that open them. The export above calls no host function, so it has no host frame to open
and the two metrics are empty by construction. An export that does call the host populates all three —
measured on `fixtures/build.sh`'s artifact, `memory_heavy_loop` at 100 iterations:

```console
$ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn memory_heavy_loop --args 429496729604 --metric hostcalls
Top 1 functions by exclusive cost (hostcalls):
  1. host[0]  102
$ cat profile.folded
wasm[0] 0
wasm[0];host[0] 102
```

`102` is `vec_new` plus one `vec_push_back` per iteration plus `vec_len`, and the same run reports
`host[0] 125022` for `--metric cpu` and `host[0] 50080` for `--metric memory`. So a zero in these two columns
is a statement about the contract, not about the tool: no host call, no number.

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

## An import the profiler does not link

A `soroban-sdk` build no longer fails here. Its imports are the positional labels the SDK links against —
`("v","_")`, `("i","_")`, `("v","6")`, `("v","3")` — and `src/host.rs` registers all 199 entries of
`soroban-env-host`'s table against the real `Host`, so the contract instantiates, runs, and its host calls
are traced (`fixtures/build.sh`'s artifact produces `wasm[0];host[0] 125022` for
`memory_heavy_loop` at 100 iterations). What still fails to link is an import from a module the profiler does
not bind at all — in practice a JS shim or a hand-written module. It is reproducible from the 52-byte module
this repository's own e2e test commits as `NEEDS_HOST` (`tests/cli_e2e.rs`), which is
`(module (import "env" "missing" (func)) (func (export "boom") unreachable))` — the shortest module whose
import cannot be linked:

```console
$ soroban-cost-profiler --wasm needs_host.wasm --fn boom
error: failed to instantiate module: cannot find definition for import (env,missing) with type Func(FuncType { core: FuncType { params: [], results: [] } })
$ echo $?
1
$ ls boom.folded
ls: boom.folded: No such file or directory
```

**Why.** `instantiate_module` links the Soroban host interface and nothing else (`src/host.rs`), so an import
whose `(module, name)` pair is not in that table fails before the export is called.

**What to do.** Note two details so you do not misread the failure: no `.folded` file is written on this path
(nothing ran, so a file would be a profile of a call that was not made), and the exit code is `1`, not `2` —
a module this tool cannot link is bad input, not a broken profiler. Read the `(module,name)` in the message: a
one-character pair inside `x i m v l d b c a t p` is a host function and should have linked, so it means this
profiler is older than the bindings; anything else — `env`, `f32`, a JS name — is a module this tool has no
business standing up.

Two things a *linked* host function can still fail at, and both now surface as a trap inside the run rather
than as this refusal, which is why they keep the partial trace (#173) instead of writing nothing:

* **Ledger state.** `--state <snapshot.json>` gives the contract a ledger to read
  ([The contract reads the ledger](#the-contract-reads-the-ledger)), and the five reads of *ledger info* —
  sequence, version, timestamp, network ID, max TTL — are answered from the file. What no file can give a
  directly invoked export is the contract *frame* `get`, `put` and `require_auth` build their key from, so
  those fail against a stack that is empty rather than against missing data.
* **Another contract.** `call` and its sibling re-enter the engine in production, which needs a live engine
  caller; the bound `Env` methods have none, so they return a host error instead of recursing.

And a third that is the user's own command line: an SDK export reads each parameter as a tagged `Val`, so
`--args 10000` reaches `compute_heavy_loop` as a word whose tag means nothing to the guest and the contract
aborts — see [The export takes arguments](#the-export-takes-arguments).

## The export takes arguments

Again not a fixture: a module whose single export takes an argument,
`(module (func (export "needs_arg") (param i64) (result i64) local.get 0 i64.const 2 i64.add))`, encoded as
`needs_arg.wasm`. The flag for it is `--args`, and the first thing to notice is that getting it wrong is not
a trap:

```console
$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --output na.folded
error: 'needs_arg' takes 1 argument (i64); --args gave no values. `--args` is one value per parameter, in
the order the signature lists them.
$ echo $?
1
$ ls na.folded
ls: na.folded: No such file or directory

$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --args 40 --output na.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
$ cat na.folded
wasm[0] 0
```

The second command prints a `warning:` line about this module's missing `.debug_info` too, elided here
because the interesting half is the exit code and the file.

**Why.** The profiler reads the export's signature off the module and checks `--args` against it *before*
invoking anything, so an arity mistake is a bad command line and not a run. It used to be a run: the same
first command printed `'needs_arg' trapped: encountered an incorrect number of parameters`, exited `1`, and
left an `na.folded` beside it. #173's rule is that a trapping contract keeps the trace it earned, and that
rule is right for a contract that stopped halfway through — it is wrong for a call that was never legal,
which is the artifact the old behaviour wrote.
[Issue 211](https://github.com/Tollcraft/soroban-cost-profiler/issues/211) moved the check in front of the
call so the refusal writes nothing.

**What to do.** Count the parameters the message names and pass that many values, comma-separated, in the
order the signature lists them: `--args 1000,7`. A negative amount is a value, not a flag (`--args -1`
parses). Two limits the flag does not paper over:

* **The values are `i64`.** A parameter of another width is named by position and type rather than coerced.
  Against a 43-byte module whose export is `(param i32) (result i32)`:

  ```console
  $ soroban-cost-profiler --wasm needs_i32.wasm --fn needs_i32 --args 1
  error: 'needs_i32' takes 1 argument (i32), and `--args` supplies i64 values only: argument 1 is i32. A
  parameter of another width cannot be named from the command line.
  ```

  The boundary an SDK export declares *is* `i64` — measured on the `fixtures/build.sh` artifact, whose
  `compute_heavy_loop` is `(i64) -> i64` — so `--args` satisfies a real contract's arity. What the flag does
  not do is tag the value, and the guest reads the word as a `Val`: a `u32` parameter wants `U32Val(10000)`,
  which is `(10000 << 32) | 4`. Measured on that artifact, the number you would write and the word the
  contract wants:

  ```console
  $ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn compute_heavy_loop --args 10000 --output dc.folded
  error: 'compute_heavy_loop' trapped: wasm `unreachable` instruction executed. The partial trace up to the
  trap is in dc.folded, and its costs are incomplete because the call never returned.
  $ echo $?
  1
  $ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn compute_heavy_loop --args 42949672960004 --output dc.folded
  no function recorded any exclusive cost (cpu)
  $ echo $?
  0
  ```

  The arity check cannot tell them apart — both are one `i64` — so the first one runs, and a run that traps
  keeps the partial trace it collected (#173) instead of refusing. Reading `dc.folded` after the first
  command is how this mistake gets past you: the file exists, and it is a profile of an aborted call.
* **Arguments are values, not state.** `--args` hands the export words; it gives the contract no ledger to
  read. That is `--state`, and it is in [The contract reads the ledger](#the-contract-reads-the-ledger).

Both refusals exit `1` and write no file, for the same reason the unlinked-import entry above does: a command
line that cannot be honoured should not leave behind an artifact that looks like it was. A value `--args`
cannot parse at all — `--args abc` — is clap's refusal instead of the profiler's, and is in
[Refused flags](#refused-flags).

## The contract reads the ledger

A contract that reads the chain has two separate things to get, and this tool now supplies both halves of one
of them. `--state <file.json>` names a **Soroban ledger snapshot** — the same JSON `soroban ledger json`
writes for a network and `Env::to_ledger_snapshot_file` writes for an integration test — and the profiler
builds its host from that file before the contract is loaded. Without the flag the host's ledger is blank, and
the first read of it traps:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --output blank.folded
error: 'read_sequence' trapped: host function 'x.3' failed: HostError: Error(Context, InternalError)
DebugInfo not available
. The partial trace up to the trap is in blank.folded, and its costs are incomplete because the call never returned.
$ echo $?
1
```

`reads_ledger.wasm` is `READS_LEDGER` from `src/main.rs`, 55 bytes:
`(module (import "x" "3" (func (result i64))) (func (export "read_sequence") (result i64) call 0))`. `x.3` is
the guest name of `get_ledger_sequence`, so this is the smallest contract that has to have a ledger. The same
run against this repository's committed snapshot finishes, and `-v` is what marks which kind of run the profile
came from:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state fixtures/state/ledger.json --output mocked.folded -v 2>&1 | grep mocked
2026-10-08T13:19:23.247313Z  INFO soroban_cost_profiler: mocked ledger state from fixtures/state/ledger.json: sequence 500, timestamp 1700000000, 2 entries
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state fixtures/state/ledger.json --output mocked.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
$ cat mocked.folded
wasm[0] 0
wasm[0];host[0] 0
```

**What is served, and what is not.** The five host functions that read the ledger *info* — `get_ledger_sequence`,
`get_ledger_timestamp`, `get_ledger_version`, `get_ledger_network_id`, `get_max_live_until_ledger` — need no
call context, so they answer from the file and the run above is one of them. A **storage** read is a different
case: `get_contract_data` and its siblings build their key from the *current contract ID*, and the profiler
invokes an export from outside a contract call, so that stack is empty and the read stops before it ever
consults the snapshot. `require_auth` reads the same frame. Nothing about the file changes that; the entries are
installed as the storage the host falls back to, and no read from a directly invoked export reaches it.

The status makes that hard to spot from a terminal, and it is worth knowing before you read one:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --output blank.folded 2>&1 | grep trapped
error: 'read_sequence' trapped: host function 'x.3' failed: HostError: Error(Context, InternalError)
```

`Error(Context, InternalError)` is what a missing ledger reports and the same word a missing contract frame
reports, because the host's sentence for its reason is a `DebugInfo` it only builds with its own `testutils`
feature — a feature that pulls `arbitrary` into the dependency tree, which `AGENTS.md` rule 3 keeps out of this
crate. So the message tells you the host refused, and the name in it (`'x.3'`, `'l.1'`) is what tells you which
read: the module's own import names, listed in `soroban-env-common`'s `env.json`.

Two `--state` files are refused before the contract is read, and neither leaves a profile:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state notes.json --output out.folded
error: notes.json is not a Soroban ledger snapshot: missing field `protocol_version` at line 1 column 12. A snapshot is the JSON written by `soroban ledger json` or by `Env::to_ledger_snapshot_file`.
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state ledger24.json --output out.folded
error: ledger24.json declares protocol 24 and this profiler's host implements 28. Cost tables differ between protocols, so a snapshot from another protocol is refused rather than silently re-stamped; set `protocol_version` to 28 only when the ledger really is that protocol.
$ echo $?
1
$ ls out.folded
ls: out.folded: No such file or directory
```

**What to do.** For the second, take a snapshot of a ledger that really is this protocol — the number the host
implements is the `soroban-env-host` version this build links, and it is printed in the message. Re-stamping an
unrelated file's `protocol_version` to make it load is the mistake the refusal exists to stop: the profile would
report one protocol's costs as another's. For the first, the field named is the field missing; a
`missing field` message is serde reading the real snapshot shape, and `A snapshot is the JSON written by …` says
which tool makes one. A file that is not there at all is `failed to read <path>: No such file or directory (os
error 2)`, the same shape as every other unreadable path here.

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
input was accepted and the profiler could not finish its own work" (`src/main.rs:433-451` splits these two on
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

The same three failures with `--format json` or `--format raw` name the format instead of `folded stack`, so
the message tells you which artifact was abandoned:

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call --format json --output missing/dir/profile.json
error: failed to write JSON call tree to missing/dir/profile.json: No such file or directory (os error 2)

$ soroban-cost-profiler --wasm contract.wasm --fn call --format raw --output .
error: failed to write raw event stream to .: Is a directory (os error 21)
$ echo $?
2
```

Omitting `--output` altogether is not an error and writes no `profile.folded` unless you asked for the folded
format: each format defaults to the file named after it (`profile.folded`, `profile.json`, `profile.raw`), so
three runs of one contract with only `--format` changing leave three files beside each other rather than one
file rewritten with bytes the next reader will misparse.

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

$ soroban-cost-profiler --wasm contract.wasm --fn call --format yaml
error: invalid value 'yaml' for '--format <FORMAT>'
  [possible values: folded, json, raw]

$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --args abc
error: invalid value 'abc' for '--args <ARGS>': invalid digit found in string

$ soroban-cost-profiler --wasm contract.wasm --fn call -v --quiet
error: the argument '--verbose...' cannot be used with '--quiet'
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

`-v` and `--quiet` are refused as a pair for a third reason: they state opposite intentions about the same
output, and clap picking a winner would leave the user reading a transcript their own command line did not
ask for.

`--args abc` is the same class of refusal as `--metric gas`: the value is not what the flag's type declares,
and clap says so with the token it could not read. The two `--args` refusals that are *not* clap's — the
wrong number of values, and a parameter that is not `i64` — come from the module's own signature and are in
[The export takes arguments](#the-export-takes-arguments), because the fix there is a different command line
rather than a different value.

## I want to see what the profiler is doing

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call -v 2>&1 >/dev/null
2026-10-08T06:05:58.562669Z  INFO soroban_cost_profiler::tracer: Loading WASM file from contract.wasm
2026-10-08T06:05:58.562988Z  INFO instantiate_module: soroban_cost_profiler::tracer: Instantiating WASM module
2026-10-08T06:05:58.569862Z  INFO invoke_function{func_name="call"}: soroban_cost_profiler::tracer: Invoking function: call
```

**What to do.** Nothing is wrong; `-v` is the flag. The default level is `WARN` and the crate keeps no
`warn!` or `error!` record — every message meant for a user leaves through `warning:`/`error:` on stderr,
independently of any flag — so a plain run is silent by design. A run that prints no records *and* no
`error:` line succeeded; check the artifact before concluding the profiler did nothing.

The notches step through what the engine can report, and the last two are the interesting ones for the
symptoms elsewhere on this page:

- `-v` (`INFO`) — the three stages: load, instantiate, invoke. A run that stops after the first line never
  found a readable module; after the second, it found one that would not link (see
  [An import the profiler does not link](#an-import-the-profiler-does-not-link)).
- `-vv` (`DEBUG`) — every call boundary the engine reports, as `WASM Call at PC: 0` / `WASM Return at PC: 0`.
  Count them: a contract that calls five helpers and reports two boundaries is
  [the profile that is one line of zeros](#the-profile-is-one-line-of-zeros), and the count is the evidence
  that the engine, not your contract, is the one hiding the calls.
- `-vvv` (`TRACE`) — the single costed step recorded at each boundary (`Stepping at PC: 0, cpu: 1, mem: 0`),
  which is `wasmi` 2.0's substitute for an instruction hook.

The transcript is stderr, timestamped in UTC, and carries no ANSI escapes, so `2>&1 >/dev/null` and
`grep -c 'WASM Call'` both work on it. Records never reach stdout: that stream is the summary callers pipe.

## `--quiet` printed nothing, is that a failure?

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call --quiet
$ echo $?
0
$ cat profile.folded
wasm[0] 0
```

It is the flag's whole job: stdout is narration, and `--quiet` says the file is the answer. Exit `0`, empty
stdout, the artifact unchanged from a loud run byte-for-byte. What it does *not* suppress is the other two
kinds of output — a `warning:` about a binary it could not symbolize and an `error:` about a run that could
not happen still print, because those are news about your command rather than commentary on the profiler's
day, and `compare`'s table stays too, since for that mode the table is the answer and not an echo.

## `--output -` printed the profile and none of the summary

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --output -
wasm[0] 0
```

That is the whole stdout, and the missing part is the `no function recorded any exclusive cost (cpu)` line
you would get on a run that wrote a file. **Why.** `--output -` means "the artifact goes to stdout" — the
convention every `flamegraph.pl` and `jq` invocation already speaks — and stdout can only hold one thing. The
summary is commentary *about* the artifact, so when the artifact is the stream the commentary is dropped
rather than interleaved into it: a `| jq .metric` on the other side of the pipe would otherwise be parsing a
line of prose. The same rule is why `-v`'s transcript is stderr.

`--format` changes what arrives but not the rule:

```console
$ soroban-cost-profiler --wasm …/dwarf_probe.wasm --fn caller_of_heavy --format json --output - | jq .metric
"cpu"
$ soroban-cost-profiler --wasm …/dwarf_probe.wasm --fn caller_of_heavy --format raw --output -
call pc=0 cpu=0 mem=0
return pc=0 cpu=0 mem=0
```

So the two ways to "get nothing on stdout" are different. `--quiet` writes the file and stays out of stdout;
`--output -` fills stdout and writes no file — `ls` after it shows no `profile.folded`, and a script that
looked for one has to redirect instead. Only the exact single dash means stdout: `--output new-run.folded` is
a file, and `--output -run.folded` never reaches the profiler at all, because clap reads the leading dash as a
short flag and refuses the command line with `error: unexpected argument '-r' found`.

The trap message tracks the destination, because its job is to say where the partial trace went:

```console
$ soroban-cost-profiler --wasm …/dwarf_probe.wasm --fn caller_of_heavy --format raw --instruction-limit 1 --output -
call pc=0 cpu=0 mem=0
return pc=0 cpu=0 mem=0
error: 'caller_of_heavy' trapped: Instruction ceiling exceeded. The partial trace up to the trap is on stdout, and its costs are incomplete because the call never returned.
$ echo $?
1
```

The events before that `error:` are the frames the run crossed, exactly as the file version keeps them, and
the exit code is still `1` — a halved stream on stdout is a truncated profile no more than a halved file is.

## `compare` complains

```console
$ soroban-cost-profiler --wasm contract.wasm --fn call compare before.folded after.folded
error: `compare` reads two .folded files and runs no contract, so `--wasm` cannot accompany it.

$ soroban-cost-profiler --args 1000,7 compare before.folded after.folded
error: `compare` reads two .folded files and runs no contract, so `--args` cannot accompany it.

$ soroban-cost-profiler --state ledger.json compare before.folded after.folded
error: `compare` reads two .folded files and runs no contract, so `--state` cannot accompany it.

$ soroban-cost-profiler compare ok.folded .
error: failed to read .: Is a directory (os error 21)

$ soroban-cost-profiler compare junk.folded ok.folded
error: baseline: line 1: expected a non-negative integer cost, got "notanumber"
```

**What to do.** Drop `--wasm` and the profiling flags: `compare` takes two files and nothing else. The
values flag is refused for the same reason as the contract — arguments are for a call and this mode makes
none — and written *after* the subcommand it is clap's refusal instead of ours, `error: unexpected argument
'--args' found`, because `compare` declares only its two files. Both exit `1`. Name a
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
instruction) is enforced as `instruction_ceiling` inside `record_step` (`src/tracer.rs:97-99`), whose error
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
  which runs once per boundary (`src/tracer.rs:357-360`), and one host-initiated call gives two of them —
  which is why `1` halts a function that computes a million instructions and `2` lets it finish. A contract
  that loops forever *inside* one function body emits no boundaries, never advances the counter, and is not
  stopped — and `wasmi`'s own fuel is set to `u64::MAX` for the run (`src/main.rs:608-613`), so the engine
  does not stop it either.

So if your symptom is "it hangs" or "my machine ran out of memory", the ceiling message is not the diagnosis,
and `--instruction-limit` is not the lever either:

* **A run that never returns** is a compute-only runaway loop. This is the sharpest edge in the tool, and the
  one place the roadmap's checked "Infinite Loop Protection" box overstates what the current engine can
  deliver — the guard is real for the tracing buffer it was written to protect (a run with many boundaries
  cannot grow the `Vec` unboundedly, and `--instruction-limit` is what bounds it) and inert against a loop
  that stays inside one body. Profile exports that terminate, and prefer the fixture-sized contracts this
  repository tests against. Turning the ceiling into an execution bound needs a per-instruction hook, and
  `wasmi` 2.0 — the engine this profiler runs on — has none to expose; no open issue in this repository owns
  that, so the guard's scope is exactly what the paragraph above describes.
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
