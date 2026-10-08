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
//! the same split). The contract build itself is what the `#[ignore]`d case at the bottom runs, once
//! `fixtures/build.sh` has produced it.

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

/// `(module (func (export "needs_arg") (param i64) (result i64) local.get 0 i64.const 2 i64.add))`.
///
/// #211's subject at 46 bytes: an export whose signature has a parameter, because none of the
/// committed fixtures do and the test job has no wasm32 target — the same reason `NEEDS_HOST` is
/// hand-assembled. The body returns `argument + 2`; that is what lets the in-process case in
/// `src/main.rs` prove the value arrived, while what is observable *here* is that the call happens at
/// all and writes an artifact.
const NEEDS_ARG: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x06\x01\x60\x01\x7e\x01\x7e\x03\x02\x01\x00\x07\x0d\x01\x09\x6e\x65\x65\x64\x73\x5f\x61\x72\x67\x00\x00\x0a\x09\x01\x07\x00\x20\x00\x42\x02\x7c\x0b";

/// `(module (import "x" "3" (func (result i64))) (func (export "read_sequence") (result i64) call 0))`.
///
/// #212's subject at 55 bytes: a contract whose only work is a ledger read, so the difference
/// between a blank ledger and a mocked one is the difference between exit 1 and exit 0 and nothing
/// else. `x.3` is the guest name of `get_ledger_sequence`; the same bytes are in `src/main.rs`, and
/// duplicated here because a test target cannot reach another target's private items.
const READS_LEDGER: &[u8] = b"\x00\x61\x73\x6d\x01\x00\x00\x00\x01\x05\x01\x60\x00\x01\x7e\x02\x07\x01\x01x\x01\x33\x00\x00\x03\x02\x01\x00\x07\x11\x01\rread_sequence\x00\x01\x0a\x06\x01\x04\x00\x10\x00\x0b";

/// The committed `--state` file: a protocol-28 ledger at sequence 500.
const LEDGER_STATE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/state/ledger.json");

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

/// #212 from outside the process, as the pair the issue asks for: the contract that traps on the
/// blank ledger finishes when the run names a snapshot.
///
/// Both invocations run the same bytes and the same export, so the flag is the only variable. The
/// second one also carries `-v`, because the mocked-ledger line is how a caller distinguishes the
/// profile of a mocked ledger from one of a blank it — and #214's rule puts that record on stderr,
/// which leaves stdout exactly as the run without the flag wrote it.
#[test]
fn a_state_snapshot_answers_a_ledger_read_that_traps_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let wasm = write_file(dir.path(), "reads_ledger.wasm", READS_LEDGER);
    let blank = dir
        .path()
        .join("blank.folded")
        .to_string_lossy()
        .into_owned();

    let trapped = profiler(&["--wasm", &wasm, "--fn", "read_sequence", "--output", &blank]);
    code(&trapped, 1);
    assert!(
        stderr(&trapped).contains("host function 'x.3' failed"),
        "the trap has to name the host function the contract called: {:?}",
        stderr(&trapped)
    );

    let mocked = dir
        .path()
        .join("mocked.folded")
        .to_string_lossy()
        .into_owned();
    let finished = profiler(&[
        "--wasm",
        &wasm,
        "--fn",
        "read_sequence",
        "--state",
        LEDGER_STATE,
        "--output",
        &mocked,
        "-v",
    ]);
    code(&finished, 0);
    assert!(
        stderr(&finished).contains("mocked ledger state"),
        "a verbose run says which ledger it priced against: {:?}",
        stderr(&finished)
    );
    assert_eq!(
        std::fs::read_to_string(&mocked).unwrap(),
        "wasm[0] 0\nwasm[0];host[0] 0\n",
        "the finished run crosses one wasm boundary and one host call"
    );
}

