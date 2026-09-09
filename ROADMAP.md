# 🗺️ Soroban Cost Profiler: Development Roadmap

> **⚠️ CRITICAL RULE:** This document MUST be updated whenever a contribution is made to the repository. If you finish a task, check it off here and update the progress.

## Phase 1: Core Scaffolding & Setup ✅
- [x] Create repository, README, and AGENTS.md instructions.
- [x] Draft PRD and Architecture documents.
- [x] Scaffold initial Rust pipeline modules (`tracer`, `aggregator`, `source_map`, `formatter`).
- [x] Define core data models (`TraceEvent`, `CallStackNode`).
- [x] Setup Tollcraft Org Landing Page.
- [x] Implement interactive demo animations for Linter and Assert tabs.
- [x] Implement Light/Dark mode toggle button in the navbar.

## Phase 2: Execution Tracing (In Progress 🚧)
- [x] **SPIKE:** Investigate `soroban-env-host` Budget API limitations.
- [x] **WASM Engine Setup:** Import `soroban-env-host` and `wasmi` as dependencies.
- [x] **Fixture Compilation:** Add a `fixtures/dummy-contract` Soroban contract with a `compute_heavy_loop` function and workspace integration.
- [ ] **Tracer Hooks:** Implement the `wasmi` execution hooks in `src/tracer.rs` to intercept instructions.
- [ ] **Instruction Counting:** Accurately measure and record CPU cost and `pc` at every step.
- [ ] **Call/Return Tracking:** Record entry and exit events for WASM function calls.

## Phase 3: DWARF Source Mapping
- [ ] **Add Dependencies:** Add `addr2line` and `gimli` for debug info parsing.
- [ ] **Load DWARF Info:** Parse the `.debug_info` and `.debug_line` sections of the loaded WASM binary in `src/source_map.rs`.
- [ ] **Address Resolution:** Implement the `resolve(pc)` function to translate a WASM Program Counter to a Rust `file:line` frame.

## Phase 4: Aggregation & Formatting
- [ ] **Tree Building:** Implement `ProfileAggregator` to consume the raw `TraceEvent` stream and build a `CallStackNode` tree.
- [ ] **Cost Math:** Calculate `inclusive_cpu` and `exclusive_cpu` correctly during aggregation.
- [ ] **Formatting:** Implement `OutputFormatter` to serialize the tree into the standard `.folded` collapsed stack format.

## Phase 5: CLI & Edge Cases (MVP Completion)
- [ ] **CLI Parsing:** Add `clap` to `src/main.rs` to accept `--wasm`, `--output`, and test arguments.
- [ ] **Panic Handling:** Ensure the aggregator flushes and formats the trace even if the contract panics mid-execution.
- [ ] **Infinite Loop Protection:** Enforce a hard ceiling (e.g. 100M instructions) to halt tracing and prevent OOM crashes.
- [ ] **Documentation:** Update README with usage examples and CLI flag details.
- [x] **FAQ:** Design and implement FAQ section for GitHub Pages (`docs/index.html`).

## Tooling & Agent Setup
- [x] Install `agentic-awesome-skills` to `.agents/` for enhanced AI workflows.
- [x] Install official Anthropic skills and plugins from `claude-plugins-official`.
- [x] Generate `INSTALLED_SKILLS.md` catalog detailing all loaded agents and plugins.
- [x] Install `frontend-design` (anthropics/skills) and `design-taste-frontend` (leonxlnx/taste-skill) UI/UX skills for site audits.

## Website Polish (Landing Page Audit) ✅
- [x] **Fix marquee full-width bug:** Missing `</div>` caused `.marq-wrap` to nest inside `.wrap.hero__grid` and render as a 649px grid column instead of a full-width band.
- [x] **Light-theme contrast:** Override `--cyan`/`--magenta`/`--faint`/`--t2` with darker variants; white text on primary CTA; visible ghost-button border; visible `flame--4` bar.
- [x] **Dark-theme contrast:** Deepen `--violet` for primary CTA (AA 4.5:1) and lighten `--faint` for small mono labels.
- [x] **Responsive nav:** Tighten `.nav__links` gap to fix overflow at ~768px; add `scroll-padding-top` for anchored sections.
- [x] **Polish:** Add inline SVG favicon, `color-scheme` for native scrollbars, remove dead CSS (`.term__flame-block`, `.card`), clean duplicate rule, swap visible em-dashes for commas/parens.
- [x] **Social sharing:** Add branded 1200x630 Open Graph image (`docs/og-image.png`) plus `og:image`/`twitter:card` (summary_large_image) meta tags with alt text.
- [x] **Favicon:** Replace the inline SVG favicon with the Tollcraft org profile picture (`docs/favicon.png`, downloaded from GitHub avatars and converted to PNG), plus an `apple-touch-icon` link.
