//! Developer switches: the `VCL_*` environment variables that turn on
//! traces (printed with [`trace!`](crate::trace)) or older code paths. They
//! are read once, the first time any is needed, and cached in [`Switches`].
//!
//! ```text
//! VCL_WRANGLE_TRACE=1 oxide-vcl prog.vcl     # each wrangle strategy's result
//! VCL_INPUT_TRACE=12 oxide-vcl prog.vcl      # input locks of instruction 12
//! ```

use std::sync::OnceLock;

/// Every switch: `true` when its variable is set (to anything), or the
/// number it is set to.
#[derive(Clone, Debug, Default)]
pub struct Switches {
    /// `VCL_ALIAS_TRACE`: memory-order decisions between a store and earlier accesses.
    pub alias_trace: bool,
    /// `VCL_ALLOC_COSTS`: the local allocator's per-register costs.
    pub alloc_costs: bool,
    /// `VCL_ALLOC_TRACE`: register allocation: global virtual registers and wanted rows.
    pub alloc_trace: bool,
    /// `VCL_CAND_TRACE`: loop placement candidates.
    pub cand_trace: bool,
    /// `VCL_COALESCE_TRACE`: coalescing of source copies.
    pub coalesce_trace: bool,
    /// `VCL_DRAG_TRACE`: the wrangler's drag passes.
    pub drag_trace: bool,
    /// `VCL_EXTENT_TRACE`: the extent each placement leaves.
    pub extent_trace: bool,
    /// `VCL_FORCED_TRACE`: the forced predecessors of the instruction with this index.
    pub forced_trace: Option<usize>,
    /// `VCL_HAZARD`: non-interlocked hazards found by the table timing.
    pub hazard: bool,
    /// `VCL_INPUT_TRACE`: input-lock checks of the instruction with this index.
    pub input_trace: Option<usize>,
    /// `VCL_LINE_TRACE`: line checks of the instruction with this index.
    pub line_trace: Option<usize>,
    /// `VCL_LPGEN_BODY`: the loop optimizer's rewritten loop body.
    pub lpgen_body: bool,
    /// `VCL_LPGEN_BUDGET`: the loop optimizer's step budget per strategy (default 20000).
    pub lpgen_budget: Option<usize>,
    /// `VCL_LPGEN_STEPS`: each insertion and undo of the loop optimizer.
    pub lpgen_steps: bool,
    /// `VCL_LPGEN_TRACE`: the loop optimizer's results and failures.
    pub lpgen_trace: bool,
    /// `VCL_MOVE_TRACE`: generated moves and the raw reduction.
    pub move_trace: bool,
    /// `VCL_OLD_LOOPS`: use the older loop scheduler instead of the loop optimizer.
    pub old_loops: bool,
    /// `VCL_OLD_SCHEDULER`: use the older table scheduler instead of the wrangler.
    pub old_scheduler: bool,
    /// `VCL_PLACE_TRACE`: each placement step of the wrangler.
    pub place_trace: bool,
    /// `VCL_RAW_MOVES`: moves in the raw-table reduction.
    pub raw_moves: bool,
    /// `VCL_RETRY_TRACE`: retries after an allocation failure.
    pub retry_trace: bool,
    /// `VCL_TIMING_DEBUG`: table planning, selection, stalls and replays.
    pub timing_debug: bool,
    /// `VCL_WEB_DEBUG`: register webs with a missing read.
    pub web_debug: bool,
    /// `VCL_WRANGLE_CANDIDATES`: the wrangler's ranked candidates.
    pub wrangle_candidates: bool,
    /// `VCL_WRANGLE_PROBLEM`: each wrangle problem and its pull-up steps.
    pub wrangle_problem: bool,
    /// `VCL_WRANGLE_TRACE`: each wrangle strategy's result.
    pub wrangle_trace: bool,
}

/// The switches, read from the environment on first use.
pub fn switches() -> &'static Switches {
    static SWITCHES: OnceLock<Switches> = OnceLock::new();
    SWITCHES.get_or_init(read)
}

fn read() -> Switches {
    let set = |name: &str| std::env::var_os(name).is_some();
    let number = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
    };
    Switches {
        alias_trace: set("VCL_ALIAS_TRACE"),
        alloc_costs: set("VCL_ALLOC_COSTS"),
        alloc_trace: set("VCL_ALLOC_TRACE"),
        cand_trace: set("VCL_CAND_TRACE"),
        coalesce_trace: set("VCL_COALESCE_TRACE"),
        drag_trace: set("VCL_DRAG_TRACE"),
        extent_trace: set("VCL_EXTENT_TRACE"),
        forced_trace: number("VCL_FORCED_TRACE"),
        hazard: set("VCL_HAZARD"),
        input_trace: number("VCL_INPUT_TRACE"),
        line_trace: number("VCL_LINE_TRACE"),
        lpgen_body: set("VCL_LPGEN_BODY"),
        lpgen_budget: number("VCL_LPGEN_BUDGET"),
        lpgen_steps: set("VCL_LPGEN_STEPS"),
        lpgen_trace: set("VCL_LPGEN_TRACE"),
        move_trace: set("VCL_MOVE_TRACE"),
        old_loops: set("VCL_OLD_LOOPS"),
        old_scheduler: set("VCL_OLD_SCHEDULER"),
        place_trace: set("VCL_PLACE_TRACE"),
        raw_moves: set("VCL_RAW_MOVES"),
        retry_trace: set("VCL_RETRY_TRACE"),
        timing_debug: set("VCL_TIMING_DEBUG"),
        web_debug: set("VCL_WEB_DEBUG"),
        wrangle_candidates: set("VCL_WRANGLE_CANDIDATES"),
        wrangle_problem: set("VCL_WRANGLE_PROBLEM"),
        wrangle_trace: set("VCL_WRANGLE_TRACE"),
    }
}

/// [`trace!`](crate::trace) when the named switch is on:
/// `trace_if!(wrangle_trace, "strategy {strategy:?}")`.
#[macro_export]
macro_rules! trace_if {
    ($switch:ident, $($argument:tt)*) => {
        if $crate::debug::switches().$switch {
            $crate::trace!($($argument)*);
        }
    };
}
