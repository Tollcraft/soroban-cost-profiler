//! #184: the CLI as a *process* — from argv to the file it leaves on disk.
//!
//! The suite in `src/main.rs` drives the pipeline by calling `run()` in-process, which is the right
//! level for the logic and the wrong level for everything a user actually meets: `main`, the exit
//! code a shell sees, stdout and stderr as two separate streams, and an artifact that exists because
//! a binary wrote it. Those are only observable from outside the process, and #183's codes and
//! #186's warning had never been checked past the point where an in-process test asserted a `Result`
//! variant.
//!
//! `CARGO_BIN_EXE_…` is cargo's own guarantee that the CLI binary this test executes was built by
//! the same `cargo test` run, so there is no stale-executable window and no build step to add to the
//! test job. It is also why the fixture here is the committed `fixtures/dwarf_probe` binary rather
//! than the 622 KB `dummy_contract.wasm` that `fixtures/build.sh` produces: the test job has no
//! wasm32 target and CI builds that artifact in a job whose files tests cannot read (#160 documents
//! the same split). The contract build's own boundary is the `#[ignore]`d case at the bottom, which
//! is #210's to invert.

use soroban_cost_profiler::formatter::OutputFormatter;
use std::path::Path;
use std::process::{Command, Output};

/// The executable cargo just built for this test run.
const PROFILER: &str = env!("CARGO_BIN_EXE_soroban-cost-profiler");
/// A contract the profiler can run today: pure computation, no host imports, DWARF line tables.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/dwarf_probe/dwarf_probe.wasm"
);
/// The same three functions built with `debug = false`, for #186's degraded-symbolization warning.
const NO_DEBUG_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/dwarf_probe/dwarf_probe_no_debug.wasm"
);
/// Where `fixtures/build.sh` leaves the real Soroban build, as in `tests/source_map_fixture.rs`.
const REAL_BUILD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/target/wasm32-unknown-unknown/release/dummy_contract.wasm"
);

/// `(module (import "env" "missing" (func)) (func (export "boom") unreachable))`.
///
/// The bytes the real contract build is full of, at 52 bytes instead of 622 KB: an unresolvable
/// import fails at link time, before a single instruction of the export runs, so no profile is
/// written. Same constant as the in-process case in `src/main.rs`; duplicated because a test file
/// cannot reach another target's private items, and these bytes are test data, not logic.
const NEEDS_HOST: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x04\x01\x60\x00\x00\x02\x0f\x01\x03env\x07missing\x00\x00\x03\x02\x01\x00\x07\x08\x01\x04boom\x00\x01\x0a\x05\x01\x03\x00\x00\x0b";

/// Run the built binary and capture everything a caller can see: code, stdout, stderr.
fn profiler(args: &[&str]) -> Output {
    Command::new(PROFILER)
        .args(args)
        .output()
        .expect("cargo built the profiler binary for this test run")
}

fn stdout(run: &Output) -> String {
    String::from_utf8_lossy(&run.stdout).into_owned()
}

fn stderr(run: &Output) -> String {
    String::from_utf8_lossy(&run.stderr).into_owned()
}

/// Write test data into the run's temp directory and return it as a CLI-ready path.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes)
        .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
    path.to_string_lossy().into_owned()
}

/// The exit code a shell would see, panicking with both streams when it is not the expected one.
fn code(run: &Output, expected: i32) {
    assert_eq!(
        run.status.code(),
        Some(expected),
        "expected exit {expected}; stdout: {:?}, stderr: {:?}",
        stdout(run),
        stderr(run)
    );
}

#[test]
fn a_finished_run_writes_a_folded_file_that_a_viewer_parser_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &output,
    ]);
    code(&run, 0);

    let artifact = std::fs::read_to_string(&output).unwrap_or_else(|error| {
        panic!("the run exited 0 but left no artifact at {output}: {error}")
    });

    // The file has to survive the parser the *other* mode feeds it to, across a process boundary.
    let stacks = OutputFormatter::parse_folded(&artifact).unwrap_or_else(|error| {
        panic!("the artifact a run writes must parse as folded stacks: {error}\n{artifact:?}")
    });
    assert!(
        !stacks.is_empty(),
        "an empty artifact is not a profile: {artifact:?}"
    );

    // And what it says today, stated rather than glossed: `wasmi` 2.0's call hook reports no program
    // counter, so every frame arrives as `wasm[0]` and every cost as 0, and the summary on stdout is
    // what makes that visible instead of the file looking like a finished profile. When PC
    // resolution lands this assertion has to change with it — that is the point of pinning it here.
    assert_eq!(artifact, "wasm[0] 0\n", "unexpected artifact: {artifact:?}");
    assert!(
        stdout(&run).contains("no function recorded any exclusive cost"),
        "a zero-cost run must say so on stdout: {:?}",
        stdout(&run)
    );
}

