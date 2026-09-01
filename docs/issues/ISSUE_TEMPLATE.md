> [!IMPORTANT]
> **System Directive: Mandatory Context Review**
> Before proposing a solution or writing code, you MUST parse and strictly adhere to the constraints defined in the following state documents:
> - [`PRD.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/PRD.md)
> - [`ARCHITECTURE.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/ARCHITECTURE.md)
> - [`ARCHITECTURE_ESSENTIALS.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/ARCHITECTURE_ESSENTIALS.md)
> - [`ROADMAP.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/ROADMAP.md)
> - [`CONTRIBUTING.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/CONTRIBUTING.md)
> - [`AGENTS.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/AGENTS.md) / [`CLAUDE.md`](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/CLAUDE.md)
> 
> **Operating Constraints:** Do not extrapolate beyond the specified scope or rely on baseline assumptions. You must autonomously verify that the code passes all CI gates (`cargo check`, `cargo clippy`, `cargo test`) before marking this task as complete.

## Description
**Context:** {Insert context here}

**Task:** {Insert simplified task here}

**Why it's independent:** {Insert why it's independent here}

## What "done" looks like
{Insert acceptance criteria here}

## Implementation guidelines
* Ensure you are strictly following the MVP cuts described in the architecture documents.
* Keep the scope strictly limited to this issue. Do not over-engineer.

## PR guidelines
* Get assigned before starting.
* PR description must include: `Closes #[this issue]`.
* Check off the corresponding box in `ROADMAP.md` upon completion!

---

### **Contact & Support**
- [Telegram](https://t.me/+Gflo5jZStw1jMjE0)
- [Discord](https://discord.gg/5aprtMSyR)

---
### 📋 Before you start
Please read our [Code Quality Standards](https://github.com/Tollcraft/soroban-cost-profiler/blob/main/CONTRIBUTING.md). Before submitting a PR, ensure you run:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
