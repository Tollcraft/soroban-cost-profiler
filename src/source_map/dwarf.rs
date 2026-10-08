//! Stage 2's DWARF half: the error the stage reports, the address map DWARF is written
//! against, and every traversal that reads a line table.
//!
//! This is the module the other two are the alternative to. [`super::wasm`] walks the container and
//! [`super::names`] reads the `name` section, and neither names a `gimli` type; everything that does
//! is here, which is what makes `AGENTS.md`'s "no custom DWARF parsing" rule checkable as a file
//! boundary rather than as a reading of the code: the traversal below is `gimli::Dwarf::load` into
//! `addr2line::Context::from_dwarf` into `find_frames`, and the only hand-written parsing in the
//! whole stage is the two container walks that live in those other files.
//!
//! Two of [`SourceMapper`](super::SourceMapper)'s methods are implemented here rather than beside the
//! rest of its `impl` block, because they are the two that walk DWARF:
//! [`resolve_dwarf`](super::SourceMapper::resolve_dwarf) is the traversal the resolution cache in the
//! parent remembers the answer of, and
//! [`degenerate_sample`](super::SourceMapper::degenerate_sample) counts how many sampled addresses
//! the line tables fail to answer. What stays in the parent is the facade — construction, the
//! DWARF-then-`name` precedence, the cache and its bound, and the threshold that turns that count
//! into a warning — plus `collapse_closures`, which is shared with the `name` path and belongs to
//! neither source: a closure has to render as `[closure#0]` whichever of the two named it.
//!
//! The measured facts the traversal rests on — the address space, the inline-stack order, the
//! locations that carry a file and no line — are `docs/internals/dwarf_mapping.md`.

use super::wasm::{NAME_SECTION, WasmSections};
use super::{SourceMapper, collapse_closures};
use crate::models::SourceFrame;
use addr2line::FunctionName;
use std::ops::Range;
use std::rc::Rc;

/// The reader type `addr2line` is instantiated with.
///
/// An `Rc`-backed slice rather than `EndianSlice<'a, ..>`: the mapper has to own the section
/// bytes, because the bytes it symbolizes against are read once from the binary and the
/// `Context` then serves a whole trace. A borrowed reader would tie the mapper's lifetime to
/// the buffer the caller passed to [`SourceMapper::new`].
pub(super) type Reader = gimli::EndianRcSlice<gimli::NativeEndian>;

pub(super) type Dwarf = gimli::Dwarf<Reader>;
pub(super) type Context = addr2line::Context<Reader>;

/// Why a binary could not be symbolized, in the user's terms.
///
/// Every variant's message names the flag or the file that fixes it: this error is what a
/// contract author reads when the flamegraph comes out unnamed, and "invalid DWARF" alone
/// tells them nothing actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceMapError {
    /// The bytes are not a WASM module — too short to hold a header, or the wrong magic.
    NotWasm,
    /// A section header claimed more bytes than the file contains, i.e. a truncated download
    /// or a partial write rather than a malformed build.
    Truncated,
    /// A valid module with no `.debug_info`, and no `name` section able to name its functions
    /// either.
    ///
    /// The ordinary case for a release build: `debug` is off for `release` by default, so nothing
    /// was emitted to map. A stripped build that kept its `name` section is *not* this error — #157
    /// loads it and resolves function-name-only frames — so reaching here means both sources are
    /// gone, which is what `wasm-opt --strip-debug` leaves. (An *optimized* artifact keeps stale
    /// DWARF and loses `name`, so it loads and resolves nothing; that is #162's case, not this one.)
    MissingDebugInfo {
        /// Custom sections that *were* present, so the message can point out a `name` section worth
        /// falling back to (see the function-name-only path in Phase 3's fallback chain).
        custom_sections: Vec<String>,
    },
    /// DWARF sections are present but `gimli` could not read them — an incomplete build, an
    /// unsupported DWARF version, or a section that was stripped after linking.
    UnreadableDwarf { reason: String },
}

