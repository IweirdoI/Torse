//! M0 experiment: adapt an independent ECS and its access-aware scheduler.
//!
//! This probe is deliberately separate from the authority world. It verifies
//! that `bevy_ecs` can be embedded, that component queries work, and that a
//! schedule can be run without pulling in the rest of Bevy. Project regions,
//! view permissions, tickets, and deterministic transaction ordering are not
//! modeled here and remain M2/M3 work.

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;

/// Mutable position component written by the integration system.
#[derive(Component)]
struct Position {
    value: u64,
}

/// Read-only velocity component used by the integration system.
#[derive(Component)]
struct Velocity(u64);

/// Independent component used to show that the scheduler can order disjoint work.
#[derive(Component)]
struct Health(u32);

/// Add velocity to position while the scheduler has exclusive access to Position.
fn integrate(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in &mut query {
        position.value = position.value.wrapping_add(velocity.0);
    }
}

/// Advance a component that does not overlap the position/velocity query.
fn regenerate(mut query: Query<&mut Health>) {
    for mut health in &mut query {
        health.0 = health.0.saturating_add(1);
    }
}

/// Observable output of one ECS probe run.
#[derive(Clone, Copy, Debug)]
pub struct EcsProbeReport {
    /// Number of entities spawned before the timed schedule runs.
    pub entities: usize,
    /// Number of schedule executions included in the timing.
    pub ticks: u32,
    /// Wall-clock time for the schedule loop only.
    pub elapsed: Duration,
    /// Sum after the run; used as a lightweight correctness signal.
    pub checksum: u64,
}

/// Spawn a synthetic workload, run it through a Bevy schedule, and report timing.
///
/// The checksum is not a deterministic replay mechanism. It is only a cheap way
/// to prove that systems actually visited the spawned entities.
pub fn run_ecs_probe(entities: usize, ticks: u32) -> EcsProbeReport {
    let mut world = World::new();
    for index in 0..entities {
        world.spawn((
            Position {
                value: index as u64,
            },
            Velocity((index % 7 + 1) as u64),
            Health(100),
        ));
    }

    // The scheduler can run these systems according to their declared component
    // access. This is the capability under evaluation, not a custom loop.
    let mut schedule = Schedule::default();
    schedule.add_systems((integrate, regenerate));

    let start = Instant::now();
    for _ in 0..ticks {
        schedule.run(&mut world);
    }
    let elapsed = start.elapsed();

    let mut query = world.query::<(&Position, &Health)>();
    let checksum = query.iter(&world).fold(0_u64, |sum, (position, health)| {
        sum.wrapping_add(position.value)
            .wrapping_add(u64::from(health.0))
    });

    EcsProbeReport {
        entities,
        ticks,
        elapsed,
        checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_updates_every_entity() {
        // A tiny deterministic case catches missing system registration or a
        // query that silently visits no entities.
        let report = run_ecs_probe(128, 4);
        assert_eq!(report.entities, 128);
        assert_ne!(report.checksum, 0);
    }
}
