//! Conservative inter-stream issue distance.
//!
//! The original sums intermediate stream lengths from a path
//! key, excluding both endpoints. This adapter takes the minimum over CFG
//! paths using caller-proven lower bounds. Unknown stream duration is zero;
//! physical address distance through alternative paths is never substituted.

use std::collections::BTreeSet;

/// Minimum sum of intermediate block lengths. `from` and `to` are excluded.
/// Nonnegative weights make cycles harmless. Invalid graph indices return
/// None, as does an unreachable target; zero-length unknown blocks are valid.
pub fn minimum_path_length(
    successors: &[Vec<usize>],
    lengths: &[usize],
    from: usize,
    to: usize,
) -> Option<usize> {
    if successors.len() != lengths.len()
        || from >= lengths.len()
        || to >= lengths.len()
        || successors.iter().flatten().any(|&id| id >= lengths.len())
    {
        return None;
    }
    let mut distance = vec![usize::MAX; lengths.len()];
    distance[from] = 0;
    let mut pending = BTreeSet::from([(0, from)]);
    while let Some((cost, block)) = pending.pop_first() {
        if block == to {
            return Some(cost);
        }
        for &next in &successors[block] {
            let weight = if next == to { 0 } else { lengths[next] };
            let Some(candidate) = cost.checked_add(weight) else {
                continue;
            };
            if candidate < distance[next] {
                pending.remove(&(distance[next], next));
                distance[next] = candidate;
                pending.insert((candidate, next));
            }
        }
    }
    None
}

// ----------------------------------------------
// Unit tests
// ----------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_producer_and_consumer_extents() {
        assert_eq!(
            minimum_path_length(&[vec![1], vec![2], vec![]], &[100, 3, 200], 0, 2),
            Some(3)
        );
        assert_eq!(
            minimum_path_length(&[vec![1], vec![]], &[100, 200], 0, 1),
            Some(0)
        );
    }

    #[test]
    fn shortest_branch_path_controls_readiness_at_a_join() {
        let graph = [vec![1, 2], vec![3], vec![3], vec![]];
        assert_eq!(minimum_path_length(&graph, &[9, 7, 2, 8], 0, 3), Some(2));
        assert_eq!(minimum_path_length(&graph, &[9, 7, 0, 8], 0, 3), Some(0));
    }

    #[test]
    fn cycles_and_unreachable_paths_do_not_invent_distance() {
        let graph = [vec![1], vec![0, 2], vec![1], vec![]];
        assert_eq!(minimum_path_length(&graph, &[1, 3, 2, 7], 0, 2), Some(3));
        assert_eq!(minimum_path_length(&graph, &[1, 3, 2, 7], 0, 3), None);
        assert_eq!(minimum_path_length(&graph, &[1, 3, 2, 7], 0, 0), Some(0));
    }

    #[test]
    fn checks_invalid_indices_and_overflow() {
        assert_eq!(minimum_path_length(&[vec![2], vec![]], &[1, 2], 0, 1), None);
        assert_eq!(minimum_path_length(&[vec![]], &[], 0, 0), None);
        assert_eq!(minimum_path_length(&[vec![]], &[1], 1, 0), None);
        let graph = [vec![1], vec![2], vec![3], vec![]];
        assert_eq!(
            minimum_path_length(&graph, &[0, usize::MAX - 1, 2, 0], 0, 3),
            None
        );
    }
}
