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

*(Coming soon. This repository is currently in the initial planning and scaffolding phase.)*

## Usage

**Important: The `debug` Precondition**
The profiler requires zero *code* instrumentation, but you **must** configure your release build to include DWARF debug information. Soroban contracts are typically stripped for size, which makes profiling impossible. 

Ensure your `Cargo.toml` contains a dedicated profile for profiling:
```toml
[profile.profiling]
inherits = "release"
debug = "line-tables-only" # REQUIRED FOR PROFILING
```
> [!CAUTION]
> **Deployment Safety:** Do NOT add `debug` to your main `[profile.release]`. If you accidentally deploy a contract with debug tables to mainnet, you will pay significantly higher on-chain fees for the binary bloat. Always compile with `cargo build --profile profiling` when generating a flamegraph.

> [!WARNING]
> **Two Caveats:**
> 1. **Downstream Stripping:** If you use `stellar contract build` instead of `cargo build`, downstream tools (like `wasm-opt`) may still strip debug sections regardless of your `Cargo.toml`. We are actively investigating reliable CLI flags.
> 2. **Inlining (LTO):** Preserving debug tables does *not* stop the compiler from aggressively inlining functions. Your flamegraph may still have coarse mappings for heavily optimized loops.

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
