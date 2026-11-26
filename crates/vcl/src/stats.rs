//! Counts of the compiler's work in one [`compile`](crate::compile): search
//! nodes, wrangler steps and checks, flow analyses, in total and by the
//! [`Phase`] that did them, and how many times the program and its regions
//! were compiled. They depend only on the program and the options, so
//! unlike time they measure a compile-time change exactly, and a change
//! that alters them changed what the searches do, not only how fast they
//! run (`docs/TESTING.md`, "Benchmarks").
//!
//! The counters are the thread's: `measure` zeroes them at the outermost
//! compile and returns what it counted, and [`enter`] sets the phase later
//! work counts to. The searches a compile runs on other threads
//! (`crate::parallel`) measure their own work, and the compile's thread
//! [`absorb`]s what the sequential search would have done.

use std::cell::Cell;

/// The work of one compile, or of one phase of it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Work {
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

impl Work {
    /// Whether nothing was counted.
    pub fn is_empty(&self) -> bool {
        *self == Work::default()
    }
}

impl std::ops::Add for Work {
    type Output = Work;

    /// The work of both.
    fn add(self, other: Work) -> Work {
        Work {
            search_nodes: self.search_nodes + other.search_nodes,
            wrangle_steps: self.wrangle_steps + other.wrangle_steps,
            wrangle_checks: self.wrangle_checks + other.wrangle_checks,
            flow_analyses: self.flow_analyses + other.flow_analyses,
            flow_walks: self.flow_walks + other.flow_walks,
        }
    }
}

/// The part of a compile that work is counted to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Phase {
    /// Everything outside a region's compile: parsing, the source checks,
    /// and the rewrites that compile the program again (`unswitch`, loop
    /// entry waits, the fixes' rounds, spills). The compiles a rewrite
    /// starts count to the other phases, and to [`Stats::programs`].
    #[default]
    Source,
    /// Expanding software-pipelined loops: the modulo search for each
    /// loop's period.
    Loops,
    /// The rest of a region's preparation: pruning, copies, the flow
    /// analyses and webs.
    Prepare,
    /// Scheduling each block's table: list scheduling and wrangling.
    Schedule,
    /// Register assignment and allocation.
    Allocate,
    /// The table work after allocation: raw reduction, merged streams and
    /// their wrangling again, scratch choices and emission.
    Tables,
}

impl Phase {
    pub const COUNT: usize = 6;
    pub const ALL: [Phase; Phase::COUNT] = [
        Phase::Source,
        Phase::Loops,
        Phase::Prepare,
        Phase::Schedule,
        Phase::Allocate,
        Phase::Tables,
    ];

    /// The phase's name in the benchmark tables.
    pub fn name(self) -> &'static str {
        match self {
            Phase::Source => "source",
            Phase::Loops => "loops",
            Phase::Prepare => "prepare",
            Phase::Schedule => "schedule",
            Phase::Allocate => "allocate",
            Phase::Tables => "tables",
        }
    }
}

/// What one compile did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Stats {
    /// All of its work.
    pub total: Work,
    /// Its work by phase, indexed by [`Phase`]; together, `total`.
    pub phases: [Work; Phase::COUNT],
    /// Compiles of the whole program: one, and one more for each rewrite
    /// that compiles it again.
    pub programs: u64,
    /// Region compiles: one per region of each program compile, and one
    /// more for each retry (a scheduling strategy after an allocation
    /// failure, a loop round without its hints).
    pub regions: u64,
}

impl std::ops::Add for Stats {
    type Output = Stats;

