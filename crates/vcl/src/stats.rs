//! Counts of the compiler's work in one [`compile`](crate::compile): search
//! nodes, wrangler steps and checks, flow analyses. They depend only on the
//! program and the options, so unlike time they measure a compile-time
//! change exactly, and a change that alters them changed what the searches
//! do, not only how fast they run (`docs/TESTING.md`, "Benchmarks").
//!
//! A compile runs on one thread, so the counters are the thread's: `measure`
//! zeroes them at the outermost compile and returns what it counted.

use std::cell::Cell;

/// What one compile did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    /// Placements the list scheduler's search tried (`scheduler::schedule`).
    pub search_nodes: u64,
    /// The wrangler's steps, a placement or a backtrack each.
    pub wrangle_steps: u64,
    /// The wrangler's dependency and resource checks.
    pub wrangle_checks: u64,
    /// Whole-graph data-flow analyses (`analysis::flow::Graph::analyze`).
    pub flow_analyses: u64,
    /// Blocks those analyses walked to their fixed points.
    pub flow_walks: u64,
}

impl std::ops::Add for Stats {
    type Output = Stats;

    /// The work of both.
    fn add(self, other: Stats) -> Stats {
        Stats {
            search_nodes: self.search_nodes + other.search_nodes,
            wrangle_steps: self.wrangle_steps + other.wrangle_steps,
            wrangle_checks: self.wrangle_checks + other.wrangle_checks,
            flow_analyses: self.flow_analyses + other.flow_analyses,
            flow_walks: self.flow_walks + other.flow_walks,
        }
    }
}

/// The counter a piece of work adds to.
#[derive(Clone, Copy)]
pub(crate) enum Counter {
    SearchNodes,
    WrangleSteps,
    WrangleChecks,
    FlowAnalyses,
    FlowWalks,
}

thread_local! {
    static COUNTS: [Cell<u64>; 5] = const { [const { Cell::new(0) }; 5] };
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Adds `amount` to `counter`.
pub(crate) fn count(counter: Counter, amount: u64) {
    COUNTS.with(|counts| {
        let cell = &counts[counter as usize];
        cell.set(cell.get() + amount);
    });
}

/// Runs `work`, and what it counted when this is the outermost measure:
/// what every compile in it did, when a caller measures several.
pub fn measure<T>(work: impl FnOnce() -> T) -> (T, Stats) {
    let outermost = DEPTH.with(|depth| {
        depth.set(depth.get() + 1);
        depth.get() == 1
    });
    if outermost {
        COUNTS.with(|counts| counts.iter().for_each(|cell| cell.set(0)));
    }
    let result = work();
    DEPTH.with(|depth| depth.set(depth.get() - 1));
    let stats = COUNTS.with(|counts| Stats {
        search_nodes: counts[Counter::SearchNodes as usize].get(),
        wrangle_steps: counts[Counter::WrangleSteps as usize].get(),
        wrangle_checks: counts[Counter::WrangleChecks as usize].get(),
        flow_analyses: counts[Counter::FlowAnalyses as usize].get(),
        flow_walks: counts[Counter::FlowWalks as usize].get(),
    });
    (result, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compile_counts_its_own_work_the_same_each_time() {
        let source = ".init_vf_all\n\
                      .init_vi_all\n\
                      --enter\n\
                      in_vi count(vi01)\n\
                      in_vi data(vi02)\n\
                      --endenter\n\
                      loop:\n\
                      lq.xyzw value, 0(data)\n\
                      add.xyzw value, value, value\n\
                      sq.xyzw value, 0(data)\n\
                      iaddi data, data, 1\n\
                      iaddi count, count, -1\n\
                      ibne count, vi00, loop\n\
                      --exit\n\
                      --endexit\n";
        let options = crate::Options::default();
        let first = crate::compile(source, "count.vcl", &options).unwrap().stats;
        let second = crate::compile(source, "count.vcl", &options).unwrap().stats;
        assert_eq!(first, second);
        assert!(first.wrangle_steps > 0 && first.wrangle_checks > 0);
        assert!(first.flow_analyses > 0 && first.flow_walks >= first.flow_analyses);
        // A measure around several compiles counts them all.
        let ((), both) = measure(|| {
            crate::compile(source, "count.vcl", &options).unwrap();
            crate::compile(source, "count.vcl", &options).unwrap();
        });
        assert_eq!(both, first + first);
    }
}