/// A file `--state` cannot use is refused before the contract is read, so the failure is a command
/// line to fix rather than a profile of a run that did not honour the request.
#[test]
fn a_state_file_that_is_not_a_snapshot_is_refused_and_writes_no_profile() {
    let dir = tempfile::tempdir().unwrap();
    let wasm = write_file(dir.path(), "reads_ledger.wasm", READS_LEDGER);
    let notes = write_file(dir.path(), "notes.json", b"{\"todo\": \"fill this in\"}");
    let output = dir
        .path()
        .join("refused.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        &wasm,
        "--fn",
        "read_sequence",
        "--state",
        &notes,
        "--output",
        &output,
    ]);
    code(&run, 1);

    let message = stderr(&run);
    assert!(
        message.contains("not a Soroban ledger snapshot") && message.contains("protocol_version"),
        "the refusal should name the file and the field it wanted: {message:?}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a run refused before it started must not leave a profile behind"
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

/// #213's flag from outside the process, where the halt is a shell-visible event: exit 1, a message
/// that names which guard fired, and the trace up to the halt left on disk.
///
/// The pair matters more than the single case. `caller_of_heavy` crosses two boundaries per
/// invocation — `wasmi` 2.0 reports the entry and the exit and nothing between them — so a ceiling of
/// 2 completes the run and a ceiling of 1 stops it, and the two runs differ only by the flag. That is
/// what "the profiler respects the limit" means while it can be tested honestly: a contract that runs
/// a million instructions inside one body still emits two boundaries and would sail past any ceiling
/// above 2, which is `README.md`'s "The 100M ceiling cannot see an infinite loop": closing that gap needs a
/// per-instruction hook, and `wasmi` 2.0 has none to hang one on.
#[test]
fn an_instruction_limit_halts_the_run_and_keeps_the_trace_so_far() {
    let dir = tempfile::tempdir().unwrap();
    let halted = dir
        .path()
        .join("halted.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &halted,
        "--instruction-limit",
        "1",
    ]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("Instruction ceiling exceeded"),
        "the halt must name its own guard rather than look like a contract trap: {:?}",
        stderr(&run)
    );
    // #173's rule, applied to the profiler's own ceiling: the guard stopped a run that had already
    // crossed boundaries, so the partial trace is the artifact and not nothing.
    let artifact = std::fs::read_to_string(&halted)
        .unwrap_or_else(|error| panic!("a halted run still writes its trace: {error}"));
    let stacks = OutputFormatter::parse_folded(&artifact)
        .unwrap_or_else(|error| panic!("a halted run must write valid folded stacks: {error}"));
    assert!(
        !stacks.is_empty(),
        "the partial profile must have a frame: {artifact:?}"
    );

    // One boundary higher and the same command succeeds, so the halt above is the flag's doing.
    let finished = dir
        .path()
        .join("finished.folded")
        .to_string_lossy()
        .into_owned();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &finished,
        "--instruction-limit",
        "2",
    ]);
    code(&run, 0);
    // The two files are the same bytes, and that is the finding rather than a redundancy: the guard
    // fires while unwinding, `wasmi` still reports `ReturningFromWasm`, and the halted profile is
    // structurally identical to the finished one. Only the exit code and stderr tell them apart, so
    // this asserts both and never lets the artifact alone be read as "the call completed".
    assert_eq!(
        std::fs::read_to_string(&finished).unwrap(),
        artifact,
        "a run that halts at the exit boundary and one that finishes it hold the same frames"
    );

    // And a refused value never reaches the engine.
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--instruction-limit",
        "0",
    ]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("--instruction-limit"),
        "the refusal has to name the flag: {:?}",
        stderr(&run)
    );
}

