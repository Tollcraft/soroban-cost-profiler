//! Stage 2 of the pipeline: the facade the rest of the profiler calls, and the three modules it
//! delegates to.
//!
//! Everything here is [`SourceMapper`], the cache it answers through, and the two policies that are
//! the facade's own rather than any one symbol source's: which source wins at an address, and what
//! ratio of unanswered line-table samples makes a binary worth warning about (#162). The reading of
//! bytes lives in the submodules, and the line that decides which one owns a piece of it is whether
//! that piece names a `gimli` type:
//!
//! * `wasm` walks the container — the section table, the code section's framing, the import count
//!   — and is the only place a malformed file becomes a [`SourceMapError`].
//! * `names` reads the `name` section: the function-name-only fallback for a `debug = false` build
//!   (#157).
//! * `dwarf` holds the traversal — `gimli::Dwarf::load`, `addr2line::Context`, `find_frames` — plus
//!   [`CodeMap`], the address map those line tables are written against.
//!
//! Nothing in the first two names a DWARF type, which is what makes `AGENTS.md`'s "no custom DWARF
//! parsing" rule checkable file by file instead of by reading the code: the hand-written parsing that
//! exists is container-level, and every DWARF byte is read by `gimli`.
//!
//! The precedence a lookup follows is DWARF, then `name`, then the `wasm[pc]` the aggregator falls
//! back to; [`SourceMapper::resolve`] is where that is decided and where the answer is remembered.
//! The long form of the measurements behind all of it — the address space, the inline-stack order,
//! what survives `wasm-opt` — is `docs/internals/dwarf_mapping.md`.

use crate::models::SourceFrame;
use std::cell::RefCell;
use std::collections::HashMap;

mod dwarf;
mod names;
mod wasm;

pub use dwarf::{CodeMap, SourceMapError};
use dwarf::{Context, DEBUG_INFO, load_dwarf};
use names::NameSection;
use wasm::WasmSections;

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
    /// A degradation this mapper detected about itself and the user has to be told about (#186).
    ///
    /// Detection lives here because the ratio that produces it is this stage's own measurement;
    /// *delivery* cannot, because a library stage has no terminal to write to and `main` owns the
    /// CLI's output. A `tracing::warn!` written here would now reach a subscriber (#214), but only
    /// at a level the user has to ask for — and a degradation the profiler knows about is news, not
    /// narration. So the CLI reads this field and prints it to stderr beside the fatal messages,
    /// which is the one channel that is always on.
    warning: Option<String>,
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

