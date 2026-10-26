//! Deterministic, bounded lattice navigation supplied with game-specific traversal rules.
//!
//! Nodes describe integer foot positions. The caller validates support, body clearance,
//! unloaded terrain, and permitted steps in `can_step`. A result is advisory: authority
//! must still resolve collision when applying movement. No route cache is retained here.

use std::collections::{BTreeMap, VecDeque};
use torse_types::BlockPos;

/// Read-only traversal policy for one immutable world observation.
pub trait WalkSource {
    /// Whether the complete step, including body clearance, is currently traversable.
    fn can_step(&self, from: BlockPos, to: BlockPos) -> bool;
}

/// Explicit limits on search memory and spatial extent.
#[derive(Clone, Copy, Debug)]
pub struct WalkBudget {
    /// Maximum admitted nodes, including the starting node. Hard maximum is 4096.
    pub nodes: usize,
    /// Maximum horizontal distance from the start. Hard maximum is 64 cells.
    pub radius: u32,
}

/// Bounded search result. Partial routes are never reported as complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WalkRoute {
    /// Ordered steps, excluding the starting node.
    Reached(Vec<BlockPos>),
    /// All admissible nodes were examined without reaching the destination.
    Unreachable,
    /// More nodes could be examined, but the admission limit was reached.
    BudgetExceeded,
    /// Invalid limits or coordinates that cannot be offset safely.
    InvalidInput,
}

/// Breadth-first search in a stable order, with at most one cell of climb or descent.
pub fn walk_route(
    source: &impl WalkSource,
    start: BlockPos,
    goal: BlockPos,
    budget: WalkBudget,
) -> WalkRoute {
    if budget.nodes == 0 || budget.nodes > 4096 || budget.radius == 0 || budget.radius > 64 {
        return WalkRoute::InvalidInput;
    }
    let radius = u64::from(budget.radius);
    if start.x.abs_diff(goal.x) > radius || start.z.abs_diff(goal.z) > radius {
        return WalkRoute::Unreachable;
    }
    let mut parent = BTreeMap::from([(start, start)]);
    let mut queue = VecDeque::from([start]);
    while let Some(from) = queue.pop_front() {
        if from == goal {
            let mut path = Vec::new();
            let mut node = goal;
            while node != start {
                path.push(node);
                node = parent[&node];
            }
            path.reverse();
            return WalkRoute::Reached(path);
        }
        for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            for dy in [0, 1, -1] {
                let (Some(x), Some(y), Some(z)) = (
                    from.x.checked_add(dx),
                    from.y.checked_add(dy),
                    from.z.checked_add(dz),
                ) else {
                    return WalkRoute::InvalidInput;
                };
                let to = BlockPos::new(x, y, z);
                if start.x.abs_diff(x) > radius
                    || start.z.abs_diff(z) > radius
                    || start.y.abs_diff(y) > radius
                    || parent.contains_key(&to)
                    || !source.can_step(from, to)
                {
                    continue;
                }
                if parent.len() == budget.nodes {
                    return WalkRoute::BudgetExceeded;
                }
                parent.insert(to, from);
                queue.push_back(to);
            }
        }
    }
    WalkRoute::Unreachable
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Floor {
        wall: bool,
        calls: Cell<usize>,
    }
    impl WalkSource for Floor {
        fn can_step(&self, _: BlockPos, to: BlockPos) -> bool {
            self.calls.set(self.calls.get() + 1);
            to.y == 1 && !(self.wall && to.x == 1 && to.z <= 1)
        }
    }

    #[test]
    fn route_detours_and_revalidates_changed_terrain() {
        let mut floor = Floor {
            wall: true,
            calls: Cell::new(0),
        };
        let start = BlockPos::new(0, 1, 0);
        let goal = BlockPos::new(2, 1, 0);
        let budget = WalkBudget {
            nodes: 256,
            radius: 4,
        };
        let WalkRoute::Reached(path) = walk_route(&floor, start, goal, budget) else {
            panic!()
        };
        assert!(path.len() > 2);
        assert!(path.iter().all(|p| !(p.x == 1 && p.z <= 1)));
        floor.wall = false;
        assert_eq!(
            walk_route(&floor, start, goal, budget),
            WalkRoute::Reached(vec![BlockPos::new(1, 1, 0), goal,])
        );
    }

    #[test]
    fn budget_limits_queries_and_never_returns_a_partial_route() {
        let floor = Floor {
            wall: false,
            calls: Cell::new(0),
        };
        assert_eq!(
            walk_route(
                &floor,
                BlockPos::new(0, 1, 0),
                BlockPos::new(8, 1, 0),
                WalkBudget {
                    nodes: 4,
                    radius: 8
                }
            ),
            WalkRoute::BudgetExceeded
        );
        assert!(floor.calls.get() <= 4 * 12);
        assert_eq!(
            walk_route(
                &floor,
                BlockPos::new(0, 1, 0),
                BlockPos::new(8, 1, 0),
                WalkBudget {
                    nodes: 0,
                    radius: 8
                }
            ),
            WalkRoute::InvalidInput
        );
    }
}
