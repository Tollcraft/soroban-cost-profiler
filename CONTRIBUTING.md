# Contributing to `soroban-cost-profiler`

Thank you for contributing to the Tollcraft ecosystem! This file is the workflow: what to install, what has
to pass before you push, and what a pull request here has to contain. The product requirements are in
`PRD.md`, the design in `ARCHITECTURE.md` (start with the 60-second version, `ARCHITECTURE_ESSENTIALS.md`),
and the state of the project in `ROADMAP.md`. The two user-facing documents are `docs/tutorial.md` (a contract,
a profile, a flamegraph, in eight steps) and `docs/troubleshooting.md` (every message the tool writes, one
symptom at a time) — a PR that changes what either of them shows has to update the other.

## ⚠️ Important: The Roadmap Rule

To maintain clear scope and visibility on this project, we strictly enforce the Roadmap Rule:
**Every single Pull Request or agent contribution MUST include a corresponding update to `ROADMAP.md`.**

If you implement a feature, check its box in `ROADMAP.md`. If you start a new phase, mark it as `(In Progress 🚧)`. This file is the single source of truth for the project's readiness.

Check the box **in the same branch**, not in a follow-up: a merged PR whose roadmap line still reads
`[ ]` is a roadmap that no longer tells anyone what is true. The entry is one line, and what makes it useful
is that it says how the claim was checked — a code change names the test that proves it, a documentation
change names the command that produced the output it quotes.

## Setup

Rust 1.85 or newer (the crate is edition 2024), `rustfmt` and `clippy`, and nothing else — every dependency
is either on crates.io in `Cargo.toml` or vendored in `vendor/`.

```sh
cargo build                 # the workspace: the profiler and fixtures/dummy-contract
cargo test --workspace
cargo run -- --help         # the CLI; `cargo run --` passes argv through to the binary
```

Build from the repository root. The root `Cargo.toml` is a workspace (`members = [".", "fixtures/dummy-contract"]`)
and carries a `[patch.crates-io]` that pins `ed25519-dalek` to the copy in `vendor/ed25519-dalek-2.1.1`;
that pin is what lets `soroban-env-host`'s testutils resolve against a `rand_core` they can compile, so keep
it and keep `vendor/`.

### The wasm fixture, and the two tests that need it

Most of the suite needs nothing beyond a toolchain. Two integration cases read the real `soroban-sdk`
contract build, and that artifact is **not in git** — about 622 KB is a poor thing to commit for a test. Build
it yourself:

```sh
rustup target add wasm32-unknown-unknown
./fixtures/build.sh        # run from anywhere; the script changes to its own directory
cargo test -- --ignored
```

`build.sh` runs `cargo build --release` inside `fixtures/dummy-contract`, which is a member of the root
workspace — so the artifact lands in **the repository's own `target/`**, at
`target/wasm32-unknown-unknown/release/dummy_contract.wasm`, which is the path both tests read. It is not
under `fixtures/dummy-contract/target/`, which is where a standalone crate would put it.

The two cases are `the_real_soroban_contract_build_still_stops_at_its_host_imports` (`tests/cli_e2e.rs`) and
`the_real_soroban_build_resolves_contract_lines` (`tests/source_map_fixture.rs`). Before `fixtures/build.sh`
they fail with `… — run fixtures/build.sh` rather than passing quietly, because a check that silently skips is
worse than one plainly marked as outside the default run. The first of the two asserts a *limitation* — that
build stops at its host imports — so the PR that fixes the limitation must invert it; that is written out in
the test, not left as a comment.

## The gates CI runs

`.github/workflows/ci.yml` has two jobs, and the first is the one a PR is judged on. Run them in this order —
they are quick, and the first failure is the cheapest to fix:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