impl std::fmt::Display for SourceMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWasm => write!(
                f,
                "not a WebAssembly module: expected the 8-byte header `\\0asm\\x01\\0\\0\\0`. \
                 Pass the `.wasm` file itself, not a `.wat` text file or an archive."
            ),
            Self::Truncated => write!(
                f,
                "WebAssembly module ended in the middle of a section. Re-download or rebuild it: \
                 a truncated file cannot be mapped, and would fail to instantiate too."
            ),
            Self::MissingDebugInfo { custom_sections } => {
                write!(
                    f,
                    "no `.debug_info` section, so program counters cannot be mapped to Rust \
                     source lines. Build the copy you profile with a profiling profile — \
                     `[profile.profiling]` with `inherits = \"release\"` and \
                     `debug = \"line-tables-only\"` is enough for `file:line` frames — and profile \
                     that artifact rather than the stripped one (`wasm-opt`, and `stellar contract \
                     build`, strip debug info). Keep `debug` out of `[profile.release]`: that is \
                     the profile whose output gets deployed, and mainnet bills for the extra bytes."
                )?;
                if custom_sections.is_empty() {
                    write!(f, " The module carries no custom sections at all.")?;
                } else {
                    write!(f, " Sections present: {}.", custom_sections.join(", "))?;
                    if custom_sections
                        .iter()
                        .any(|section| section == NAME_SECTION)
                    {
                        // After #157 a readable `name` section loads, so saying "name" without
                        // saying why it did not help would leave the user reading a contradiction.
                        write!(
                            f,
                            " Its `name` section holds no function name for this module's code \
                             section, so the function-name-only fallback has nothing to offer."
                        )?;
                    }
                }
                Ok(())
            }
            Self::UnreadableDwarf { reason } => write!(
                f,
                "the binary has DWARF sections but they could not be read ({reason}). \
                 This usually means the artifact was partially stripped or built with an \
                 unsupported DWARF version; rebuild it from source."
            ),
        }
    }
}

impl std::error::Error for SourceMapError {}

impl SourceMapper {
    /// How many sampled addresses this binary's line tables fail to answer, and how many were
    /// sampled: `(missing_or_duplicate, total_sampled)`.
    ///
    /// Samples every tenth address of every function body and counts the ones that resolve to
    /// nothing or to a line some other address already claimed. `None` when there is nothing to
    /// sample — no code section, no DWARF, or a code section whose bodies are all empty — which is
    /// "nothing to say", not a ratio of zero.
    ///
    /// The counting lives here because it walks line tables; what the ratio *means* is the facade's
    /// (`SourceMapper::degenerate_warning`, with the threshold beside it), so the measurement and the
    /// policy can change independently.
    pub(super) fn degenerate_sample(&self) -> Option<(usize, usize)> {
        let code_map = self.code_map()?;
        let context = self.context.as_ref()?;

        let mut total_sampled = 0;
        let mut missing_or_duplicate = 0;
        let mut unique_lines = std::collections::HashSet::new();

        for body in code_map.bodies() {
            // Sample every 10th instruction to avoid blocking startup on huge binaries
            for pc in (body.start..body.end).step_by(10) {
                total_sampled += 1;

                let mut resolved = false;
                if let Ok(mut frames) = context.find_frames(pc as u64).skip_all_loads()
                    && let Ok(Some(frame)) = frames.next()
                    && let Some(loc) = frame.location
                {
                    resolved = true;
                    let line_id = (loc.file.map(String::from), loc.line);
                    if !unique_lines.insert(line_id) {
                        missing_or_duplicate += 1;
                    }
                }

                if !resolved {
                    missing_or_duplicate += 1;
                }
            }
        }

        (total_sampled > 0).then_some((missing_or_duplicate, total_sampled))
    }

