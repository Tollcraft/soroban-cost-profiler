<div align="center">
  <h1>soroban-cost-profiler</h1>
  <p><strong>Visual flamegraphs and execution tracing for Soroban smart contracts</strong></p>
  <p>
    <img src="https://img.shields.io/github/actions/workflow/status/Tollcraft/soroban-cost-profiler/ci.yml?branch=main" alt="CI Status" />
    <img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License" />
  </p>
  <p>
    <a href="https://tollcraft.gitbook.io/docs"><strong>Documentation</strong></a> ·
    <a href="#"><strong>Demo</strong></a>
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

# after changing the contract: profile again, then diff the two runs
soroban-cost-profiler --wasm contract.wasm --fn call --output after.folded
soroban-cost-profiler compare before.folded after.folded
```

### Flags

| Flag | Default | Notes |
|---|---|---|
| `-w, --wasm <PATH>` | — | The compiled contract. Required for profiling; not accepted next to `compare`, which runs nothing. |
| `--fn <EXPORT>` | — | The exported function to invoke. Profiling refuses to start without a name, and a name the module does not export is an error that lists the exports it does have. |
| `-o, --output <PATH>` | `profile.folded` | Where the collapsed stacks are written. |
| `--metric <METRIC>` | `cpu` | `cpu`, `memory` or `hostcalls`. Sets what the counts in the file are denominated in; a `.folded` file does not record which, so both sides of a `compare` must have agreed on this flag beforehand. |
| `--sample-rate <N>` | `1000` | Record one trace event every N traced steps; `0` is rejected, because it would silently turn sampling off and buffer every step. Today the engine reports one step per call boundary, so this flag changes *when* events are emitted rather than what they measure — see [Limitations](#the-counts-are-boundary-counts-not-instructions). |
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
> **A contract that imports host functions does not run yet.** `instantiate_module` links against an empty
> linker, so a full `soroban-sdk` build — which imports the Soroban environment interface — stops with
> `failed to instantiate module: cannot find definition for import …` before its export is called. Pure
> computation exports profile today; wiring the host bindings is
> [#210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210).

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
boundary (`src/tracer.rs:352-356`). At the default `--sample-rate 1000` the accumulator gains 1 per boundary
and so never reaches the threshold, which is why the first run writes `0` rather than a small number.

Consequences worth stating plainly:

* `--metric cpu`, `--metric memory` and `--metric hostcalls` all produced `wasm[0] 0` for that run, measured
  on the same binary. Memory bytes and host-call counts reach the tree only through host frames — the budget
  deltas read around a host call (`record_host_return`, `src/tracer.rs:172-187`) and the `HostCall` events
  that open them — and no host frame can open while nothing links a host function. So until
  [#210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210) lands, those two metrics are
  structurally empty rather than merely small.
* The numbers are a **floor and a shape**, not a budget reading. Use `soroban-budget-assert` (Tier 2) for the
  instructions the network will actually charge you; this profiler is for *where* to look.
* Nothing here is invented to look better: `wasm[0] 0` is what the Getting Started transcript shows, and a
  test pins that exact string so PC attribution cannot land as a silent change (`tests/cli_e2e.rs`).

### Every frame lands at `wasm[0]`

The call hook is handed the hook *variant* and nothing else — no callee, no offset — so all events are
recorded at `pc = 0` (`src/tracer.rs:306-313`). And an internal instruction pointer would not fix it either:
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
`invoke_function` (`src/tracer.rs:289-322`) records both the limitation and what to change rather than delete
when a per-call hook exists. `docs/internals/call_boundaries.md` describes the two boundary *types* the trace
does distinguish — wasm calls and host transitions.

### Contracts that import the Soroban host do not run

Linked to the empty `Linker` (`src/tracer.rs:171`), this is the blocker the IMPORTANT note above points at,
and it has two neighbours of the same kind, both measured on the built binary:

* **No arguments are passed.** The export is invoked with an empty parameter list, so a contract export that
  takes arguments ends the run before it starts. Against a 43-byte `(func (export "needs_arg") (param i64)
  (result i64))` module — whose no-debug warning is elided here, since the interesting half is the exit code:

  ```console
  $ soroban-cost-profiler --wasm needs_arg.wasm --fn needs_arg --output out.folded
  error: 'needs_arg' trapped: encountered an incorrect number of parameters. The partial trace up to the
  trap is in out.folded, and its costs are incomplete because the call never returned.
  $ echo $?
  1
  ```

  Passing values is [`--args` (issue 211)](https://github.com/Tollcraft/soroban-cost-profiler/issues/211).
* **No ledger state.** There is no `--state`, no network and no snapshot, so anything reading storage has
  nothing to read — [issue 212](https://github.com/Tollcraft/soroban-cost-profiler/issues/212).

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
per instruction) is why a 100M ceiling exists, and `record_step` enforces it (`src/tracer.rs:93`). But its
only caller in the live path is the call hook, which runs once per boundary —
so the counter advances per boundary, not per instruction. A contract that loops forever *inside* one
function body emits no boundaries, never advances the counter, and is not stopped; `wasmi`'s own fuel is set
to `u64::MAX` for the run (`src/main.rs:348-350`), so the engine does not stop it either.

This is the sharpest edge in the tool, and it is the one place where the roadmap's
"Infinite Loop Protection" box reads more strongly than the current engine can deliver — `ROADMAP.md` now
annotates it. The guard is real for the tracing buffer it was written to protect (a run with many boundaries
cannot grow the `Vec` unboundedly) and inert against a compute-only runaway loop. A ceiling that also halts
execution needs the instruction hook, and
[issue 213](https://github.com/Tollcraft/soroban-cost-profiler/issues/213) makes its limit configurable once
there is something for it to bound. Until then: profile exports that terminate, and prefer the fixture-sized
contracts this repository tests against.

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

[![Contributors](https://contrib.rocks/image?repo=Tollcraft/soroban-cost-profiler)](https://github.com/Tollcraft/soroban-cost-profiler/graphs/contributors)
