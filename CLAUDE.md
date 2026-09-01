# AI Agent Instructions for `soroban-cost-profiler`

Welcome! If you are an AI coding assistant (like Claude, Gemini, or ChatGPT) working in this repository, please read these instructions before writing any code.

## 🎯 Repository Context
This repository (`soroban-cost-profiler`) is **Tier 3** of the Tollcraft cost-awareness pipeline for Stellar's Soroban smart contracts. 
* **Goal:** Generate execution traces and flamegraphs from compiled WASM contracts.
* **Architecture:** Read `ARCHITECTURE_ESSENTIALS.md` for a 60-second primer on the pipeline.

## ⚠️ Critical Rules (MVP Constraints)
1. **Mandatory Roadmap Updates:** Whenever you make a contribution or finish a task, you MUST update `ROADMAP.md` to reflect the progress. This file is our source of truth.
2. **Zero-Instrumentation:** You must NOT require the user to add tracing macros to their smart contract code. Our profiler must work on standard, compiled WASM binaries (with DWARF debug info).
3. **Avoid Dependency Bloat:** Do NOT add `inferno` or any SVG rendering libraries for the MVP. We are only outputting `.folded` stack text files to be consumed by external tools like `speedscope.app`.
4. **No Custom DWARF Parsing:** Do NOT write custom `gimli` parsers if you can avoid it. Rely on `addr2line` for source mapping.
5. **OOM Safety:** Smart contracts can run up to 100M instructions. You can buffer these in a flat `Vec` in memory for the MVP, but do not allocate massive heap objects per instruction.

## 🛠️ Common Commands
* **Build:** `cargo build`
* **Test:** `cargo test`
* **Format:** `cargo fmt --all`
* **Lint:** `cargo clippy --workspace --all-targets -- -D warnings`

## 📖 Key Files
* `PRD.md`: The product requirements and edge cases we must support.
* `ARCHITECTURE.md`: Deep dive into the technical design and known risks.
* `ARCHITECTURE_ESSENTIALS.md`: The quick outline.