| Gate | What it catches | Notes |
|---|---|---|
| `cargo fmt --all -- --check` | formatting | `cargo fmt --all` is the fix; the `--check` form is what CI runs. |
| `cargo clippy --workspace --all-targets -- -D warnings` | lints in `src/`, `tests/`, `benches/` and fixtures | `--all-targets` means an unused `mut` in a test you just added reddens the job, and `-D warnings` makes every lint a failure. |
| `cargo test --workspace` | unit, integration, process-level CLI tests, **and doc-tests** | Code blocks inside `///` comments are compiled and run, so a doc example that drifts from the API fails here rather than in review. |

The second job, `Build Fixture`, runs `fixtures/build.sh` with the `wasm32-unknown-unknown` target. Nothing
else notices a fixture that stopped building, so that job is the guard.

`cargo bench` runs the two criterion benchmarks, `benches/tracer_benchmark.rs` and
`benches/source_map_benchmark.rs`. They are not a gate; they exist so a claim about cost or throughput can be
measured instead of asserted.

## Before you write code

1. **Get assigned to the issue first.** Say which issue you are taking and wait for the assignment. Two PRs
   on one issue is one merged PR and one conversation nobody wanted.
2. **Branch from a current `main`.** `git fetch origin && git checkout -b <your-branch> origin/main`. One
   issue per branch; a PR carrying unrelated commits cannot be reviewed against the issue it closes.
3. **Read the MVP constraints below.** They decide more designs than any other rule in this repository, and
   they are the reason a PR that "just adds a dependency" is asked to justify itself.

## The MVP constraints

`AGENTS.md` is authoritative. These four come up in review:

* **Zero instrumentation.** A user must never add a macro, a hook, or a special recompile for the profiler to
  read their contract. A design that needs source changes in the user's contract is wrong here by
  construction.
* **No SVG renderer, no `inferno`.** The artifact is a `.folded` text file in the standard collapsed-stack
  format, which speedscope.app and `flamegraph.pl` already read. Drawing the picture is someone else's tool,
  on purpose.
* **No hand-written DWARF parsing.** Source mapping goes through `addr2line` and `gimli` as configured in
  `Cargo.toml`.
* **No mass allocation per traced step.** A contract can run 100M instructions, so anything that allocates
  per instruction is an out-of-memory error waiting for the biggest contract in the backlog. Buffer flat, and
  sample.

## What a PR has to contain

GitHub pre-fills this list when you open a pull request, from `.github/pull_request_template.md`: keep the
headings, replace the comments, and tick what you actually ran.

The body is a **breakdown, not a summary** (rule 6 of `AGENTS.md`): what changed, what you ran to believe it,
and what is deliberately out of scope. "Closes #123" is not a description, and neither is a restatement of
the issue.

* **One closing line**: `Closes #<the issue you are working>`, and only one. Other issues are named without a
  closing keyword — "issue 210", "see issue 162's coverage check" — because a `Closes`/`Fixes`/`Resolves`
  prefix on an issue your diff does not finish closes somebody else's open work, and some automation in this
  repo reads more references as closings than GitHub does.
* **A `ROADMAP.md` box checked in the same branch.**
* **Claims backed by a command you ran.** This is a profiler: its most convincing failure is output that
  looks right. A transcript quoted in `README.md` or `docs/` should be the one a built binary of that branch
  actually printed, and a statement about behavior should cite the line (`src/tracer.rs:357-360`) so the next
  reader — and the next refactor — can check it. If a number is read out of the code rather than measured,
  say which it is.
* **Tests of behavior, not of your implementation.** A CLI change is tested by running the built binary as a
  process (`tests/cli_e2e.rs`), because what a user meets is `main`, the exit code, stdout and stderr as two
  streams, and a file that exists on disk — not a `Result` variant an in-process call can assert instead.
* **Scope kept to the issue.** A rename, a reformat or a "while I was here" refactor in a PR about something
  else costs the reviewer the diff and the reviewer's time costs the next contributor a rebase.

## Keeping the contributor grid current