#[test]
fn an_unknown_export_is_refused_and_writes_no_profile() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("never.wasm").to_string_lossy().into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "not_in_this_module",
        "--output",
        &output,
    ]);
    code(&run, 1);

    let message = stderr(&run);
    assert!(
        message.contains("not an exported function"),
        "the refusal should name the mistake, got {message:?}"
    );
    // #179's promise: the error lists what the module does export.
    assert!(
        message.contains("caller_of_heavy"),
        "the refusal should list the real exports, got {message:?}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a run that never started must not leave a profile behind"
    );
}

/// The failure a real contract meets today, at the process level: link first, profile never.
///
/// This is the `fixtures/dummy-contract` case without its 622 KB artifact, which the ignored test at
/// the bottom covers directly.
#[test]
fn a_contract_that_imports_a_host_function_fails_before_it_can_profile() {
    let dir = tempfile::tempdir().unwrap();
    let wasm = write_file(dir.path(), "needs_host.wasm", NEEDS_HOST);
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&["--wasm", &wasm, "--fn", "boom", "--output", &output]);
    code(&run, 1);

    assert!(
        stderr(&run).contains("instantiate"),
        "the message must say the module never ran, not just that something went wrong: {:?}",
        stderr(&run)
    );
    assert!(
        !Path::new(&output).exists(),
        "a profile of a call that was never made is not a profile"
    );
}

#[test]
fn an_absent_contract_is_named_rather_than_mysterious() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.wasm").to_string_lossy().into_owned();

    let run = profiler(&["--wasm", &missing, "--fn", "caller_of_heavy"]);
    code(&run, 1);
    assert!(
        stderr(&run).contains(&missing),
        "the error has to say which path it could not read: {:?}",
        stderr(&run)
    );
}

/// #183's `2` from the outside: the input was accepted, the run began, and the machine refused the
/// write for a reason that has nothing to do with the command line.
#[test]
fn an_unwritable_output_path_is_the_internal_code() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where a file was asked for: refused by the OS on every account, including root.
    let output = dir.path().to_string_lossy().into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &output,
    ]);
    code(&run, 2);
    assert!(
        stderr(&run).contains("error:"),
        "an internal failure still has to be printed: {:?}",
        stderr(&run)
    );
}

#[test]
fn help_and_version_exit_zero_and_print_on_stdout() {
    let help = profiler(&["--help"]);
    code(&help, 0);
    assert!(
        stdout(&help).contains("Exit codes:"),
        "#183's table belongs in the help, and this is the process that has to print it"
    );
    assert!(
        stderr(&help).is_empty(),
        "a print the user asked for is not a warning: {:?}",
        stderr(&help)
    );

    let version = profiler(&["--version"]);
    code(&version, 0);
    assert_eq!(
        stdout(&version).trim(),
        format!("soroban-cost-profiler {}", env!("CARGO_PKG_VERSION")),
        "the version the process prints is the crate's"
    );

    // And the refusals a shell has to be able to tell apart from a crash.
    code(&profiler(&["--wasm"]), 1);
    code(&profiler(&["--sample-rate", "0"]), 1);
}

/// #186 from the outside: the warning reaches the user, and stdout stays what a caller pipes.
#[test]
fn a_binary_without_line_tables_warns_on_stderr_and_keeps_stdout_clean() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        NO_DEBUG_FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &output,
    ]);
    code(&run, 0);

    let warning = stderr(&run);
    assert!(
        warning.starts_with("warning:"),
        "a degraded run has to say so before the summary: {warning:?}"
    );
    assert!(
        !stdout(&run).contains("warning:"),
        "the warning must not be mixed into the stream callers read: {:?}",
        stdout(&run)
    );
    assert!(
        Path::new(&output).exists(),
        "the degradation is about names, not about whether the profile exists"
    );
}

