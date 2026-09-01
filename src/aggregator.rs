use crate::models::{CallStackNode, TraceEvent};
use crate::source_map::SourceMapper;

/// Consumes a stream of `TraceEvent`s and builds a logical call stack tree.
#[derive(Default)]
pub struct ProfileAggregator {
    // TODO: Internal stack state
}

impl ProfileAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn aggregate(&mut self, _events: Vec<TraceEvent>, _mapper: &SourceMapper) -> CallStackNode {
        // TODO: Fold events into a CallStackNode tree
        unimplemented!()
    }
}
