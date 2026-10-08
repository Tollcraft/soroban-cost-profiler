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

Now a contract. Two things make the difference in this tutorial: the exports take **no arguments**, so step
3's command line has nothing to get wrong (an export that does take them is profiled the same way, with
`--args 1000,7`), and the crate builds for wasm. This one is deliberately small — three functions, two loops
and a wrapper that calls both, which is enough to have a shape worth looking at:

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

> **Why no arguments?** To keep step 3 one command long. An export that declares parameters is profiled the
> same way with `--args 1000,7` added, and the profiler checks that list against the module's own signature
> before it calls anything, so a wrong count is a refused command line rather than a trap. What no command
> line can supply is a real `soroban-sdk` build's environment and object handles — and an argument is
> irrelevant to a module that cannot link. Step 6 shows what that looks like when you try.

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
is the honest picture of today's engine. It is not a gap this repository can close with a flag: a per-frame
tree needs a program counter at every step, and the engine this profiler runs on (`wasmi` 2.0) reports a cost
event only at call boundaries — it has no instruction hook to expose, and no open issue here owns one. What
the profiler *can* attribute is the boundary itself, which is why a real contract's host calls arrive as a
second frame (`host[0]`) rather than a flat zero — see step 6.
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
`#[contractimpl]` names. Take a function that really is there, and the profiler asks for its argument:

```console
$ soroban-cost-profiler --wasm target/wasm32-unknown-unknown/release/dummy_contract.wasm --fn compute_heavy_loop --output dummy.folded
error: 'compute_heavy_loop' takes 1 argument (i64); --args gave no values. `--args` is one value per parameter, in the order the signature lists them.
$ echo $?
1
$ ls dummy.folded
ls: dummy.folded: No such file or directory
```

Exit `1`, and **no `.folded` file**, because a run that cannot call the export is a run that measured nothing.
The host interface is not a wall any more: a `soroban-sdk` build imports the Soroban environment — here the
module `i`, function `_`, taking and returning an `I64` — and the profiler registers the real host functions
against that linker, so the module instantiates and the export runs.

What the export wants is the *tagged* word, not the number. An SDK entry point reads each parameter as a
`Val`, and a `Val`'s type tag is its low byte, so a `u32` arrives as `value << 32 | 4`: `10` is
`42949672964`, `100` is `429496729604`. Hand it the plain number and the guest reads a word whose tag is not
what it expected and aborts — `error: … trapped: wasm `unreachable` instruction executed`, exit `1`, and a
partial trace on disk. [The troubleshooting
entry](troubleshooting.md#the-export-takes-arguments) has both halves of that sentence, measured. With the
tag in place, the same command runs:

```console
$ soroban-cost-profiler --wasm target/wasm32-unknown-unknown/release/dummy_contract.wasm --fn memory_heavy_loop --args 429496729604 --output dummy.folded
Top 1 functions by exclusive cost (cpu):
  1. host[0]  125022

$ echo $?
0
$ cat dummy.folded
wasm[0] 0
wasm[0];host[0] 125022
```

Two frames, and the difference between them is the whole point of profiling a contract like this one.
`wasm[0]` is the contract's own body; it is `0` because the engine charges a call boundary to the callee, and
every boundary this run crossed left the wasm into a host function. `wasm[0];host[0]` is those host calls —
one `vec_new`, 100 `vec_push_back`, one `vec_len` — and asking for the call count instead of the cost makes
the same shape explicit:

```console
$ soroban-cost-profiler --wasm …/dummy_contract.wasm --fn memory_heavy_loop --args 429496729604 --metric hostcalls --output host.folded
$ cat host.folded
wasm[0] 0
wasm[0];host[0] 102
```

A pure-computation export of the same contract shows the other half: `--fn compute_heavy_loop --args
42949672964` exits `0` and writes exactly `wasm[0] 0`, with the summary saying `no function recorded any
exclusive cost (cpu)`. That is not a failure — a loop that never crosses into the host has no boundary for
the engine to attribute, which is the same statement steps 2 and 3 make about a contract with no SDK
dependency at all.

A contract that reads the *chain* needs a ledger, and that is a flag rather than a missing feature:
`--state` builds the host from a Soroban ledger snapshot before the contract is loaded. Here is the smallest
case, run from the repository root — `reads_ledger.wasm` is the 55-byte `READS_LEDGER` test module in
`src/main.rs`,
`(module (import "x" "3" (func (result i64))) (func (export "read_sequence") (result i64) call 0))`, whose
only work is `get_ledger_sequence`, and `fixtures/state/ledger.json` is the snapshot this repository commits:

```console
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --output blank.folded
error: 'read_sequence' trapped: host function 'x.3' failed: HostError: Error(Context, InternalError)
DebugInfo not available
. The partial trace up to the trap is in blank.folded, and its costs are incomplete because the call never returned.
$ echo $?
1
$ soroban-cost-profiler --wasm reads_ledger.wasm --fn read_sequence --state fixtures/state/ledger.json --output mocked.folded
no function recorded any exclusive cost (cpu)
$ echo $?
0
```

Sequence 500 and timestamp 1700000000 come out of the file and into the contract, which is the difference
between the two runs above. What a snapshot cannot give is the **contract frame** a host call normally runs
inside: `get_contract_data` and its siblings build their ledger key from the current contract ID, and the
profiler invokes an export from outside a contract call, so those reads and `require_auth` stop before they
consult the state. Contract-to-contract `call` is the other gap — it needs to re-enter the engine, which this
profiler never sets up, so it returns a host error instead of running the callee. Both are named in the
bindings' own module documentation, and neither stops the runs above.

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
row beneath them (`src/formatter.rs:475-479`). Functions that did not move are left out of the table but
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