`README.md`'s Contributors section is a **dated snapshot, not a badge**: one `<img>` tile per person, served by
GitHub itself, and a sentence underneath stating how many people the grid lists and the day it was read.
Nothing refreshes it on its own, and that is the deliberate trade — an honest list with a date on it instead of
a live image whose definition of "contributor" is somebody else's cache. So **the PR that lands someone's
first contribution is the PR that adds them to the grid**, by running this and replacing the tile lines inside
the section's first `<p align="center">` block (the second one holds the provenance sentence, not tiles):

```sh
gh api "repos/Tollcraft/soroban-cost-profiler/contributors?per_page=100" \
  --jq '.[]
        | select(.type != "Bot" and .login != "web-flow")
        | "  <a href=\"https://github.com/\(.login)\"><img src=\"https://github.com/\(.login).png\" width=\"64\" height=\"64\" alt=\"\(.login)\" title=\"\(.login)\" /></a>"'
```

Then update the sentence below the grid — the count in words, and the date — and let the command's output
stand for who is in it. Do not add a name by hand and do not re-sort the list: a hand-added tile is a claim
with no command behind it, which is exactly what the rule above exists to prevent. Re-run on 2026-10-08 this
prints the fifteen lines already in `README.md` **byte for byte**, so that equality is the check to redo if
the command itself is ever edited.

Four choices inside it are load-bearing:

* **The order is the endpoint's** — descending by the count it reports — and grouping happens by GitHub
  *account*, which is what makes it a list of people rather than a list of spellings. `wagmiiii` appears once,
  where `main`'s history gives one person three author names (`adajala`, `Ademola`, `WAGMI`) across two
  addresses, so `git log --format='%ae' origin/main | grep -cE 'ademola2993k@gmail.com|130152505'` is the check
  that the endpoint's top count describes one human being rather than three. What it pins is the **equality of
  those two counts**, not their value: measured on 2026-10-08 the endpoint said 71 and the grep said 72, because the
  grep had already counted the merge commit `d30a28f` and the endpoint had not yet reported it, while the two
  accounts with nothing new merged matched exactly (`mallison031` 19 against 19, `Gabbydunkk` 6 against 6 over two
  addresses). The endpoint trails the newest commit rather than disagreeing with the history, which a grid that
  carries its own date can afford. `git shortlog`
  would render one maintainer as three contributors, so the grid takes the endpoint's grouping rather than
  re-deriving one locally — and the section links the repository's own contributors graph for anyone who wants
  the live view.
* **`select(.type != "Bot")` alone is not enough**, which is why `web-flow` is also excluded by name: GitHub's
  merge bot reports `"type": "User"`, and one commit, so it would otherwise sit at the foot of a list of
  people with an avatar nobody owns.
* **Avatars come from `https://github.com/<login>.png`**, the image the profile page itself serves, so every
  tile is verifiable by clicking it. A third-party contributors badge would redraw the whole list from a cache
  this repository does not control and cannot audit.
* **`per_page=100` is the endpoint's own page cap.** Past a hundred contributors, add `--paginate`. The list is
  fifteen today, so this is the failure that arrives years from now rather than next month — noted here so the
  next person does not silently truncate the grid.

## Review and merge

* A PR merges when **both CI jobs are green** and the branch is current with `main`.
* A red gate is fixed at its cause. Never loosen a check to get green — not a `fmt`/`clippy` invocation, not
  an assertion, not a threshold; and never skip a hook with `--no-verify`. If a gate is genuinely wrong, that
  is its own issue with its own argument.
* This repository squash-merges, with the issue number in the subject line, so the history reads as a list of
  closed boxes: one commit per issue.
* If your PR's honest content is a limitation — a test that asserts today's failure so that the fix turns it
  red, or a doc that says what the engine cannot do yet — name the issue that owns removing it, in the body
  and in the test's doc comment. A "not yet" that nobody is holding quietly becomes a lie.

## Getting started ideas

Issues labeled `good first issue` and `help wanted` are the on-ramp. `PRD.md`'s edge-case list is where most
of the open Phase work comes from.

## Contact & Support

* [Discord](https://discord.gg/5aprtMSyR) — general discussion
* [Telegram](https://t.me/+Gflo5jZStw1jMjE0) — the Tollcraft team
