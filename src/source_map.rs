use crate::models::SourceFrame;
use addr2line::FunctionName;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

/// The reader type `addr2line` is instantiated with.
///
/// An `Rc`-backed slice rather than `EndianSlice<'a, ..>`: the mapper has to own the section
/// bytes, because the bytes it symbolizes against are read once from the binary and the
/// `Context` then serves a whole trace. A borrowed reader would tie the mapper's lifetime to
/// the buffer the caller passed to [`SourceMapper::new`].
type Reader = gimli::EndianRcSlice<gimli::NativeEndian>;

type Dwarf = gimli::Dwarf<Reader>;
type Context = addr2line::Context<Reader>;

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
                     source lines. Build the contract with debug info enabled — \
                     `[profile.release] debug = \"line-tables-only\"` is enough for `file:line` \
                     frames — and profile that artifact rather than the stripped one \
                     (`wasm-opt`, and `stellar contract build`, strip debug info)."
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

/// Stage 2 of the pipeline: turn a program counter into a Rust source frame.
///
/// # Why the pipeline needs it
///
/// Tracer events carry a `pc`, and a bare `pc` is not actionable. A flamegraph frame named
/// `wasm[17]` tells a contract author nothing; the same frame named
/// `compute_heavy_loop (src/lib.rs:5)` names the loop to fix. This type is the only place
/// that translation happens, which is why [`ProfileAggregator::aggregate`] borrows a
/// `&SourceMapper` instead of naming frames itself, and why [`SourceFrame`] splits the name
/// from the location: a stripped binary can still yield a function name, and a line table
/// can still yield a line with no name.
///
/// # What it holds
///
/// An `addr2line::Context` built from the DWARF custom sections of the binary that was loaded
/// ([`SourceMapper::new`]), the same binary's `name` section when it has no DWARF at all (#157),
/// or nothing ([`SourceMapper::unmapped`]) for a caller that chose to continue without symbols.
/// Construction is fallible and reports *why* there are no symbols, because "the binary has no
/// debug info" and "the address is outside every range" need different fixes and only the first is
/// the user's to make.
///
/// The facts below were checked against real builds rather than assumed, because each one
/// decided how much code Phase 3 is:
///
/// * **DWARF arrives as separate WASM custom sections, so loading is a section walk.** A
///   `wasm32-unknown-unknown` build with debug info carries `.debug_abbrev`, `.debug_info`,
///   `.debug_str`, `.debug_line`, `.debug_ranges` (and `.debug_loc` where applicable) as
///   individual custom sections, alongside `name`, `producers`, `target_features` and
///   Soroban's `contractspecv0`. They are not merged into one `DWARF` section, so
///   `ROADMAP.md`'s "parse the `.debug_info` and `.debug_line` sections" is literally right,
///   and the only hand-written parsing here is the WASM container — the section table, the code
///   section's function framing [`CodeMap`] needs, and the two short subsection walks #157 reads for
///   function names — never DWARF itself.
///   `AGENTS.md`'s "no custom DWARF parsing" rule holds: `gimli::Dwarf::load` reads the
///   sections, `addr2line::Context::from_dwarf` builds the index.
/// * **`addr2line` needs no file wrapper and adds no dependency weight.** Its
///   `default-features = false, features = ["std", "rustc-demangle"]` build pulls neither
///   `object` nor `cpp_demangle` nor `memmap2`, and both it and `gimli` were already in
///   `Cargo.lock` via `backtrace`.
/// * **`gimli` has to be a direct dependency anyway.** `addr2line` exposes `gimli` only as a
///   re-export of its own instantiation, and that build enables `features = ["read"]`, which
///   omits `endian-reader` — so `addr2line::gimli::EndianRcSlice`, the owned reader a mapper
///   that outlives the caller's buffer needs, does not exist through it. Naming gimli in
///   `Cargo.toml` adds `endian-reader` and `std` to the same 0.32.3 already resolved, and `std`
///   is not optional here: `EndianRcSlice` only implements `gimli::Reader` once
///   `stable_deref_trait`'s `std` feature gives `Rc<[u8]>: CloneStableDeref`. The whole footprint
///   is those two small pure-Rust crates entering `Cargo.lock`.
/// * **Debug info costs more than the contract.** The same fixture is 3.1 KB stripped and 622 KB
///   with `debug = "line-tables-only"` — 619 KB of which is DWARF custom sections against a
///   488-byte code section — because a `std`-linked build emits debug info for every inlined
///   dependency, not just the contract's own functions. That is the price of profiling a real
///   Soroban build, and it is why `fixtures/build.sh`'s output is a *profiling input*, never
///   something to deploy. A `#![no_std]` crate with `panic = "abort"` shows the floor: the
///   same three functions cost 1.7 KB total.
/// * **DWARF addresses are offsets into the code section payload, and [`CodeMap`] is what puts a
///   value into that space.** The base is measured rather than assumed: address `0` is the code
///   section's function-count byte, so a file offset translates by subtracting the payload's start.
///   In `fixtures/dwarf_probe/dwarf_probe.wasm` that start is file offset `111`, and the three
///   function bodies land on `2..16`, `18..157` and `158..165` — precisely where `resolve` begins
///   answering, which is the check that pins the base. Sweeping that code section resolves 160 of
///   its 166 addresses, 133 of them to a line. What is still missing is an address worth
///   resolving: `wasmi` 2.0's only execution hook is `Store::call_hook`, whose closure receives a
///   `CallHook` *variant* and nothing else — no callee, no instruction hook, no program counter —
///   and because the engine re-encodes wasm bytecode into its own instruction stream and discards
///   the original offsets as it goes, a runtime position would not map back even if one were
///   exposed. So every event [`invoke_function`] records is written at `pc = 0`, and the finest
///   attribution the engine can be given is one function body.
/// * **The `name` section survives this build path**, so a function-name-only fallback is real
///   (#157); a plain `cargo build` does not strip it, while
///   `stellar contract build` does. That is the precedence the docs should describe: DWARF ->
///   `name` -> function index.
///
/// Two traps the resolution issues will hit, both seen in the probe that produced the numbers
/// above:
///
/// * A location can carry a file and *no* line: 26 addresses of the committed fixture's code
///   section (`61`–`71` and `75`–`89`, inside `memory_heavy_loop`) resolve to
///   `file: Some(".../src/lib.rs"), line: None`. `SourceFrame`'s `Option` location fields are
///   independent for exactly this reason, so fill them separately instead of treating a
///   missing line as line 0 or as a failed lookup.
/// * Paths are the absolute paths of whatever machine built the contract, and can contain
///   `../` segments. The probe resolved to `/private/tmp/<crate>/src/lib.rs`, and an address
///   in inlined dependency code resolved through
///   `/rustc/<hash>/library/compiler-builtins/.../../../../libm/src/math/...`. So anything
///   that groups or shortens frames by file must expect std/registry paths next to contract
///   paths, and a report is only readable next to the build that produced it.
///
/// The long form of this material — the measured address table, the inline-stack semantics, and
/// how to produce a binary that maps at all — is `docs/internals/dwarf_mapping.md`.
///
/// [`ProfileAggregator::aggregate`]: crate::aggregator::ProfileAggregator::aggregate
/// [`invoke_function`]: crate::tracer::invoke_function
/// [`SourceMapper::new`]: SourceMapper::new
/// [`SourceMapper::unmapped`]: SourceMapper::unmapped
/// [`SourceMapper::resolve`]: SourceMapper::resolve
pub struct SourceMapper {
    context: Option<Context>,
    /// Where this binary's code section is, so file offsets can be moved into the address space
    /// `context` indexes. `None` when the module has no code section or its function list does not
    /// match its declared size — DWARF resolution does not need it, so it is not an error.
    code: Option<CodeMap>,
    /// The `name` section's function names: the fallback for a binary that carries symbols but no
    /// DWARF (#157).
    ///
    /// `None` when there is no function-names subsection to read, which is also what makes
    /// [`SourceMapper::new`] report [`SourceMapError::MissingDebugInfo`] rather than load a binary
    /// that can say nothing about itself.
    names: Option<NameSection>,
    /// Addresses whose inline stack this mapper has already computed (#158).
    ///
    /// A trace revisits addresses: the tracer records one event per call boundary and per sample,
    /// and a loop is the same handful of addresses over and over. Resolving one costs 250 ns for a
    /// single frame and 1.05 µs at an inlined call site (measured on the committed fixture's
    /// `opt-level = "z"` release build, where walking the line program dominates), so a run that
    /// attributes thousands of events spends most of Stage 2 recomputing answers it just gave.
    ///
    /// A `RefCell` rather than `&mut self`, because [`ProfileAggregator::aggregate`] takes the mapper
    /// by `&`: making every caller hold a mutable mapper to save a line-program walk is the worse
    /// trade, and the pipeline is single-threaded, so the borrow check is all the synchronization
    /// this needs.
    ///
    /// Bounded by [`RESOLUTION_CACHE_LIMIT`], and an address that resolves to nothing is stored too:
    /// "nothing" is the most expensive answer to recompute on the stale-DWARF binary #162 warns about,
    /// whose tables have to be walked to find out they describe no live code.
    cache: RefCell<HashMap<usize, Vec<SourceFrame>>>,
}

