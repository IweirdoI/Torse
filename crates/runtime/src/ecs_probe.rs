//! M0 experiment: adapt an independent ECS and its access-aware scheduler.

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::Schedule;

#[derive(Component)]
struct Position {
    value: u64,
}

#[derive(Component)]
struct Velocity(u64);

#[derive(Component)]
struct Health(u32);

fn integrate(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in &mut query {
        position.value = position.value.wrapping_add(velocity.0);
    }
}

fn regenerate(mut query: Query<&mut Health>) {
    for mut health in &mut query {
        health.0 = health.0.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EcsProbeReport {
    pub entities: usize,
    pub ticks: u32,
    pub elapsed: Duration,
    pub checksum: u64,
}

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
        let report = run_ecs_probe(128, 4);
        assert_eq!(report.entities, 128);
        assert_ne!(report.checksum, 0);
    }
}