    /// Ask DWARF for one address's inline stack, innermost frame first.
    ///
    /// [`SourceMapper::resolve`] is the public door and the one that remembers; this is the walk it
    /// caches. Splitting them keeps the cache out of the semantics: everything #146–#156 pinned —
    /// the range guards, `skip_all_loads`, a nameless frame ending only itself — lives here, and is
    /// reached at most once per address per mapper.
    pub(super) fn resolve_dwarf(&self, context: &Context, pc: usize) -> Vec<SourceFrame> {
        // `addr2line` probes the half-open range `[address, address + 1)`, so `u64::MAX` overflows
        // inside that computation and panics a debug build. No code section is within orders of
        // magnitude of that, so an address this high is not an offset and gets the same answer as
        // any other address outside every range.
        let Ok(address) = u64::try_from(pc) else {
            return Vec::new();
        };
        if address == u64::MAX {
            return Vec::new();
        }

        // `skip_all_loads`: every section was copied into the reader at construction, so there is
        // nothing to load, and a split-DWARF request would try to open a file that never existed.
        let Ok(mut frames) = context.find_frames(address).skip_all_loads() else {
            return Vec::new();
        };

        // A `gimli` error partway through the stack stops the walk and keeps what was collected:
        // the frames already read are ones the trace can be charged to, and dropping them because a
        // frame further out is unreadable would throw away real attribution.
        let mut stack = Vec::new();
        while let Ok(Some(frame)) = frames.next() {
            let Some(function_name) = frame.function.as_ref().and_then(frame_name) else {
                continue;
            };

            let location = frame.location.as_ref();
            stack.push(SourceFrame {
                function_name,
                file_path: location.and_then(|loc| loc.file).map(str::to_string),
                line_number: location.and_then(|loc| loc.line),
            });
        }

        stack
    }
}

/// The code section's address map: what an offset in the file means to DWARF.
///
/// `addr2line`'s line tables for a wasm build are written against offsets into the code section's
/// **payload** — the bytes after the section id and length — so address `0` is the function-count
/// byte, not the first instruction. That makes the relationship between the two spaces a constant
/// the module itself carries, and this type holds it: [`Self::to_code_address`] subtracts the
/// payload's start, [`Self::bodies`] gives where each function begins, and the two agree only if the
/// base is exactly right, which is how the rule was pinned against the committed fixtures rather
/// than copied from a spec reading.
///
/// Built by walking the section table and the code section's function framing — wasm container bytes
/// again, never DWARF — and best effort: an unreadable code section yields no map from
/// [`SourceMapper::code_map`] instead of an error, because resolving a DWARF address does not need
/// one.
///
/// # Examples
///
/// ```
/// use soroban_cost_profiler::source_map::SourceMapper;
///
/// let fixture = include_bytes!("../../fixtures/dwarf_probe/dwarf_probe.wasm");
/// let mapper = SourceMapper::new(fixture).expect("the fixture carries DWARF");
/// let map = mapper.code_map().expect("the fixture has a code section");
///
/// // Three functions, and every body begins where the line tables begin to answer.
/// let bodies = map.bodies();
/// assert_eq!(bodies.len(), 3);
/// assert_eq!(bodies[0], 2..16);
/// assert_eq!(map.function_at(2), Some(0));
/// assert_eq!(map.function_at(0), None, "the count byte belongs to no function body");
/// for (index, body) in bodies.iter().enumerate() {
///     let stack = mapper.resolve(body.start);
///     assert!(!stack.is_empty(), "a body's first byte is its prologue");
///     assert_eq!(map.function_at(body.start), Some(index), "{stack:?}");
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeMap {
    /// File offset of the code section payload, which is DWARF address `0`.
    base: usize,
    /// Payload length, so an offset past the last function is rejected rather than translated.
    len: usize,
    /// `start..end` of each defined function's body, relative to [`Self::base`], in code-section
    /// order. The code section lists only *defined* functions, so these are positions in that list —
    /// a module function index is this plus the import count, which is how [`NameSection`](super::names::NameSection)
    /// lines the two up.
    bodies: Vec<Range<usize>>,
}