#[test]
fn compare_reads_two_files_and_a_regression_is_still_a_successful_run() {
    let dir = tempfile::tempdir().unwrap();
    let base = write_file(
        dir.path(),
        "base.folded",
        b"wasm[0];caller_of_heavy 1000\nwasm[0];legacy_pack 79210\n",
    );
    let current = write_file(
        dir.path(),
        "new.folded",
        b"wasm[0];caller_of_heavy 1500\nwasm[0];packed_reader 1200\n",
    );

    let run = profiler(&["compare", &base, &current]);
    code(&run, 0);

    let report = stdout(&run);
    for expected in [
        "caller_of_heavy",
        "+500",
        "legacy_pack",
        "-79210",
        "packed_reader",
        "total",
    ] {
        assert!(
            report.contains(expected),
            "the table must carry {expected:?}:\n{report}"
        );
    }
    assert!(
        stderr(&run).is_empty(),
        "a comparison that worked has nothing to warn about: {:?}",
        stderr(&run)
    );
}

#[test]
fn a_malformed_profile_is_an_input_error_that_names_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let base = write_file(dir.path(), "base.folded", b"wasm[0];caller_of_heavy 1000\n");
    let broken = write_file(
        dir.path(),
        "broken.folded",
        b"wasm[0];no_cost_on_this_line\n",
    );

    let run = profiler(&["compare", &base, &broken]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("current: line 1"),
        "two files are in play, so the error has to say which one: {:?}",
        stderr(&run)
    );
}

/// The whole user story in one test: profile, then diff the two things this binary wrote.
#[test]
fn the_profiles_this_tool_writes_are_what_its_compare_reads() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir
        .path()
        .join("base.folded")
        .to_string_lossy()
        .into_owned();
    let current = dir.path().join("new.folded").to_string_lossy().into_owned();

    for output in [&base, &current] {
        let run = profiler(&[
            "--wasm",
            FIXTURE,
            "--fn",
            "caller_of_heavy",
            "--output",
            output,
        ]);
        code(&run, 0);
    }

    let run = profiler(&["compare", &base, &current]);
    code(&run, 0);
    assert!(
        stdout(&run).contains("no function's cost changed between the two profiles"),
        "two runs of the same contract must compare to nothing moved:\n{}",
        stdout(&run)
    );
}

/// #184's literal subject: the real Soroban build, through the real binary.
///
/// It is not in git (622 KB, and CI builds it in a job whose files the test job cannot read), so the
/// case is `#[ignore]`d rather than quietly skipping when the file is absent — the same reasoning
/// `tests/source_map_fixture.rs` gives for its counterpart. Run it after `fixtures/build.sh`:
///
/// ```sh
/// cargo test --test cli_e2e -- --ignored
/// ```
///
/// What it asserts today is the boundary, not the profile: the contract build imports Soroban host
/// functions, `instantiate_module` links against an empty linker, and the run therefore ends before
/// the export is called. **#210 owns the linker bindings, and the PR that lands them should invert
/// this test** to `code(&run, 0)` plus a parseable artifact — if it does not, this fails, which is
/// why the expected failure is written out as an assertion instead of left in a comment.
#[test]
#[ignore = "requires fixtures/build.sh; the 622 KB artifact is not in git"]
fn the_real_soroban_contract_build_still_stops_at_its_host_imports() {
    let bytes = std::fs::read(REAL_BUILD)
        .unwrap_or_else(|error| panic!("reading {REAL_BUILD}: {error} — run fixtures/build.sh"));
    assert!(
        !bytes.is_empty(),
        "the contract build should not be an empty file"
    );
    let dir = tempfile::tempdir().unwrap();
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();

    // The exports the build actually has, as the error from a wrong name reports them.
    let run = profiler(&[
        "--wasm",
        REAL_BUILD,
        "--fn",
        "compute_heavy_loop",
        "--output",
        &output,
    ]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("import"),
        "the refusal should name the missing host import, got {:?}",
        stderr(&run)
    );
    assert!(
        !Path::new(&output).exists(),
        "no host bindings means no run, and no run means no profile"
    );
}
