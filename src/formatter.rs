use crate::models::{CallStackNode, Metric};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// Converts the aggregated call tree into standard profiling formats (e.g., collapsed stack).
pub struct OutputFormatter;

/// Which side of a differential flamegraph a single call stack falls on.
///
/// `flamegraph.pl --diff` shades each frame by the ratio between a baseline and a current
/// count: regressions red, improvements blue. Rendering SVG is an explicit MVP cut
/// (`AGENTS.md` forbids adding `inferno` or any SVG library), so this type is where the
/// red/blue *decision* lives and gets tested, and the external viewer applies the hue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaScale {
    /// The stack costs more in the current run — the renderer draws it red.
    Regression,
    /// The stack costs less in the current run — the renderer draws it blue.
    Improvement,
    /// Identical cost in both runs, including a stack that costs nothing.
    Neutral,
}

/// One function's exclusive cost in two runs, as whoever asked for a comparison reads it (#190).
///
/// Per *function* and not per stack, because the question is "did my change make this cheaper",
/// which a stack-level diff answers only if the reader sums the lines themselves. The two raw
/// counts travel with it rather than just the difference: `+51489` on its own cannot say whether
/// that is noise on a 50-million-instruction function or the whole of a small one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDelta {
    /// The frame name as both artifacts spell it.
    pub name: String,
    /// Its exclusive cost in the baseline, `0` when the function is new since then.
    pub baseline: u64,
    /// Its exclusive cost in the current run, `0` when it no longer appears.
    pub current: u64,
}

impl FunctionDelta {
    /// Signed change, `current - baseline`, so a negative number is the improvement the user hopes
    /// for. Widened to `i128` because the difference of two `u64` costs is not representable as an
    /// `i64` — saturating would hide a change that large rather than report it.
    pub fn delta(&self) -> i128 {
        self.current as i128 - self.baseline as i128
    }

    /// Which side of the red/blue split this function falls on, sharing [`DeltaScale`]'s rule with
    /// the differential artifact.
    pub fn scale(&self) -> DeltaScale {
        OutputFormatter::delta_scale(self.baseline, self.current)
    }
}

/// The metric's name as it appears in the summary header and in `--metric`.
fn metric_name(metric: &Metric) -> &'static str {
    match metric {
        Metric::Cpu => "cpu",
        Metric::Memory => "memory",
        Metric::Hostcalls => "hostcalls",
    }
}

impl OutputFormatter {
    /// Rank the tree's functions by the cost each caused directly, hottest first, at most `n`.
    ///
    /// Exclusive cost and not inclusive, because an entry function that called everything would
    /// top every list by definition, and a "hottest functions" answer that names the caller is not
    /// actionable. Frames sharing a name pool together — the same collapse the `.folded` output
    /// does — so a function that appears on ten stacks is ranked on its total direct cost, not ten
    /// times separately. Zero-cost frames are left out: a list of hot functions has nothing to say
    /// about them. Ties break on the name, so the same tree always prints the same ranking.
    pub fn top_functions(root: &CallStackNode, metric: &Metric, n: usize) -> Vec<(String, u64)> {
        let mut costs = BTreeMap::new();
        Self::traverse_costs(root, metric, &mut costs);

        let mut ranked: Vec<(String, u64)> = costs.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.truncate(n);
        ranked
    }

    fn traverse_costs(node: &CallStackNode, metric: &Metric, costs: &mut BTreeMap<String, u64>) {
        let cost = match metric {
            Metric::Cpu => node.exclusive_cpu,
            Metric::Memory => node.exclusive_mem,
            Metric::Hostcalls => node.exclusive_hostcalls,
        };

        if cost > 0 {
            *costs.entry(node.frame.function_name.clone()).or_insert(0) += cost;
        }

        for child in node.children.values() {
            Self::traverse_costs(child, metric, costs);
        }
    }

