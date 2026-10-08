//! Phase 3's integration test (#160): does a `line-tables-only` build resolve an address to the
//! *right* line?
//!
//! The unit tests under `src/source_map/` prove the mapper loads a binary and answers for most of
//! its code section. Answering is not the same as being correct: subtracting the code section's base
//! one byte off, or stepping the line program to the neighbouring row, still yields a plausible
//! `file:line` — it just names the wrong place, and a flamegraph built from it looks exactly as
//! confident. So this file checks the resolved line against the *text* of the source the fixture was
//! built from, which is committed next to the binary: the assertion is "address `96` is the
//! statement `sum = sum.wrapping_add(buf[j]);`", not "address `96` is line `31`".
//!
//! Both fixtures here are line-tables-only builds — `fixtures/dwarf_probe` at `debug = 1`,
//! `dummy-contract` through the root manifest's `debug = "line-tables-only"` — which is all source
//! mapping needs: no variable table, no `.debug_loc`, just the address→line rows.
//!
//! The address literals are measurements against the committed artifacts, listed in
//! `docs/internals/dwarf_mapping.md`. They hold because those binaries are committed, and they move
//! when `build.sh` is rerun and its output committed — which is also what shifts the absolute paths
//! inside the artifact, so file paths here are matched by suffix only.

use soroban_cost_profiler::models::SourceFrame;
use soroban_cost_profiler::source_map::SourceMapper;

/// The committed line-tables-only fixture: `fixtures/dwarf_probe/src/lib.rs` compiled by
/// `fixtures/dwarf_probe/build.sh`, 1,690 bytes, small enough to live in git.
const FIXTURE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");
/// The source that binary was built from, so a resolved line can be read back as text.
const FIXTURE_SOURCE: &str = include_str!("../fixtures/dwarf_probe/src/lib.rs");
/// The real Soroban contract's source, for the check against `fixtures/build.sh`'s output.
const CONTRACT_SOURCE: &str = include_str!("../fixtures/dummy-contract/src/lib.rs");

/// Where `fixtures/build.sh` leaves the 622 KB contract build.
const REAL_BUILD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/target/wasm32-unknown-unknown/release/dummy_contract.wasm"
);

/// The end of every path in the committed fixture's own line tables.
///
/// Suffix rather than equality because DWARF stores the absolute path of whatever machine ran
/// `rustc`, and this artifact was built in a different worktree from this checkout.
const FIXTURE_FILE: &str = "fixtures/dwarf_probe/src/lib.rs";
/// As above, for the contract build.
const CONTRACT_FILE: &str = "fixtures/dummy-contract/src/lib.rs";

fn fixture_mapper() -> SourceMapper {
    SourceMapper::new(FIXTURE).expect("the committed fixture carries line tables")
}

/// The trimmed source text at DWARF's 1-based `line`, panicking with the source's length.
fn text_at(source: &str, line: u32) -> String {
    let index = usize::try_from(line).expect("DWARF line numbers are 1-based and positive");
    let lines: Vec<&str> = source.lines().collect();
    lines
        .get(index - 1)
        .unwrap_or_else(|| {
            panic!(
                "line {line} is past the end of a {}-line source",
                lines.len()
            )
        })
        .trim()
        .to_owned()
}

fn names_file(frame: &SourceFrame, file: &str) -> bool {
    frame
        .file_path
        .as_deref()
        .is_some_and(|path| path.ends_with(file))
}

/// The innermost frame of `stack` that belongs to `file` — the frame that attributes cost to it.
fn frame_in<'a>(stack: &'a [SourceFrame], file: &str) -> Option<&'a SourceFrame> {
    stack.iter().find(|frame| names_file(frame, file))
}

/// Asserts that `pc` resolves inside `name`, to a line of `file` whose text is `expected`.
fn assert_resolves_to(
    mapper: &SourceMapper,
    file: &str,
    source: &str,
    pc: usize,
    name: &str,
    expected: &str,
) {
    let stack = mapper.resolve(pc);
    let frame = frame_in(&stack, file)
        .unwrap_or_else(|| panic!("pc {pc} resolved to {stack:?}, which has no frame in {file}"));
    assert_eq!(frame.function_name, name, "pc {pc}");
    let line = frame
        .line_number
        .unwrap_or_else(|| panic!("pc {pc} has a file but no line: {frame:?}"));
    assert_eq!(text_at(source, line), expected, "pc {pc} is line {line}");
}

