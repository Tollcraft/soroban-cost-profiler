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

### Issue 9: [Phase 4] Document the `.folded` stack format
* **Tags:** `docs`, `good first issue`
* **Context:** Contributors formatting the output need to know exactly what the string should look like.
* **Simplified Task:** Create a `docs/internals/folded_stack_spec.md` file detailing the syntax requirements of a collapsed stack file.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** The spec clearly shows examples of the `.folded` format.

### Issue 10: [Phase 4] Document `inclusive` vs `exclusive` math
* **Tags:** `docs`, `good first issue`
* **Context:** The mathematical difference between inclusive and exclusive costs often confuses new developers.
* **Simplified Task:** Create a `docs/internals/cost_math.md` file (or add it to `ARCHITECTURE.md`) explaining the difference with a simple diagram/example.
* **Why it's independent:** Writing markdown documentation.
* **Acceptance Criteria:** The difference is clearly explained for future contributors.


### Issue 22: [Phase 4] Track exclusive/inclusive memory cost alongside CPU
* **Tags:** `feat`, `test`
* **Simplified Task:** Extend aggregation logic to accumulate and roll up `mem_cost`.
* **Acceptance Criteria:** `inclusive_mem` correctly sums children's memory costs.

### Issue 23: [Phase 4] Merge sibling calls to the same function (Preserve Recursion)
* **Tags:** `feat`, `test`, `bug`
* **Context:** A loop calling a function N times should merge into one sibling node, but true recursive calls (`fib -> fib`) must remain distinct to preserve stack depth.
* **Simplified Task:** Update `push_frame` to sum costs if a *sibling* child with the same name exists, without merging recursive depth instances.
* **Why it's independent:** Isolated fix to `Call` handling that avoids flamegraph corruption.
* **Acceptance Criteria:** Sequential calls merge; recursive calls nest distinctly.

### Issue 24: [Phase 4] Finalize partial trees from unmatched `Call` events
* **Tags:** `feat`, `bug`
* **Simplified Task:** Add `finalize(&mut self)` to pop still-open frames for panic-recovery.
* **Acceptance Criteria:** Partial trace produces well-formed tree.

### Issue 25: [Phase 4] Sanitize function names for the `.folded` format
* **Tags:** `feat`, `bug`
* **Simplified Task:** Escape or strip characters (`;`, whitespace) from function names in the formatter.
* **Acceptance Criteria:** Names with semicolons parse correctly.

### Issue 26: [Phase 4] Add `--metric` support: CPU, Memory, and Host-Calls
* **Tags:** `feat`
* **Context:** The formatter needs to emit outputs for all 3 PRD dimensions.
* **Simplified Task:** Parametrize `to_collapsed_stack` with `CostMetric` (`Cpu`, `Memory`, `HostCalls`) to select which field to append.
* **Why it's independent:** Plugs directly into the formatter.
* **Acceptance Criteria:** Formatter successfully emits all 3 distinct cost dimensions.

### Issue 27: [Phase 4] Deterministic child ordering in formatted output
* **Tags:** `feat`, `test`
* **Simplified Task:** Sort children by function name/cost before writing out.
* **Acceptance Criteria:** Identical traces produce byte-identical files.

### Issue 28: [Phase 4] Represent host-function costs as opaque tree nodes
* **Tags:** `feat`
* **Simplified Task:** Insert child nodes labeled `[host: <CostType>]` for host boundaries.
* **Acceptance Criteria:** Host costs appear explicitly in the tree.

### Issue 29: [Phase 4] Full pipeline integration test (events → tree → `.folded` string)
* **Tags:** `test`
* **Simplified Task:** Test full aggregation pipeline end-to-end.
* **Acceptance Criteria:** Asserts a specific `.folded` string for synthetic input.

### Issue 30: [Phase 4] Document formatting conventions
* **Tags:** `docs`, `good first issue`
* **Simplified Task:** Update folded stack spec doc with metric-specific examples.
* **Acceptance Criteria:** Spec doc shows memory and host-call examples.