    /// [`top_functions`]'s ranking as the list the terminal shows: one header, one row per
    /// function, costs right-aligned so the ranking is scannable without reading the digits.
    ///
    /// Separate from `to_collapsed_stack` because the two have different audiences — the file is
    /// for a viewer that re-lays-out every frame, this is for whoever just ran the CLI and wants
    /// to know where to look first. `color` paints the header and the costs with ANSI escapes, and
    /// stays a parameter rather than a probe of the stream, so a redirected or piped summary is
    /// plain text and this function remains testable. An empty ranking is a real outcome today —
    /// the engine reports no program counter, so a run can leave every frame at zero cost — and it
    /// says so instead of printing a header over a blank list.
    ///
    /// [`top_functions`]: OutputFormatter::top_functions
    pub fn to_top_summary(ranked: &[(String, u64)], metric: &Metric, color: bool) -> String {
        let paint = |code: &str, text: String| {
            if color {
                format!("\x1b[{code}m{text}\x1b[0m")
            } else {
                text
            }
        };

        if ranked.is_empty() {
            return paint(
                "33",
                format!(
                    "no function recorded any exclusive cost ({})",
                    metric_name(metric)
                ),
            );
        }

        let name_width = ranked
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .unwrap_or(0);
        let cost_width = ranked
            .iter()
            .map(|(_, cost)| cost.to_string().len())
            .max()
            .unwrap_or(0);
        let mut output = String::new();
        let _ = writeln!(
            output,
            "{}",
            paint(
                "1",
                format!(
                    "Top {} functions by exclusive cost ({}):",
                    ranked.len(),
                    metric_name(metric)
                )
            )
        );
        for (index, (name, cost)) in ranked.iter().enumerate() {
            let _ = writeln!(
                output,
                " {:>2}. {:<name_width$}  {}",
                index + 1,
                name,
                paint("33", format!("{cost:>cost_width$}")),
            );
        }
        output
    }

    /// Formats the tree into a collapsed stack efficiently.
    pub fn to_collapsed_stack(root: &CallStackNode, metric: &Metric) -> String {
        let mut output = String::with_capacity(1024); // Pre-allocate to optimize memory allocations
        let mut current_path = String::new();
        Self::format_node(root, metric, &mut current_path, &mut output);
        output
    }

    /// Recursively walks the tree, avoiding unnecessary clones by using mutable string references.
    fn format_node(
        node: &CallStackNode,
        metric: &Metric,
        current_path: &mut String,
        output: &mut String,
    ) {
        let original_len = current_path.len();

        if !current_path.is_empty() {
            current_path.push(';');
        }
        current_path.push_str(&node.frame.function_name);

        // Folded format: `<path> <cost>`
        let cost = match metric {
            Metric::Cpu => node.exclusive_cpu,
            Metric::Memory => node.exclusive_mem,
            Metric::Hostcalls => node.exclusive_hostcalls,
        };
        let _ = writeln!(output, "{} {}", current_path, cost);

        for child in node.children.values() {
            Self::format_node(child, metric, current_path, output);
        }

        // Backtrack efficiently by truncating to the original length
        current_path.truncate(original_len);
    }

    /// Read a folded-stack artifact (`<stack> <count>` per line) into stack -> cost.
    ///
    /// [`to_collapsed_stack`](Self::to_collapsed_stack) writes this format, but the input
    /// here comes off disk and may be truncated or hand-edited, so a malformed line is
    /// reported with its line number instead of being skipped. Repeated stacks are summed,
    /// which is what the consuming viewers do when they merge duplicate lines.
    pub fn parse_folded(input: &str) -> Result<BTreeMap<String, u64>, String> {
        let mut stacks: BTreeMap<String, u64> = BTreeMap::new();

        for (index, line) in input.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }

            let (path, cost) = line.rsplit_once(' ').ok_or_else(|| {
                format!(
                    "line {}: expected `<stack> <count>`, got {line:?}",
                    index + 1
                )
            })?;
            let cost = cost.parse::<u64>().map_err(|_| {
                format!(
                    "line {}: expected a non-negative integer cost, got {cost:?}",
                    index + 1
                )
            })?;