/// #214 from outside the process, which is the only place its two halves are observable: that a
/// subscriber writes, and which stream it writes to.
///
/// The split is the whole point of the test. Records go to stderr because stdout is the contract
/// #184 pinned — the ranked summary is what callers pipe — so a subscriber on stdout would break
/// exactly the runs that use `-v` and nothing else. Meanwhile the default level must stay silent:
/// every message a user is meant to read leaves through `warn_user` or `main`, and a subscriber
/// printing the same sentence twice over is the defect #198 removed.
#[test]
fn verbose_prints_the_stages_on_stderr_and_leaves_stdout_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();
    let base = [
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &output,
    ];

    let plain = profiler(&base);
    code(&plain, 0);
    assert!(
        stderr(&plain).is_empty(),
        "the default is WARN, and the crate has no `warn!` record left, so a plain run \
         must print no tracing at all: {:?}",
        stderr(&plain)
    );

    let verbose = profiler(&[base.as_slice(), &["-v"]].concat());
    code(&verbose, 0);
    let transcript = stderr(&verbose);
    assert!(
        transcript.contains("INFO") && transcript.contains("Invoking function: caller_of_heavy"),
        "`-v` must name the stages a run passes through: {transcript:?}"
    );
    assert!(
        !transcript.contains("\x1b["),
        "the subscriber is built without `ansi`, so the transcript stays greppable and \
         redirect-friendly: {transcript:?}"
    );
    assert_eq!(
        stdout(&verbose),
        stdout(&plain),
        "records must not leak into the stream callers pipe"
    );

    // Deeper notches reach the engine's own breadcrumbs: every boundary the call hook reports,
    // then the single costed step recorded at each one.
    let boundaries = profiler(&[base.as_slice(), &["-vv"]].concat());
    code(&boundaries, 0);
    assert!(
        stderr(&boundaries).contains("WASM Call at PC: 0"),
        "`-vv` must show the boundaries: {:?}",
        stderr(&boundaries)
    );

    let steps = profiler(&[base.as_slice(), &["-vvv"]].concat());
    code(&steps, 0);
    assert!(
        stderr(&steps).contains("Stepping at PC: 0, cpu: 1, mem: 0"),
        "`-vvv` must show the per-boundary step the tracer substitutes for an instruction hook: \
         {:?}",
        stderr(&steps)
    );
}

/// `--quiet` is the conventional Unix meaning, and the e2e half is what makes it real: stdout empty
/// while the artifact is byte-for-byte the one a loud run writes. Warnings and errors are not
/// narration, so `a_degraded_run_under_quiet` below pins that they still arrive.
#[test]
fn quiet_writes_the_artifact_and_prints_nothing_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let loud = dir.path().join("loud.folded");
    let silent = dir.path().join("silent.folded");
    let loud_path = loud.to_string_lossy().into_owned();
    let silent_path = silent.to_string_lossy().into_owned();

    let plain = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &loud_path,
    ]);
    code(&plain, 0);
    assert!(
        !stdout(&plain).is_empty(),
        "the default run prints its summary: {:?}",
        stdout(&plain)
    );

    let quiet = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--output",
        &silent_path,
        "--quiet",
    ]);
    code(&quiet, 0);
    assert_eq!(
        stdout(&quiet),
        "",
        "`--quiet` means the file is the answer, so stdout carries nothing at all"
    );
    assert!(
        stderr(&quiet).is_empty(),
        "a healthy quiet run has no news either: {:?}",
        stderr(&quiet)
    );
    assert_eq!(
        std::fs::read_to_string(&silent).unwrap(),
        std::fs::read_to_string(&loud).unwrap(),
        "quiet suppresses narration, not the profile"
    );
}

/// The half a pure log-level flag would have got wrong. `--quiet` still reports: #186's degraded
/// warning precedes the summary and survives the flag, because a run that worked less than the user
/// asked for is news rather than commentary.
#[test]
fn quiet_keeps_the_warning_about_a_degraded_run() {
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
        "--quiet",
    ]);
    code(&run, 0);
    assert!(
        stderr(&run).starts_with("warning:"),
        "a degraded run must say so even when asked to be quiet: {:?}",
        stderr(&run)
    );
    assert_eq!(stdout(&run), "", "and still print no summary");
}

