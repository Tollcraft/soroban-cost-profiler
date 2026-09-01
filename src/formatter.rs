use crate::models::CallStackNode;

/// Converts the aggregated call tree into standard profiling formats (e.g., collapsed stack).
pub struct OutputFormatter;

impl OutputFormatter {
    pub fn to_collapsed_stack(_root: &CallStackNode) -> String {
        // TODO: Walk the tree and format it
        String::new()
    }
}