#[test]
fn a_function_body_resolves_to_the_statement_it_contains() {
    let mapper = fixture_mapper();
    // `caller_of_heavy` is one expression long, so every instruction of its body except the
    // prologue and the epilogue belongs to that expression.
    for pc in [6, 8, 14] {
        assert_resolves_to(
            &mapper,
            FIXTURE_FILE,
            FIXTURE_SOURCE,
            pc,
            "caller_of_heavy",
            "compute_heavy_loop().wrapping_add(memory_heavy_loop())",
        );
    }
    assert_resolves_to(
        &mapper,
        FIXTURE_FILE,
        FIXTURE_SOURCE,
        15,
        "caller_of_heavy",
        "}",
    );
}

#[test]
fn a_loop_resolves_to_its_own_lines_and_not_only_to_its_function() {
    let mapper = fixture_mapper();
    // `memory_heavy_loop`'s body is `18..157`, and the line program walks it statement by
    // statement: signature, buffer, fill loop, sum loop, closing brace.
    for (pc, expected) in [
        (20, "pub extern \"C\" fn memory_heavy_loop() -> u64 {"),
        (50, "let mut buf = [0u64; 64];"),
        (72, "while i < 64 {"),
        (117, "buf[i] = (i as u64).wrapping_mul(7);"),
        (90, "while j < 64 {"),
        (96, "sum = sum.wrapping_add(buf[j]);"),
        (142, "}"),
    ] {
        assert_resolves_to(
            &mapper,
            FIXTURE_FILE,
            FIXTURE_SOURCE,
            pc,
            "memory_heavy_loop",
            expected,
        );
    }
}

#[test]
fn an_inlined_call_site_resolves_to_the_line_that_made_the_call() {
    let mapper = fixture_mapper();
    // `101..=109` is `wrapping_add` inlined into `memory_heavy_loop`: two frames, and the one that
    // attributes cost to *this contract* is the outer one — whose line must be the call site, not
    // the callee's definition and not the enclosing function's signature.
    let stack = mapper.resolve(104);
    assert_eq!(stack.len(), 2, "{stack:?}");
    assert_eq!(stack[0].function_name, "<u64>::wrapping_add");
    assert!(
        stack[0]
            .file_path
            .as_deref()
            .is_some_and(|path| path.ends_with("core/src/num/uint_macros.rs")),
        "the inlined frame should name core, got {:?}",
        stack[0].file_path
    );
    assert_resolves_to(
        &mapper,
        FIXTURE_FILE,
        FIXTURE_SOURCE,
        104,
        "memory_heavy_loop",
        "sum = sum.wrapping_add(buf[j]);",
    );
}

#[test]
fn every_resolved_line_of_the_fixture_is_a_real_line_of_the_fixture_source() {
    let mapper = fixture_mapper();
    let code_map = mapper.code_map().expect("the fixture has a code section");
    let last = code_map.bodies().last().map(|body| body.end).unwrap_or(0);

    // The stronger form of the check: a line number this test cannot read would fail here rather
    // than pass vacuously, because `text_at` panics past the end of the source.
    let mut with_line = 0;
    for pc in 0..last {
        for frame in &mapper.resolve(pc) {
            // Only the fixture's own frames; the inlined core frame points into the toolchain's
            // source, whose text is not something this test can read.
            if !names_file(frame, FIXTURE_FILE) {
                continue;
            }
            if let Some(line) = frame.line_number {
                with_line += 1;
                assert!(
                    !text_at(FIXTURE_SOURCE, line).is_empty(),
                    "pc {pc}: {frame:?}"
                );
            }
            // A frame can legitimately carry a file and no line — `61..=71` and `75..=89` do —
            // which is why the two fields are separate `Option`s (#146, #147).
        }
    }
    assert!(
        with_line > 100,
        "most of the code section should carry a line, got {with_line}"
    );
}

