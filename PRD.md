# Product Requirements Document (PRD): Soroban Cost Profiler

## 1. Product Overview
**Name:** `soroban-cost-profiler`
**Tagline:** Visual flamegraphs and execution tracing for Soroban smart contracts.
**Role in Tollcraft Suite:** Tier 3 - Diagnostic (The "Why"). It complements Tier 1 (Static Analysis via `soroban-cost-linter`) and Tier 2 (Runtime Measurement via `soroban-budget-assert`).

## 2. Who is this for? (Target Audience)
* **Smart Contract Developers:** Engineers building on Stellar who need to minimize transaction fees and resource consumption.
* **Security Auditors:** Professionals looking for DoS attack vectors where specific inputs cause a massive spike in resource usage.
* **Optimization Engineers:** Developers tasked with squeezing maximum performance out of existing protocols to stay under the Soroban network's strict per-transaction budget limits.

## 3. The Problem
Currently, developers can use `soroban-budget-assert` to determine *if* their contract exceeds a budget (e.g., 8,000,000 CPU instructions), but they lack visibility into *why*. When a test fails due to resource exhaustion, the developer is forced to guess which loops, host function calls, or data structures are responsible for the spike. There is no easy way to introspect the internal execution cost of a Soroban WASM binary during testing.

## 4. What the Product Needs to Do (Core Capabilities)

### 4.1. Execution Tracing
* **WASM Instrumentation:** The tool must be able to hook into the Soroban environment's WASM execution engine during local testing to count the exact number of CPU instructions, memory allocations, and host function calls executed per function.
* **Granularity:** Tracing must be precise enough to distinguish between different contract function calls and internal Rust functions.

### 4.2. Source Mapping
* **DWARF Debug Info Resolution:** The tool must parse the DWARF debug information embedded in the compiled Soroban WASM binary.
* **WASM-to-Rust Translation:** It must accurately map the raw WASM instruction offsets back to the human-readable Rust source code lines and function names.

### 4.3. Visual Output Generation
* **Format Compatibility:** The profiler must generate output in standard profiling formats (e.g., collapsed stack format) that can be immediately consumed by popular visualization tools like `inferno`, `speedscope`, or standard `flamegraph` generators.
* **Cost Dimensions:** It should allow developers to generate flamegraphs based on different dimensions (e.g., CPU instructions, memory allocated, host function calls).

### 4.4. Seamless Developer Experience
* **Drop-in Integration:** It should be easy to run against existing tests. For example, `cargo run -p soroban-cost-profiler -- benchmark_test`.
* **Zero Code Modification:** Developers should not have to manually instrument their smart contract code with tracing macros to use the tool.

## 5. Non-Goals (Out of Scope for v1)
* **Live Network Profiling:** The tool will trace execution locally in a simulated environment, not by querying mainnet/testnet transactions.
* **Automated Fixing:** The profiler will only diagnose the issue; it will not rewrite the AST to fix it (that is the domain of `cargo-soroban-fix`).
* **Custom Web Dashboard:** We will rely on existing, battle-tested rendering tools (like `speedscope` or SVG flamegraphs) rather than building a custom HTML dashboard from scratch.

## 6. Success Metrics
* **Resolution Time:** Reduces the time a developer spends finding a resource bottleneck from hours (manual binary search via commenting out code) to seconds.
* **Adoption:** Number of projects generating flamegraphs as part of their CI artifacts alongside `budget-assert` reports.
