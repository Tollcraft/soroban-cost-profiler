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
[speedscope.app](https://www.speedscope.app) (`File → Open`), or hand it to
`flamegraph.pl` for an SVG of your own — this tool deliberately writes text and no pictures.

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
| `--sample-rate <N>` | `1000` | Record one trace event every N instructions. Smaller is a denser trace and a bigger file; `0` is rejected, because it would silently turn sampling off and buffer every instruction. |
| `compare <BASE> <CURRENT>` | — | The second mode: reads two `.folded` files, prints the functions whose cost moved, biggest move first. |

`--help` prints these with their long-form notes and the exit-code table; `-h` is the short version;
`--version` prints the crate name and version.

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
>    (like `wasm-opt`) may still strip debug sections regardless of your `Cargo.toml`. We are actively
>    investigating reliable CLI flags.
> 2. **Inlining (LTO):** preserving line tables does *not* stop the compiler from inlining aggressively. A
>    heavily optimized loop can map to one line, which is correct but coarser than you expected.

> [!IMPORTANT]
> **A contract that imports host functions does not run yet.** `instantiate_module` links against an empty
> linker, so a full `soroban-sdk` build — which imports the Soroban environment interface — stops with
> `failed to instantiate module: cannot find definition for import …` before its export is called. Pure
> computation exports profile today; wiring the host bindings is
> [#210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210).

## Contributing

We are actively looking for contributors in cost-model research, WASM tracing, and source mapping.

1. Check the open issues to find tasks labeled `good first issue` or `help wanted`.
2. Fork the repository.
3. Ensure all Pull Requests target the `main` branch.
4. Pass all local tests before submitting.

See [CONTRIBUTING.md](CONTRIBUTING.md) for more detailed guidelines.

## Community

Join the discussion on our [Discord](https://discord.gg/5aprtMSyR).

## Maintainers

| Name | Role | Contact |
|---|---|---|
| Tollcraft Team | Core Maintainers | [Tollcraft on Telegram](https://t.me/+Gflo5jZStw1jMjE0) |

## Contributors

[![Contributors](https://contrib.rocks/image?repo=Tollcraft/soroban-cost-profiler)](https://github.com/Tollcraft/soroban-cost-profiler/graphs/contributors)