#[test]
fn a_prologue_names_a_function_without_a_line_and_a_folded_loop_keeps_the_function_line() {
    let mapper = fixture_mapper();
    // Address `2` is the first byte of `caller_of_heavy`: the function is known, the line is not.
    let stack = mapper.resolve(2);
    assert_eq!(
        stack.last().expect("non-empty").function_name,
        "caller_of_heavy"
    );
    assert_eq!(
        stack.last().expect("non-empty").line_number,
        None,
        "{stack:?}"
    );

    // `compute_heavy_loop`'s counted loop folds away at `opt-level = "z"`, so its whole body
    // attributes to the signature (`158..=163`) and the closing brace (`164`) — never to
    // `while i < 1000` or the accumulate statement. That is correct, and it is why a
    // `line-tables-only` profile of an optimized contract shows a loop as one line.
    for pc in 158..164 {
        assert_resolves_to(
            &mapper,
            FIXTURE_FILE,
            FIXTURE_SOURCE,
            pc,
            "compute_heavy_loop",
            "pub extern \"C\" fn compute_heavy_loop() -> u64 {",
        );
    }
    assert_resolves_to(
        &mapper,
        FIXTURE_FILE,
        FIXTURE_SOURCE,
        164,
        "compute_heavy_loop",
        "}",
    );
    assert!(
        mapper.resolve(165).is_empty(),
        "one past the last body resolves to nothing"
    );
}

/// The 622 KB Soroban build is what a user actually profiles, and the only artifact here with real
/// inlined-dependency frames. It stays out of git — CI builds it in a job whose files the test job
/// cannot read — so this case is `#[ignore]`d rather than quietly skipping when the file is absent:
/// a check that silently does not run is worse than one that is plainly outside the suite.
///
/// Run it after `fixtures/build.sh`:
///
/// ```sh
/// cargo test --test source_map_fixture -- --ignored
/// ```
#[test]
#[ignore = "requires fixtures/build.sh; the 622 KB artifact is not in git"]
fn the_real_soroban_build_resolves_contract_lines() {
    let bytes = std::fs::read(REAL_BUILD)
        .unwrap_or_else(|error| panic!("reading {REAL_BUILD}: {error} — run fixtures/build.sh"));
    let mapper = SourceMapper::new(&bytes).expect("the contract build carries line tables");
    let code_map = mapper.code_map().expect("the contract has a code section");
    let last = code_map.bodies().last().map(|body| body.end).unwrap_or(0);

    // The contract's own functions are inlined into the exported `invoke_raw_extern` body, so the
    // frame that names `src/lib.rs` sits partway down the stack, not at either end.
    for (pc, name, expected) in [
        (55, "compute_heavy_loop", "for i in 0..iterations {"),
        (184, "memory_heavy_loop", "let mut vec = Vec::new(env);"),
        (255, "memory_heavy_loop", "for i in 0..iterations {"),
        (271, "memory_heavy_loop", "vec.len()"),
    ] {
        assert_resolves_to(&mapper, CONTRACT_FILE, CONTRACT_SOURCE, pc, name, expected);
    }

    // And the paths are the build machine's absolute ones, dependency sources mixed in with the
    // contract's: anything that groups or shortens frames has to expect `.cargo/registry` and
    // `/rustc/<hash>` next to the contract path.
    let mut contract_frames = 0;
    for pc in 0..last {
        for frame in &mapper.resolve(pc) {
            if !names_file(frame, CONTRACT_FILE) {
                continue;
            }
            contract_frames += 1;
            let path = frame.file_path.clone().expect("matched by suffix");
            assert!(
                path.starts_with('/'),
                "DWARF stores absolute build paths: {path}"
            );
            // `125` and `312` are two of the addresses that name the contract's file with no line
            // in it, the same shape `61..=71` has in the committed fixture.
            if let Some(line) = frame.line_number {
                assert!(
                    !text_at(CONTRACT_SOURCE, line).is_empty(),
                    "pc {pc}: {frame:?}"
                );
            }
        }
    }
    assert!(
        contract_frames > 100,
        "got {contract_frames} contract frames"
    );
    assert!(
        mapper.resolve(0).is_empty(),
        "address 0 is the code section's count byte, not code"
    );
}
