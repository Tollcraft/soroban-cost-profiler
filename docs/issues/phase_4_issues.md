# Phase 4: Aggregation & Formatting - Issue Bank

### Issue 1: [Phase 4] Scaffold `ProfileAggregator` internal stack
* **Tags:** `feat`, `good first issue`
* **Context:** The aggregator needs a stack to track which function is currently executing as it walks through the `TraceEvent` stream.
* **Simplified Task:** Add a `current_stack: Vec<String>` (or similar) field to `ProfileAggregator` in `src/aggregator.rs`.
* **Why it's independent:** Basic struct initialization.
* **Acceptance Criteria:** The struct holds the stack state and compiles.

### Issue 2: [Phase 4] Implement `push_frame` on `Call` event
* **Tags:** `feat`, `test`
* **Context:** When a `Call` event is processed, a new frame must be pushed to the active stack tree.
* **Simplified Task:** Write a method that processes a `TraceEvent` of type `Call` and updates the internal call tree.
* **Why it's independent:** Isolated logic for handling one specific enum variant.
* **Acceptance Criteria:** The internal tree correctly adds a child node.

### Issue 3: [Phase 4] Implement `pop_frame` on `Return` event
* **Tags:** `feat`, `test`
* **Context:** When a `Return` event is processed, the current frame must be popped off the active stack.
* **Simplified Task:** Write a method that processes a `TraceEvent` of type `Return` and moves the active pointer up to the parent.
* **Why it's independent:** Isolated logic for handling one specific enum variant.
* **Acceptance Criteria:** The active pointer correctly returns to the parent node.

### Issue 4: [Phase 4] Implement `add_cost` on `Step` event
* **Tags:** `feat`, `test`
* **Context:** When a `Step` event occurs, the CPU cost must be added to the currently active frame.
* **Simplified Task:** Write a method that processes a `TraceEvent` of type `Step` and increments the active node's cost.
* **Why it's independent:** Isolated math logic.
* **Acceptance Criteria:** The active node's cost is successfully incremented.

### Issue 5: [Phase 4] Calculate exclusive vs inclusive CPU
* **Tags:** `feat`, `test`
* **Context:** "Exclusive" is the cost of the function itself; "Inclusive" is the cost of the function plus all functions it called.
* **Simplified Task:** Write a post-processing function that walks the built `CallStackNode` tree and accurately calculates the `inclusive_cpu` for every node.
* **Why it's independent:** Pure tree-traversal math.
* **Acceptance Criteria:** A unit test proves that a parent's inclusive cost equals its exclusive cost plus all children's inclusive costs.

### Issue 6: [Phase 4] Scaffold `OutputFormatter` logic
* **Tags:** `feat`, `good first issue`
* **Context:** We need to output the tree in the `.folded` stack format.
* **Simplified Task:** Write a dummy function `to_collapsed_stack(root: &CallStackNode) -> String` that just returns an empty string.
* **Why it's independent:** API definition.
* **Acceptance Criteria:** The function compiles.

### Issue 7: [Phase 4] Implement recursive tree traversal for formatting
* **Tags:** `feat`
* **Context:** To format the tree, we must recursively walk down every path from root to leaf.
* **Simplified Task:** Implement the recursive traversal inside `to_collapsed_stack` that builds a string path (e.g., `main;child;grandchild`).
* **Why it's independent:** Pure string manipulation and recursion.
* **Acceptance Criteria:** A unit test proves the string paths are generated correctly.

### Issue 8: [Phase 4] Append costs to formatted string
* **Tags:** `feat`
* **Context:** The `.folded` format requires the exclusive cost at the end of the line (e.g., `main;child 500`).
* **Simplified Task:** Update the traversal to append the node's `exclusive_cpu` cost at the end of each path string.
* **Why it's independent:** Simple string concatenation.
* **Acceptance Criteria:** The final string perfectly matches the `.folded` spec.