/// How large a share of sampled addresses a binary's line tables may fail to answer before the run
/// is told not to trust its line numbers (#162).
///
/// The bound is deliberately near the top of the range, and the measurement that puts it there is
/// the committed fixture: 7 of its 17 sampled addresses count as unanswered, not because its line
/// tables miss but because the counter also charges an address whose line another sampled address
/// already claimed, which a loop body of a few source lines does constantly. So a *healthy* binary
/// scores 41% on this metric and a binary whose DWARF describes different code — the pre-inlining,
/// pre-optimization shape an optimizer step leaves behind — scores almost 100%. The two populations
/// are far apart and the threshold only has to stay out of the middle: anything stricter would fire
/// on ordinary optimized builds, and a warning that fires on a correct binary is a warning the user
/// learns to skip past.
///
/// The silent band includes the bound: exactly 90% unanswered is not degenerate, because a binary
/// that still answers one sample in ten is degraded rather than wrong, and the frames it does give
/// are the ones a flamegraph is built from.
const DEGENERATE_RATIO: f64 = 0.90;

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
    /// let stripped = include_bytes!("../../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");
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
    /// let mapped = SourceMapper::new(include_bytes!("../../fixtures/dwarf_probe/dwarf_probe.wasm"));
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
                // Degraded, but in the way #157 designed: names without lines. The CLI says so from
                // `has_debug_info`, because the fix is a build flag and that is the CLI's register,
                // not this stage's measurement.
                warning: None,
                cache: RefCell::new(HashMap::new()),
            });
        }

        let context = Context::from_dwarf(load_dwarf(&sections)).map_err(|error| {
            SourceMapError::UnreadableDwarf {
                reason: error.to_string(),
            }
        })?;

        let mut mapper = Self {
            context: Some(context),
            code: sections.code,
            names,
            warning: None,
            cache: RefCell::new(HashMap::new()),
        };
        mapper.warning = mapper.degenerate_warning();
        Ok(mapper)
    }

    /// The message for a mapper whose own line tables mostly fail to answer, or `None`.
    ///
    /// The sample is taken in `dwarf`, because taking it walks line tables; the judgement is here,
    /// because it is the facade's — [`DEGENERATE_RATIO`] is what this stage promises a user about
    /// when it will interrupt them, and the CLI reads the answer through [`SourceMapper::warning`].
    /// Over the bound means the DWARF that did load describes different code from the bytes that
    /// ran, which is #162's finding and the reason this is a warning rather than an error: the run
    /// still profiles, the frames are just not to be trusted line by line.
    fn degenerate_warning(&self) -> Option<String> {
        let (missing_or_duplicate, sampled) = self.degenerate_sample()?;
        degenerate_message(missing_or_duplicate, sampled)
    }

    /// A mapper that resolves nothing, for a run that continues without symbols.
    ///
    /// This is the degraded-but-working path: Stages 3 and 4 still produce a tree, keyed by the
    /// `wasm[pc]` / `host[pc]` names the tracer already has. The CLI reaches it when
    /// [`SourceMapper::new`] refuses a binary it cannot name at all, and prints that error as the
    /// warning (#186) — the run continues, it is not silently unnamed.
    pub fn unmapped() -> Self {
        Self {
            context: None,
            code: None,
            names: None,
            warning: None,
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

    /// Whether this mapper can name functions at all, from DWARF or from the `name` section (#157).
    ///
    /// Beside [`has_debug_info`] this separates the two degradations a user can hit, which need
    /// different sentences: a binary with a `name` section but no line tables produces frames that
    /// name the right functions and nothing else, while one with neither produces frames named only
    /// by address.
    pub fn names_functions(&self) -> bool {
        self.names.is_some()
    }

    /// What this mapper detected about its own quality, for the CLI to print (#186).
    ///
    /// `None` means "nothing to report", not "nothing is wrong" — the degradations that are a
    /// missing build flag are visible from [`has_debug_info`] and [`names_functions`] instead, and
    /// this one is only measurable after the DWARF loaded.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
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
    /// let fixture = include_bytes!("../../fixtures/dwarf_probe/dwarf_probe.wasm");
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
    /// let stripped = include_bytes!("../../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");
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
    /// let fixture = include_bytes!("../../fixtures/dwarf_probe/dwarf_probe.wasm");
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

/// The warning one line-table sample earns, or `None` when it is within [`DEGENERATE_RATIO`].
///
/// Split out from [`SourceMapper::degenerate_warning`] so the threshold and the sentence are
/// testable on their own: no committed fixture is degenerate, so a test that only asked a mapper
/// could never reach the branch that tells a user their line numbers are wrong. The percentage is
/// the measured one rather than the threshold, so the message says what this binary did.
fn degenerate_message(missing_or_duplicate: usize, sampled: usize) -> Option<String> {
    let degenerate_ratio = missing_or_duplicate as f64 / sampled as f64;
    (degenerate_ratio > DEGENERATE_RATIO).then(|| {
        format!(
            "{:.0}% of the sampled addresses in this binary map to no line or to one another \
             address already claimed, so its DWARF describes different code from the bytes that \
             ran — typically pre-inlining, pre-optimization output. The frames below are not wrong \
             about which functions ran, but read their line numbers with suspicion.",
            degenerate_ratio * 100.0
        )
    })
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

#[cfg(test)]
mod tests {
    use super::wasm::tests::module;
    use super::*;

    /// The name each defined function's first byte answers with, through the fallback.
    ///
    /// `pub(super)` because `names`'s own tests ask the same question of the modules they assemble,
    /// and the answer has to come from the mapper rather than from a second copy of the walk.
    pub(super) fn named_bodies(mapper: &SourceMapper) -> Vec<String> {
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
    ///
    /// `pub(super)` because `dwarf`'s tests sweep the same binary: the cross-source checks compare
    /// what its line tables and its `name` section say about one address space, and that comparison
    /// means nothing unless both halves are reading these exact bytes.
    pub(super) const DWARF_PROBE: &[u8] =
        include_bytes!("../../fixtures/dwarf_probe/dwarf_probe.wasm");

    /// The same three functions built with `debug = false`: no DWARF, same `name` section.
    const NO_DEBUG_PROBE: &[u8] =
        include_bytes!("../../fixtures/dwarf_probe/dwarf_probe_no_debug.wasm");

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
    fn an_unmapped_mapper_has_no_addresses_to_translate() {
        // The degraded path keeps working: no binary, so no code section, so no translation — and
        // no panic in the caller that asked anyway.
        let mapper = SourceMapper::unmapped();

        assert!(mapper.code_map().is_none());
        assert!(mapper.resolve_file_offset(113).is_empty());
    }

    #[test]
    fn a_sample_at_the_threshold_is_not_a_degenerate_binary() {
        // The silent band includes the threshold itself: a binary that still answers one sample
        // in ten is degraded, not wrong, and interrupting a run over it is how warnings get
        // ignored.
        assert_eq!(degenerate_message(9, 10), None, "exactly 90% is inside");
        assert_eq!(degenerate_message(90, 100), None);
        assert_eq!(degenerate_message(0, 17), None, "answers every sample");

        // `degenerate_sample` returns `None` rather than a zero total when there is nothing to walk,
        // so this ratio is never formed in practice — but a NaN would compare false against the
        // bound, so even an unguarded zero would answer "no warning" rather than panic.
        assert_eq!(degenerate_message(0, 0), None);
    }

    #[test]
    fn a_sample_past_the_threshold_names_the_percentage_it_measured() {
        let message = degenerate_message(91, 100).expect("91% is past the bound");

        assert!(
            message.starts_with("91%"),
            "the message says what this binary measured, not the threshold: {message}"
        );
        assert!(
            message.contains("different code from the bytes that ran"),
            "and what that means: {message}"
        );
        assert!(
            message.contains("read their line numbers with suspicion"),
            "and what to do about it: {message}"
        );
        // Every address unanswered is the shape #162 measured on an optimized artifact whose DWARF
        // came from the pre-inlining build.
        assert!(
            degenerate_message(166, 166)
                .expect("nothing answered at all")
                .starts_with("100%"),
            "the ratio is the measured one, so a total miss reads as 100%"
        );
    }

    #[test]
    fn the_committed_fixture_samples_its_line_tables_and_earns_no_warning() {
        // The sampler's half of the split, against a real binary: three bodies of 14, 139 and 7
        // bytes sampled every tenth address is 2 + 14 + 1 addresses. 7 of those 17 count as
        // unanswered, and none of them is a miss — the counter also charges an address whose line
        // another sampled address already claimed, which a loop body of a few source lines does
        // constantly. That is the measurement behind `DEGENERATE_RATIO` sitting at 0.90 rather than
        // anywhere near a healthy binary's 41%: only a table describing *different* code misses
        // almost everything. `main.rs` pins the same fact from the CLI side, where it reads as "a
        // properly built artifact earns no warning"; this pins the count the ratio is built from.
        let mapper = SourceMapper::new(DWARF_PROBE).expect("the fixture carries DWARF");

        let (missing_or_duplicate, sampled) = mapper
            .degenerate_sample()
            .expect("the fixture has function bodies and DWARF to walk");

        assert_eq!(sampled, 17, "one sample per ten addresses of each body");
        assert_eq!(
            missing_or_duplicate, 7,
            "repeated lines in a small fixture's loop bodies, not misses"
        );
        assert_eq!(mapper.warning(), None);

        // A mapper with no DWARF has nothing to sample, and says so rather than reporting a ratio.
        let named = SourceMapper::new(NO_DEBUG_PROBE).expect("the stripped fixture");
        assert_eq!(named.degenerate_sample(), None);
        assert_eq!(SourceMapper::unmapped().degenerate_sample(), None);
    }
}
