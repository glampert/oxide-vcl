//! Independent searches of one compile run at once, each on its own
//! thread. Their results are folded in the items' order on the caller's
//! thread, so the fold sees what running them one after another gives, and
//! it may stop early: the items after that point are cancelled, as the
//! sequential search would not have run them.
//!
//! Each item's work ([`crate::stats`]) is counted on its own and handed to
//! the fold, which adds what the sequential search would have done, so the
//! counts do not depend on how the threads ran. With a `VCL_*` debug switch
//! set, or one core, the items run one after another on the caller's
//! thread, so that traces come out in order.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use crate::stats::{self, Stats};

/// How many threads a search may use: one with a debug switch set.
pub(crate) fn threads() -> usize {
    #[cfg(test)]
    if SEQUENTIAL.with(std::cell::Cell::get) {
        return 1;
    }
    if crate::debug::switches().any() {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, usize::from)
}

#[cfg(test)]
thread_local! {
    /// Set by [`tests::sequentially`]: searches run as with a debug switch.
    static SEQUENTIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the fold no longer needs an item: a search polls it, and gives
/// up when it is set.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stop {
    /// The first item no longer needed; none when this is not a parallel
    /// search.
    from: Option<Arc<AtomicUsize>>,
    index: usize,
}

impl Stop {
    pub(crate) fn requested(&self) -> bool {
        self.from
            .as_ref()
            .is_some_and(|from| self.index >= from.load(Ordering::Relaxed))
    }
}

/// `work` on every item, folded in order: `fold` takes each result with
/// the work it counted, and returns false when no later item is needed.
/// The work is `None` when the item ran on the caller's thread and its
/// work is counted already.
pub(crate) fn search<T: Sync, R: Send>(
    items: &[T],
    work: impl Fn(&T, &Stop) -> R + Sync,
    mut fold: impl FnMut(R, Option<Stats>) -> bool,
) {
    let threads = threads().min(items.len());
    if threads <= 1 {
        for item in items {
            if !fold(work(item, &Stop::default()), None) {
                return;
            }
        }
        return;
    }
    let phase = stats::current_phase();
    let next = AtomicUsize::new(0);
    let from = Arc::new(AtomicUsize::new(usize::MAX));
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let sender = sender.clone();
            let (next, from, work) = (&next, &from, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= items.len() || index >= from.load(Ordering::Relaxed) {
                        break;
                    }
                    let stop = Stop {
                        from: Some(Arc::clone(from)),
                        index,
                    };
                    let (result, counted) = stats::measure(|| {
                        let _phase = stats::enter(phase);
                        work(&items[index], &stop)
                    });
                    if sender.send((index, result, counted)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let mut arrived: Vec<Option<(R, Stats)>> = (0..items.len()).map(|_| None).collect();
        let mut folded = 0;
        while folded < items.len() {
            // A worker that panicked leaves its item missing: the scope
            // reports the panic when it ends.
            let Ok((index, result, counted)) = receiver.recv() else {
                break;
            };
            arrived[index] = Some((result, counted));
            while let Some((result, counted)) = arrived.get_mut(folded).and_then(Option::take) {
                folded += 1;
                if !fold(result, Some(counted)) {
                    from.store(folded, Ordering::Relaxed);
                    return;
                }
            }
        }
    });
}

/// `work` on every item; the results in the items' order, with every
/// item's work counted.
pub(crate) fn map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let mut results = Vec::with_capacity(items.len());
    search(
        items,
        |item, _| work(item),
        |result, counted| {
            if let Some(counted) = counted {
                stats::absorb(&counted);
            }
            results.push(result);
            true
        },
    );
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Counter, Phase};

    /// `work` with every search on this thread, one item after another.
    fn sequentially<T>(work: impl FnOnce() -> T) -> T {
        SEQUENTIAL.with(|sequential| sequential.set(true));
        let result = work();
        SEQUENTIAL.with(|sequential| sequential.set(false));
        result
    }

    /// A public corpus program compiled as its recorded run was, in `mode`.
    fn corpus_compile(case: &str, mode: crate::Mode) -> crate::Compiled {
        let directory = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("tests/corpus")
            .join(case);
        let input = std::fs::read_to_string(directory.join("input.vcl")).unwrap();
        let end = "; -- end of corpus header --";
        let source = input
            .find(end)
            .map_or(&input[..], |at| &input[at + end.len()..]);
        let options = crate::Options {
            mode,
            unique: Some("CORPUS".into()),
            ..crate::Options::default()
        };
        crate::compile(
            source.trim_start_matches('\n'),
            &format!("{case}.vcl"),
            &options,
        )
        .unwrap_or_else(|errors| panic!("{case}: {errors:?}"))
    }

    #[test]
    fn parallel_searches_compile_and_count_as_sequential_ones() {
        // Loop searches under a work budget whose strategies stop early
        // (sprite, in oxide mode), linear tables whose first strategy misses
        // the bound (world_strips), and hinted loop searches (fast).
        let cases = [
            ("openvcl__examples_sprite_vu1Sprite", crate::Mode::Oxide),
            (
                "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips",
                crate::Mode::Oxide,
            ),
            (
                "doom3-bfg-ps2__src_ps2_renderer_vu1progs_world_strips",
                crate::Mode::Vcl,
            ),
            ("openvcl__test_fixtures_fast", crate::Mode::Vcl),
        ];
        for (case, mode) in cases {
            let parallel = corpus_compile(case, mode);
            let sequential = sequentially(|| corpus_compile(case, mode));
            assert_eq!(parallel.assembly, sequential.assembly, "{case} {mode:?}");
            assert_eq!(parallel.stats, sequential.stats, "{case} {mode:?}");
        }
    }

    #[test]
    fn results_keep_their_order_and_work_is_counted_to_the_caller() {
        let items: Vec<u64> = (0..40).collect();
        let (results, counted) = stats::measure(|| {
            let _phase = stats::enter(Phase::Loops);
            map(&items, |&item| {
                stats::count(Counter::WrangleSteps, item);
                item * 2
            })
        });
        assert_eq!(
            results,
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
        let total: u64 = items.iter().sum();
        assert_eq!(counted.phases[Phase::Loops as usize].wrangle_steps, total);
        assert_eq!(counted.total.wrangle_steps, total);
    }

    #[test]
    fn a_search_folds_in_order_and_counts_only_what_it_folded() {
        let items: Vec<u64> = (0..40).collect();
        let mut folded = Vec::new();
        let ((), counted) = stats::measure(|| {
            search(
                &items,
                |&item, stop| {
                    // Later items wait to be stopped, as a long search would.
                    while item > 10 && !stop.requested() && item < 12 {
                        std::thread::yield_now();
                    }
                    stats::count(Counter::WrangleChecks, 1);
                    item
                },
                |item, work| {
                    if let Some(work) = work {
                        stats::absorb(&work);
                    }
                    folded.push(item);
                    item < 5
                },
            )
        });
        assert_eq!(folded, [0, 1, 2, 3, 4, 5]);
        assert_eq!(counted.total.wrangle_checks, 6);
    }
}
