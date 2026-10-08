<!--
Keep the headings, replace the comments. The body of a PR is the review record: `AGENTS.md` rule 6
requires a breakdown of what changed and what you ran to believe it, not a restatement of the issue
and not just a closing line. Delete this comment before submitting.
-->

## What this changes

<!-- Files, functions, flags, tests — the specifics, not the issue title. -->

## Why

<!-- Which issue, and what that issue's own "done" line required. If this branch does not finish the
issue, say what is left, so nobody merges it believing more than it does. -->

## How it was verified

<!-- The commands you ran and what they printed. This is a profiler: its most convincing failure is
output that looks right, so a claim about behavior needs the transcript, the exit code, or the line
(`src/tracer.rs:357-360`) it came from. If a number is read out of the code rather than measured,
say which it is. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace` (doc-tests run inside this one)
- [ ] User-visible behavior was exercised through the **built binary** (`tests/cli_e2e.rs` runs it as a
      process — argv in, exit code, stdout and stderr as two streams, file on disk), not only asserted
      against an in-process `Result`
- [ ] If the wasm fixture is involved: `./fixtures/build.sh` first, and the artifact lands in the
      **repository root's** `target/`, not under `fixtures/`

## Roadmap

- [ ] `ROADMAP.md` has this work's box **in this branch** — the Roadmap Rule in `CONTRIBUTING.md` makes
      that file the source of truth, so an unrecorded change is an unfinished one. A code change's entry
      names the test that covers it; a docs change's entry names the command that produced the output it
      quotes.

## Scope

- [ ] This branch changes only what the issue asked for — no rename, reformat or "while I was here"
      refactor riding along
- [ ] Exactly **one** closing line, `Closes #<your issue>`, placed last; every other issue is named
      **without** a closing keyword ("see issue 210"), because a `Closes`/`Fixes`/`Resolves` prefix on an
      issue this diff does not finish closes somebody else's open work — and automation here reads more
      references as closings than GitHub does
- [ ] The MVP constraints still hold (`AGENTS.md`, `ARCHITECTURE_ESSENTIALS.md`): no tracing macros in the
      user's contract, no `inferno` or SVG renderer dependency, no hand-written `gimli` parser where
      `addr2line` answers, no mass heap allocation per traced step
- [ ] Both CI jobs (`Build and Test`, `Build Fixture`) are green and the branch is current with `main`;
      a red gate is fixed at its cause, never loosened, and hooks are never skipped with `--no-verify`

Closes #
