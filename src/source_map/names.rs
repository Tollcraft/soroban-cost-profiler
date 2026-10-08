//! The `name`-section fallback: function names for a binary that carries no DWARF (#218).
//!
//! This is Stage 2's second symbol source, and a complete one — a stripped `debug = false` build
//! keeps its `name` section, so a contract whose author did everything the README's CAUTION block
//! asks still gets frames that say `memory_heavy_loop` instead of `wasm[72]`. What it cannot give
//! is a `file:line`, because a `name` section stores symbols and no line tables, and the CLI says so
//! out loud rather than letting a name-only profile pass for a mapped one.
//!
//! Nothing here reads DWARF and nothing here uses `gimli`. The input is one custom section's bytes,
//! which [`WasmSections`](super::wasm::WasmSections) has already located, and the output is a
//! `Vec<Option<String>>` positioned by *defined*-function index. That independence is why this can
//! be its own module: the fallback is a separate execution path, taken only when DWARF resolution
//! is unavailable or answered nothing, and it shares no state with the traversal.
//!
//! Two things it shares with the DWARF path anyway, deliberately:
//!
//! * [`collapse_closures`](super::collapse_closures) stays in the parent, because
//!   [`frame_name`](super::frame_name) needs it too. A closure has to render as `[closure#0]`
//!   whichever source named it, or the same function would appear as two frames in one flamegraph
//!   depending on a build flag.
//! * `addr2line`'s demangler, through [`addr2line::demangle_auto`]. The section stores no language,
//!   so this uses the same rustc-then-C++ heuristics `addr2line` applies to a DWARF name with no
//!   `DW_AT_language`. That is what `Cargo.toml`'s `features = ["rustc-demangle", "std"]` buys, and
//!   why no demangler is a direct dependency of this crate.
//!
//! The framing facts were measured for #141's spike rather than taken from the spec, and
//! `docs/spikes/02_wasm_name_section_fallback.md` records the measurement: the payload is a chain of
//! `u8 kind` + `uleb128 size` + body subsections with **no leading version byte**, unambiguous only
//! because the bytes tile the payload exactly. Under the version-byte reading the same bytes claim a
//! kind-`0x11` subsection, which no reader should obey.

use super::collapse_closures;
use super::wasm::{NAME_SECTION, WasmSections};

/// The `name` subsection that maps function indices to symbols.
const FUNCTION_NAMES: u8 = 1;

/// The `name` custom section's function names, positioned by defined-function index.
///
/// #141's spike is where this layout was measured, and the two facts that decide the parser are
/// recorded there: the payload is a chain of `u8 kind` + `uleb128 size` + body subsections with
/// **no leading version byte** — the framing is only unambiguous because the bytes tile the payload
/// exactly — and only kind `1`, function names, matters here. Its body is `uleb128 count` then
/// `count` records of `uleb128 funcidx` + `uleb128 len` + `len` UTF-8 bytes.
///
/// `funcidx` counts the whole function index space, imports first, so aligning it with a code
/// section address needs the import count; see [`SourceMapper::resolve_from_name_section`](super::SourceMapper::resolve_from_name_section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NameSection {
    /// One entry per defined function, in code-section order, already rendered by
    /// [`demangle_symbol`]. Names for imported functions are dropped rather than stored: no
    /// code-section address can ever reach them.
    names: Vec<Option<String>>,
}

impl NameSection {
    /// Read one module's function names, or conclude it has none worth loading.
    ///
    /// `None` — which is what makes [`SourceMapper::new`](super::SourceMapper::new) report
    /// [`SourceMapError::MissingDebugInfo`](super::SourceMapError::MissingDebugInfo) instead — when the section is absent, when its
    /// subsection framing does not tile the payload, when its records do not tile the function-names
    /// body, or when nothing in it names a function this module's code section contains.
    ///
    /// Both declared counts are bounded by the bytes that carry them, not the reverse: reading a
    /// section claiming millions of names costs one failed lookup and no allocation, which is
    /// `AGENTS.md`'s OOM rule applied to a binary rather than to a trace.
    pub(super) fn parse(sections: &WasmSections) -> Option<Self> {
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

    pub(super) fn name_at(&self, defined_index: usize) -> Option<String> {
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

/// The `name` section's counterpart to [`frame_name`](super::frame_name): demangle, then collapse closures.
///
/// The section stores no language, so this goes through `addr2line`'s heuristics — the same
/// rustc-then-C++ order it applies to a DWARF name with no `DW_AT_language` — and returns a name it
/// cannot parse byte-for-byte as stored. That is what makes `#[no_mangle] extern "C"` export names
/// arrive plain here too, exactly as they do from DWARF.
fn demangle_symbol(raw: &str) -> Option<String> {
    let name = addr2line::demangle_auto(std::borrow::Cow::Borrowed(raw), None);
    (!name.is_empty()).then(|| collapse_closures(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_map::tests::named_bodies;
    use crate::source_map::wasm::tests::{module, uleb};
    use crate::source_map::wasm::{CODE_SECTION, IMPORT_SECTION};
    use crate::source_map::{SourceMapError, SourceMapper};

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
}
