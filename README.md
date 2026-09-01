<div align="center">
  <h1>🔥 Soroban Cost Profiler</h1>
  <p><strong>Visual flamegraphs and execution tracing for Soroban smart contracts.</strong></p>
</div>

> Part of the **[Tollcraft](https://github.com/Tollcraft)** initiative.

`soroban-cost-profiler` is Tier 3 of the Tollcraft cost-awareness pipeline. When `soroban-budget-assert` fails your CI because your contract used too many CPU instructions, the Cost Profiler tells you *exactly where* those instructions were spent.

## 🎯 The Problem

Testing tools can tell you that `my_expensive_function()` consumed 8,000,000 CPU instructions, but they don't tell you *why*. Was it a specific loop? A costly host function call? An inefficient standard library operation? 

## ✨ The Solution

`soroban-cost-profiler` traces the execution of your WebAssembly (WASM) smart contract instruction-by-instruction. It maps the runtime execution cost back to your Rust source code and generates visual **flamegraphs**, making it trivial to spot the bottlenecks in your logic.

## 🚀 How it Fits into Tollcraft

1. **Tier 1 (Prevent):** [`soroban-cost-linter`](https://github.com/Tollcraft/soroban-cost-linter) catches structurally expensive code before compilation.
2. **Tier 2 (Detect):** [`soroban-budget-assert`](https://github.com/Tollcraft/soroban-budget-assert) measures total execution cost and fails CI if budgets are exceeded.
3. **Tier 3 (Diagnose):** **`soroban-cost-profiler`** visualizes the execution trace to help you find and fix the specific bottleneck causing the budget failure.

## 🛠️ Architecture (Planned)

- **WASM Interpreter Hook:** Hooks into the Soroban environment WASM execution engine to trace instruction counts per function call.
- **Source Map Resolution:** Uses DWARF debug info embedded in the WASM binary to map WASM functions back to the original Rust source lines.
- **Flamegraph Generator:** Outputs standard `collapsed` stack formats that can be rendered by tools like `inferno` or Speedscope.

## 🏁 Getting Started

*(Coming soon. This repository is currently in the initial planning and scaffolding phase.)*