impl CodeMap {
    /// Read a code section payload, given where it starts in the file.
    ///
    /// `None` if the declared function list runs past the payload, which is a malformed or truncated
    /// module rather than a file worth profiling. Iteration is bounded by the bytes rather than by
    /// the declared count, so a count field that claims millions cannot allocate a table for them —
    /// `AGENTS.md`'s OOM rule applies to the whole binary, not only the trace.
    pub(super) fn parse(payload: &[u8], base: usize) -> Option<Self> {
        let mut cursor = 0;
        let count = WasmSections::read_uleb(payload, &mut cursor).ok()?;
        let mut bodies = Vec::new();

        for _ in 0..count {
            let size = usize::try_from(WasmSections::read_uleb(payload, &mut cursor).ok()?).ok()?;
            let start = cursor;
            let end = start.checked_add(size)?;
            if end > payload.len() {
                return None;
            }
            bodies.push(start..end);
            cursor = end;
        }

        Some(Self {
            base,
            len: payload.len(),
            bodies,
        })
    }

    /// Move a byte offset in the file into the address space `addr2line` indexes.
    ///
    /// `None` when the offset is outside the code section — including the bytes before it, which a
    /// naive subtraction would turn into a huge address that resolves to nothing.
    pub fn to_code_address(&self, file_offset: usize) -> Option<usize> {
        let address = file_offset.checked_sub(self.base)?;
        (address < self.len).then_some(address)
    }

    /// The body range of each defined function, in code-relative addresses.
    pub fn bodies(&self) -> &[Range<usize>] {
        &self.bodies
    }

    /// Which defined function's body contains this code-relative address.
    ///
    /// `None` for the gaps every code section has: the count byte, the per-function size prefixes,
    /// and one past the end. Those are real addresses that belong to no instruction, which is why
    /// they resolve to no frame and why #162's "everything is unmapped" check has to tell the two
    /// cases apart.
    pub fn function_at(&self, address: usize) -> Option<usize> {
        let index = self
            .bodies
            .partition_point(|body| body.start <= address)
            .checked_sub(1)?;
        self.bodies[index].contains(&address).then_some(index)
    }
}

/// The section whose absence means the binary cannot be source-mapped at all.
///
/// Line tables alone cannot name a function, and an address only resolves through a compilation
/// unit, so `.debug_info` is what makes the other `.debug_*` sections meaningful.
pub(super) const DEBUG_INFO: &str = ".debug_info";

/// DWARF section names, as they appear in a WASM custom section.
///
/// Rust emits each section as its own custom section rather than one merged `DWARF` blob, so
/// [`WasmSections::parse`] can hand gimli exactly the bytes [`gimli::SectionId::name`] asks for.
/// A lookup miss returns empty bytes, which is how gimli is told "this optional section does not
/// exist" — DWARF 4 builds have no `.debug_line_str`, `.debug_addr` or `.debug_str_offsets`.
///
/// Infallible by construction: `Dwarf::load` only fails through the closure, and copying bytes
/// into an owned reader cannot fail. What *can* fail is reading the contents, which is
/// [`addr2line::Context::from_dwarf`]'s error and the caller's to report.
pub(super) fn load_dwarf(sections: &WasmSections) -> Dwarf {
    // Copied per section, once, at load: the mapper outlives the caller's buffer, so the bytes
    // it indexes have to be owned.
    let reader =
        |bytes: &[u8]| Reader::new(Rc::from(bytes.to_vec()), gimli::NativeEndian::default());

    Dwarf::load(&mut |id: gimli::SectionId| {
        Ok::<_, std::convert::Infallible>(reader(sections.get(id.name()).unwrap_or(&[])))
    })
    .expect("copying section bytes into a reader cannot fail")
}