            let total = stacks.entry(path.to_string()).or_insert(0);
            *total = total.saturating_add(cost);
        }

        Ok(stacks)
    }

    /// Pair two folded-stack artifacts into the diff format `flamegraph.pl --diff` reads:
    /// `<stack> <baseline> <current>`.
    ///
    /// A stack present in only one run gets `0` on the missing side, so a frame that was
    /// added or removed shows up downstream as an unbounded red or blue frame rather than
    /// silently vanishing from the comparison. Keys are taken from the sorted maps, making
    /// the output byte-stable across runs — which is the whole point of the artifact, since
    /// a cost regression is only trustworthy if re-running the diff produces no noise.
    pub fn to_differential_folded(baseline: &str, current: &str) -> Result<String, String> {
        let baseline = Self::parse_folded(baseline)?;
        let current = Self::parse_folded(current)?;

        let paths: BTreeSet<&str> = baseline
            .keys()
            .map(String::as_str)
            .chain(current.keys().map(String::as_str))
            .collect();

        let mut output = String::with_capacity(1024);
        for path in paths {
            let base = baseline.get(path).copied().unwrap_or(0);
            let now = current.get(path).copied().unwrap_or(0);
            let _ = writeln!(output, "{path} {base} {now}");
        }

        Ok(output)
    }

    /// Classify one stack's baseline/current pair for the [`DeltaScale`] color mapping.
    ///
    /// Comparison is on the raw counts rather than a ratio: a stack added since the
    /// baseline has ratio `inf` and a removed one has `0/0`, both of which the sign rule
    /// already classifies correctly. Intensity is left to the renderer, which is the only
    /// component that knows a picture's dynamic range.
    pub fn delta_scale(baseline: u64, current: u64) -> DeltaScale {
        if current > baseline {
            DeltaScale::Regression
        } else if current < baseline {
            DeltaScale::Improvement
        } else {
            DeltaScale::Neutral
        }
    }

    /// Sum a parsed artifact's costs onto the function each stack *ends* in.
    ///
    /// The last frame of a folded path is the frame that was executing when the cost landed, and
    /// that is exactly what [`to_collapsed_stack`](Self::to_collapsed_stack) writes as the node's
    /// own count — so pooling by leaf rebuilds exclusive cost from the file alone, without the
    /// tree. Inclusive cost would make the comparison useless: the entry function is the parent of
    /// every change in it, so it would top the list whatever the user optimized.
    fn exclusive_by_function(stacks: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
        let mut by_function = BTreeMap::new();
        for (path, cost) in stacks {
            // `rsplit` always yields at least one piece, so an empty path pools under "" rather
            // than vanishing — `parse_folded` accepted it, and a cost with no name is a fact the
            // reader should see, not one to drop.
            let function = path.rsplit(';').next().unwrap_or(path);
            let total = by_function.entry(function.to_string()).or_insert(0u64);
            *total = total.saturating_add(*cost);
        }
        by_function
    }

    /// How each function's cost moved between two `.folded` artifacts, biggest move first (#190).
    ///
    /// Both sides are parsed with [`parse_folded`], so the input is the tool's own output format
    /// and a malformed file is reported with its line number rather than compared wrongly. A
    /// function on only one side is kept with `0` on the missing side — the same rule
    /// [`to_differential_folded`] applies — because "this function disappeared" is one of the
    /// answers an optimization report exists to give.
    ///
    /// Ranking is by the *size* of the change, not its direction: the two things worth reading
    /// first are a large regression and a large win, and sorting by signed delta would bury the
    /// regression list under the improvements. Ties break on the name, so two runs over the same
    /// pair of files print the same table.
    pub fn function_deltas(baseline: &str, current: &str) -> Result<Vec<FunctionDelta>, String> {
        // Which side broke is part of the error. `parse_folded` counts lines from the start of the
        // string it is handed, so a bare "line 3" from a two-file comparison names a line in an
        // unknown file, and the reader cannot even open the right one to fix it.
        let baseline =
            Self::parse_folded(baseline).map_err(|error| format!("baseline: {error}"))?;
        let current = Self::parse_folded(current).map_err(|error| format!("current: {error}"))?;
        let baseline = Self::exclusive_by_function(&baseline);
        let current = Self::exclusive_by_function(&current);

        let names: BTreeSet<&str> = baseline
            .keys()
            .map(String::as_str)
            .chain(current.keys().map(String::as_str))
            .collect();

        let mut deltas: Vec<FunctionDelta> = names
            .into_iter()
            .map(|name| FunctionDelta {
                name: name.to_string(),
                baseline: baseline.get(name).copied().unwrap_or(0),
                current: current.get(name).copied().unwrap_or(0),
            })
            .collect();

        deltas.sort_by(|a, b| {
            b.delta()
                .abs()
                .cmp(&a.delta().abs())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(deltas)
    }

    /// [`function_deltas`] as the table the terminal shows: one row per function whose cost moved.
    ///
    /// Both counts and the delta are printed rather than the delta alone, because a delta is only
    /// readable against the numbers that made it. Functions that did not move are left out — a
    /// comparison the user asked for because something changed should not spend its first screenful
    /// confirming that most things did not — but how many were left out is said, so an empty table
    /// cannot be mistaken for a broken one. `color` stays a parameter rather than a stream probe,
    /// exactly as in [`to_top_summary`](Self::to_top_summary): a redirected report is plain text and
    /// this function remains testable.
    ///
    /// [`function_deltas`]: OutputFormatter::function_deltas
    pub fn to_compare_report(deltas: &[FunctionDelta], color: bool) -> String {
        let paint = |code: &str, text: String| {
            if color {
                format!("\x1b[{code}m{text}\x1b[0m")
            } else {
                text
            }
        };

        let changed: Vec<&FunctionDelta> =
            deltas.iter().filter(|delta| delta.delta() != 0).collect();
        let total = FunctionDelta {
            name: String::from("total"),
            baseline: deltas.iter().map(|delta| delta.baseline).sum(),
            current: deltas.iter().map(|delta| delta.current).sum(),
        };

        let name_width = changed
            .iter()
            .map(|delta| delta.name.chars().count())
            .chain([total.name.chars().count()])
            .max()
            .unwrap_or(0);
        let cost_width = deltas
            .iter()
            .flat_map(|delta| [delta.baseline, delta.current])
            .map(|cost| cost.to_string().len())
            .max()
            .unwrap_or(0);
        // Each delta is rendered as text first, because "right-align and always show the sign" has
        // no single format spec: in `+>width$` the `+` is read as the *fill* character, not the sign.
        let signed = |delta: &FunctionDelta| format!("{:+}", delta.delta());
        let delta_width = deltas
            .iter()
            .map(|delta| signed(delta).chars().count())
            .max()
            .unwrap_or(0);

        let mut output = String::new();
        let _ = writeln!(
            output,
            "{}",
            paint(
                "1",
                String::from("Cost comparison, baseline → current (exclusive cost per function):")
            )
        );
        for delta in &changed {
            let code = match delta.scale() {
                DeltaScale::Regression => "31",
                DeltaScale::Improvement => "32",
                // Unreachable: `changed` holds only rows whose two counts differ.
                DeltaScale::Neutral => "",
            };
            let _ = writeln!(
                output,
                " {:<name_width$}  {:>cost_width$}  {:>cost_width$}  {:>delta_width$}",
                delta.name,
                delta.baseline,
                delta.current,
                paint(code, signed(delta)),
            );
        }
        let _ = writeln!(
            output,
            " {:<name_width$}  {:>cost_width$}  {:>cost_width$}  {:>delta_width$}",
            total.name,
            total.baseline,
            total.current,
            signed(&total),
        );

        let unchanged = deltas.len() - changed.len();
        if changed.is_empty() {
            let _ = write!(
                output,
                "no function's cost changed between the two profiles ({unchanged} compared)"
            );
        } else {
            let _ = write!(
                output,
                "{} of {} functions changed cost, {unchanged} unchanged",
                changed.len(),
                deltas.len()
            );
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CallStackNode, SourceFrame};

    /// Build a leaf node carrying the given exclusive cost.
    fn leaf(name: &str, exclusive_cpu: u64) -> CallStackNode {
        node(name, exclusive_cpu, Vec::new())
    }

    /// Build a node with the given children, keyed by their function name.
    fn node(name: &str, exclusive_cpu: u64, children: Vec<CallStackNode>) -> CallStackNode {
        CallStackNode {
            frame: SourceFrame {
                function_name: name.to_string(),
                file_path: None,
                line_number: None,
            },
            exclusive_cpu,
            inclusive_cpu: exclusive_cpu
                + children
                    .iter()
                    .map(|child| child.inclusive_cpu)
                    .sum::<u64>(),
            exclusive_mem: 0,
            inclusive_mem: 0,
            exclusive_hostcalls: 0,
            inclusive_hostcalls: 0,
            children: children
                .into_iter()
                .map(|child| (child.frame.function_name.clone(), child))
                .collect(),
        }
    }

    /// Sorted output lines, because `children` is a `HashMap` and emission order is not stable.
    fn lines(output: &str) -> Vec<&str> {
        let mut lines: Vec<&str> = output.lines().collect();
        lines.sort_unstable();
        lines
    }

    #[test]
    fn root_without_children_emits_one_line() {
        let output = OutputFormatter::to_collapsed_stack(&leaf("main", 42), &Metric::Cpu);

        assert_eq!(lines(&output), vec!["main 42"]);
    }

    #[test]
    fn path_is_semicolon_delimited_per_depth() {
        let tree = node("a", 1, vec![node("b", 2, vec![leaf("c", 3)])]);

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert_eq!(lines(&output), vec!["a 1", "a;b 2", "a;b;c 3"]);
    }

    #[test]
    fn every_node_emits_exactly_one_line() {
        let tree = node(
            "main",
            0,
            vec![leaf("alpha", 10), leaf("beta", 20), leaf("gamma", 30)],
        );

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert_eq!(output.lines().count(), 4);
        assert_eq!(
            lines(&output),
            vec!["main 0", "main;alpha 10", "main;beta 20", "main;gamma 30"]
        );
    }

    #[test]
    fn sibling_subtrees_do_not_contaminate_each_other() {
        // Guards the `truncate(original_len)` backtracking: without rewinding the path
        // after the "b" subtree, "c" would be emitted as "a;b;c".
        let tree = node(
            "a",
            1,
            vec![
                node("b", 2, vec![leaf("d", 4)]),
                node("c", 3, vec![leaf("e", 5)]),
            ],
        );

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert_eq!(
            lines(&output),
            vec!["a 1", "a;b 2", "a;b;d 4", "a;c 3", "a;c;e 5"]
        );
    }

    #[test]
    fn exclusive_cost_is_reported_not_inclusive() {
        let tree = node("main", 5, vec![leaf("callee", 95)]);

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert!(output.contains("main 5\n"), "got: {output:?}");
        assert!(!output.contains("main 100"));
    }

    #[test]
    fn zero_cost_frames_are_still_emitted() {
        // Folded stacks are merged by the consuming tool, so a zero-cost frame must stay
        // in the output to keep its call path intact.
        let tree = node("main", 0, vec![leaf("callee", 0)]);

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert_eq!(lines(&output), vec!["main 0", "main;callee 0"]);
    }

    #[test]
    fn output_ends_with_a_single_trailing_newline() {
        let tree = node("main", 1, vec![leaf("callee", 2)]);

        let output = OutputFormatter::to_collapsed_stack(&tree, &Metric::Cpu);

        assert!(output.ends_with('\n'));
        assert!(!output.ends_with("\n\n"));
    }

    #[test]
    fn parse_folded_sums_repeated_stacks_and_skips_blank_lines() {
        // A real artifact emits the same stack once per traversal, so counts must add up
        // rather than overwrite.
        let parsed = OutputFormatter::parse_folded("main 10\n\nmain 15\nmain;leaf 3\n").unwrap();

        assert_eq!(parsed.get("main"), Some(&25));
        assert_eq!(parsed.get("main;leaf"), Some(&3));
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn parse_folded_names_the_offending_line() {
        // Input comes from disk and may be truncated or hand-edited, so it is untrusted:
        // a bad line must produce an error naming its position, never a panic.
        let missing_cost = OutputFormatter::parse_folded("main 1\nmain\n").unwrap_err();
        assert!(missing_cost.contains("line 2"), "got: {missing_cost}");

        let negative_cost = OutputFormatter::parse_folded("main 1\nmain;-5\n").unwrap_err();
        assert!(negative_cost.contains("line 2"), "got: {negative_cost}");
    }

    #[test]
    fn identical_runs_diff_to_equal_counts() {
        let trace = "main 10\nmain;leaf 3\n";

        let output =
            OutputFormatter::to_differential_folded(trace, trace).expect("well-formed input");

        assert_eq!(output, "main 10 10\nmain;leaf 3 3\n");
        for line in output.lines() {
            let cost = line.split(' ').nth(1).unwrap().parse().unwrap();
            assert_eq!(
                OutputFormatter::delta_scale(cost, cost),
                DeltaScale::Neutral
            );
        }
    }

    #[test]
    fn stacks_missing_from_one_side_get_a_zero() {
        // Dropping a one-sided stack would hide exactly the two cases a regression hunt
        // cares about: a frame that appeared and a frame that disappeared.
        let output =
            OutputFormatter::to_differential_folded("main;gone 7\n", "main;new 4\n").unwrap();

        assert_eq!(output, "main;gone 7 0\nmain;new 0 4\n");
    }

    #[test]
    fn differential_output_is_sorted_by_stack() {
        // Byte-stability is load-bearing: if the diff changed only in line order, a real
        // comparison against a stored artifact would report noise as a regression.
        let baseline = "z 1\na 2\nm 3\n";
        let current = "z 9\na 9\nm 9\n";

        let output = OutputFormatter::to_differential_folded(baseline, current).unwrap();
        let paths: Vec<&str> = output
            .lines()
            .map(|line| line.split(' ').next().unwrap())
            .collect();

        assert_eq!(paths, vec!["a", "m", "z"]);
    }

    #[test]
    fn top_functions_ranks_and_truncates() {
        let tree = node(
            "main",
            10,
            vec![
                node("a", 50, vec![leaf("a_child", 200)]),
                node("b", 100, vec![leaf("b_child", 30)]),
            ],
        );

        // top 3 by CPU
        let top = OutputFormatter::top_functions(&tree, &Metric::Cpu, 3);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], ("a_child".to_string(), 200));
        assert_eq!(top[1], ("b".to_string(), 100));
        assert_eq!(top[2], ("a".to_string(), 50));
    }

    /// A function that runs under two different callers appears on two stacks, and the ranking
    /// must charge it once for its whole direct cost — otherwise the second appearance is a
    /// second row pushing a genuinely hot function off the list.
    #[test]
    fn top_functions_pool_one_name_across_the_whole_tree() {
        let tree = node(
            "main",
            1,
            vec![
                node("left", 5, vec![leaf("shared", 10)]),
                node("right", 2, vec![leaf("shared", 20)]),
            ],
        );

        let top = OutputFormatter::top_functions(&tree, &Metric::Cpu, 5);
        assert_eq!(top[0], ("shared".to_string(), 30));
        assert_eq!(
            top.iter().filter(|(name, _)| name == "shared").count(),
            1,
            "one function is one row: {top:?}"
        );
    }

    /// Equal costs break on the name. Without this the order comes out of a hash map, so the same
    /// contract profiled twice can print the same tie in either order and a reader cannot tell
    /// whether anything changed between runs.
    #[test]
    fn top_functions_breaks_ties_by_name() {
        let tree = node("main", 0, vec![leaf("zeta", 7), leaf("alpha", 7)]);
        let top = OutputFormatter::top_functions(&tree, &Metric::Cpu, 2);
        assert_eq!(top, vec![("alpha".to_string(), 7), ("zeta".to_string(), 7)]);
    }

    #[test]
    fn the_summary_is_a_readable_ranked_list() {
        let tree = node(
            "main",
            10,
            vec![
                node("a", 50, vec![leaf("a_child", 200)]),
                node("b", 100, vec![leaf("b_child", 30)]),
            ],
        );
        let top = OutputFormatter::top_functions(&tree, &Metric::Cpu, 3);
        let summary = OutputFormatter::to_top_summary(&top, &Metric::Cpu, false);

        assert_eq!(
            summary,
            "Top 3 functions by exclusive cost (cpu):\n  \
             1. a_child  200\n  2. b        100\n  3. a         50\n"
        );
    }

    /// Colour is opt-in, and the escapes wrap only the header and the costs: a summary piped into
    /// a file or a terminal that is not a terminal has to stay plain text.
    #[test]
    fn the_summary_colours_only_when_asked() {
        let top = vec![("hot".to_string(), 42u64)];
        let plain = OutputFormatter::to_top_summary(&top, &Metric::Memory, false);
        let colored = OutputFormatter::to_top_summary(&top, &Metric::Memory, true);

        assert!(!plain.contains('\x1b'), "{plain}");
        assert!(colored.contains("\x1b[1m"), "{colored}");
        assert!(
            plain.contains("exclusive cost (memory)"),
            "the metric names the numbers being ranked: {plain}"
        );
    }

    #[test]
    fn an_uncosted_run_says_so_rather_than_printing_an_empty_table() {
        let summary = OutputFormatter::to_top_summary(&[], &Metric::Hostcalls, false);
        assert_eq!(
            summary,
            "no function recorded any exclusive cost (hostcalls)"
        );
    }

    #[test]
    fn delta_scale_classifies_by_sign_of_the_difference() {
        assert_eq!(OutputFormatter::delta_scale(10, 11), DeltaScale::Regression);
        assert_eq!(OutputFormatter::delta_scale(10, 9), DeltaScale::Improvement);
        assert_eq!(OutputFormatter::delta_scale(10, 10), DeltaScale::Neutral);

        // Added and removed frames fall out of the same rule: 0 -> cost is a regression,
        // cost -> 0 is an improvement.
        assert_eq!(OutputFormatter::delta_scale(0, 5), DeltaScale::Regression);
        assert_eq!(OutputFormatter::delta_scale(5, 0), DeltaScale::Improvement);
        assert_eq!(OutputFormatter::delta_scale(0, 0), DeltaScale::Neutral);
    }

    #[test]
    fn the_differ_consumes_the_formatters_own_output() {
        // The two halves of the feature must compose: stage 4 writes collapsed stacks, and
        // the differ reads them back without a format translation step.
        let baseline = OutputFormatter::to_collapsed_stack(
            &node("main", 10, vec![leaf("compute_heavy_loop", 400)]),
            &Metric::Cpu,
        );
        let current = OutputFormatter::to_collapsed_stack(
            &node("main", 10, vec![leaf("compute_heavy_loop", 900)]),
            &Metric::Cpu,
        );

        let output = OutputFormatter::to_differential_folded(&baseline, &current).unwrap();
        let regression = output
            .lines()
            .find(|line| line.starts_with("main;compute_heavy_loop"))
            .expect("the child stack should survive the diff");

        assert_eq!(regression, "main;compute_heavy_loop 400 900");
        assert_eq!(
            OutputFormatter::delta_scale(400, 900),
            DeltaScale::Regression,
            "a 500-unit cost increase should read red"
        );
    }

    /// #190's comparison is only as honest as its pooling rule, so it is pinned on the file format
    /// rather than on a tree: the last frame of a stack is the function that was running, and every
    /// stack that ends in the same name adds to the same row.
    #[test]
    fn function_deltas_pool_each_stack_onto_the_function_that_ran() {
        let baseline = "caller_of_heavy 900\ncaller_of_heavy;memory_heavy_loop 100\n";
        let current = "caller_of_heavy 400\ncaller_of_heavy;memory_heavy_loop 350\n";

        let deltas = OutputFormatter::function_deltas(baseline, current).unwrap();
        let by_name: BTreeMap<&str, i128> = deltas
            .iter()
            .map(|delta| (delta.name.as_str(), delta.delta()))
            .collect();

        assert_eq!(
            by_name.get("caller_of_heavy"),
            Some(&-500),
            "the outer frame's own count moved from 900 to 400: {deltas:?}"
        );
        assert_eq!(
            by_name.get("memory_heavy_loop"),
            Some(&250),
            "the inner frame is charged its own 100 → 350, not the path above it"
        );
    }

    #[test]
    fn a_function_on_one_side_only_compares_against_zero() {
        // The rule `to_differential_folded` already uses, so a function that appeared or vanished is
        // a row in the report rather than an absence the reader has to go and diff by hand.
        let deltas = OutputFormatter::function_deltas("gone 700\n", "fresh 300\ngone 0\n").unwrap();
        let gone = deltas
            .iter()
            .find(|delta| delta.name == "gone")
            .expect("`gone` is on both sides");
        let fresh = deltas
            .iter()
            .find(|delta| delta.name == "fresh")
            .expect("`fresh` is new");

        assert_eq!((gone.baseline, gone.current, gone.delta()), (700, 0, -700));
        assert_eq!(
            (fresh.baseline, fresh.current, fresh.delta()),
            (0, 300, 300)
        );
        assert_eq!(fresh.scale(), DeltaScale::Regression);
        assert_eq!(gone.scale(), DeltaScale::Improvement);
    }

    #[test]
    fn the_biggest_move_is_first_and_ties_break_on_the_name() {
        let baseline = "small 10\nbig 10\nalpha 1000\nomega 1000\n";
        let current = "small 20\nbig 1010\nalpha 10\nomega 10\n";

        let names: Vec<String> = OutputFormatter::function_deltas(baseline, current)
            .unwrap()
            .into_iter()
            .map(|delta| delta.name)
            .collect();

        // 990 each for `alpha` and `omega`, tied on size and broken by name; `big`'s +1000 is the
        // largest move, so it leads; `small`'s +10 is last.
        assert_eq!(names, ["big", "alpha", "omega", "small"]);
    }

    #[test]
    fn a_malformed_profile_is_reported_before_any_comparison() {
        // Which file is broken has to be discoverable, and a delta computed from a half-read file
        // would be a wrong answer rather than a refused one.
        let error = OutputFormatter::function_deltas("stack 1\n", "not a folded line\n")
            .expect_err("the current file has no cost");
        assert!(error.contains("line 1"), "{error}");
        assert!(
            error.starts_with("current:"),
            "two files were read and the message must say which one: {error}"
        );
    }

    #[test]
    fn the_compare_report_shows_both_counts_and_the_move() {
        let deltas = OutputFormatter::function_deltas("a 100\nb 100\n", "a 40\nb 160\n").unwrap();
        let report = OutputFormatter::to_compare_report(&deltas, false);
        let lines: Vec<&str> = report.lines().collect();

        assert_eq!(
            lines.first().expect("a header"),
            &"Cost comparison, baseline → current (exclusive cost per function):"
        );
        // `b` moved +60 and `a` moved -60, tied on size, so `a` leads and `b` follows.
        assert!(
            lines[1].starts_with(" a  ") && lines[1].ends_with("-60"),
            "expected `a`'s row first: {:?}",
            lines[1]
        );
        assert!(
            lines[2].starts_with(" b  ") && lines[2].ends_with("+60"),
            "expected `b`'s row second: {:?}",
            lines[2]
        );
        // Both counts are on the line, because a delta alone cannot say how large a change it is.
        assert!(
            lines[1].contains("100") && lines[1].contains("40"),
            "{}",
            lines[1]
        );
        assert!(
            lines[3].starts_with(" total") && lines[3].contains("200") && lines[3].ends_with("+0"),
            "the totals row: {:?}",
            lines[3]
        );
        assert_eq!(
            lines.last().expect("the tally"),
            &"2 of 2 functions changed cost, 0 unchanged"
        );
    }

    #[test]
    fn the_compare_report_leaves_unchanged_functions_out_and_says_how_many() {
        let deltas = OutputFormatter::function_deltas(
            "moved 100\nstill 50\nalso 25\n",
            "moved 10\nstill 50\nalso 25\n",
        )
        .unwrap();
        let report = OutputFormatter::to_compare_report(&deltas, false);
        let lines: Vec<&str> = report.lines().collect();

        assert_eq!(
            lines.len(),
            4,
            "header, one row, totals, tally — the two unchanged functions are not rows: {report:?}"
        );
        assert!(lines[1].starts_with(" moved"), "{}", lines[1]);
        assert!(
            lines.last().unwrap().contains("2 unchanged"),
            "the omitted count is stated: {:?}",
            lines.last()
        );
    }

    #[test]
    fn two_identical_profiles_say_so_instead_of_printing_a_bare_header() {
        let profile = "a 100\nb 50\n";
        let deltas = OutputFormatter::function_deltas(profile, profile).unwrap();
        let report = OutputFormatter::to_compare_report(&deltas, false);

        assert!(report.contains("no function's cost changed"), "{report:?}");
        assert!(report.contains("2 compared"), "{report:?}");
        // The totals row still prints: "nothing moved per function" and "the two runs add up
        // differently" cannot both be true, and the reader checking that wants the number.
        assert!(report.contains(" total"), "{report:?}");
    }

    #[test]
    fn the_compare_report_colours_moves_only_when_asked() {
        let deltas = OutputFormatter::function_deltas("a 100\nb 100\n", "a 40\nb 160\n").unwrap();
        let plain = OutputFormatter::to_compare_report(&deltas, false);
        let painted = OutputFormatter::to_compare_report(&deltas, true);

        assert!(!plain.contains('\x1b'), "{plain:?}");
        // Red for the regression, green for the improvement, each on its own row.
        assert!(painted.contains("\x1b[31m+60\x1b[0m"), "{painted:?}");
        assert!(painted.contains("\x1b[32m-60\x1b[0m"), "{painted:?}");
        assert_eq!(
            painted.lines().count(),
            plain.lines().count(),
            "color must not change how many lines the report is"
        );
    }

    #[test]
    fn the_compare_report_consumes_the_formatters_own_output() {
        // Same composition check `to_differential_folded` has: what stage 4 writes is what
        // `compare` reads, with no format translation in between.
        let baseline = OutputFormatter::to_collapsed_stack(
            &node("main", 10, vec![leaf("compute_heavy_loop", 400)]),
            &Metric::Cpu,
        );
        let current = OutputFormatter::to_collapsed_stack(
            &node("main", 10, vec![leaf("compute_heavy_loop", 900)]),
            &Metric::Cpu,
        );

        let deltas = OutputFormatter::function_deltas(&baseline, &current).unwrap();
        let moved = deltas
            .iter()
            .find(|delta| delta.name == "compute_heavy_loop")
            .expect("the fixture's hot loop should be a row");
        assert_eq!(moved.delta(), 500);
        assert!(
            OutputFormatter::to_compare_report(&deltas, false).contains("1 of 2 functions changed"),
            "the unchanged `main` frame is tallied, not printed"
        );
    }
}
