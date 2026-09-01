use crate::models::SourceFrame;

/// Translates raw WASM Program Counters (PC) to Rust source lines using DWARF.
pub struct SourceMapper {
    // TODO: Hold the parsed DWARF data / addr2line context here
}

impl SourceMapper {
    pub fn new(_wasm_bytes: &[u8]) -> Self {
        Self {}
    }

    pub fn resolve(&self, _pc: usize) -> Option<SourceFrame> {
        // TODO: Map PC to file/line/function using addr2line
        None
    }
}