    /// The work of both.
    fn add(self, other: Stats) -> Stats {
        let mut phases = self.phases;
        for (phase, other) in phases.iter_mut().zip(other.phases) {
            *phase = *phase + other;
        }
        Stats {
            total: self.total + other.total,
            phases,
            programs: self.programs + other.programs,
            regions: self.regions + other.regions,
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
    Programs,
    Regions,
}

const COUNTERS: usize = 7;

/// A thread's counters: one thread-local, so that a count, which the
/// wrangler makes for every position check, looks up one.
struct Counters {
    counts: [[Cell<u64>; COUNTERS]; Phase::COUNT],
    phase: Cell<Phase>,
    depth: Cell<usize>,
}

thread_local! {
    static COUNTERS_OF_THREAD: Counters = const {
        Counters {
            counts: [const { [const { Cell::new(0) }; COUNTERS] }; Phase::COUNT],
            phase: Cell::new(Phase::Source),
            depth: Cell::new(0),
        }
    };
}

/// Adds `amount` to `counter`, in the current phase.
pub(crate) fn count(counter: Counter, amount: u64) {
    COUNTERS_OF_THREAD.with(|counters| {
        let cell = &counters.counts[counters.phase.get() as usize][counter as usize];
        cell.set(cell.get() + amount);
    });
}

/// Work is counted to `phase` until the guard drops, then to the phase
/// before.
pub(crate) fn enter(phase: Phase) -> PhaseGuard {
    PhaseGuard {
        before: COUNTERS_OF_THREAD.with(|counters| counters.phase.replace(phase)),
    }
}

/// Runs `work` with its work counted to `phase` ([`enter`]).
pub(crate) fn in_phase<T>(phase: Phase, work: impl FnOnce() -> T) -> T {
    let _phase = enter(phase);
    work()
}

/// The phase [`enter`] set, until it drops.
pub(crate) struct PhaseGuard {
    before: Phase,
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        COUNTERS_OF_THREAD.with(|counters| counters.phase.set(self.before));
    }
}

/// The phase work is counted to now.
pub(crate) fn current_phase() -> Phase {
    COUNTERS_OF_THREAD.with(|counters| counters.phase.get())
}

/// Adds `stats`, the work another thread counted, to this thread's counts.
pub(crate) fn absorb(stats: &Stats) {
    COUNTERS_OF_THREAD.with(|counters| {
        let add = |phase: usize, counter: Counter, amount: u64| {
            let cell = &counters.counts[phase][counter as usize];
            cell.set(cell.get() + amount);
        };
        for (phase, work) in stats.phases.iter().enumerate() {
            add(phase, Counter::SearchNodes, work.search_nodes);
            add(phase, Counter::WrangleSteps, work.wrangle_steps);
            add(phase, Counter::WrangleChecks, work.wrangle_checks);
            add(phase, Counter::FlowAnalyses, work.flow_analyses);
            add(phase, Counter::FlowWalks, work.flow_walks);
        }
        add(Phase::Source as usize, Counter::Programs, stats.programs);
        add(Phase::Source as usize, Counter::Regions, stats.regions);
    });
}

/// Runs `work`, and what it counted when this is the outermost measure:
/// what every compile in it did, when a caller measures several.
pub fn measure<T>(work: impl FnOnce() -> T) -> (T, Stats) {
    COUNTERS_OF_THREAD.with(|counters| {
        counters.depth.set(counters.depth.get() + 1);
        if counters.depth.get() == 1 {
            counters
                .counts
                .iter()
                .flatten()
                .for_each(|cell| cell.set(0));
            counters.phase.set(Phase::Source);
        }
    });
    let result = work();
    let mut stats = Stats::default();
    COUNTERS_OF_THREAD.with(|counters| {
        counters.depth.set(counters.depth.get() - 1);
        for (phase, counts) in stats.phases.iter_mut().zip(&counters.counts) {
            let get = |counter: Counter| counts[counter as usize].get();
            *phase = Work {
                search_nodes: get(Counter::SearchNodes),
                wrangle_steps: get(Counter::WrangleSteps),
                wrangle_checks: get(Counter::WrangleChecks),
                flow_analyses: get(Counter::FlowAnalyses),
                flow_walks: get(Counter::FlowWalks),
            };
            stats.programs += get(Counter::Programs);
            stats.regions += get(Counter::Regions);
        }
    });
    stats.total = stats
        .phases
        .iter()
        .fold(Work::default(), |total, &phase| total + phase);
    (result, stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOP: &str = ".init_vf_all\n\
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

    #[test]
    fn a_compile_counts_its_own_work_the_same_each_time() {
        let options = crate::Options::default();
        let first = crate::compile(LOOP, "count.vcl", &options).unwrap().stats;
        let second = crate::compile(LOOP, "count.vcl", &options).unwrap().stats;
        assert_eq!(first, second);
        let total = first.total;
        assert!(total.wrangle_steps > 0 && total.wrangle_checks > 0);
        assert!(total.flow_analyses > 0 && total.flow_walks >= total.flow_analyses);
        assert_eq!((first.programs, first.regions), (1, 1));
        // A measure around several compiles counts them all.
        let ((), both) = measure(|| {
            crate::compile(LOOP, "count.vcl", &options).unwrap();
            crate::compile(LOOP, "count.vcl", &options).unwrap();
        });
        assert_eq!(both, first + first);
    }

    #[test]
    fn work_is_counted_to_its_phase() {
        let stats = crate::compile(LOOP, "count.vcl", &crate::Options::default())
            .unwrap()
            .stats;
        let phase = |phase: Phase| stats.phases[phase as usize];
        assert_eq!(
            stats.phases.iter().fold(Work::default(), |a, &b| a + b),
            stats.total
        );
        // Blocks are wrangled when they are scheduled, and analysed while
        // the region is prepared.
        assert!(phase(Phase::Schedule).wrangle_steps > 0);
        assert!(phase(Phase::Prepare).flow_analyses > 0);
        assert!(phase(Phase::Source).wrangle_steps == 0);
    }

    #[test]
    fn a_phase_ends_where_its_work_does() {
        let ((), stats) = measure(|| {
            in_phase(Phase::Schedule, || {
                count(Counter::WrangleSteps, 2);
                in_phase(Phase::Loops, || count(Counter::SearchNodes, 3));
                count(Counter::WrangleSteps, 1);
            });
            count(Counter::FlowAnalyses, 1);
        });
        assert_eq!(stats.phases[Phase::Schedule as usize].wrangle_steps, 3);
        assert_eq!(stats.phases[Phase::Loops as usize].search_nodes, 3);
        assert_eq!(stats.phases[Phase::Source as usize].flow_analyses, 1);
        assert_eq!(stats.total.wrangle_steps, 3);
    }
}