/// Two flags that contradict each other are refused before the engine starts, with #183's code and
/// both names in the message.
#[test]
fn verbose_and_quiet_are_refused_as_a_command_line_to_fix() {
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
        "-v",
        "--quiet",
    ]);
    code(&run, 1);
    let message = stderr(&run);
    assert!(
        message.contains("--verbose") && message.contains("--quiet"),
        "the refusal has to name both flags the user typed: {message:?}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a refused command line never reaches the engine"
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

/// #215's JSON format, through the real binary: the artifact has to be a document a program reads,
/// not prose that happens to contain braces.
///
/// The metric assertion is the part no other format can carry. A `.folded` file records no metric, so
/// `compare` cannot check that its two inputs agreed on `--metric` — here the disagreement is visible
/// in the file itself, which is the actual reason this format exists beside the folded one.
#[test]
fn the_json_format_writes_a_document_that_names_its_own_metric() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir
        .path()
        .join("profile.json")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "json",
        "--output",
        &output,
    ]);
    code(&run, 0);

    let artifact = std::fs::read_to_string(&output).unwrap_or_else(|error| {
        panic!("the run exited 0 but left no artifact at {output}: {error}")
    });
    let document: serde_json::Value = serde_json::from_str(&artifact).unwrap_or_else(|error| {
        panic!("`--format json` wrote something that is not JSON: {error}\n{artifact}")
    });

    assert_eq!(document["metric"], "cpu");
    assert_eq!(document["root"]["function"], "wasm[0]");
    // What the tree says today, pinned in the new format the same way the folded artifact pins it:
    // no program counter means no attribution, and the JSON cannot invent a cost the engine did not
    // report. This assertion has to change when PC resolution lands, and that is the point.
    assert_eq!(document["root"]["exclusive"]["cpu"], 0);
    assert_eq!(document["root"]["inclusive"]["cpu"], 0);
    assert!(
        document["root"]["children"].is_array(),
        "children are an array so a reader walks them without guessing at keys"
    );

    // The same contract under `--metric memory`: the numbers are still zero, the field is not.
    let memory = dir
        .path()
        .join("memory.json")
        .to_string_lossy()
        .into_owned();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "json",
        "--metric",
        "memory",
        "--output",
        &memory,
    ]);
    code(&run, 0);
    let document = std::fs::read_to_string(&memory).unwrap();
    assert!(
        document.contains("\"metric\": \"memory\""),
        "the metric a run selected has to be readable from its artifact:\n{document}"
    );
}

/// `--output -` is a stream, not a file named `-`.
///
/// Both halves matter: the artifact has to arrive on stdout with nothing else in the way (a summary
/// printed after it is a second document in a stream that parses as neither), and a file called `-`
/// must not materialize in whoever's directory the run happened in.
#[test]
fn a_dash_output_sends_the_artifact_to_stdout_and_the_summary_stays_out() {
    let dir = tempfile::tempdir().unwrap();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "json",
        "--output",
        "-",
    ]);
    code(&run, 0);

    let text = stdout(&run);
    let document: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("stdout has to be one JSON document and nothing else: {error}\n{text}")
    });
    assert_eq!(document["metric"], "cpu");
    assert!(
        !text.contains("no function recorded"),
        "the terminal summary cannot share stdout with the artifact: {text}"
    );
    assert!(
        !dir.path().join("-").exists(),
        "`-` is a convention for stdout, not a file this run may write"
    );
    // stderr is still stderr: warnings about a degraded run are not narration.
    assert_eq!(stderr(&run), "", "a healthy run writes nothing on stderr");
}

/// `--format raw` with the byte-for-byte stream it writes, and the count the terminal adds.
///
/// Two rates, one contract: the engine reports two call boundaries and steps are what `--sample-rate`
/// throttles, so the default run writes the two unconditional events and `--sample-rate 1` writes
/// four. That is the ceiling `AGENTS.md` rule 5 and README's Limitations section describe, visible in
/// an artifact without a tree in it — which is what this format is for.
#[test]
fn the_raw_format_writes_the_stream_the_engine_reported() {
    let dir = tempfile::tempdir().unwrap();
    let sampled = dir
        .path()
        .join("profile.raw")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "raw",
        "--output",
        &sampled,
    ]);
    code(&run, 0);
    assert_eq!(
        std::fs::read_to_string(&sampled).unwrap(),
        "call pc=0 cpu=0 mem=0\nreturn pc=0 cpu=0 mem=0\n",
        "at the default sample rate the only events are the two boundaries the engine emits itself"
    );
    assert_eq!(
        stdout(&run),
        format!("2 trace events written to {sampled}\n"),
        "the terminal says what the file holds, because a raw run has no tree to rank"
    );

    let dense = dir.path().join("dense.raw").to_string_lossy().into_owned();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "raw",
        "--sample-rate",
        "1",
        "--output",
        &dense,
    ]);
    code(&run, 0);
    assert_eq!(
        std::fs::read_to_string(&dense).unwrap(),
        "call pc=0 cpu=0 mem=0\nstep pc=0 cpu=1 mem=0\nreturn pc=0 cpu=0 mem=0\nstep pc=0 cpu=1 mem=0\n",
        "`--sample-rate 1` un-throttles the step events beside the boundaries"
    );
    assert!(
        !stdout(&run).contains("no function recorded any exclusive cost"),
        "a raw run never builds a tree, so it has no ranking to print: {:?}",
        stdout(&run)
    );
}

