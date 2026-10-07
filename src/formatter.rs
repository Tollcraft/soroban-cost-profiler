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
}