/// How many resolved addresses [`SourceMapper::cache`] holds before it starts over (#158).
///
/// The bound exists because a cache keyed by program counter is only as small as the trace that
/// fills it, and `AGENTS.md`'s OOM rule does not stop at the event buffer. The number is a working
/// set, not a guess: a run attributes one address per call boundary and per sample, and the address
/// space those can name is the binary's code section — 166 addresses for the committed fixture, 489
/// for the real 622 KB build #141 measured — so 4,096 holds every address either of them can present
/// twenty times over, and the frames it retains are worth a few hundred KB at that cap.
///
/// Overflow clears rather than evicts least-recently-used. Clearing costs recomputation and never
/// correctness, which is all a cache is allowed to cost; an LRU's bookkeeping (or a new dependency)
/// only pays for a workload that cycles through more distinct addresses than the cap, and a trace
/// that does is one whose answers are too scattered for any reuse policy to help.
const RESOLUTION_CACHE_LIMIT: usize = 4096;

impl SourceMapper {
    /// Build a mapper for one already-loaded WASM binary, reading its DWARF or, failing that, its
    /// `name` section.
    ///
    /// Takes bytes rather than a path because [`load_wasm_file`] has already read and validated
    /// the file, and the same bytes are handed to `parse_module` — reading twice would let the
    /// traced binary and the symbolized binary disagree.
    ///
    /// The `Context` is built here rather than on first lookup: it walks every compilation unit,
    /// which is expensive, and a trace then reads it millions of times. A caller that cannot
    /// afford to fail on a binary without symbols should match on [`SourceMapError`] and fall
    /// back to [`SourceMapper::unmapped`] — the profiler's job is to keep running and say why
    /// frames are unnamed, not to abort the run being measured.
    ///
    /// # Errors
    ///
    /// Returns [`SourceMapError`] when the bytes are not a module ([`SourceMapError::NotWasm`],
    /// [`SourceMapError::Truncated`]) or carry neither DWARF nor function names
    /// ([`SourceMapError::MissingDebugInfo`], [`SourceMapError::UnreadableDwarf`]). Each message
    /// names the flag or step that would fix it.
    ///
    /// # Examples
    ///
    /// ```
    /// use soroban_cost_profiler::source_map::{SourceMapError, SourceMapper};
    ///
    /// // A build with debug info off but its `name` section left in is degraded, not hopeless:
    /// // the mapper loads, and every frame it gives is a function with no file and no line.
    /// let stripped = include_bytes!("../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");
    /// let mapper = SourceMapper::new(stripped).expect("this fixture keeps its `name` section");
    /// assert!(!mapper.has_debug_info(), "names only — there is no DWARF to ask");
    /// let stack = mapper.resolve(3);
    /// assert_eq!(stack.len(), 1);
    /// assert_eq!(stack[0].function_name, "caller_of_heavy");
    /// assert_eq!(stack[0].file_path, None, "`name` is function-level, not line-level");
    ///
    /// // A binary with neither is the case that fails, and the message says what to change.
    /// let mut bare = b"\0asm\x01\0\0\0".to_vec();
    /// bare.extend([10, 2, 1, 0]); // the code section: one function, empty body
    /// let error = SourceMapper::new(&bare).err().expect("no DWARF and no names");
    /// assert!(matches!(error, SourceMapError::MissingDebugInfo { .. }));
    ///
    /// // The same functions built with `debug = 1` load DWARF, and the difference is the point.
    /// let mapped = SourceMapper::new(include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm"));
    /// assert!(mapped.unwrap().has_debug_info());
    /// ```
    ///
    /// [`load_wasm_file`]: crate::tracer::load_wasm_file
    pub fn new(wasm_bytes: &[u8]) -> Result<Self, SourceMapError> {
        let sections = WasmSections::parse(wasm_bytes)?;
        let names = NameSection::parse(&sections);

        // `.debug_info` is what makes the other sections meaningful; a module that has line
        // tables but no compilation units cannot yield a file or a line. It can still name the
        // function an address belongs to, and that is #157's fallback — enough to load. A module
        // with neither has nothing to say about itself, and only the user's build flag fixes that.
        if sections.get(DEBUG_INFO).is_none() {
            let Some(names) = names else {
                return Err(SourceMapError::MissingDebugInfo {
                    custom_sections: sections.custom_names(),
                });
            };

            return Ok(Self {
                context: None,
                code: sections.code,
                names: Some(names),
                cache: RefCell::new(HashMap::new()),
            });
        }

        let context = Context::from_dwarf(load_dwarf(&sections)).map_err(|error| {
            SourceMapError::UnreadableDwarf {
                reason: error.to_string(),
            }
        })?;

        let mapper = Self {
            context: Some(context),
            code: sections.code,
            names,
            cache: RefCell::new(HashMap::new()),
        };

        mapper.check_degenerate_mappings();
        Ok(mapper)
    }

    /// Computes the ratio of PCs mapping to duplicate or `None` lines and emits a warning if it is highly degenerate.
    fn check_degenerate_mappings(&self) {
        let Some(code_map) = self.code_map() else {
            return;
        };
        let Some(context) = self.context.as_ref() else {
            return;
        };

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

        if total_sampled > 0 {
            let degenerate_ratio = missing_or_duplicate as f64 / total_sampled as f64;
            if degenerate_ratio > 0.90 {
                tracing::warn!(
                    "Heavily mangled or degenerate line mappings detected (ratio: {:.2}).                      Your DWARF info may describe pre-optimization code.",
                    degenerate_ratio
                );
            }
        }
    }