/// The trap message names the artifact's destination whatever that destination is (#215's `-`).
///
/// The file form is the sentence `docs/troubleshooting.md` quotes and the `--instruction-limit` test
/// pins; the stdout form is new, and both have to stay true because together they are how a reader
/// finds out that a truncated profile is truncated.
#[test]
fn a_halved_run_reports_where_its_partial_trace_went_in_either_destination() {
    let dir = tempfile::tempdir().unwrap();
    let halted = dir
        .path()
        .join("halted.json")
        .to_string_lossy()
        .into_owned();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "json",
        "--instruction-limit",
        "1",
        "--output",
        &halted,
    ]);
    code(&run, 1);
    assert!(
        stderr(&run).contains(&format!("is in {halted},")),
        "the file form keeps the wording the troubleshooting guide quotes: {:?}",
        stderr(&run)
    );

    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "raw",
        "--instruction-limit",
        "1",
        "--output",
        "-",
    ]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("The partial trace up to the trap is on stdout"),
        "a run that halted while writing to a stream has to say so about the stream: {:?}",
        stderr(&run)
    );
    assert_eq!(
        stdout(&run),
        "call pc=0 cpu=0 mem=0\nreturn pc=0 cpu=0 mem=0\n",
        "and the partial stream still reaches that stream"
    );
}

#[test]
fn an_unknown_format_is_refused_and_names_the_ones_that_exist() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("never.raw").to_string_lossy().into_owned();
    let run = profiler(&[
        "--wasm",
        FIXTURE,
        "--fn",
        "caller_of_heavy",
        "--format",
        "yaml",
        "--output",
        &output,
    ]);
    code(&run, 1);
    let message = stderr(&run);
    assert!(
        message.contains("--format") && message.contains("folded") && message.contains("raw"),
        "the refusal has to name the flag and the values it accepts: {message}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a refused command line runs no contract and writes nothing"
    );
}

