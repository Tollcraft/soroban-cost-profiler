# Tutorial: a contract, a profile, a picture

Ten minutes, and you end up with a `.folded` file of your own contract open in a flamegraph viewer.
Nothing here instruments your code: the profiler reads the wasm you already build.

Read [the Limitations section](../README.md#limitations) first if you want the short version of what the
picture can say today; this file shows you the whole path and lets you see it for yourself.

**You need:** Rust 1.85 or newer, the `wasm32-unknown-unknown` target, and a browser.

```sh
rustup target add wasm32-unknown-unknown   # if you do not have it
```

## 1. Build the profiler

From a checkout of this repository:

```sh
cargo build --release
./target/release/soroban-cost-profiler --version
```

```console
soroban-cost-profiler 0.1.0
```

That is the whole install: one binary, no macros to add to your contract, no test hooks, no recompile with
instrumentation. `-h` gives you the flags, `--help` the long version of each one plus the exit codes.

Because the contract you are profiling lives in a *different* directory, most readers will want it on `PATH`
instead of reached by relative path:

```sh
cargo install --path .      # soroban-cost-profiler, wherever cargo puts your binaries
```

Every command below is written with that bare name, from inside the contract's own directory.

## 2. Compile a contract that keeps its line tables

The profiler needs nothing *from* your source, but it can only name a frame from a `file:line` if the binary
kept the line tables that say where each instruction came from. A default `release` build does not: Soroban
contracts are compiled small and stripped, because mainnet charges you for every byte of the deployed
artifact.

So the trick is a profile you build for reading and never deploy. In your contract's `Cargo.toml`:

```toml
[profile.profiling]
inherits = "release"
debug = "line-tables-only" # enough for source mapping, far cheaper than full -g
```

Keep `debug` out of `[profile.release]` — that is the profile whose output gets deployed, and the extra
sections cost real fees on chain.

One expectation to set here rather than discover in step 3: line tables are not what makes *today's* picture
rich. Every frame still arrives named `wasm[0]`, whatever you build, because the engine reports no program
counter to look a line up with. What the tables buy you is (a) the warnings below, which tell you which half
of your artifact is missing, and (b) the source-mapping stage being already built and correct underneath — so
the frame names land the moment PC attribution does, rather than a second build being required then.

Now a contract. Two things make the difference in this tutorial: the exports take **no arguments**, and the
crate builds for wasm. This one is deliberately small — three functions, two loops and a wrapper that calls
both, which is enough to have a shape worth looking at:

```rust
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
pub extern "C" fn sum_squares() -> u64 {
    let mut acc: u64 = 0;
    let mut i: u64 = 0;
    while i < 500 {
        acc = acc.wrapping_add(i.wrapping_mul(i));
        i += 1;
    }
    acc
}

#[no_mangle]
pub extern "C" fn checksum() -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut i: u64 = 0;
    while i < 200 {
        h = h.wrapping_mul(0x100000001b3).wrapping_add(i);
        i += 1;
    }
    h
}

#[no_mangle]
pub extern "C" fn total() -> u64 {
    sum_squares().wrapping_add(checksum())
}
```

With `crate-type = ["cdylib"]` under `[lib]`, build it:

```sh
cargo build --profile profiling --target wasm32-unknown-unknown
ls -l target/wasm32-unknown-unknown/profiling/ledger.wasm
```

```console
-rwxr-xr-x  1482 Oct  7 23:04 target/wasm32-unknown-unknown/profiling/ledger.wasm
```

1,482 bytes, and it carries the sections the profiler reads: `.debug_info`, `.debug_line`, `.debug_abbrev`,
`.debug_ranges`, `.debug_str` and a `name` section. The same source through `[profile.release]` instead comes
out at 714 bytes — the 768-byte difference *is* the debug info, which is the reason it stays out of the
deployed profile. You do not have to check any of this by hand: step 3 tells you out loud if a half is
missing.

> **Why no arguments?** The profiler invokes the export with an empty parameter list, so an export that takes
> arguments traps before it runs. This is a known ceiling, not a mistake in your contract, and it is why a
> real `soroban-sdk` build's `call` entry — which takes three — cannot be profiled by name yet. Step 6 shows
> what that looks like when you try.

## 3. Profile one export

```sh
soroban-cost-profiler \
  --wasm target/wasm32-unknown-unknown/profiling/ledger.wasm \
  --fn total
```

```console
no function recorded any exclusive cost (cpu)
```

```console
$ cat profile.folded
wasm[0] 0
```

Three things just happened, and one of them is easy to misread:

* **Your contract ran.** It executed under the instrumented engine, and the artifact was written. The summary
  line is on stdout and anything the profiler wants you to notice about the build is on stderr, so the second
  file to check is `profile.folded`, not the terminal.
* **No warning fired on stderr.** That is the line-tables check: a binary the profiler cannot symbolize
  prints which half is missing and how to add it back, so silence here means your `[profile.profiling]`
  recipe worked. Try it on the same contract built by `cargo build --release --target wasm32-unknown-unknown`
  and you get the other half of the story:

  ```console
  warning: this artifact has a `name` section but no DWARF line tables, so frames will name functions and
  never `file:line`. Build the copy you profile with a profiling profile — `[profile.profiling]` with
  `inherits = "release"` and `debug = "line-tables-only"` — and keep `debug` out of `[profile.release]`: that
  is the profile whose output gets deployed, and mainnet bills for the extra bytes.
  no function recorded any exclusive cost (cpu)
  ```

  Note that it still profiles, and still exits `0`. A binary with no line tables is not a failed run; it is a
  run whose frames cannot be named.
* **The cost column is `0`.** This is the part not to misread. `wasmi` 2.0 gives the tracer no
  instruction-level hook, so what it can charge is one synthetic unit per *call boundary* — the call in and
  the return out, two of them — and at the default `--sample-rate 1000` an accumulator that gains 1 per
  boundary never reaches the threshold that emits an event. The zero is structural, not a measurement of
  your contract being cheap.

To see the tracer itself working, lower the rate until the boundaries become chargeable:

```sh
soroban-cost-profiler --wasm …/ledger.wasm --fn total --sample-rate 1 --output sr1.folded
```

```console
Top 1 functions by exclusive cost (cpu):
  1. wasm[0]  2
```

```console
$ cat sr1.folded
wasm[0] 2
```

That `2` is the two boundaries. It is a floor and a shape, not a budget reading: for the instructions the
network will actually charge you, `soroban-budget-assert` (Tier 2) is the tool, and this profiler is for
*where* to look once you know how much.

## 4. Understand the file before you open it

One line per call path:

```
<frame>;<frame>;<frame> <cost>
```

`;` marks a call boundary — the path from the entry point down to the frame that was running — and the number
after the space is that path's **exclusive** cost, charged to the innermost frame rather than to everything
above it. Repeated paths are summed by whoever reads the file.

This is the standard collapsed-stack format, which is why the tool writes text and no pictures: every serious
flamegraph viewer already speaks it, and none of them has to be a dependency. Your file today has one frame
per line, because every boundary the engine reports arrives with no program counter, so it is named
`wasm[0]`.

## 5. Open it in Speedscope

Go to [speedscope.app](https://www.speedscope.app) and drag the `.folded` file onto the window — or press
**Browse**, which is the same thing with a file picker. The app is entirely client-side: nothing is uploaded
anywhere, the file never leaves your browser.

Speedscope recognises the collapsed-stack format on its own. Load `sr1.folded` and its console says so —
`Importing as collapsed stack format` — before the window switches to the profile view with **Time Order**,
**Left Heavy** and **Sandwich** across the top. If the app ever shows something else, the file is not the
problem: check it is the `.folded` and not the `.wasm`.

What you will see is **one bar filling the chart**: `wasm[0]`, the single frame every boundary lands in. That
is the honest picture of today's engine, and it is what PC attribution
([issue 210's follow-ups](https://github.com/Tollcraft/soroban-cost-profiler/issues/210)) turns into a tree.
The viewer is not the missing half — it already draws any stack you give it. Once the same path arrives as
`wasm[0];total;sum_squares`, the flamegraph is the point of the whole tool: the widest bar is where the
instructions went, and Left Heavy is the panel that answers "what should I fix first".

The same file feeds [flamegraph.pl](https://github.com/brendangregg/FlameGraph) if you would rather have an
SVG of your own:

```sh
flamegraph.pl sr1.folded > flame.svg
```

This repository bundles no SVG renderer and promises none; the text file is the artifact, and both viewers are
optional readers of it.

## 6. When the contract is a real Soroban build

Point the same command at a `soroban-sdk` contract — the one you actually want to profile — and the first
thing you meet is the export list:

```console
$ soroban-cost-profiler --wasm target/wasm32-unknown-unknown/release/dummy_contract.wasm --fn call
error: 'call' is not an exported function; the module exports _, compute_heavy_loop, memory_heavy_loop
$ echo $?
1
```

This is the repository's own `soroban-sdk` fixture — `fixtures/build.sh` builds it, about 622 KB, and the path
above is relative to the repository root, because that contract is a member of this workspace and the artifact
lands in the root's `target/`. Its export list is not the one a reader expects from their own
`#[contractimpl]` names. Take a function that really is there, and you meet the wall:

```console
$ soroban-cost-profiler --wasm target/wasm32-unknown-unknown/release/dummy_contract.wasm --fn compute_heavy_loop --output dummy.folded
error: failed to instantiate module: cannot find definition for import (i,_) with type Func(FuncType { core: FuncType { params: [I64], results: [I64] } })
$ echo $?
1
$ ls dummy.folded
ls: dummy.folded: No such file or directory
```

Exit `1`, and **no `.folded` file**, because nothing ran. A `soroban-sdk` build imports the Soroban
environment interface — here the module `i`, function `_`, taking and returning an `I64` — and this profiler
links against an empty linker, so the module never instantiates. That is
[issue 210](https://github.com/Tollcraft/soroban-cost-profiler/issues/210), the single blocker between this
tutorial and a version of it that profiles your actual contract. Two neighbours of the same kind are
[`--args`, issue 211](https://github.com/Tollcraft/soroban-cost-profiler/issues/211) for exports that take
arguments and [issue 212](https://github.com/Tollcraft/soroban-cost-profiler/issues/212) for ledger state.

So for now, the contracts that profile are the pure-computation ones: the loops, parsers and arithmetic your
contract is built out of, exported without arguments — which is exactly why steps 2 and 3 used a contract with
no SDK dependency.

## 7. Did my change help?

Profile twice — before and after — and diff the two files. `compare` reads profiles and runs no contract, so
it takes no `--wasm`:

```sh
soroban-cost-profiler --wasm …/ledger.wasm --fn total --output before.folded
# change the contract, rebuild the profiling profile, then:
soroban-cost-profiler --wasm …/ledger.wasm --fn total --output after.folded
soroban-cost-profiler compare before.folded after.folded
```

A measured pair, from the two runs in step 3 (the second one with `--sample-rate 1`, so the "after" file has
cost to report):

```console
$ soroban-cost-profiler compare before.folded after.folded
Cost comparison, baseline → current (exclusive cost per function):
 wasm[0]  0  2  +2
 total    0  2  +2
1 of 1 functions changed cost, 0 unchanged
```

Rows are ranked by the size of the move. The last row is `total`, the **whole-profile sum**, which is the
number you actually asked for — and here it collides with the export in step 2, which is also called `total`
and is *not* what that row is: the two files above contain one frame each, `wasm[0]`, and `total` is the sum
row beneath them (`src/formatter.rs:370-371`). Functions that did not move are left out of the table but
counted in the tally line, so an empty table cannot be mistaken for a broken one, and a regression still exits
`0` — bad news is still an answer. One thing to watch: a `.folded` file records no metric of its own, so both
sides of a `compare` have to come from runs that agreed on `--metric` already. Nothing can check that for you.

## 8. Where to go next

* A message you do not recognise, or a run whose output looks wrong:
  [docs/troubleshooting.md](troubleshooting.md) — every string this tool writes, symptom first.
* What the tool can and cannot claim: [README's Limitations](../README.md#limitations).
* How a wasm address becomes a `file:line`, and what survives `wasm-opt`:
  [docs/internals/dwarf_mapping.md](internals/dwarf_mapping.md) and
  [docs/spikes/02_wasm_name_section_fallback.md](spikes/02_wasm_name_section_fallback.md).
* The pipeline this ran through — tracer, source map, aggregator, formatter: `ARCHITECTURE_ESSENTIALS.md`.

```sh
soroban-cost-profiler --help   # flags, the exit-code table, worked examples
```
