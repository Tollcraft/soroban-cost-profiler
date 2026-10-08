# `dwarf_probe` — source-mapping test data

Two pre-built WASM binaries of the same three functions in `src/lib.rs`, committed so that
the `src/source_map/` tests can assert against real Rust DWARF without a wasm build in the test
job (`ci.yml` builds the fixture in a separate job whose artifact the tests cannot read).

| File | Built with | Size | Custom sections |
| --- | --- | --- | --- |
| `dwarf_probe.wasm` | `debug = 1` | 1.7 KB | `.debug_abbrev`, `.debug_info`, `.debug_ranges`, `.debug_str`, `.debug_line`, `name`, `producers`, `target_features` |
| `dwarf_probe_no_debug.wasm` | `debug = false` | 595 B | `name`, `producers`, `target_features` |

## Why this is not the Soroban fixture

`fixtures/dummy-contract` is what the tracing tests execute, but it is the wrong shape for test
data here: turning on its debug info grows it from 3.1 KB to **622 KB**, 619 KB of which is
DWARF custom sections against a 488-byte code section. That is `soroban-sdk` and `std` — a
release build emits debug info for every inlined dependency, not just for the contract's own
functions. This crate links neither (`#![no_std]`, `panic = "abort"`), so it carries the same
*kind* of DWARF at a fraction of the size and can live in git.

The tradeoff to be aware of: the inlined-dependency paths a real contract produces (a
`/rustc/<hash>/library/...` frame, or `../`-containing `libm` paths) cannot be exercised here.
Those are covered by the `source_map` module docs and are worth a case in any fixture-based test
once Phase 3's integration test runs against a real build.

## Rebuilding

```sh
./build.sh   # requires the wasm32-unknown-unknown target
```

`src/lib.rs` is deliberately plain loops and arithmetic: the function names and line numbers the
tests resolve are its own, and changing it changes what a resolution test must expect.