/// #211's flag from outside the process, as the pair of runs that differ only by `--args`.
///
/// The half that is not about the happy path is the one that changed behaviour. Before the signature
/// check, `needs_arg` with no argument was `wasmi`'s trap: exit 1, `encountered an incorrect number of
/// parameters`, **and a `.folded` file left beside it** — a profile of a call that was never legal,
/// which is the worst artifact this tool can produce. So both cases assert the absence or presence of
/// the file, not only the exit code.
#[test]
fn an_export_that_takes_arguments_profiles_only_once_they_are_given() {
    let dir = tempfile::tempdir().unwrap();
    let wasm = write_file(dir.path(), "needs_arg.wasm", NEEDS_ARG);
    let output = dir
        .path()
        .join("profile.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&["--wasm", &wasm, "--fn", "needs_arg", "--output", &output]);
    code(&run, 1);
    let message = stderr(&run);
    assert!(
        message.contains("'needs_arg' takes 1 argument (i64)")
            && message.contains("--args gave no values"),
        "the refusal has to state the export's own signature, got {message:?}"
    );
    assert!(
        !message.contains("incorrect number of parameters"),
        "the engine's trap text must not be what a user is left with: {message:?}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a call that was never legal must not leave a profile behind"
    );

    let run = profiler(&[
        "--wasm",
        &wasm,
        "--fn",
        "needs_arg",
        "--args",
        "40",
        "--output",
        &output,
    ]);
    code(&run, 0);
    // `wasm[0] 0`, byte-for-byte the artifact `caller_of_heavy` writes: this fixture has no line
    // tables and `wasmi` 2.0 hands the hook no program counter, so what the flag proves is that the
    // call *happened* — the value itself is asserted in `src/main.rs`, where the return is reachable.
    assert_eq!(
        std::fs::read_to_string(&output).unwrap_or_else(|error| panic!(
            "with `--args 40` the export runs, so it must leave an artifact: {error}"
        )),
        "wasm[0] 0\n"
    );
    assert!(
        stdout(&run).contains("no function recorded any exclusive cost"),
        "the summary is still what tells the reader this zero is a zero: {:?}",
        stdout(&run)
    );

    // Too many values is the same refusal, from the same place.
    let run = profiler(&["--wasm", &wasm, "--fn", "needs_arg", "--args", "1,2"]);
    code(&run, 1);
    assert!(
        stderr(&run).contains("--args gave 2 values"),
        "the count given is as informative as the count wanted: {:?}",
        stderr(&run)
    );
}

/// A value `--args` cannot parse never reaches the engine. clap's own refusal names the flag and the
/// offending text, exits with #183's input code, and leaves no artifact — the same shape as every
/// other refused command line in this file, which is why it is pinned here rather than trusted to the
/// flag's doc comment.
#[test]
fn an_argument_that_is_not_a_number_is_refused_by_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let wasm = write_file(dir.path(), "needs_arg.wasm", NEEDS_ARG);
    let output = dir
        .path()
        .join("never.folded")
        .to_string_lossy()
        .into_owned();

    let run = profiler(&[
        "--wasm",
        &wasm,
        "--fn",
        "needs_arg",
        "--args",
        "abc",
        "--output",
        &output,
    ]);
    code(&run, 1);
    let message = stderr(&run);
    assert!(
        message.contains("--args") && message.contains("abc"),
        "the refusal has to name the flag and the value it could not read: {message:?}"
    );
    assert!(
        !Path::new(&output).exists(),
        "a refused command line runs no contract and writes nothing"
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
/// #210 inverted this test, as it said it should. The build imports four Soroban host functions —
/// `vec_new`, `obj_from_u64`, `vec_push_back`, `vec_len` — and they now resolve against the real
/// `Host`, so `memory_heavy_loop` runs to completion and its host calls land in the profile as
/// costed frames. The two runs here are the pair that makes that claim checkable: the host-using
/// export produces a nonzero `host[…]` frame, the pure one still produces `wasm[0] 0` and nothing
/// else, so a regression in either direction shows up as the wrong half of the pair.
///
/// The arguments are words, not numbers: an SDK export reads each parameter as a `Val`, so a `u32`
/// argument arrives tagged (`value << 32 | 4` for `U32Val`, which is 42949672964 for `10`).
#[test]
#[ignore = "requires fixtures/build.sh; the 622 KB artifact is not in git"]
fn the_real_soroban_contract_build_runs_and_its_host_calls_are_costed() {
    let bytes = std::fs::read(REAL_BUILD)
        .unwrap_or_else(|error| panic!("reading {REAL_BUILD}: {error} — run fixtures/build.sh"));
    assert!(
        !bytes.is_empty(),
        "the contract build should not be an empty file"
    );
    let dir = tempfile::tempdir().unwrap();

    let run = profiler(&[
        "--wasm",
        REAL_BUILD,
        "--fn",
        "memory_heavy_loop",
        "--args",
        "42949672964", // U32Val(10)
        "--output",
        &dir.path().join("host.folded").to_string_lossy(),
    ]);
    code(&run, 0);
    let path = dir.path().join("host.folded");
    let artifact = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("the run exited 0 but left no artifact: {error}"));
    let stacks = OutputFormatter::parse_folded(&artifact).unwrap_or_else(|error| {
        panic!("a real contract run must write valid folded stacks: {error}")
    });
    assert_eq!(stacks.len(), 2, "unexpected artifact: {artifact:?}");
    // 10 pushes, a `vec_new` and a `len` — 102 host calls — charged from the host budget, which is
    // the one accurate cost the trace carries today. The exact figure tracks `soroban-env-host`.
    let host_cost: u64 = artifact
        .lines()
        .find(|line| line.contains("host["))
        .unwrap_or_else(|| panic!("the host frame is missing from {artifact:?}"))
        .rsplit(' ')
        .next()
        .and_then(|value| value.parse().ok())
        .expect("the host frame's cost parses as a number");
    assert!(
        host_cost > 0,
        "a real host call that costs nothing: {artifact:?}"
    );
    assert!(
        stdout(&run).contains("host[0]"),
        "the summary should rank the host frame it measured: {:?}",
        stdout(&run)
    );

    // The same binary, an export that touches no host function: unchanged behaviour, and the
    // contrast that says the frame above came from the bindings rather than from new accounting.
    let pure = profiler(&[
        "--wasm",
        REAL_BUILD,
        "--fn",
        "compute_heavy_loop",
        "--args",
        "42949672964", // U32Val(10)
        "--output",
        &dir.path().join("pure.folded").to_string_lossy(),
    ]);
    code(&pure, 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("pure.folded")).unwrap(),
        "wasm[0] 0\n",
        "a pure export crosses no host boundary"
    );
}
