//! The WASM container walk Stage 2 starts from (#217).
//!
//! Everything here reads the *container* and nothing else: the magic and version, then the section
//! table — id, payload length, and for a custom section the name in front of its bytes. That is the
//! whole of the module structure symbolization needs, and it is why this file can exist at all
//! inside `AGENTS.md`'s "no custom DWARF parsing" rule: the walk locates `.debug_*` sections and
//! hands their bytes to `addr2line`/`gimli` untouched, and it never interprets a byte of DWARF
//! itself.
//!
//! Two things come out of one pass over the section table, because a second pass would mean reading
//! the same lengths twice:
//!
//! * the retained custom sections ([`WasmSections::get`]), which is what `load_dwarf` and the
//!   `name`-section fallback consume;
//! * the code section's address map ([`CodeMap`]) and the import section's function count, both of
//!   which are *located* here and computed by whoever owns their format — the walk only knows where
//!   a section's payload starts and ends.
//!
//! The strictness is asymmetric on purpose, and the tests below are what pin it: truncation is an
//! error, because a section that claims bytes the file does not have is a partial download rather
//! than a build this tool can say anything about; everything else is lenient, because a module
//! carrying sections this stage ignores (`producers`, `target_features`, Soroban's
//! `contractspecv0`) is a normal binary and still maps fine.
//!
//! Counts are read but never believed: a LEB128 that would overflow `u64` is `Truncated` rather
//! than a wrap, and a declared function count is walked against the bytes that are actually there
//! instead of being used as a capacity — the same habit `AGENTS.md`'s OOM rule asks of the trace,
//! applied to reading a binary.

use super::{CodeMap, SourceMapError};

/// The section whose bytes define the address space DWARF line tables are written against.
///
/// Section id `10` in the WASM binary spec's ordering — the code section, one size-prefixed body
/// per defined function, in the order the function section lists them.
pub(super) const CODE_SECTION: u64 = 10;

/// The custom section that names functions without naming files: `name`.
///
/// Present in every `rust-lld` build, including `debug = false` ones, and the only symbol source
/// for a binary with no DWARF (#157). Binaryen's optimizer deletes it unless `-g` is passed — see
/// `docs/spikes/02_wasm_name_section_fallback.md`.
pub(super) const NAME_SECTION: &str = "name";

/// The section whose function entries offset a `name` index into a defined-function position.
pub(super) const IMPORT_SECTION: u64 = 2;

/// The custom sections a mapper keeps, in file order.
pub(super) struct WasmSections {
    /// `(name, payload)`, holding only the sections this stage can use: `.debug_*` for source
    /// mapping and `name` for the function-name-only fallback. Everything else — `producers`,
    /// `target_features`, Soroban's `contractspecv0` — would be a copy of bytes nothing reads.
    retained: Vec<(String, Vec<u8>)>,
    /// The code section's address map, from the same walk, for #153's translation. `None` when the
    /// module has no code section or its function list does not fit its declared size; only the
    /// section's *location* is kept, never its bytes, so this costs a `Vec` of ranges and no copy.
    pub(super) code: Option<CodeMap>,
    /// How many function indices belong to imports, so a `name` section's indices line up with
    /// [`CodeMap`]'s defined-function list (#157).
    ///
    /// `Some(0)` when the module carries no import section, which is what both `dwarf_probe`
    /// fixtures do, and `None` when one is present but does not parse — a missing fallback is
    /// honest, an offset guessed from a section that would not read is not.
    pub(super) imported_functions: Option<u64>,
}

impl WasmSections {
    /// Walk the WASM section table, keeping the sections Stage 2 reads.
    ///
    /// A hand-written parser for the *container* only: section id, payload length, and for
    /// custom sections the name prefix. That is the whole module structure `addr2line` needs and
    /// it never inspects DWARF itself, which is what keeps this inside `AGENTS.md`'s "no custom
    /// DWARF parsing" rule. Deliberately strict about truncation and lenient about everything
    /// else: a module with sections this stage ignores still maps fine.
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, SourceMapError> {
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
    pub(super) fn read_uleb(bytes: &[u8], cursor: &mut usize) -> Result<u64, SourceMapError> {
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

    pub(super) fn get(&self, name: &str) -> Option<&[u8]> {
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
    pub(super) fn custom_names(&self) -> Vec<String> {
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

/// Whether a custom section is worth keeping in memory.
fn retain(name: &str) -> bool {
    name == NAME_SECTION || name.starts_with(".debug")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encode one LEB128 unsigned integer.
    pub(crate) fn uleb(mut value: u64) -> Vec<u8> {
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
    pub(crate) fn module(custom: &[(&str, &[u8])]) -> Vec<u8> {
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
}
