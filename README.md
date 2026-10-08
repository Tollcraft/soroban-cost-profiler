<div align="center">
  <h1>soroban-cost-profiler</h1>
  <p><strong>Visual flamegraphs and execution tracing for Soroban smart contracts</strong></p>
  <p>
    <img src="https://img.shields.io/github/actions/workflow/status/Tollcraft/soroban-cost-profiler/ci.yml?branch=main" alt="CI Status" />
    <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License" /></a>
  </p>
  <p>
    <a href="https://tollcraft.github.io/docs/"><strong>Documentation</strong></a> ·
    <a href="https://tollcraft.github.io/soroban-cost-profiler/"><strong>Demo</strong></a>
  </p>
</div>

> Part of the **[`Tollcraft`](https://github.com/Tollcraft)** initiative.

`soroban-cost-profiler` is Tier 3 of the Tollcraft cost-awareness pipeline. When `soroban-budget-assert` fails your CI because your contract used too many CPU instructions, the Cost Profiler traces your WASM execution and tells you exactly where those instructions were spent.

## The Problem

Testing tools can tell you that `my_expensive_function()` consumed 8,000,000 CPU instructions, but they don't tell you *why*. Was it a specific loop? A costly host function call? An inefficient standard library operation? There is no easy way to introspect the internal execution cost of a Soroban WASM binary during testing.

## Features

`soroban-cost-profiler` traces the execution of your WebAssembly (WASM) smart contract instruction-by-instruction. It maps the runtime execution cost back to your Rust source code and generates visual flamegraphs, making it trivial to spot the bottlenecks in your logic.

* **Execution Tracing:** Hooks into the Soroban environment's WASM execution engine during local testing to count CPU instructions.
* **Source Mapping:** Parses DWARF debug information embedded in the compiled Soroban WASM binary to map WASM instruction offsets back to human-readable Rust source code.
* **Format Compatibility:** Generates output in standard profiling formats (e.g., collapsed stack format) for consumption by tools like Speedscope.

## How it Fits into Tollcraft

1.  **Linter (`soroban-cost-linter`):** Runs at compile-time (or via `cargo check`). Catches obvious, static structural flaws.
2.  **Assert (`soroban-budget-assert`):** Runs at test-time. Simulates your cleanly-linted code against the network to measure actual execution costs based on real runtime inputs.
3.  **Profiler (`soroban-cost-profiler`):** Runs when a budget assertion fails, generating visual flamegraphs to diagnose exactly where the budget was spent.

## Getting Started

The profiler is one Rust binary that reads a compiled contract. It requires no macros, no test hooks and
no changes to your contract's source.

```sh
cargo build --release                    # Rust 1.85 or newer (the crate is edition 2024)
./target/release/soroban-cost-profiler --help
```

Then point it at a contract build that kept its line tables — see [the `debug`
precondition](#the-debug-precondition) — and name the export you want to run:

```sh
cargo build --profile profiling --target wasm32-unknown-unknown
./target/release/soroban-cost-profiler \
  --wasm target/wasm32-unknown-unknown/profiling/my_contract.wasm \
  --fn call
```

That writes `profile.folded` in the directory you ran it from and prints a summary of the run — the five
costliest functions, when the trace carries any cost. Open the file at
[speedscope.app](https://www.speedscope.app) by dragging it onto the window, or hand it to
`flamegraph.pl` for an SVG of your own — this tool deliberately writes text and no pictures.

Want the whole path, with the output of each step shown? [docs/tutorial.md](docs/tutorial.md) walks from
`cargo build --release` to a flamegraph in eight steps, on a contract you can compile yourself.

## Usage

Two modes: **profile** a contract, or **compare** two profiles you already have.

```sh
# profile one exported function into a file
soroban-cost-profiler --wasm contract.wasm --fn call --output before.folded

# a contract that reads the ledger gets a snapshot to read it from
soroban-cost-profiler --wasm contract.wasm --fn call --state ledger.json --output before.folded

# after changing the contract: profile again, then diff the two runs
soroban-cost-profiler --wasm contract.wasm --fn call --output after.folded
soroban-cost-profiler compare before.folded after.folded
```

### Flags

| Flag | Default | Notes |
|---|---|---|
| `-w, --wasm <PATH>` | — | The compiled contract. Required for profiling; not accepted next to `compare`, which runs nothing. |
| `--fn <EXPORT>` | — | The exported function to invoke. Profiling refuses to start without a name, and a name the module does not export is an error that lists the exports it does have. |
| `--args <N,N>` | — | Values for that export's parameters, comma-separated and `i64` only: `--args 1000,7`. The count and widths are checked against the module's own signature before the call, so a mismatch is refused as a bad command line (`takes 1 argument (i64); --args gave no values`) rather than running as a trap that leaves a half-profile on disk. On a `soroban-sdk` build the word is what the guest reads, which is a tagged `Val` rather than the number you wrote — see [A real contract runs; chain state is what it cannot read](#a-real-contract-runs-chain-state-is-what-it-cannot-read). Refused next to `compare`, which makes no call to pass anything to. |
| `--state <PATH>` | — | A **ledger snapshot** to run the contract against: the JSON `soroban ledger json` writes for a network and `Env::to_ledger_snapshot_file` writes for an integration test. Without it the host's ledger is blank in both senses — no ledger info, and an empty storage map — so a contract that reads its sequence, timestamp or network ID traps before it does any work. A snapshot whose `protocol_version` is not this build's host protocol is refused by name, because the cost tables the profile reports belong to the protocol that ships them. A *storage* read still stops at the contract frame the profiler never pushes — same link as `--args`. Refused next to `compare`, which runs nothing. |
| `-o, --output <PATH>` | the format's own name | Where the artifact is written, or `-` for stdout. Omit it and the name follows `--format`: `profile.folded`, `profile.json`, `profile.raw`. |
| `--metric <METRIC>` | `cpu` | `cpu`, `memory` or `hostcalls`. Sets what the counts in the file are denominated in; a `.folded` file does not record which, so both sides of a `compare` must have agreed on this flag beforehand. `--format json` writes the metric into the document; `--format raw` ignores it, because a trace event carries its cpu and memory deltas unselected. |
| `--format <FORMAT>` | `folded` | `folded`, `json` or `raw` — how the run's result is serialized. See [Three output formats](#three-output-formats). |
| `--sample-rate <N>` | `1000` | Record one trace event every N traced steps; `0` is rejected, because it would silently turn sampling off and buffer every step. Today the engine reports one step per call boundary, so this flag changes *when* events are emitted rather than what they measure — see [Limitations](#the-counts-are-boundary-counts-not-instructions). |
| `--instruction-limit <N>` | `100000000` | The bound on the trace buffer, now yours to set: the run stops past this many traced steps, the partial trace is still written, and the exit is `1` with `Instruction ceiling exceeded`. `0` is rejected — the counter increments before it compares, so a ceiling of 0 would stop the run at its first boundary. Same caveat as `--sample-rate`: steps are boundaries, so raising this lets a *boundary*-heavy contract finish and does nothing for a loop that never calls anything. |
| `-v, --verbose` | off | Print the profiler's internal progress on **stderr**: `-v` the stages a run passes through, `-vv` every call boundary the engine reports, `-vvv` the costed step recorded at each one. Plain runs print none of it — see [What the two streams are for](#what-the-two-streams-are-for). |
| `-q, --quiet` | off | Write the artifact and print nothing on stdout. Not a silence button: `warning:` lines about a degraded run, every `error:`, and `compare`'s table still print, because those are news rather than narration. Refused next to `-v`. |
| `compare <BASE> <CURRENT>` | — | The second mode: reads two `.folded` files, prints the functions whose cost moved, biggest move first. |

`--help` prints these with their long-form notes and the exit-code table; `-h` is the short version;
`--version` prints the crate name and version. When a run does not look right,
[docs/troubleshooting.md](docs/troubleshooting.md) works through every message this tool writes, one symptom
at a time, and says which of them are the engine's ceilings rather than your contract.

### A real run, start to finish

Against the fixture this repository commits for source-mapping tests:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
no function recorded any exclusive cost (cpu)
$ cat profile.folded
wasm[0] 0
```

That one line is what the tool produces today, and the terminal says so instead of printing an empty
table: `wasmi`'s call hook reports no program counter, so frames arrive at `wasm[0]` and cost columns read
`0` — the finding `ROADMAP.md`'s metering probes recorded and pinned in a test. Until an address reaches a
frame there is nothing to attribute, which is exactly why the summary exists: a profile of zeros and a
profiler that never ran are otherwise indistinguishable.

A binary built without line tables still profiles; it just cannot name source, and it tells you so on
**stderr** while stdout stays clean for the summary:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe_no_debug.wasm --fn caller_of_heavy
warning: this artifact has a `name` section but no DWARF line tables, so frames will name functions and
never `file:line`. Build the copy you profile with a profiling profile — `[profile.profiling]` with
`inherits = "release"` and `debug = "line-tables-only"` — and keep `debug` out of `[profile.release]`: that
is the profile whose output gets deployed, and mainnet bills for the extra bytes.
no function recorded any exclusive cost (cpu)
```

### Three output formats

`--format` chooses how the run's result is serialized. Three values, three different questions:

| `--format` | Writes | Default file | For |
|---|---|---|---|
| `folded` (default) | the call tree, one line per stack path | `profile.folded` | speedscope.app, `flamegraph.pl`, `compare` |
| `json` | the call tree as structured data | `profile.json` | a script or CI job that walks frames |
| `raw` | the event stream, one line per recorded event | `profile.raw` | finding out what the engine actually reported |

The name follows the format when `--output` is omitted, because the three are not interchangeable
downstream: `compare` reads collapsed stacks and would call a JSON document malformed on its first line.

`json` carries the two things a stack path cannot: which `--metric` the counts are denominated in, and
`file:line` per frame.

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --format json
no function recorded any exclusive cost (cpu)
$ cat profile.json
{
  "metric": "cpu",
  "root": {
    "children": [],
    "exclusive": {
      "cpu": 0,
      "hostcalls": 0,
      "memory": 0
    },
    "file": null,
    "function": "wasm[0]",
    "inclusive": {
      "cpu": 0,
      "hostcalls": 0,
      "memory": 0
    },
    "line": null
  }
}
```

`"metric"` is the field a `.folded` file has nowhere to put, which is why both sides of a `compare` have to
agree on `--metric` in advance and nothing can check it afterwards. `"file"` and `"line"` are `null` here
because this fixture's frames arrive with no program counter — see
[Every frame lands at `wasm[0]`](#every-frame-lands-at-wasm0). `"children"` is an array sorted by function
name and never a map's iteration order, so two runs of one contract write byte-identical JSON and a diff of
the artifact says something about the contract instead of about the allocator.

`raw` is stage 1's output: `kind pc=<n> cpu=<n> mem=<n>` per event, before symbolization and before the tree
is built. It takes no `--metric` — each event carries its own cpu and memory deltas, unselected — and prints
no ranking, because nothing was aggregated. It is also the one format that never warns about debug info: the
symbolization stage is not run at all, since the format has no field a name could go in, so #186's `warning:`
on a stripped binary would be advice about a column that does not exist.

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --format raw
2 trace events written to profile.raw
$ cat profile.raw
call pc=0 cpu=0 mem=0
return pc=0 cpu=0 mem=0
```

Those two lines are the whole of what the engine reports for this call, and therefore the reason the profile
is one frame of zeros: a `call` and a `return`, no program counter, nothing between them. The same run at
`--sample-rate 1` writes **four** events across the same two boundaries — two `step` lines appear between
them, each `cpu=1` — because call and return are emitted unconditionally while steps are what the throttle
controls. That pair, read straight off the artifact, is
[the counts-are-boundary-counts finding](#the-counts-are-boundary-counts-not-instructions) with no prose
attached to it.

`--output -` sends the artifact to stdout and prints nothing else, which is how a profile becomes a pipe
stage instead of a file:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy \
    --format json --output - | jq .metric
"cpu"
```

The summary stays silent there because two documents in one stream parse as neither; warnings, errors and the
exit code are unchanged. `-` is a convention and not a file — no file named `-` appears. A run that traps while
writing to a stream says so about the stream (`The partial trace up to the trap is on stdout`), and a refused
write names the document it was attempting (`failed to write JSON call tree to …`).

### What the two streams are for

Three kinds of text leave this tool, and they do not share a stream:

| | stdout | stderr |
|---|---|---|
| the ranked summary, `compare`'s table | always (unless `--quiet`) | — |
| `warning:` about a degraded run, `error:` about a refused input | never | always |
| `tracing` records about the profiler's own progress | never | only with `-v` |

The last row is #214, and it is the one worth a measured transcript, because the default is
*nothing*: every stage of this pipeline has logged through `tracing` since the tracer was written, and
until a subscriber was installed those records were discarded. `-v` is the subscriber, on stderr, with
no ANSI escapes so it stays greppable:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy -v 2>&1 >/dev/null
2026-10-08T06:05:58.562669Z  INFO soroban_cost_profiler::tracer: Loading WASM file from fixtures/dwarf_probe/dwarf_probe.wasm
2026-10-08T06:05:58.562988Z  INFO instantiate_module: soroban_cost_profiler::tracer: Instantiating WASM module
2026-10-08T06:05:58.569862Z  INFO invoke_function{func_name="caller_of_heavy"}: soroban_cost_profiler::tracer: Invoking function: caller_of_heavy
```

The timestamp is UTC ISO-8601 and the `invoke_function{…}` prefix is the span the record was emitted
inside, both from `tracing_subscriber`'s default format. `-vv` adds each call boundary the engine
reports (`DEBUG … WASM Call at PC: 0`), and `-vvv` adds the single costed step recorded at each one
(`TRACE … Stepping at PC: 0, cpu: 1, mem: 0`) — which is also the clearest statement of
[the ceiling this tool works under](#the-counts-are-boundary-counts-not-instructions): two boundaries
for a function that calls two helpers, because the engine hands the tracer nothing between them.

`--quiet` suppresses the first row and nothing else. The name is the conventional Unix one — the
artifact is the answer, so stdout goes empty — while warnings, errors, and `compare`'s table stay,
because a run that worked less than you asked for is news:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --quiet
$ cat profile.folded
wasm[0] 0
```

`-v` and `--quiet` contradict each other, so clap refuses the pair rather than picking a winner
silently (exit `1`, like any other command line to fix).

### Comparing two runs

`compare` reads two files and runs no contract, so it takes no `--wasm`. Its table is exclusive cost per
function — each stack's cost is attributed to the frame that was actually running, not to everything above
it — with both counts printed beside the move, because `+51489` alone cannot say whether that is noise on a
1.5-million-instruction loop or the whole of a small function.

Sample input, the pair a before/after change would produce (`wasm[0];…` is the stack path, the number is
its cost):

```console
$ cat before.folded
wasm[0];caller_of_heavy 1512612
wasm[0];caller_of_heavy;memory_heavy_loop 448512
wasm[0];legacy_pack 79210

$ cat after.folded
wasm[0];caller_of_heavy 1200000
wasm[0];caller_of_heavy;memory_heavy_loop 500001
wasm[0];packed_reader 12000

$ soroban-cost-profiler compare before.folded after.folded
Cost comparison, baseline → current (exclusive cost per function):
 caller_of_heavy    1512612  1200000  -312612
 legacy_pack          79210        0   -79210
 memory_heavy_loop   448512   500001   +51489
 packed_reader            0    12000   +12000
 total              2040334  1712001  -328333
4 of 4 functions changed cost, 0 unchanged
```

Functions that did not move are left out of the table but counted in the last line, so an empty table cannot
be mistaken for a broken one. A function that appears on one side only is a row against `0`, not an absence.
Regressions print red and improvements green **only when the output is a terminal**, so piping the report
into a file or `grep` stays plain text.

### The `.folded` file

One line per call path: `<frame>;<frame>;<frame> <cost>` — the stack path, a space, the cost. `;` marks a
call boundary, and repeated paths are summed when a viewer merges lines. This is the standard collapsed-stack
format, so [speedscope.app](https://www.speedscope.app) opens it directly and `flamegraph.pl` turns it into a
picture.

For a picture of a *change* rather than a run, the crate can also write the `<stack> <baseline> <current>`
form `flamegraph.pl --diff` reads (`OutputFormatter::to_differential_folded`, covered by
`tests/differential.rs`). That is a library call today, not a flag — the CLI's own way to look at two runs is
the `compare` table above.

### Exit codes

| Code | Meaning |
|---|---|
| `0` | The run was honoured as asked — including a `compare` that reports a regression, because bad news is still an answer. |
| `1` | The invocation could not be honoured as asked: a contract that cannot be read, parsed or linked, an export the module does not have, a contract that trapped, a `.folded` file that is missing or malformed, or a refused flag. |
| `2` | The input was accepted and the profiler could not finish its own work: a write the machine refused for a reason other than the path, or an engine that would not configure. |

A contract that traps keeps the trace it collected up to the trap and writes it, and still exits `1`: the
contract failed, the profiler did not.

### The `debug` precondition

The profiler requires zero *code* instrumentation, but a build with no debug info gives you frames named
by address instead of `file:line`. Soroban contracts are typically stripped for size, so ask for line
tables in a profile that never ships:

```toml
[profile.profiling]
inherits = "release"
debug = "line-tables-only" # enough for source mapping, far cheaper than full -g
```

```sh
cargo build --profile profiling --target wasm32-unknown-unknown
```

> [!CAUTION]
> **Deployment safety:** do **not** add `debug` to your main `[profile.release]`. A contract deployed with
> debug tables pays mainnet fees for the binary bloat. `[profile.profiling]` exists so the build you profile
> is not the build you deploy.

> [!WARNING]
> **Two caveats:**
> 1. **Downstream stripping:** if you use `stellar contract build` instead of `cargo build`, downstream tools
>    (like `wasm-opt`) may still strip debug sections regardless of your `Cargo.toml`. Both halves of that
>    sentence — what survives, and what survives *broken* — are measured in
>    [Limitations](#the-build-pipeline-can-leave-the-binary-unmappable-and-say-nothing).
> 2. **Inlining (LTO):** preserving line tables does *not* stop the compiler from inlining aggressively. A
>    heavily optimized loop can map to one line, which is correct but coarser than you expected.

> [!IMPORTANT]
> **A real `soroban-sdk` build runs.** `instantiate_module` links the Soroban host interface — all 199
> functions of `soroban-env-host`'s table, bound to the same `Host` the network runs (`src/host.rs`) — so a
> contract that uses `Vec`, `Map` or `obj_from_u64` instantiates, executes, and leaves costed `host[…]`
> frames in the profile. Chain state comes from `--state`, which builds the host from a ledger snapshot
> (`src/state.rs`); what is still out of reach is a *storage* read, which stops at the contract frame these
> bindings never push, and `call` to another contract, which returns a host error instead of recursing. An
> import from outside that interface, such as a JS shim's `(import "env" "missing" …)`, still stops the run
> before the export is called.

## Limitations

The pipeline is complete and tested — load, trace, symbolize, aggregate, format, diff — and the ceiling is
at its *input*: `wasmi` 2.0 exposes no instruction-level hook, so what the tracer can see is a list of call
boundaries rather than the instructions between them. Every limitation below is either a consequence of that
or a measured property of the build pipeline, and each names what would lift it. Read this before you read a
number out of a `.folded` file.

### The counts are boundary counts, not instructions

The same contract, the same build, twice — the second with the sample rate forced down to one:

```console
$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy
no function recorded any exclusive cost (cpu)
$ cat profile.folded
wasm[0] 0

$ soroban-cost-profiler --wasm fixtures/dwarf_probe/dwarf_probe.wasm --fn caller_of_heavy --sample-rate 1
Top 1 functions by exclusive cost (cpu):
  1. wasm[0]  2
$ cat profile.folded
wasm[0] 2
```

That `2` is not a measurement of work. It is the number of boundaries the engine reported for one
host-initiated call — `CallingWasm` and `ReturningFromWasm` — each charged one synthetic unit, because
`invoke_function`'s hook has no instruction hook to hang a real count on and substitutes one step per
boundary (`src/tracer.rs:357-360`). At the default `--sample-rate 1000` the accumulator gains 1 per boundary
and so never reaches the threshold, which is why the first run writes `0` rather than a small number.

Consequences worth stating plainly:

* `--metric cpu`, `--metric memory` and `--metric hostcalls` all produced `wasm[0] 0` for that run, measured
  on the same binary — because that export calls no host function. Memory bytes and host-call counts reach the
  tree only through host frames: the budget deltas read around a host call (`record_host_return`,
  `src/tracer.rs:176-191`) and the `HostCall` events that open them. Give the profiler an export that does
  call the host and all three metrics have numbers, measured on `fixtures/build.sh`'s artifact:
  `memory_heavy_loop` at 100 iterations is `host[0] 125022` in cpu, `host[0] 50080` in memory and
  `host[0] 102` in host calls — the 102 being exactly `vec_new` + 100 × `vec_push_back` + `vec_len`. So the
  zeros above are a property of the contract, not of the tool.
* The numbers are a **floor and a shape**, not a budget reading. Use `soroban-budget-assert` (Tier 2) for the
  instructions the network will actually charge you; this profiler is for *where* to look.
* Nothing here is invented to look better: `wasm[0] 0` is what the Getting Started transcript shows, and a
  test pins that exact string so PC attribution cannot land as a silent change (`tests/cli_e2e.rs`).

### Every frame lands at `wasm[0]`

The call hook is handed the hook *variant* and nothing else — no callee, no offset — so all events are
recorded at `pc = 0` (`src/tracer.rs:344-355`). And an internal instruction pointer would not fix it either:
`wasmi` re-encodes wasm bytecode into its own instruction stream during translation and keeps no table back
to the original offsets, so the finest address any future hook could hand this profiler is a function body's
start.

This is the reason source mapping exists as a separate, complete stage rather than a nice-to-have:
`CodeMap` indexes exactly that body-start space, and `tests/source_map_fixture.rs` checks resolved addresses
against the *text* of the source they came from. When an address reaches a frame, the `file:line` half is
already built and correct; today the trace never asks for anything but `0`.

### Only the outer call is traced

`Store::call_hook` fires for the host-initiated call into wasm, not for calls made from inside running wasm.
A contract whose entry point calls five helpers yields one `Call`/`Return` pair, not six, so the call tree
the aggregator rebuilds is one level deep no matter how deep the contract goes. The probe
`only_the_outer_invocation_is_recorded_as_a_boundary` in `tests/meter_probe.rs` pins it, and the doc block on
`invoke_function` (`src/tracer.rs:329-371`) records both the limitation and what to change rather than delete
when a per-call hook exists. `docs/internals/call_boundaries.md` describes the two boundary *types* the trace
does distinguish — wasm calls and host transitions.

### A real contract runs; chain state is what it cannot read

The host interface is bound — `src/host.rs` generates one registration per entry of
`soroban-env-host`'s table, 199 of them, and calls the real `Host` through it — so a `soroban-sdk` build
instantiates, executes, and is traced. Three ceilings remain, all of them the parts of a network run this
tool does not stand up:

* **No contract frame.** `--state <snapshot.json>` now supplies the ledger itself (`src/state.rs`): its
  `LedgerInfo` goes into the host, which answers the five context-free reads of it, and the snapshot's
  entries are installed as the storage the host reads through. What no state file can supply is the
  *contract frame* a host call normally runs inside, because the profiler invokes an export from outside a
  contract call. A read whose ledger key is built from the current contract ID — `get_contract_data` and its
  siblings, `require_auth` — stops on that empty stack and never reaches the file.
* **No contract-to-contract `call`.** Production hands its dispatch a live engine caller so a contract can
  re-enter wasm; the `Env` methods bound here run with none, so those two functions return a host error
  instead of recursing. A single-module trace never reaches them.
* **Arguments are words, not numbers.** On an SDK build a parameter arrives as a tagged `Val`, which is what
  the `--args` note below says out loud with the measured pair.

The ledger half is a pair of runs on one 55-byte module,
`(module (import "x" "3" (func (result i64))) (func (export "read_sequence") (result i64) call 0))` — `x.3`
is `get_ledger_sequence` — and the snapshot this repository commits at `fixtures/state/ledger.json`, with the
no-debug warning elided because the interesting halves are the exit code and whether a file appears:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --output blank.folded
$ echo $?
1
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state fixtures/state/ledger.json --output mocked.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
$ cat mocked.folded
wasm[0] 0
wasm[0];host[0] 0
```

The first run's `error:` line is the host's own refusal, quoted whole because its shape surprises people:

```console
error: 'read_sequence' trapped: host function 'x.3' failed: HostError: Error(Context, InternalError)
DebugInfo not available
. The partial trace up to the trap is in blank.folded, and its costs are incomplete because the call never returned.
```

`DebugInfo not available` is the host's sentence and not a defect here: the reason it would print after it is
a `DebugInfo` that `soroban-env-host` builds only with its own `testutils` feature, which this crate does not
enable — that feature is what pulls `arbitrary` into the tree, and `AGENTS.md` rule 3 keeps it out. So
`Error(Context, InternalError)` is all a refusal says, which is why the two refusals in this list — no ledger
and no frame — read identically from a terminal. `-v` marks the run that did get a ledger, on stderr:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state fixtures/state/ledger.json --output mocked.folded -v 2>&1 | grep mocked
2026-10-08T13:19:23.247313Z  INFO soroban_cost_profiler: mocked ledger state from fixtures/state/ledger.json: sequence 500, timestamp 1700000000, 2 entries
```

Two command lines are refused before the contract is read, because the run they describe is not the run the
file belongs to. `notes.json` here is `{"todo":"x"}` and `ledger24.json` is this repository's snapshot with
`protocol_version` changed to `24`:

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

Re-stamping a protocol would price the contract with another protocol's cost tables and report them as the
ledger it was given, which is the one thing a profile cannot survive.

Passing *values* is not one of them any more: `--args 1000,7` hands arguments to an export that takes
parameters. Two things about it are worth knowing before a run looks wrong:

The check happens **before** the call, against the module's own signature. This same 46-byte
`(module (func (export "needs_arg") (param i64) (result i64) local.get 0 i64.const 2 i64.add))` module, with
the no-debug warning elided because the interesting half is the exit code and whether a file appears:

```console
$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --output out.folded
error: 'needs_arg' takes 1 argument (i64); --args gave no values. `--args` is one value per parameter, in
the order the signature lists them.
$ echo $?
1
$ ls out.folded
ls: out.folded: No such file or directory
$ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --args 40 --output out.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
$ cat out.folded
wasm[0] 0
```

Before [issue 211](https://github.com/Tollcraft/soroban-cost-profiler/issues/211) the first command was a
*trap* — `encountered an incorrect number of parameters`, exit `1`, and an `out.folded` beside it. A profile
of a call that was never legal is the worst artifact this tool can write, which is why the refusal is an
input error and writes nothing.

And the values are `i64`. A parameter of another width is named and refused rather than guessed at, so the
flag cannot silently coerce `--args 1` into an `i32`. On an SDK build that leaves one thing to know: the word
is what the *guest* reads, and the guest reads a tagged `Val`. `compute_heavy_loop(iterations: u32)` wants
`U32Val(10000)` — `(10000 << 32) | 4` — not `10000`. Measured on the artifact `fixtures/build.sh` leaves,
whose `compute_heavy_loop` is indeed `(i64) -> i64` so both commands satisfy the arity check:

```console
$ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn compute_heavy_loop --args 10000 --output out.folded
error: 'compute_heavy_loop' trapped: wasm `unreachable` instruction executed. The partial trace up to the
trap is in out.folded, and its costs are incomplete because the call never returned.
$ echo $?
1
$ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn compute_heavy_loop --args 42949672960004 --output out.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
```

The first is the contract rejecting a value whose tag says nothing to it, after the run started — so it
traps rather than refuses, and writes the partial trace #173 asks for. The arity check cannot see the
difference: it reads the module's signature, and the signature says `i64` either way.

One export per run is by design rather than a gap: a trace of a whole transaction is a different artifact
from a profile of a function, and the file format already supports the nested case.

### The build pipeline can leave the binary unmappable and say nothing

The `debug` precondition above is the *easy* case: no line tables, an error fires, you are told. The hard
case is a binary whose DWARF loads and is wrong. Measured in
[`docs/spikes/02_wasm_name_section_fallback.md`](docs/spikes/02_wasm_name_section_fallback.md), on the
committed `dwarf_probe.wasm`:

| Applied to the artifact | `.debug_*` | `name` | Addresses that resolve |
|---|---|---|---|
| nothing (as built) | 1,002 B | present | 160 of 166 probed |
| `wasm-opt -O0` | 855 B, **stale** | **gone** | **0 of 152** |
| `wasm-opt -Oz` | 855 B, **stale** | **gone** | **0 of 145** |
| `wasm-opt -Oz -g` | 1,098 B | present | 140 of 145 |
| `wasm-opt --strip-dwarf` | none | gone | not applicable |

`SourceMapper::new` finds `.debug_info` after `wasm-opt`, gimli parses it, `has_debug_info()` returns `true`,
and **every lookup returns nothing** — the line table describes the pre-optimization code section the
optimizer rewrote. No error fires, because there is nothing structurally wrong to fire on. The defence is
[#162](https://github.com/Tollcraft/soroban-cost-profiler/issues/162)'s ratio check: every tenth address of
the code section is sampled, and if more than 90% of them map to no line or to a line another address already
claimed, the run warns that the DWARF describes different code from the bytes that ran. It is the only signal
for this case, so a run that warns about *coverage* is not broken — it is telling you the artifact is.

In short: build the profiling profile yourself with `cargo build --profile profiling`, and if you must run
Binaryen, pass `-g`. `stellar contract optimize` passes no `-g` today, so its output is unmappable by either
source. Two smaller edges from the same measurement: `--strip-debug`/`--strip-dwarf` drop DWARF *and* the
`name` fallback together, so "just strip it" recipes lose both; and the paths in the tables are the absolute
ones from whatever machine ran `rustc`, which is why file matching in the tests is by suffix.

### The 100M ceiling cannot see an infinite loop

The MVP's memory rule (`AGENTS.md` rule 5: a contract can run 100M instructions, so nothing may allocate
per instruction) is why a 100M ceiling exists, and `record_step` enforces it (`src/tracer.rs:97`). Its limit
is now a flag — `--instruction-limit`, [#213](https://github.com/Tollcraft/soroban-cost-profiler/issues/213)
— but the flag does not change what the counter counts: its only caller in the live path is the call hook,
which runs once per boundary —
so the counter advances per boundary, not per instruction. A contract that loops forever *inside* one
function body emits no boundaries, never advances the counter, and is not stopped; `wasmi`'s own fuel is set
to `u64::MAX` for the run (`src/main.rs:608-613`), so the engine does not stop it either.

This is the sharpest edge in the tool, and it is the one place where the roadmap's
"Infinite Loop Protection" box reads more strongly than the current engine can deliver — `ROADMAP.md` now
annotates it. The guard is real for the tracing buffer it was written to protect (a run with many boundaries
cannot grow the `Vec` unboundedly, and `--instruction-limit` is what bounds it) and inert against a
compute-only runaway loop. A ceiling that also halts execution needs an instruction hook, and `wasmi` 2.0
does not have one to expose — no open issue in this repository owns that, it is the engine's gap that every
other limitation here traces back to. The flag does have a measurable effect today on contracts that make
many host calls, now that those calls link: the fixture's `memory_heavy_loop` crosses 102 of them at 100
iterations, and on the smaller fixture the pair `--instruction-limit 1` / `--instruction-limit 2` is exactly
the difference between a halted run and a finished one (`tests/cli_e2e.rs`). Until then: profile exports that
terminate, and prefer the fixture-sized contracts this repository tests against.

### What is *not* a limitation

No macros, no test hooks, no recompiled-with-instrumentation source — a standard `cargo build --profile
profiling` artifact is enough, which is the whole point of the zero-instrumentation rule. Output is plain
collapsed-stack text that [speedscope.app](https://www.speedscope.app) opens and `flamegraph.pl` pictures;
no SVG renderer is bundled and none is promised. Exit codes are the documented `0`/`1`/`2`, a trap writes
the partial trace it earned, and `compare` reports a regression as an answer rather than a failure.

## Contributing

We are actively looking for contributors in cost-model research, WASM tracing, and source mapping.

1. Check the open issues to find tasks labeled `good first issue` or `help wanted`.
2. Fork the repository.
3. Ensure all Pull Requests target the `main` branch.
4. Pass all local tests before submitting.

See [CONTRIBUTING.md](CONTRIBUTING.md) for more detailed guidelines.

## Community

Join the discussion on our [Discord](https://discord.gg/5aprtMSyR).

Participation in this repository, its issue tracker and that Discord is governed by our
[Code of Conduct](CODE_OF_CONDUCT.md) — the Contributor Covenant, version 2.1. Reports go to the
maintainers listed below, by direct message, so they are private by default.

## Maintainers

| Name | Role | Contact |
|---|---|---|
| Tollcraft Team | Core Maintainers | [Tollcraft on Telegram](https://t.me/+Gflo5jZStw1jMjE0) |

## Contributors

<p align="center">
  <a href="https://github.com/wagmiiii"><img src="https://github.com/wagmiiii.png" width="64" height="64" alt="wagmiiii" title="wagmiiii" /></a>
  <a href="https://github.com/mallison031"><img src="https://github.com/mallison031.png" width="64" height="64" alt="mallison031" title="mallison031" /></a>
  <a href="https://github.com/bolabeatz"><img src="https://github.com/bolabeatz.png" width="64" height="64" alt="bolabeatz" title="bolabeatz" /></a>
  <a href="https://github.com/Gabbydunkk"><img src="https://github.com/Gabbydunkk.png" width="64" height="64" alt="Gabbydunkk" title="Gabbydunkk" /></a>
  <a href="https://github.com/composure-lad"><img src="https://github.com/composure-lad.png" width="64" height="64" alt="composure-lad" title="composure-lad" /></a>
  <a href="https://github.com/king-daveed"><img src="https://github.com/king-daveed.png" width="64" height="64" alt="king-daveed" title="king-daveed" /></a>
  <a href="https://github.com/allison"><img src="https://github.com/allison.png" width="64" height="64" alt="allison" title="allison" /></a>
  <a href="https://github.com/TemiMustapha"><img src="https://github.com/TemiMustapha.png" width="64" height="64" alt="TemiMustapha" title="TemiMustapha" /></a>
  <a href="https://github.com/abrcrmb"><img src="https://github.com/abrcrmb.png" width="64" height="64" alt="abrcrmb" title="abrcrmb" /></a>
  <a href="https://github.com/achiever2110"><img src="https://github.com/achiever2110.png" width="64" height="64" alt="achiever2110" title="achiever2110" /></a>
  <a href="https://github.com/anitahhhh"><img src="https://github.com/anitahhhh.png" width="64" height="64" alt="anitahhhh" title="anitahhhh" /></a>
  <a href="https://github.com/simplex001"><img src="https://github.com/simplex001.png" width="64" height="64" alt="simplex001" title="simplex001" /></a>
  <a href="https://github.com/Teescom"><img src="https://github.com/Teescom.png" width="64" height="64" alt="Teescom" title="Teescom" /></a>
  <a href="https://github.com/Ayomikun2005"><img src="https://github.com/Ayomikun2005.png" width="64" height="64" alt="Ayomikun2005" title="Ayomikun2005" /></a>
  <a href="https://github.com/YazarAyobami"><img src="https://github.com/YazarAyobami.png" width="64" height="64" alt="YazarAyobami" title="YazarAyobami" /></a>
</p>

<p align="center">
  <sub>Fifteen people, in commit-count order, read from the GitHub contributors API on 2026-10-08.
  Avatars are served by GitHub directly (<code>github.com/&lt;user&gt;.png</code>) rather than by a
  third-party badge service, so the grid cannot drift behind the repository's own history. Excluded:
  <a href="https://github.com/web-flow"><code>web-flow</code></a>, GitHub's own merge bot. See the full
  <a href="https://github.com/Tollcraft/soroban-cost-profiler/graphs/contributors">contributors graph</a>.</sub>
</p>

## License

Licensed under the Apache License, Version 2.0 — see [LICENSE](LICENSE). The badge at the top of
this page links to the same file; `Cargo.toml` carries the matching `license = "Apache-2.0"`.