/// The name to put in a frame, demangled where the DWARF says how.
///
/// `addr2line`'s `demangle()` applies `rustc-demangle` for `DW_LANG_Rust` and uses its *alternate*
/// format, which drops the `-<hash>` suffixes crate symbols carry — the difference between
/// `_RNvNtNtCs..17soroban_env_guest5guest3vec13vec_push_back` and a readable path. When the
/// language is absent or the name will not parse, it hands back the raw symbol unchanged, which is
/// why `#[no_mangle] extern "C"` functions and C symbols arrive as plain names here.
///
/// What demangling does *not* fix is the closure segments it leaves in place, so the name goes
/// through [`collapse_closures`] before it becomes a frame.
///
/// `None` means "no name to build a frame from", which [`SourceMapper::resolve`] treats as
/// unattributable; an empty string would key every such address to the same flamegraph frame.
fn frame_name(function: &FunctionName<Reader>) -> Option<String> {
    let name = function.demangle().ok()?;
    (!name.is_empty()).then(|| collapse_closures(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_map::tests::DWARF_PROBE;
    use crate::source_map::wasm::tests::{module, uleb};

    #[test]
    fn a_real_build_with_line_tables_loads() {
        // The happy path against DWARF a toolchain actually emitted, not a fixture assembled by
        // hand: this is the artifact `fixtures/dwarf_probe/build.sh` produces from Rust source.
        let mapper = SourceMapper::new(DWARF_PROBE)
            .unwrap_or_else(|error| panic!("the DWARF-bearing fixture should load: {error}"));

        assert!(mapper.has_debug_info());

        // Present is not the same as usable. Walking every compilation unit's line table is the
        // check that the section bytes reached gimli whole: a mis-sliced custom section still
        // loads, then fails here with an unexpected end of input.
        let context = mapper.context.as_ref().expect("has_debug_info()");
        context.parse_lines().unwrap_or_else(|error| {
            panic!("the fixture's line tables should be readable: {error}")
        });
    }

    #[test]
    fn unreadable_dwarf_is_reported_rather_than_panicking() {
        // Present-but-nonsense sections: `gimli` rejects them, and Phase 5's CLI needs that to be
        // a message about rebuilding, not a crash in the middle of a profile run.
        let garbage = module(&[
            (".debug_info", b"not dwarf at all"),
            (".debug_abbrev", b"also not dwarf"),
            (".debug_line", b"nor this"),
        ]);

        let error = SourceMapper::new(&garbage)
            .err()
            .expect("malformed DWARF must not load as if it were fine");

        let SourceMapError::UnreadableDwarf { ref reason } = error else {
            panic!("expected the unreadable-DWARF error, got {error:?}");
        };
        assert!(
            error.to_string().contains("rebuild"),
            "the message has to point at a fix: {error}"
        );
        assert!(!reason.is_empty(), "and say what gimli objected to");
    }

    /// The innermost frame the fixture's DWARF gives for `pc`, with a failure that names the
    /// address. Tests that care about the rest of the inline stack read `resolve` directly.
    fn frame_at(mapper: &SourceMapper, pc: usize) -> SourceFrame {
        mapper
            .resolve(pc)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("pc {pc} lies inside the fixture's code section"))
    }

    #[test]
    fn a_code_offset_resolves_to_its_function_file_and_line() {
        // One address from the middle of each of the fixture's three functions. `pc` is
        // code-section-relative, which is the form #153 hands to this call. The line is checked
        // against the function's source span rather than a single number so that rebuilding the
        // fixture with a different toolchain cannot fail the test for the wrong reason.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        for (pc, function, span) in [
            (3usize, "caller_of_heavy", 38..=40u32),
            (20, "memory_heavy_loop", 21..=35),
            (158, "compute_heavy_loop", 10..=18),
        ] {
            let frame = frame_at(&mapper, pc);

            assert_eq!(
                frame.function_name, function,
                "pc {pc} named the wrong function"
            );
            assert!(
                frame
                    .file_path
                    .as_deref()
                    .is_some_and(|file| file.ends_with("fixtures/dwarf_probe/src/lib.rs")),
                "pc {pc} resolved to {:?}, not the fixture's only source file",
                frame.file_path
            );
            let line = frame
                .line_number
                .unwrap_or_else(|| panic!("pc {pc} in `{function}` should carry a line number"));
            assert!(
                span.contains(&line),
                "pc {pc} resolved to `{function}:{line}`, outside its span {span:?}"
            );
        }
    }

    #[test]
    fn a_prologue_resolves_a_function_name_with_no_location() {
        // The first instructions of `caller_of_heavy` precede the line program's first entry, so
        // DWARF knows the function but not the line. Cost still has to land somewhere, so a name
        // is enough to make a frame, and the two location fields stay `None` rather than becoming
        // line 0. (`61`..`71` are the mirror case: a file and no line.)
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let frame = frame_at(&mapper, 2);

        assert_eq!(frame.function_name, "caller_of_heavy");
        assert_eq!(frame.file_path, None);
        assert_eq!(frame.line_number, None);
    }

    #[test]
    fn a_frame_in_inlined_dependency_code_is_demangled() {
        // `caller_of_heavy`'s body inlines `u64::wrapping_add`, so the innermost frame at `14` is
        // core code reached through a Rust v0 symbol. This is the whole of #148's example
        // (`my_contract::swap` is the same kind of name) and it shows the frame pointing at a
        // registry path next to the contract's own -- the reason anything that groups frames by
        // file has to expect both.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let frame = frame_at(&mapper, 14);

        assert_eq!(frame.function_name, "<u64>::wrapping_add");
        assert!(
            frame
                .file_path
                .as_deref()
                .is_some_and(|file| file.ends_with("core/src/num/uint_macros.rs")),
            "expected inlined core code, got {:?}",
            frame.file_path
        );
        assert!(frame.line_number.is_some_and(|line| line > 0));
    }

    #[test]
    fn an_inlined_call_site_answers_with_the_whole_stack_innermost_first() {
        // #156's acceptance criterion, on the fixture's own inlined call. Address `14` is
        // `wrapping_add` inlined into `caller_of_heavy`, and the two frames name different files
        // *and* different lines: one `SourceFrame` per address could say which code the address is,
        // but never which call site put it there. The order is the one `addr2line` walks -- callee
        // first, caller last -- so `stack[0]` stays the frame the tree keys on and the callers
        // beneath it are available to anyone who wants the depth.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let stack = mapper.resolve(14);
        let names: Vec<&str> = stack
            .iter()
            .map(|frame| frame.function_name.as_str())
            .collect();

        assert_eq!(names, ["<u64>::wrapping_add", "caller_of_heavy"]);
        assert_eq!(
            stack[0].line_number,
            Some(2612),
            "core's line, from the fixture's DWARF"
        );
        assert_eq!(stack[1].line_number, Some(39), "the contract's call site");
        assert_ne!(
            stack[0].file_path, stack[1].file_path,
            "an inline stack whose frames share a file is a different shape than this one: {stack:?}"
        );
    }

    #[test]
    fn the_stack_depth_is_measured_across_the_whole_code_section() {
        // Not one hand-picked address but the fixture's entire 166-byte code section, because the
        // interesting fact is the *distribution*: inlining is the exception here, not the rule. 6
        // addresses are framing bytes that belong to no instruction, 150 yield one frame, and 10
        // yield two -- `14` in `caller_of_heavy`, then `101`..=`109`, nine consecutive bytes of
        // `memory_heavy_loop`'s inlined `wrapping_add`. Nothing is deeper than two, which is what a
        // three-line fixture should produce; a third level would mean the walk is reading past the
        // function that owns the address.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let depths: Vec<(usize, usize)> = (0..166usize)
            .map(|pc| (pc, mapper.resolve(pc).len()))
            .filter(|(_, depth)| *depth != 1)
            .collect();
        let singles = 166 - depths.len();

        assert_eq!(
            depths,
            vec![
                (0, 0),
                (1, 0),
                (14, 2),
                (16, 0),
                (17, 0),
                (101, 2),
                (102, 2),
                (103, 2),
                (104, 2),
                (105, 2),
                (106, 2),
                (107, 2),
                (108, 2),
                (109, 2),
                (157, 0),
                (165, 0),
            ]
        );
        assert_eq!(singles, 150);

        // The nine inlined bytes of one loop body are the same call, so they must be the same stack
        // -- otherwise the tree would pool them under nine different names.
        let loop_stack = mapper.resolve(101);
        for address in 101..110 {
            assert_eq!(mapper.resolve(address), loop_stack, "address {address}");
        }
        assert_eq!(
            loop_stack
                .iter()
                .map(|frame| frame.function_name.as_str())
                .collect::<Vec<_>>(),
            ["<u64>::wrapping_add", "memory_heavy_loop"]
        );
    }

    #[test]
    fn every_stack_ends_in_a_function_the_contract_exports() {
        // The outermost frame is the one a wasm call boundary can name, so `addr2line`'s walk has to
        // finish there: if a stack ever bottomed out inside `core`, the depth Stage 3 would build
        // from it is wrong rather than merely richer.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let exported = ["caller_of_heavy", "memory_heavy_loop", "compute_heavy_loop"];

        for address in 0..166 {
            let stack = mapper.resolve(address);
            let Some(outer) = stack.last() else { continue };

            assert!(
                exported.contains(&outer.function_name.as_str()),
                "address {address} bottoms out in {:?}: {stack:?}",
                outer.function_name
            );
        }
    }

    #[test]
    fn resolution_covers_most_of_the_code_section() {
        // Not "DWARF is present" but "DWARF maps an executed address": the fixture's code section
        // is 165 bytes, and one address per byte is swept. 160 of those 166 resolve — and because
        // 10 of them are inlined call sites, the stack they hand back is 170 frames long. The
        // sweep is flat rather than one-frame-per-address so the names below include the inlined
        // half of every stack, which is where a mangled symbol would actually leak through.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let frames: Vec<SourceFrame> = (0..166usize).flat_map(|pc| mapper.resolve(pc)).collect();
        let names: Vec<&str> = frames
            .iter()
            .map(|frame| frame.function_name.as_str())
            .collect();

        assert!(
            frames.len() > 150,
            "only {} of 166 code-section offsets resolved",
            frames.len()
        );
        for function in ["caller_of_heavy", "memory_heavy_loop", "compute_heavy_loop"] {
            assert!(
                names.contains(&function),
                "`{function}` was never named; frames resolved as {names:?}"
            );
        }
        for frame in &frames {
            assert!(!frame.function_name.is_empty(), "an empty frame: {frame:?}");
            assert!(
                !frame.function_name.starts_with("_R"),
                "a mangled symbol reached a frame: {:?}",
                frame.function_name
            );
        }
    }

    #[test]
    fn addresses_outside_the_code_section_resolve_to_nothing() {
        // The gaps between functions (`16`, `17`, `157`), the first address past the end (`166`),
        // and the values a mis-translated or uninitialised pc looks like. `usize::MAX` is the
        // interesting one: `addr2line` probes `[address, address + 1)` and overflows on it, which
        // would panic a debug build rather than report an unattributable address.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        for pc in [
            0usize,
            1,
            16,
            17,
            157,
            166,
            4096,
            1 << 20,
            usize::MAX - 1,
            usize::MAX,
        ] {
            assert!(mapper.resolve(pc).is_empty(), "pc {pc} should not resolve");
        }
    }

    #[test]
    fn address_zero_is_the_code_sections_function_count_byte() {
        // The base #153 translates against, pinned to the fixture's own bytes: its code section
        // payload runs from file offset 111 to 276, so those two are the boundary cases — the
        // offset before the payload must fail rather than wrap into a huge address, and one past
        // the end must fail rather than look like an unattributed line.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let map = mapper.code_map().expect("the fixture has a code section");

        assert_eq!(map.to_code_address(111), Some(0));
        assert_eq!(map.to_code_address(275), Some(164));
        assert_eq!(map.to_code_address(276), None);
        assert_eq!(map.to_code_address(110), None);
        assert_eq!(map.to_code_address(0), None);
        assert_eq!(map.to_code_address(usize::MAX), None);
    }

    #[test]
    fn every_function_body_starts_where_dwarf_starts_answering() {
        // The strongest check available on a committed binary, and the one that cannot pass by
        // being self-consistent: `bodies` comes from the wasm framing and `resolve` comes from
        // DWARF, so they agree only if the translation base is exactly right. Move the base by one
        // byte and these addresses land on a size prefix or outside the section, and `resolve`
        // answers an empty stack for all three.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let map = mapper.code_map().expect("the fixture has a code section");
        let base = 111; // the fixture's code section payload, measured from its section table

        assert_eq!(map.bodies(), &[2..16, 18..157, 158..165]);

        let names = ["caller_of_heavy", "memory_heavy_loop", "compute_heavy_loop"];
        for (index, body) in map.bodies().iter().enumerate() {
            let stack = mapper.resolve_file_offset(base + body.start);
            let frame = stack
                .first()
                .unwrap_or_else(|| panic!("offset {} is a function prologue", base + body.start));
            assert_eq!(frame.function_name, names[index]);
            assert_eq!(map.function_at(body.start), Some(index));
        }
    }

    #[test]
    fn the_gaps_between_bodies_are_addresses_but_not_instructions() {
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let map = mapper.code_map().expect("the fixture has a code section");

        // The count byte, both size prefixes of the 139-byte second body, and everything from the
        // last `end` opcode onward. These are valid addresses that belong to no function, which is
        // why "resolved nothing" and "was never an instruction" have to stay separate answers.
        for gap in [0usize, 1, 16, 17, 165, 166, usize::MAX] {
            assert_eq!(map.function_at(gap), None, "address {gap} is a gap");
        }

        for (address, index) in [
            (2, 0),
            (15, 0),
            (18, 1),
            (75, 1),
            (156, 1),
            (158, 2),
            (164, 2),
        ] {
            assert_eq!(map.function_at(address), Some(index), "address {address}");
        }
    }

    #[test]
    fn an_offset_outside_the_code_section_is_not_an_unattributed_line() {
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        // Header, another section's bytes, and past EOF. A file offset that is not in the code
        // section is a caller's mistake, and reporting it as "this line has no frame" would send
        // someone debugging the line table instead of their arithmetic.
        for offset in [0usize, 8, 10, 110, 276, 279, 1_000, 1_000_000, usize::MAX] {
            assert!(
                mapper.resolve_file_offset(offset).is_empty(),
                "file offset {offset} is outside the code section"
            );
        }

        // And the boundary the other way: the first offset inside it does resolve.
        assert!(!mapper.resolve_file_offset(113).is_empty());
    }

    #[test]
    fn an_absurd_function_count_never_becomes_a_capacity() {
        // A 12-byte module declaring 2^64-1 functions must not allocate for them: the walk is
        // bounded by the bytes, not the count. `AGENTS.md`'s OOM rule is about the trace, and the
        // same habit applies to reading a binary.
        let payload = [uleb(u64::MAX), uleb(1), vec![0x00]].concat();

        assert_eq!(CodeMap::parse(&payload, 0), None);
    }
}