    /// A mapper that resolves nothing, for a run that continues without symbols.
    ///
    /// This is the degraded-but-working path: Stages 3 and 4 still produce a tree, keyed by the
    /// `wasm[pc]` / `host[pc]` names the tracer already has. `main`'s harness uses it because it
    /// has no binary to read yet — Phase 5's CLI replaces it with [`SourceMapper::new`] plus the
    /// warning the returned error carries.
    pub fn unmapped() -> Self {
        Self {
            context: None,
            code: None,
            names: None,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Whether this mapper has DWARF to resolve against.
    ///
    /// Lets a caller distinguish "no frames because nothing was attributed" from "no frames
    /// because no symbols were loaded" — the difference between a bug in the profiler and a
    /// user's missing build flag, which the CLI has to report.
    pub fn has_debug_info(&self) -> bool {
        self.context.is_some()
    }

    /// Resolve one program counter to the inline stack that produced it, innermost frame first.
    ///
    /// `pc` is an offset into the WASM **code section**, the address space the DWARF line tables
    /// are written against; #153 owns translating whatever the engine reports into that form.
    ///
    /// The order is the one `addr2line` walks a stack: the inlined function, then whoever inlined
    /// it, down to the function the wasm call actually entered. Measured against
    /// `fixtures/dwarf_probe`, 160 of its 166 code-section addresses answer at all and **10 of
    /// those answer with two frames** — address `14` is `<u64>::wrapping_add` inside
    /// `caller_of_heavy`, each with its own file and line, which is the reason one `SourceFrame`
    /// per address cannot describe what ran.
    ///
    /// An empty `Vec` means "nothing attributable here": an address outside every range, one too
    /// large to be a code-section offset, a lookup `gimli` could not complete, or — with no DWARF
    /// loaded — no `name` entry for the function the address falls in either. A
    /// frame DWARF gives no name for is dropped rather than ending the stack — it cannot be a
    /// [`crate::models::CallStackNode`] key, but the callers beneath it still can be, so the name
    /// rule no longer costs a whole address its attribution the way it did when only the innermost
    /// frame was read. The location fields stay optional and independent: address `2` yields a name
    /// and no location at all (the prologue precedes the first line program), and `61`..`71` yield
    /// a file with no line.
    ///
    /// When no DWARF is loaded — a `debug = false` build whose `name` section survived — this answers
    /// through [`SourceMapper::resolve_from_name_section`] instead. The order is DWARF, then `name`,
    /// then the `wasm[pc]` the aggregator falls back to, and #163 is the write-up of why.
    ///
    /// Takes `&self`, so one mapper can serve a whole trace, and repeats itself through an internal
    /// address cache rather than through a `&mut self` signature #158 could have asked for —
    /// `aggregate` holds the mapper by reference, and a cache that cost every caller a `&mut` would
    /// be a worse trade than the walk it saves. Allocating one
    /// `Vec` per event sits inside `AGENTS.md`'s OOM rule for the same reason the rest of Stage 2
    /// does: the tracer emits one event per call boundary, not per instruction.
    ///
    /// # Examples
    ///
    /// ```
    /// use soroban_cost_profiler::source_map::SourceMapper;
    ///
    /// // A mapper without symbols resolves nothing, and must not panic.
    /// let mapper = SourceMapper::unmapped();
    /// assert!(mapper.resolve(0).is_empty());
    ///
    /// // A real build resolves: this fixture is Rust code compiled for wasm32-unknown-unknown.
    /// let fixture = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");
    /// let mapper = SourceMapper::new(fixture).expect("the fixture carries DWARF");
    /// let stack = mapper.resolve(3);
    /// assert_eq!(stack.len(), 1, "address 3 is not an inlined call site");
    /// assert_eq!(stack[0].function_name, "caller_of_heavy");
    /// assert_eq!(stack[0].line_number, Some(39));
    ///
    /// // Address 14 is inlined core code, so the stack names both halves, innermost first.
    /// let stack = mapper.resolve(14);
    /// let names: Vec<&str> = stack.iter().map(|frame| frame.function_name.as_str()).collect();
    /// assert_eq!(names, ["<u64>::wrapping_add", "caller_of_heavy"]);
    /// assert_eq!(stack[1].line_number, Some(39), "the caller's line is the call site");
    /// ```
    pub fn resolve(&self, pc: usize) -> Vec<SourceFrame> {
        let Some(context) = self.context.as_ref() else {
            // No DWARF means there is nothing to walk, and no cache either: #157's fallback finds a
            // body by binary search over [`CodeMap::bodies`] and clones one name, measured at 61 ns —
            // less than a hash lookup and a stack clone would cost. Caching it would make the
            // degraded path slower, which is not what a cache is for.
            return self.resolve_from_name_section(pc);
        };

        if let Some(stack) = self.cache.borrow().get(&pc) {
            return stack.clone();
        }

        let stack = self.resolve_dwarf(context, pc);

        let mut cache = self.cache.borrow_mut();
        if cache.len() >= RESOLUTION_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(pc, stack.clone());

        stack
    }

    /// Ask DWARF for one address's inline stack, innermost frame first.
    ///
    /// [`SourceMapper::resolve`] is the public door and the one that remembers; this is the walk it
    /// caches. Splitting them keeps the cache out of the semantics: everything #146–#156 pinned —
    /// the range guards, `skip_all_loads`, a nameless frame ending only itself — lives here, and is
    /// reached at most once per address per mapper.
    fn resolve_dwarf(&self, context: &Context, pc: usize) -> Vec<SourceFrame> {
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

    /// Resolve one program counter from the `name` section alone: at most one frame, no file and no
    /// line.
    ///
    /// `pc` is the same code-section address [`SourceMapper::resolve`] takes. `name` records one
    /// symbol per *function*, so any address inside a body gets that body's name and nothing finer:
    /// `caller_of_heavy` at `3` and at `14` alike, where DWARF answers `14` with
    /// `<u64>::wrapping_add`. That is the whole of what a binary without DWARF can be told, which is
    /// why this is a fallback and not a substitute.
    ///
    /// Two index spaces meet here. `name` keys its entries by module function index, **imports
    /// included**; [`CodeMap::bodies`] is the *defined* function list, which begins after them. The
    /// import count is read from the import section in the same walk, and a module whose import
    /// section does not parse yields no names at all — charging `memory_heavy_loop`'s cost to the
    /// name of a host import would be worse than naming nothing.
    ///
    /// Names arrive demangled and closure-collapsed exactly as DWARF names do, because the section
    /// stores rustc's raw symbols (`_RNvNtNtCs…5guest3vec13vec_push_back`) and a frame keyed on one
    /// would not be the frame DWARF gives for the same function.
    ///
    /// Returns an empty `Vec` for an address in no function body, and for a mapper that loaded no
    /// names — including [`SourceMapper::unmapped`], which never has any.
    ///
    /// # Examples
    ///
    /// ```
    /// use soroban_cost_profiler::source_map::SourceMapper;
    ///
    /// let stripped = include_bytes!("../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");
    /// let mapper = SourceMapper::new(stripped).expect("the fixture carries a `name` section");
    ///
    /// // Every address inside the first function's body answers with its name.
    /// let stack = mapper.resolve_from_name_section(3);
    /// assert_eq!(stack.len(), 1);
    /// assert_eq!(stack[0].function_name, "caller_of_heavy");
    /// assert_eq!(stack[0].line_number, None, "`name` has no line tables to read");
    ///
    /// // The count byte and the size prefixes belong to no function, and so does anything outside.
    /// assert!(mapper.resolve_from_name_section(0).is_empty());
    /// assert!(mapper.resolve_from_name_section(4000).is_empty());
    /// ```
    pub fn resolve_from_name_section(&self, pc: usize) -> Vec<SourceFrame> {
        let (Some(names), Some(code)) = (self.names.as_ref(), self.code.as_ref()) else {
            return Vec::new();
        };

        let Some(function_name) = code.function_at(pc).and_then(|index| names.name_at(index))
        else {
            return Vec::new();
        };

        vec![SourceFrame {
            function_name,
            file_path: None,
            line_number: None,
        }]
    }

    /// Where this binary's code section is, for translating a position in the file.
    ///
    /// `None` when the module carries no code section or its declared function list runs past the
    /// section's bytes. That is deliberately not a [`SourceMapError`]: a caller with a DWARF address
    /// already in hand needs no translation, and failing a whole profiling run over an address map
    /// nothing asked for would be the wrong trade.
    pub fn code_map(&self) -> Option<&CodeMap> {
        self.code.as_ref()
    }

    /// Resolve a byte offset *in the file* to the inline stack that produced it.
    ///
    /// The same lookup as [`SourceMapper::resolve`], one address space earlier: the offset is moved
    /// into code-section-relative form by [`CodeMap::to_code_address`] before DWARF is asked, which
    /// is the step #153 exists because the two spaces are not the same and passing one for the other
    /// resolves nothing without saying so.
    ///
    /// Use this for anything that reads the binary — a section walk, a `wasm-objdump` figure, a
    /// hand-checked offset. Use [`SourceMapper::resolve`] for anything already in DWARF's space.
    ///
    /// Returns an empty `Vec` if this mapper has no [`CodeMap`], if the offset is outside the code
    /// section, or if the translated address resolves to no frame.
    ///
    /// # Examples
    ///
    /// ```
    /// use soroban_cost_profiler::source_map::SourceMapper;
    ///
    /// let fixture = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");
    /// let mapper = SourceMapper::new(fixture).expect("the fixture carries DWARF");
    ///
    /// // In this fixture the code section's payload begins at file offset 111, and the first
    /// // function's body two bytes later: the count byte and its size prefix are addresses too,
    /// // they simply precede the line program.
    /// let map = mapper.code_map().expect("the fixture has a code section");
    /// assert_eq!(map.to_code_address(111), Some(0));
    /// assert_eq!(map.to_code_address(110), None, "before the section is not in it");
    ///
    /// let stack = mapper.resolve_file_offset(114);
    /// assert_eq!(stack.len(), 1, "file offset 114 is code address 3");
    /// assert_eq!(stack[0].function_name, "caller_of_heavy");
    /// assert_eq!(stack[0].line_number, Some(39));
    ///
    /// // The offset translation reaches inlined code on the same two frames `resolve` names.
    /// let stack = mapper.resolve_file_offset(125);
    /// assert_eq!(stack.len(), 2, "file offset 125 is code address 14");
    /// assert_eq!(stack[0].function_name, "<u64>::wrapping_add");
    /// ```
    pub fn resolve_file_offset(&self, file_offset: usize) -> Vec<SourceFrame> {
        let Some(address) = self
            .code
            .as_ref()
            .and_then(|map| map.to_code_address(file_offset))
        else {
            return Vec::new();
        };

        self.resolve(address)
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
/// let fixture = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");
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
    /// a module function index is this plus the import count, which is how [`NameSection`] lines
    /// the two up.
    bodies: Vec<Range<usize>>,
}

impl CodeMap {
    /// Read a code section payload, given where it starts in the file.
    ///
    /// `None` if the declared function list runs past the payload, which is a malformed or truncated
    /// module rather than a file worth profiling. Iteration is bounded by the bytes rather than by
    /// the declared count, so a count field that claims millions cannot allocate a table for them —
    /// `AGENTS.md`'s OOM rule applies to the whole binary, not only the trace.
    fn parse(payload: &[u8], base: usize) -> Option<Self> {
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

/// The section whose bytes define the address space DWARF line tables are written against.
///
/// Section id `10` in the WASM binary spec's ordering — the code section, one size-prefixed body
/// per defined function, in the order the function section lists them.
const CODE_SECTION: u64 = 10;

/// The section whose absence means the binary cannot be source-mapped at all.
///
/// Line tables alone cannot name a function, and an address only resolves through a compilation
/// unit, so `.debug_info` is what makes the other `.debug_*` sections meaningful.
const DEBUG_INFO: &str = ".debug_info";

/// The custom section that names functions without naming files: `name`.
///
/// Present in every `rust-lld` build, including `debug = false` ones, and the only symbol source
/// for a binary with no DWARF (#157). Binaryen's optimizer deletes it unless `-g` is passed — see
/// `docs/spikes/02_wasm_name_section_fallback.md`.
const NAME_SECTION: &str = "name";

/// The `name` subsection that maps function indices to symbols.
const FUNCTION_NAMES: u8 = 1;

/// The section whose function entries offset a `name` index into a defined-function position.
const IMPORT_SECTION: u64 = 2;

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
fn load_dwarf(sections: &WasmSections) -> Dwarf {
    // Copied per section, once, at load: the mapper outlives the caller's buffer, so the bytes
    // it indexes have to be owned.
    let reader =
        |bytes: &[u8]| Reader::new(Rc::from(bytes.to_vec()), gimli::NativeEndian::default());

    Dwarf::load(&mut |id: gimli::SectionId| {
        Ok::<_, std::convert::Infallible>(reader(sections.get(id.name()).unwrap_or(&[])))
    })
    .expect("copying section bytes into a reader cannot fail")
}

/// The `name` custom section's function names, positioned by defined-function index.
///
/// #141's spike is where this layout was measured, and the two facts that decide the parser are
/// recorded there: the payload is a chain of `u8 kind` + `uleb128 size` + body subsections with
/// **no leading version byte** — the framing is only unambiguous because the bytes tile the payload
/// exactly — and only kind `1`, function names, matters here. Its body is `uleb128 count` then
/// `count` records of `uleb128 funcidx` + `uleb128 len` + `len` UTF-8 bytes.
///
/// `funcidx` counts the whole function index space, imports first, so aligning it with a code
/// section address needs the import count; see [`SourceMapper::resolve_from_name_section`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct NameSection {
    /// One entry per defined function, in code-section order, already rendered by
    /// [`demangle_symbol`]. Names for imported functions are dropped rather than stored: no
    /// code-section address can ever reach them.
    names: Vec<Option<String>>,
}

impl NameSection {
    /// Read one module's function names, or conclude it has none worth loading.
    ///
    /// `None` — which is what makes [`SourceMapper::new`] report
    /// [`SourceMapError::MissingDebugInfo`] instead — when the section is absent, when its
    /// subsection framing does not tile the payload, when its records do not tile the function-names
    /// body, or when nothing in it names a function this module's code section contains.
    ///
    /// Both declared counts are bounded by the bytes that carry them, not the reverse: reading a
    /// section claiming millions of names costs one failed lookup and no allocation, which is
    /// `AGENTS.md`'s OOM rule applied to a binary rather than to a trace.
    fn parse(sections: &WasmSections) -> Option<Self> {
        let payload = sections.get(NAME_SECTION)?;
        let imported = sections.imported_functions?;
        let mut names = vec![None; sections.code.as_ref()?.bodies().len()];

        let mut cursor = 0;
        while cursor < payload.len() {
            let kind = payload[cursor];
            cursor += 1;

            let size = usize::try_from(WasmSections::read_uleb(payload, &mut cursor).ok()?).ok()?;
            let body_end = cursor.checked_add(size)?;
            if body_end > payload.len() {
                return None;
            }

            // Unknown subsection kinds are skipped, not rejected: the section is shared with
            // proposals this stage never reads (kind 7, seen in the fixture, names globals).
            if kind == FUNCTION_NAMES {
                read_function_names(&payload[cursor..body_end], imported, &mut names)?;
            }

            cursor = body_end;
        }

        // A section that names nothing is not a fallback — it is the same silence in a smaller
        // envelope, and loading it would let a run finish with no warning at all.
        names.iter().any(Option::is_some).then_some(Self { names })
    }

    fn name_at(&self, defined_index: usize) -> Option<String> {
        self.names.get(defined_index).and_then(Clone::clone)
    }
}

/// Fill `names` from one function-names subsection body.
///
/// `None` when the records do not tile the body. Partial reads are not offered: a name attached to
/// the wrong function is a wrong flamegraph, and the alternative is an unnamed one.
fn read_function_names(body: &[u8], imported: u64, names: &mut [Option<String>]) -> Option<()> {
    let mut cursor = 0;
    let count = WasmSections::read_uleb(body, &mut cursor).ok()?;

    for _ in 0..count {
        let funcidx = WasmSections::read_uleb(body, &mut cursor).ok()?;
        let len = usize::try_from(WasmSections::read_uleb(body, &mut cursor).ok()?).ok()?;
        let end = cursor.checked_add(len)?;
        let raw = body.get(cursor..end)?;
        cursor = end;

        // A name for an import (below the module's function-import count) is not a name for anything
        // a code-section address can reach, and a name past the defined-function list is the same
        // kind of noise. Neither is a reason to abandon the section.
        let Some(defined) = funcidx.checked_sub(imported) else {
            continue;
        };
        let Some(slot) = usize::try_from(defined)
            .ok()
            .and_then(|index| names.get_mut(index))
        else {
            continue;
        };

        if slot.is_some() {
            continue; // first entry wins; a duplicate is noise, not a rename
        }
        *slot = std::str::from_utf8(raw).ok().and_then(demangle_symbol);
    }

    (cursor == body.len()).then_some(())
}

/// The `name` section's counterpart to [`frame_name`]: demangle, then collapse closures.
///
/// The section stores no language, so this goes through `addr2line`'s heuristics — the same
/// rustc-then-C++ order it applies to a DWARF name with no `DW_AT_language` — and returns a name it
/// cannot parse byte-for-byte as stored. That is what makes `#[no_mangle] extern "C"` export names
/// arrive plain here too, exactly as they do from DWARF.
fn demangle_symbol(raw: &str) -> Option<String> {
    let name = addr2line::demangle_auto(std::borrow::Cow::Borrowed(raw), None);
    (!name.is_empty()).then(|| collapse_closures(&name))
}

/// The custom sections a mapper keeps, in file order.
struct WasmSections {
    /// `(name, payload)`, holding only the sections this stage can use: `.debug_*` for source
    /// mapping and `name` for the function-name-only fallback. Everything else — `producers`,
    /// `target_features`, Soroban's `contractspecv0` — would be a copy of bytes nothing reads.
    retained: Vec<(String, Vec<u8>)>,
    /// The code section's address map, from the same walk, for #153's translation. `None` when the
    /// module has no code section or its function list does not fit its declared size; only the
    /// section's *location* is kept, never its bytes, so this costs a `Vec` of ranges and no copy.
    code: Option<CodeMap>,
    /// How many function indices belong to imports, so a `name` section's indices line up with
    /// [`CodeMap`]'s defined-function list (#157).
    ///
    /// `Some(0)` when the module carries no import section, which is what both `dwarf_probe`
    /// fixtures do, and `None` when one is present but does not parse — a missing fallback is
    /// honest, an offset guessed from a section that would not read is not.
    imported_functions: Option<u64>,
}

impl WasmSections {
    /// Walk the WASM section table, keeping the sections Stage 2 reads.
    ///
    /// A hand-written parser for the *container* only: section id, payload length, and for
    /// custom sections the name prefix. That is the whole module structure `addr2line` needs and
    /// it never inspects DWARF itself, which is what keeps this inside `AGENTS.md`'s "no custom
    /// DWARF parsing" rule. Deliberately strict about truncation and lenient about everything
    /// else: a module with sections this stage ignores still maps fine.
    fn parse(bytes: &[u8]) -> Result<Self, SourceMapError> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
            return Err(SourceMapError::NotWasm);
        }

        let mut cursor = 8; // past magic and version
        let mut retained = Vec::new();
        let mut code = None;
        let mut imported_functions = Some(0);
        let mut seen_imports = false;

        while cursor < bytes.len() {
            let id = Self::read_uleb(bytes, &mut cursor)?;
            let size = usize::try_from(Self::read_uleb(bytes, &mut cursor)?)
                .map_err(|_| SourceMapError::Truncated)?;
            let end = cursor.checked_add(size).ok_or(SourceMapError::Truncated)?;
            if end > bytes.len() {
                return Err(SourceMapError::Truncated);
            }

            // Section id 0 is a custom section: its payload is a name then the data.
            if id == 0 {
                let name_len = usize::try_from(Self::read_uleb(bytes, &mut cursor)?)
                    .map_err(|_| SourceMapError::Truncated)?;
                let name_start = cursor;
                let name_end = cursor
                    .checked_add(name_len)
                    .ok_or(SourceMapError::Truncated)?;
                if name_end > end {
                    return Err(SourceMapError::Truncated);
                }
                let name = std::str::from_utf8(&bytes[name_start..name_end])
                    .map_err(|_| SourceMapError::Truncated)?
                    .to_string();

                if retain(&name) {
                    // The section's own data starts after the name bytes, not after the length.
                    retained.push((name, bytes[name_end..end].to_vec()));
                }
            }

            // Section id 10 is the code section, whose payload starts the address space DWARF
            // indexes. Only the first counts; a module with two is not a module worth mapping.
            if code.is_none() && id == CODE_SECTION {
                code = CodeMap::parse(&bytes[cursor..end], cursor);
            }

            // Section id 2 is the import section. Its function entries are how many indices a
            // `name` section counts before it reaches the first defined function (#157).
            if !seen_imports && id == IMPORT_SECTION {
                seen_imports = true;
                imported_functions = count_function_imports(&bytes[cursor..end]);
            }

            cursor = end;
        }

        Ok(Self {
            retained,
            code,
            imported_functions,
        })
    }

    /// Read a LEB128 unsigned integer, advancing the cursor.
    ///
    /// Rejects a byte that would overflow `u64` and a run that ends at the file's
    /// [`SourceMapError::Truncated`] rather than reading past the end or wrapping silently.
    fn read_uleb(bytes: &[u8], cursor: &mut usize) -> Result<u64, SourceMapError> {
        let mut value: u64 = 0;
        let mut shift = 0;

        loop {
            let byte = *bytes.get(*cursor).ok_or(SourceMapError::Truncated)?;
            *cursor += 1;

            if shift >= 64 || (byte & 0x7f) as u64 > u64::MAX >> shift {
                return Err(SourceMapError::Truncated);
            }
            value |= u64::from(byte & 0x7f) << shift;

            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn get(&self, name: &str) -> Option<&[u8]> {
        self.retained
            .iter()
            .find(|(section, _)| section == name)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// Names of the custom sections that were kept, for the error message that tells a user what
    /// their build actually contains.
    ///
    /// Only retained sections appear: the point of the list is "no `.debug_*` here, but there is
    /// a `name` section to fall back to", and `producers`/`target_features` would be noise.
    fn custom_names(&self) -> Vec<String> {
        self.retained.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// Count the function imports in an import section payload.
///
/// `None` if the payload does not parse or leaves bytes over, which [`WasmSections`] reads as "the
/// offset between a `name` index and a defined function is unknown" rather than as zero. Each entry
/// is a module name, a field name, and an extern descriptor; the descriptor's shape is what has to
/// be walked correctly to reach the next entry, and a kind this does not recognize stops the count
/// instead of resuming on a misaligned byte.
fn count_function_imports(payload: &[u8]) -> Option<u64> {
    let mut cursor = 0;
    let count = WasmSections::read_uleb(payload, &mut cursor).ok()?;
    let mut functions = 0;

    for _ in 0..count {
        // Both names are length-prefixed bytes, and the section's own framing is the only thing
        // telling where one entry's descriptor ends.
        for _ in 0..2 {
            let len = usize::try_from(WasmSections::read_uleb(payload, &mut cursor).ok()?).ok()?;
            cursor = cursor.checked_add(len)?;
            if cursor > payload.len() {
                return None;
            }
        }

        match payload.get(cursor).copied()? {
            0 => {
                cursor += 1;
                WasmSections::read_uleb(payload, &mut cursor).ok()?;
                functions += 1;
            }
            // A table or memory is `limits`: a flags byte, a minimum, a maximum when the flags say
            // bounded, and one more byte when the memory is shared.
            1 | 2 => {
                cursor += 1;
                let flags = *payload.get(cursor)?;
                cursor += 1;
                WasmSections::read_uleb(payload, &mut cursor).ok()?;
                if flags & 1 == 1 {
                    WasmSections::read_uleb(payload, &mut cursor).ok()?;
                }
                if matches!(flags, 0x40 | 0x41) {
                    cursor += 1;
                }
            }
            // A global is a value type byte and a mutability byte.
            3 => cursor += 3,
            _ => return None,
        }
    }

    (cursor == payload.len()).then_some(functions)
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

/// Rewrite rustc's anonymous closure segments into bracketed markers.
///
/// Two spellings reach this function from a compiler, both measured in a `wasm32-unknown-unknown`
/// build of a probe crate containing a closure inside a closure:
///
/// * `closure_probe::outer::{closure#0}` — at `debug = 2`, `opt-level = 1`, where each closure gets
///   its own function. It is also what `rustc-demangle` renders a v0 closure into: the same crate's
///   `name` section carries `…13closure_probe5outer0E…`, and the trailing index is the disambiguator
///   that becomes `#0`.
/// * `{closure_env#0}` — at `line-tables-only`, `opt-level = 3`, where the closure body is inlined
///   into its parent and the only surviving name is the synthetic environment type inside the
///   generic arguments of whatever takes it (`map_fold<u32, u32, u32,
///   closure_probe::outer::{closure_env#0}, …>`).
///
/// `{{closure}}`, the spelling #155 names, is legacy mangling and appears in neither build. It is
/// still handled — trimming one more brace level costs no branch — because that is what a
/// pre-2020 `.wasm` will hand to #157's `name`-section path.
///
/// Each segment becomes `[closure]`, or `[closure#N]` when it carries an index, and a segment whose
/// *entire* prefix since the previous marker is `::` is dropped rather than written:
/// `outer::{closure#0}::{closure#1}` describes one closure frame reached by nesting, and the
/// flamegraph has nowhere to put a stack of markers. A segment reached through anything else
/// (`>::`, inside generic arguments) is a different closure at a different nesting level and stays.
///
/// The index is kept when a closure stands alone because siblings in one function are different
/// work, and [`crate::models::CallStackNode`]'s children are keyed by `function_name` — dropping
/// it would pool `outer`'s two closures into a single frame and lose which one spent the fuel.
///
/// Every other byte is preserved exactly, including segments this does not touch (`{impl#0}`,
/// which is an anonymous impl block rather than a closure and is #155's sibling, not #155) and a
/// `{` that never closes.
fn collapse_closures(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    let mut after_marker = false;

    while let Some(open) = rest.find('{') {
        let Some(len) = group_len(&rest[open..]) else {
            out.push_str(rest);
            return out;
        };
        let separator = &rest[..open];
        let marker = closure_marker(&rest[open + 1..open + len - 1]);

        match marker {
            Some(_) if after_marker && separator == "::" => rest = &rest[open + len..],
            Some(marker) => {
                out.push_str(separator);
                out.push_str(&marker);
                rest = &rest[open + len..];
                after_marker = true;
            }
            None => {
                out.push_str(separator);
                out.push('{');
                rest = &rest[open + 1..];
                after_marker = false;
            }
        }
    }

    out.push_str(rest);
    out
}

/// The length of the balanced `{…}` group at the front of `group`, braces included.
///
/// `None` when the braces never close, which leaves the caller to copy the name verbatim. Nesting
/// is counted because a group can contain one (`{a{b}c}`), and `group_len` then spans the outer
/// pair rather than stopping at the first `}`.
///
/// Cannot underflow `depth`: `group` starts at a `{`, so the count reaches zero exactly once, at
/// the matching brace, and returns.
fn group_len(group: &str) -> Option<usize> {
    let mut depth = 0usize;

    for (index, character) in group.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + character.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

/// The marker for a group's contents, or `None` when the group is not a closure segment.
///
/// `inner` is the text *between* the outermost braces, so the `{{closure}}` spelling arrives here
/// as `{closure}` — trimming braces once more finds the same `closure` prefix the other forms
/// start with. The index after `#` is kept when present; the span form (`{closure@…}`) and the
/// fully anonymous legacy form have none, and `[closure]` is all either of them can say.
///
/// A segment that merely starts with the letters is not a closure — the character after `closure`
/// has to be punctuation rustc actually uses (`#`, `_`, `@`, or nothing).
fn closure_marker(inner: &str) -> Option<String> {
    let core = inner.trim_matches(['{', '}']);
    let tail = core.strip_prefix("closure")?;
    if tail.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }

    match tail.find('#') {
        Some(index) if tail.len() > index + 1 => Some(format!("[closure#{}]", &tail[index + 1..])),
        _ => Some("[closure]".to_string()),
    }
}

/// Whether a custom section is worth keeping in memory.
fn retain(name: &str) -> bool {
    name == NAME_SECTION || name.starts_with(".debug")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode one LEB128 unsigned integer.
    fn uleb(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            out.push(if value == 0 { byte } else { byte | 0x80 });
            if value == 0 {
                return out;
            }
        }
    }

    /// Build a module carrying the given custom sections, so a test can describe a binary
    /// exactly rather than depending on whatever a toolchain happened to emit.
    fn module(custom: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();

        for (name, payload) in custom {
            let name_bytes = name.as_bytes();
            let mut section = uleb(name_bytes.len() as u64);
            section.extend_from_slice(name_bytes);
            section.extend_from_slice(payload);

            bytes.push(0); // custom section id
            bytes.extend(uleb(section.len() as u64));
            bytes.extend(section);
        }

        // A code section, so the module is not nothing: a function count of one and a body of zero
        // bytes. Real bodies come from `fixtures/dwarf_probe`, never from here.
        bytes.push(10);
        bytes.extend(uleb(2));
        bytes.extend([0x01, 0x00]);
        bytes
    }

    /// Build a module whose code section payload is exactly `payload`, so a test can describe a
    /// malformed function list instead of trusting a toolchain to emit one.
    fn module_with_code(payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();

        bytes.push(10);
        bytes.extend(uleb(payload.len() as u64));
        bytes.extend_from_slice(payload);
        bytes
    }

    /// Assemble a module from `(section id, payload)` pairs, so #157's tests can place a real
    /// import section in front of a `name` section instead of only custom sections.
    fn raw_module(sections: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();

        for (id, payload) in sections {
            bytes.extend(uleb(*id));
            bytes.extend(uleb(payload.len() as u64));
            bytes.extend_from_slice(payload);
        }

        bytes
    }

    /// One custom section's payload: its name, then its bytes.
    fn custom(name: &str, body: &[u8]) -> Vec<u8> {
        let mut out = uleb(name.len() as u64);
        out.extend(name.as_bytes());
        out.extend(body);
        out
    }

    /// A function-names subsection — kind `1`, size, then `count` records of index, length, bytes —
    /// framed the way `rust-lld` frames it, with the version byte #141 measured as absent.
    fn function_names(entries: &[(u64, &str)]) -> Vec<u8> {
        let mut body = uleb(entries.len() as u64);
        for (index, name) in entries {
            body.extend(uleb(*index));
            body.extend(uleb(name.len() as u64));
            body.extend(name.as_bytes());
        }

        let mut out = vec![1]; // FUNCTION_NAMES
        out.extend(uleb(body.len() as u64));
        out.extend(body);
        out
    }

    /// An import section payload of `functions` function imports: two names and a type index each.
    fn imports(functions: u64) -> Vec<u8> {
        let mut out = uleb(functions);
        for _ in 0..functions {
            out.extend(uleb(3));
            out.extend(b"env");
            out.extend(uleb(4));
            out.extend(b"call");
            out.push(0); // extern kind: function
            out.extend(uleb(0)); // type index
        }
        out
    }

    /// A code section of `count` two-byte bodies, so each function owns addresses an offset can land
    /// in and the gaps between them are still just the size prefixes.
    fn code_bodies(count: u64) -> Vec<u8> {
        let mut out = uleb(count);
        for _ in 0..count {
            out.extend(uleb(2));
            out.extend([0x00, 0x0b]); // no locals, then `end`
        }
        out
    }

    /// The name each defined function's first byte answers with, through the fallback.
    fn named_bodies(mapper: &SourceMapper) -> Vec<String> {
        let map = mapper
            .code_map()
            .expect("these modules carry a code section");

        map.bodies()
            .iter()
            .map(|body| {
                mapper
                    .resolve_from_name_section(body.start)
                    .pop()
                    .map(|frame| frame.function_name)
                    .unwrap_or_else(|| panic!("no name for the body at {}", body.start))
            })
            .collect()
    }

    /// The fixture that carries DWARF, built by `fixtures/dwarf_probe/build.sh`.
    const DWARF_PROBE: &[u8] = include_bytes!("../fixtures/dwarf_probe/dwarf_probe.wasm");

    /// The same three functions built with `debug = false`: no DWARF, same `name` section.
    const NO_DEBUG_PROBE: &[u8] =
        include_bytes!("../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");

    #[test]
    fn an_empty_input_is_not_a_module() {
        // #150: the ask is that `&[]` fails with the expected error rather than panicking or
        // silently yielding a mapper that resolves nothing.
        let error = SourceMapper::new(&[]).err().expect("no bytes, no module");

        assert_eq!(error, SourceMapError::NotWasm);
        assert!(
            error.to_string().contains("not a WebAssembly module"),
            "the message has to tell the user what to pass instead: {error}"
        );
    }

    #[test]
    fn bytes_that_are_not_wasm_are_rejected_by_magic() {
        for input in [
            b"definitely not a wasm module".as_slice(),
            b"WAT!(module)".as_slice(),
            b"\x7fELF\x02\x01\x01\x00".as_slice(),
        ] {
            assert_eq!(
                SourceMapper::new(input).err(),
                Some(SourceMapError::NotWasm),
                "a `{}`-byte input is not a module",
                input.len()
            );
        }
    }

    #[test]
    fn a_module_without_debug_info_reports_the_build_flag_that_fixes_it() {
        // The ordinary case for a release build, and the one a user can actually act on. Since #157
        // a readable `name` section would load instead, so this payload is deliberately not one: the
        // bytes after the section name are ASCII, whose first subsection claims more length than the
        // payload holds.
        let stripped = module(&[("name", b"functions"), ("producers", b"CL 17")]);

        let error = SourceMapper::new(&stripped)
            .err()
            .expect("a binary with no DWARF must not look like a successful load");

        let message = error.to_string();
        let SourceMapError::MissingDebugInfo { custom_sections } = &error else {
            panic!("expected the missing-DWARF error, got {error:?}");
        };
        assert!(
            message.contains("line-tables-only"),
            "the message has to name the setting that emits debug info: {message}"
        );
        assert!(
            message.contains(&custom_sections.join(", ")),
            "listing what *is* there is how a user notices a `name` section to fall back to: \
             {message}"
        );
    }

    #[test]
    fn truncated_section_headers_error_instead_of_reading_past_the_end() {
        // A section claims more bytes than the file holds — a partial download, not a bad build.
        let claims_too_much = [
            b"\0asm\x01\0\0\0\x00\x7f".to_vec(),
            b"\0asm\x01\0\0\0\x00\x04\x0b.debu".to_vec(),
            b"\0asm\x01\0\0\0\x00\x05\x0b\x00.debug_info".to_vec(),
        ];

        for input in claims_too_much {
            assert_eq!(
                SourceMapper::new(&input).err(),
                Some(SourceMapError::Truncated),
                "input {:?} must be reported as truncated, not panicking",
                String::from_utf8_lossy(&input)
            );
        }
    }

    #[test]
    fn sections_this_stage_ignores_are_skipped_without_error() {
        // Soroban emits `contractspecv0`/`contractmetav0` and LLVM `producers`/`target_features`;
        // a module carrying those plus no DWARF is a normal binary, so the only complaint is the
        // missing debug info — and the retained names show which sections were noticed.
        let with_spec = module(&[
            ("contractspecv0", b"\x01\x02\x03"),
            (".debug_abbrev", b"\x01"),
            ("target_features", b"\x00"),
        ]);

        let error = SourceMapper::new(&with_spec)
            .err()
            .expect("no .debug_info present");

        match error {
            SourceMapError::MissingDebugInfo { custom_sections } => assert_eq!(
                custom_sections,
                vec![".debug_abbrev".to_string()],
                "only DWARF and `name` are worth keeping a copy of"
            ),
            other => panic!("expected the missing-DWARF error, got {other:?}"),
        }
    }

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

    /// One frame measured out of a `debug = 2`, `opt-level = 1` `wasm32-unknown-unknown` build of a
    /// probe crate whose `outer` holds a closure that holds a closure, copied from what
    /// `addr2line` handed `frame_name` after demangling. Every `{closure#0}` in it is a real closure
    /// segment at a real nesting level, and the whole string is what a flamegraph frame has to
    /// render.
    const MEASURED_CLOSURE_FRAME: &str = "<core::ops::range::Range<u32> as core::iter::traits::iterator::Iterator>::fold::<u32, core::iter::adapters::map::map_fold<u32, u32, u32, closure_probe::outer::{closure#0}, <u32 as core::iter::traits::accum::Sum>::sum<core::iter::adapters::map::Map<core::ops::range::Range<u32>, closure_probe::outer::{closure#0}>>::{closure#0}>::{closure#0}>";

    #[test]
    fn nested_closure_segments_collapse_into_one_marker() {
        // #155's acceptance criterion, in the two spellings a compiler actually produces plus the
        // one the issue names. The nested segment and its `::` both go, because a run of them
        // describes one closure frame reached by nesting and the outer index is the one that
        // identifies it.
        assert_eq!(
            collapse_closures("outer::{closure#0}::{closure#1}"),
            "outer::[closure#0]"
        );
        assert_eq!(
            collapse_closures("dummy_contract::call::{{closure}}::{{closure}}::{{closure}}"),
            "dummy_contract::call::[closure]"
        );
        // Mixed spellings in one run still collapse: the second segment is the legacy form.
        assert_eq!(
            collapse_closures("a::{closure}::{{closure}}::b"),
            "a::[closure]::b"
        );
        // A span-form segment names no index, so the marker says only what it can.
        assert_eq!(
            collapse_closures("sort::{closure@src/lib.rs:12:9: 12:20}"),
            "sort::[closure]"
        );
    }

    #[test]
    fn closures_that_are_not_nested_keep_their_index() {
        // The other half of the rule, and the reason the marker is not a flat `[closure]`: these two
        // are different closures in the same function, reached from different addresses, and
        // `CallStackNode`'s children are keyed by name. Merging them would pool two costs into one
        // frame with nothing in the trace to explain it.
        let first = collapse_closures("outer::{closure#0}");
        let second = collapse_closures("outer::{closure#1}");

        assert_eq!(first, "outer::[closure#0]");
        assert_eq!(second, "outer::[closure#1]");
        assert_ne!(first, second);

        // The `{closure_env#N}` spelling is the environment type of the same closure, so it gets
        // the same marker rather than a fourth thing to tell apart.
        assert_eq!(
            collapse_closures("map_fold<u32, closure_probe::outer::{closure_env#0}>"),
            "map_fold<u32, closure_probe::outer::[closure#0]>"
        );
    }

    #[test]
    fn the_measured_closure_frame_renders_without_braces() {
        // Four closure segments, only two of which are adjacent: the `>::` between the third and
        // fourth is a generic-argument boundary, so those are different closures at different
        // nesting levels and both markers stay. Everything outside a `{…}` group is untouched,
        // including the generic arguments that make this name long.
        let cleaned = collapse_closures(MEASURED_CLOSURE_FRAME);
        let expected = "<core::ops::range::Range<u32> as core::iter::traits::iterator::Iterator>::fold::<u32, core::iter::adapters::map::map_fold<u32, u32, u32, closure_probe::outer::[closure#0], <u32 as core::iter::traits::accum::Sum>::sum<core::iter::adapters::map::Map<core::ops::range::Range<u32>, closure_probe::outer::[closure#0]>>::[closure#0]>::[closure#0]>";

        assert_eq!(cleaned, expected);
        assert!(
            !cleaned.contains("closure#0}"),
            "a brace survived: {cleaned}"
        );
    }

    #[test]
    fn a_name_with_no_closure_segment_is_returned_byte_for_byte() {
        // The rewrite must not be a reformatting. `{impl#0}` is an anonymous impl block, not a
        // closure (its own cleanup, not #155's); a leading `::` and an empty name are the shapes a
        // split-and-rejoin implementation would silently corrupt.
        for name in [
            "caller_of_heavy",
            "<u64>::wrapping_add",
            "<T as core::fmt::Debug>::fmt",
            "core::fmt::builders::{impl#0}::is_pretty",
            "Vec<u32, alloc::global_alloc>::push",
            "::leading",
            "a::b::c",
            "",
            "no braces but a stray }{",
            "closure",
            "closures",
            "my_closure_factory",
        ] {
            assert_eq!(collapse_closures(name), name, "{name} was changed");
        }
    }

    #[test]
    fn an_unclosed_brace_is_copied_rather_than_parsed() {
        // A truncated or hand-edited symbol can end mid-segment. `group_len` answers `None` and the
        // rest is copied verbatim, so the frame still gets a name instead of a panic or a swallow.
        assert_eq!(collapse_closures("outer::{closure#0"), "outer::{closure#0");
        assert_eq!(
            collapse_closures("outer::{closure#0}::{tail"),
            "outer::[closure#0]::{tail"
        );
        // Nesting is counted, so an inner group does not end the outer one early.
        assert_eq!(
            collapse_closures("f<{a{b}c}>::{closure#0}"),
            "f<{a{b}c}>::[closure#0]"
        );
    }

    #[test]
    fn every_frame_the_fixture_yields_survives_the_rewrite_unchanged() {
        // The collateral-damage check on a real binary rather than on strings: this fixture's
        // demangled names (`caller_of_heavy`, `<u64>::wrapping_add`, and the rest) carry no closure
        // segment, so #155 must not touch any of them. If the rewrite ever eats a byte from a name
        // that has no closure in it, this fails on the same binary #145-#153 were pinned against.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        for address in 0..166 {
            for name in mapper
                .resolve(address)
                .into_iter()
                .map(|frame| frame.function_name)
            {
                assert_eq!(
                    collapse_closures(&name),
                    name,
                    "address {address} named {name:?} was rewritten"
                );
            }
        }
    }

    #[test]
    fn a_build_with_names_but_no_dwarf_loads_and_names_its_functions() {
        // #157's "done" row: a stripped binary resolves to function names. `debug = false` keeps
        // the `name` section and drops every `.debug_*`, so this is the case the fallback exists for
        // and the reason `new` no longer refuses to load it.
        let mapper =
            SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture keeps its `name`");

        assert!(
            !mapper.has_debug_info(),
            "names are not DWARF; the CLI has to be able to tell the two apart"
        );

        let stack = mapper.resolve(3);
        assert_eq!(stack.len(), 1, "`name` never yields an inline stack");
        assert_eq!(stack[0].function_name, "caller_of_heavy");
        assert_eq!(stack[0].file_path, None);
        assert_eq!(
            stack[0].line_number, None,
            "18 names for a code section is not a line table"
        );
    }

    #[test]
    fn every_defined_function_answers_with_its_name_in_code_section_order() {
        // The table is keyed by function index, so the ordering claim is really about the bodies:
        // these three names must line up with the three ranges #153 measured, not merely exist.
        let mapper = SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture");

        assert_eq!(
            named_bodies(&mapper),
            [
                "caller_of_heavy".to_string(),
                "memory_heavy_loop".to_string(),
                "compute_heavy_loop".to_string()
            ]
        );
    }

    #[test]
    fn dwarf_and_names_agree_on_the_function_that_owns_every_address() {
        // The strongest check available on these two fixtures, and it is cross-source: the names
        // come from the `name` section's index table and the outermost DWARF frame from gimli's
        // line tables, read out of two binaries built separately. They can only agree if the
        // import offset, the body framing and the index alignment are all right. Sweeping every
        // address rather than three bodies also catches a name applied where DWARF has none.
        let mapped = SourceMapper::new(DWARF_PROBE).expect("the DWARF fixture");
        let named = SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture");

        assert_eq!(
            mapped.code_map().map(CodeMap::bodies),
            named.code_map().map(CodeMap::bodies),
            "the two fixtures differ only in debug info, so their address spaces must match"
        );

        for address in 0..166 {
            let owner = mapped
                .resolve(address)
                .last()
                .map(|frame| frame.function_name.clone());
            let name = named
                .resolve_from_name_section(address)
                .first()
                .map(|frame| frame.function_name.clone());

            assert_eq!(
                name, owner,
                "address {address} names a different function per source"
            );
        }
    }

    #[test]
    fn the_names_fallback_is_coarser_at_an_inlined_call_site() {
        // DWARF answers `14` with two frames and `name` cannot see the inline at all: it names the
        // function that owns the address. The same function either way, which is what makes the
        // precedence DWARF-then-names a fallback rather than a conflict.
        let mapped = SourceMapper::new(DWARF_PROBE).expect("the DWARF fixture");
        let named = SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture");

        assert_eq!(mapped.resolve(14).len(), 2);
        assert_eq!(named.resolve(14).len(), 1);
        assert_eq!(
            named.resolve(14)[0].function_name,
            mapped.resolve(14).last().unwrap().function_name
        );
    }

    #[test]
    fn dwarf_wins_where_both_are_available() {
        // A DWARF-bearing binary also has a `name` section, and `resolve` must not degrade to it.
        // The assertion is about location: a name-only frame is the fallback's shape, and getting
        // one from `resolve` here would mean the mapper picked the coarser source.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the DWARF fixture");

        assert_eq!(mapper.resolve(3).len(), 1);
        assert!(
            mapper.resolve(3)[0].file_path.is_some(),
            "DWARF answers this address with a file, so `name` is not consulted"
        );
        assert_eq!(
            mapper.resolve_from_name_section(3)[0].function_name,
            mapper.resolve(3)[0].function_name,
            "both sources name the same function; only one of them has a line"
        );
        assert_eq!(mapper.resolve_from_name_section(3)[0].file_path, None);

        // And a mapper built with neither source names nothing, as it always has.
        assert!(
            SourceMapper::unmapped()
                .resolve_from_name_section(3)
                .is_empty()
        );
    }

    #[test]
    fn a_names_index_counts_imports_before_the_defined_functions() {
        // #141 measured 18 entries for a contract that imports 4 functions and defines 14, so a
        // table read as if it started at the first defined function would name every function by
        // the wrong one. Two imports here, three bodies, and names starting at index 2.
        let bytes = raw_module(&[
            (IMPORT_SECTION, imports(2)),
            (
                0,
                custom(
                    NAME_SECTION,
                    &function_names(&[
                        (0, "host_a"),
                        (1, "host_b"),
                        (2, "alpha"),
                        (3, "beta"),
                        (4, "gamma"),
                    ]),
                ),
            ),
            (CODE_SECTION, code_bodies(3)),
        ]);

        let mapper = SourceMapper::new(&bytes).expect("names are enough to load");

        assert_eq!(
            named_bodies(&mapper),
            ["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            "the two import names shift the table by exactly two"
        );
    }

    #[test]
    fn an_unreadable_import_section_switches_the_names_off() {
        // A module whose import list does not parse has an unknown offset, and an offset guessed at
        // zero would attach the wrong name to every function. No fallback is the honest answer, and
        // it is still the error that tells the user what their binary holds.
        let bytes = raw_module(&[
            (IMPORT_SECTION, vec![0x02, 0xff]),
            (
                0,
                custom(NAME_SECTION, &function_names(&[(2, "alpha"), (3, "beta")])),
            ),
            (CODE_SECTION, code_bodies(2)),
        ]);

        let error = SourceMapper::new(&bytes)
            .err()
            .expect("an unalignable name table must not be used");

        assert!(matches!(error, SourceMapError::MissingDebugInfo { .. }));
    }

    #[test]
    fn a_name_section_that_names_nothing_in_the_code_section_is_not_a_fallback() {
        // Kind 1 present, records well-formed, and not one of them names a function this module
        // defines. Loading that would let a whole run finish unnamed with nothing to warn about.
        let beyond = raw_module(&[
            (
                0,
                custom(NAME_SECTION, &function_names(&[(7, "not_in_this_module")])),
            ),
            (CODE_SECTION, code_bodies(2)),
        ]);
        // Only the module-name subsection (kind 0), which is `dwarf_probe.wasm`, not a function.
        let module_name_only = module(&[("name", &[0, 4, b'd', b'w', b'p', b'f'])]);

        for bytes in [&beyond, &module_name_only] {
            assert!(
                matches!(
                    SourceMapper::new(bytes).err(),
                    Some(SourceMapError::MissingDebugInfo { .. })
                ),
                "no name for a defined function means no fallback: {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn an_absurd_names_count_never_becomes_a_capacity() {
        // The subsection declares 2^32 function names and then stops. The walk is bounded by the
        // bytes, so this fails on the first missing record instead of allocating a table for the
        // claim — `AGENTS.md`'s OOM rule applied to reading a binary, as #153 does for the code
        // section.
        let mut body = uleb(4_294_967_296);
        body.extend(uleb(0));
        body.extend(uleb(2));
        body.extend(b"ok");
        let mut payload = vec![1];
        payload.extend(uleb(body.len() as u64));
        payload.extend(body);

        let bytes = module(&[("name", &payload)]);

        assert!(matches!(
            SourceMapper::new(&bytes).err(),
            Some(SourceMapError::MissingDebugInfo { .. })
        ));
    }

    #[test]
    fn a_names_symbol_arrives_rendered_the_way_dwarf_names_arrive() {
        // The section stores raw symbols, so the fallback runs the same pipeline #148 and #155 put
        // behind DWARF names. Both inputs here are measured: the v0 symbol is what the committed
        // fixture's DWARF carries at address 14, and a `#[no_mangle] extern "C"` name is what the
        // fixture's own functions are stored as.
        assert_eq!(
            demangle_symbol("_RNvMs7_NtCsknUcikIyyBm_4core3numy12wrapping_add").as_deref(),
            Some("<u64>::wrapping_add")
        );
        assert_eq!(
            demangle_symbol("caller_of_heavy").as_deref(),
            Some("caller_of_heavy")
        );
        assert_eq!(
            demangle_symbol("closure_probe::outer::{closure#0}").as_deref(),
            Some("closure_probe::outer::[closure#0]"),
            "the closure rewrite is part of every frame name, from either source"
        );
        assert_eq!(demangle_symbol(""), None, "an empty name keys nothing");
    }

    // #158's cache is invisible to answers and visible to cost, so these tests are about what a
    // second lookup returns, which mapper it returns it from, and how much a trace may make it hold.

    #[test]
    fn a_resolved_address_is_remembered_and_a_name_only_lookup_is_not() {
        // The entry-count assertions are the only evidence that `resolve` has a cache at all: every
        // other test here would pass with the map deleted.
        let mapped = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        assert!(
            mapped.cache.borrow().is_empty(),
            "constructing must not pre-resolve the module"
        );

        mapped.resolve(3);
        assert_eq!(
            mapped.cache.borrow().len(),
            1,
            "the lookup was not remembered"
        );
        mapped.resolve(3);
        assert_eq!(
            mapped.cache.borrow().len(),
            1,
            "a repeat must not add a second entry for the same address"
        );

        // #158's measurement says the fallback stays uncached: a `name`-only lookup costs 59 ns and
        // a cache hit costs 103 ns, so remembering it would make the degraded path slower.
        let named = SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture");
        named.resolve(3);
        assert!(
            named.cache.borrow().is_empty(),
            "the `name` path resolves by body range, not by line program, and must not be cached"
        );
    }

    #[test]
    fn a_repeated_address_answers_with_the_stack_it_first_returned() {
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        for pc in [2usize, 3, 14, 101, 158] {
            let first = mapper.resolve(pc);
            assert_eq!(first, mapper.resolve(pc), "cached at {pc}");
        }

        // The inlined call site is the case worth caching: two frames to rebuild, and the address
        // #156 measured as the most expensive in the fixture.
        let stack = mapper.resolve(14);
        assert_eq!(stack.len(), 2);
        assert_eq!(
            vec!["<u64>::wrapping_add", "caller_of_heavy"],
            stack
                .iter()
                .map(|frame| frame.function_name.as_str())
                .collect::<Vec<_>>(),
            "the cached stack must keep the innermost-first order"
        );
        assert_eq!(stack, mapper.resolve(14));
    }

    #[test]
    fn the_cache_cannot_change_an_answer_across_a_whole_trace() {
        // Sweeping the address space twice on one mapper has to give what a mapper nobody asked
        // before gives. A wrong cache is a wrong flamegraph, and this is the cheap way to say it
        // cannot happen.
        let warm = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let cold = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let once: Vec<Vec<SourceFrame>> = (0..166usize).map(|pc| warm.resolve(pc)).collect();
        let twice: Vec<Vec<SourceFrame>> = (0..166usize).map(|pc| warm.resolve(pc)).collect();
        let fresh: Vec<Vec<SourceFrame>> = (0..166usize).map(|pc| cold.resolve(pc)).collect();

        assert_eq!(once, twice, "a second sweep differs from the first");
        assert_eq!(once, fresh, "a cached answer differs from a cold one");
    }

    #[test]
    fn one_mappers_answers_never_reach_another() {
        // Nothing here is keyed by which binary a mapper read, so a shared cache would attribute one
        // module's addresses to another module's symbols. Each mapper owns its map.
        let mapped = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");
        let unmapped = SourceMapper::unmapped();

        assert!(!mapped.resolve(3).is_empty());
        assert!(
            unmapped.resolve(3).is_empty(),
            "an unmapped mapper answered from another mapper's cache"
        );
        assert!(
            mapped.resolve(usize::MAX).is_empty(),
            "an out-of-range address is not a frame, cached or not"
        );
    }

    #[test]
    fn a_trace_that_never_repeats_cannot_grow_the_cache_past_its_bound() {
        // Distinct addresses are the case a cache cannot plan for, so the bound holds whatever
        // arrives: three times the limit, none of them a repeat. These are addresses past the code
        // section, which is what a corrupt or non-wasm offset looks like.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        for pc in 0..(3 * RESOLUTION_CACHE_LIMIT) {
            mapper.resolve(1_000 + pc);
        }

        let held = mapper.cache.borrow().len();
        assert!(
            held <= RESOLUTION_CACHE_LIMIT,
            "the cache holds {held} entries for a limit of {RESOLUTION_CACHE_LIMIT}"
        );
        // Overflow costs recomputation, never correctness.
        assert_eq!(mapper.resolve(14).len(), 2);
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
    fn an_unmapped_mapper_resolves_nothing_at_any_address() {
        let mapper = SourceMapper::unmapped();

        assert!(!mapper.has_debug_info());
        for pc in [0usize, 1, 64, 4096, usize::MAX] {
            assert!(
                mapper.resolve(pc).is_empty(),
                "pc {pc} resolved to a frame from a mapper with no symbols"
            );
        }
    }

    #[test]
    fn section_walk_survives_a_module_that_ends_on_a_boundary() {
        // The loop must stop cleanly at the last section's end byte rather than reading one past.
        let exact = module(&[(".debug_info", b"\x00")]);

        assert_eq!(
            WasmSections::parse(&exact).unwrap().get(".debug_info"),
            Some(&[0u8][..])
        );
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
    fn a_code_section_that_overruns_its_bytes_leaves_the_module_loadable() {
        // Count says three functions, the payload holds one. The section table is intact, so this
        // is not `SourceMapError::Truncated`, and DWARF resolution must not be lost over an address
        // map nothing asked for.
        let payload = [uleb(3), uleb(1), vec![0x00]].concat();
        let sections = WasmSections::parse(&module_with_code(&payload))
            .expect("the section table itself is well formed");

        assert_eq!(
            sections.code, None,
            "the function list runs past the section"
        );
    }

    #[test]
    fn an_absurd_function_count_never_becomes_a_capacity() {
        // A 12-byte module declaring 2^64-1 functions must not allocate for them: the walk is
        // bounded by the bytes, not the count. `AGENTS.md`'s OOM rule is about the trace, and the
        // same habit applies to reading a binary.
        let payload = [uleb(u64::MAX), uleb(1), vec![0x00]].concat();

        assert_eq!(CodeMap::parse(&payload, 0), None);
    }

    #[test]
    fn an_unmapped_mapper_has_no_addresses_to_translate() {
        // The degraded path keeps working: no binary, so no code section, so no translation — and
        // no panic in the caller that asked anyway.
        let mapper = SourceMapper::unmapped();

        assert!(mapper.code_map().is_none());
        assert!(mapper.resolve_file_offset(113).is_empty());
    }
}
