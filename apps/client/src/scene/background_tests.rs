use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn config(workers: usize, jobs: usize) -> ExecutorConfig {
    ExecutorConfig {
        workers,
        max_jobs: jobs,
        max_input_bytes: 2 * 1024 * 1024,
        max_output_bytes: 32 * 1024 * 1024,
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "background tasks did not finish");
        std::thread::yield_now();
    }
}
fn idle(scene: &Scene) -> bool {
    let usage = scene.background.as_ref().unwrap().pool.usage();
    usage.queued == 0 && usage.running == 0
}
fn drain(scene: &mut Scene, center: SectionPos) {
    wait(|| {
        scene.stream_around(center, 4).unwrap();
        scene.refresh_visuals(4).unwrap();
        let complete = scene.sections.iter().all(|p| {
            scene
                .meshes
                .get(p)
                .is_some_and(|(stamp, _)| stamp == &MeshStamp::capture(scene, *p))
        });
        complete && scene.background.as_ref().unwrap().pool.usage().jobs == 0
    });
}
fn network_scene() -> Scene {
    Scene::from_snapshot(&ReplicaSnapshot {
        side: 16,
        bounds: WorldBounds::new(0, 32).unwrap(),
        states: vec![StateEntry {
            id: BlockStateId(7),
            block: key("base:stone"),
            values: Vec::new(),
        }],
        sections: vec![SnapshotSection {
            pos: SectionPos::ZERO,
            cells: SnapshotCells::Uniform(BlockStateId(7)),
        }],
        entities: Vec::new(),
    })
    .unwrap()
}
fn drain_meshes(scene: &mut Scene) {
    wait(|| {
        scene.refresh_visuals(4).unwrap();
        scene.sections.iter().all(|p| {
            scene
                .meshes
                .get(p)
                .is_some_and(|(s, _)| s == &MeshStamp::capture(scene, *p))
        })
    });
}

#[test]
fn one_and_four_workers_match_serial_generation_and_meshes_under_backpressure() {
    let mut reference = Scene::new(1).unwrap();
    reference.refresh_meshes();
    let expected = reference.geometry();
    for workers in [1, 4] {
        let mut scene = Scene::new(0).unwrap();
        scene.radius = 1;
        scene.enable_background(config(workers, 2)).unwrap();
        drain(&mut scene, SectionPos::ZERO);
        assert_eq!(scene.geometry(), expected);
        let background = scene.background.as_ref().unwrap();
        assert!(background.stats.backpressure > 0);
        assert!(background.stats.peak_jobs <= 2);
        assert!(background.stats.peak_input_bytes <= 2 * 1024 * 1024);
        assert!(background.stats.peak_output_bytes <= 32 * 1024 * 1024);
        assert_eq!(background.pool.usage().jobs, 0);
        println!(
            "workers={workers} sections={} stats={:?}",
            scene.mesh_count(),
            background.stats
        );
    }
}

#[test]
fn tinted_overlay_meshes_match_serial_and_reject_completed_provider_revisions() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use torse_assets::texture::{Atlas, MaterialEntry, PositionTintProvider, TintSource};
    struct Tint(AtomicU64);
    impl PositionTintProvider for Tint {
        fn sample(&self, _: TintSource, _: BlockPos) -> [u8; 3] {
            if self.0.load(Ordering::Relaxed) == 0 {
                [255, 0, 0]
            } else {
                [0, 255, 0]
            }
        }
        fn revision(&self, _: SectionPos) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }
    for workers in [1, 4] {
        let atlas = Atlas::decode(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../games/base/assets/textures/atlas.png"
        )))
        .unwrap();
        let pack = TexturePack::new(
            atlas,
            vec![MaterialEntry {
                key: "base:material/stone",
                rect: [4, 4, 16, 16],
                tint_source: Some(TintSource::Grass),
                ..MaterialEntry::new("", [0; 4])
            }],
            [76, 4, 16, 16],
            [100, 4, 16, 16],
        )
        .unwrap();
        let mut scene = Scene::with_pack(0, pack).unwrap();
        let stone = scene
            .authority
            .registry()
            .default_state(&key("base:stone"))
            .unwrap();
        let material = scene.state_materials.get_mut(&stone).unwrap();
        material.overlays = [Some(material.base[0]); 6];
        let provider = Arc::new(Tint(AtomicU64::new(0)));
        scene
            .set_position_tint_provider(Some(provider.clone()))
            .unwrap();
        scene.enable_background(config(workers, 2)).unwrap();
        scene.refresh_visuals(1).unwrap();
        wait(|| idle(&scene));
        provider.0.store(1, Ordering::Relaxed);
        assert!(scene.refresh_visuals(1).unwrap().is_empty());
        assert_eq!(scene.background.as_ref().unwrap().stats.stale, 1);
        drain_meshes(&mut scene);
        let actual = scene.geometry();
        assert!(actual.0.iter().any(|v| v.overlay_material != u32::MAX));
        assert!(
            actual
                .0
                .iter()
                .all(|v| v.tint == [0.0, 1.0, 0.0] && v.overlay_tint == [0.0, 1.0, 0.0])
        );
        scene.meshes.clear();
        scene.refresh_meshes();
        assert_eq!(scene.geometry(), actual);
        assert!(scene.background.as_ref().unwrap().stats.peak_input_bytes <= 2 * 1024 * 1024);
    }
}

#[test]
fn completed_meshes_reject_terrain_neighbor_reload_epoch_and_asset_changes() {
    fn replace(scene: &mut Scene, pos: SectionPos) {
        let symbols = [(
            BlockStateId(7),
            scene
                .authority
                .registry()
                .default_state(&key("base:stone"))
                .unwrap(),
        )]
        .into();
        scene
            .install_replica_section(
                &SnapshotSection {
                    pos,
                    cells: SnapshotCells::Uniform(BlockStateId(7)),
                },
                &symbols,
            )
            .unwrap();
    }
    for change in 0..6 {
        let mut scene = network_scene();
        if change == 3 {
            replace(&mut scene, SectionPos::new(1, 0, 0));
            scene.sections.push(SectionPos::new(1, 0, 0));
        }
        scene.enable_background(config(1, 2)).unwrap();
        scene.refresh_visuals(1).unwrap();
        wait(|| idle(&scene));
        match change {
            0 => {
                scene
                    .authority
                    .store_mut()
                    .edit(BlockPos::new(1, 1, 1), BlockStateId::EMPTY)
                    .unwrap();
            }
            1 => {
                let section = SnapshotSection {
                    pos: SectionPos::new(1, 0, 0),
                    cells: SnapshotCells::Uniform(BlockStateId(7)),
                };
                let symbols = [(
                    BlockStateId(7),
                    scene
                        .authority
                        .registry()
                        .default_state(&key("base:stone"))
                        .unwrap(),
                )]
                .into();
                scene.install_replica_section(&section, &symbols).unwrap();
                scene.sections.push(section.pos);
            }
            2 => {
                let before = scene.authority.store().revision(SectionPos::ZERO);
                let symbols = [(
                    BlockStateId(7),
                    scene
                        .authority
                        .registry()
                        .default_state(&key("base:stone"))
                        .unwrap(),
                )]
                .into();
                scene
                    .install_replica_section(
                        &SnapshotSection {
                            pos: SectionPos::ZERO,
                            cells: SnapshotCells::Uniform(BlockStateId(7)),
                        },
                        &symbols,
                    )
                    .unwrap();
                assert_eq!(
                    scene.authority.store().revision(SectionPos::ZERO),
                    before,
                    "same-revision reload exercises ABA protection"
                );
            }
            3 => {
                let neighbor = SectionPos::new(1, 0, 0);
                let before = scene.authority.store().revision(neighbor);
                replace(&mut scene, neighbor);
                assert_eq!(scene.authority.store().revision(neighbor), before);
            }
            4 => scene.epoch = scene_epoch().unwrap(),
            _ => scene.asset_generation += 1,
        }
        assert!(
            scene.refresh_visuals(1).unwrap().is_empty(),
            "old result must not enter the cache"
        );
        assert_eq!(scene.background.as_ref().unwrap().stats.stale, 1);
        drain_meshes(&mut scene);
        let async_geometry = scene.geometry();
        scene.meshes.clear();
        scene.refresh_meshes();
        assert_eq!(scene.geometry(), async_geometry);
    }
}

#[test]
fn recenter_storm_cancels_queued_generation_without_fabricating_ready_cells() {
    let mut scene = Scene::new(0).unwrap();
    scene.enable_background(config(1, 2)).unwrap();
    let (started, observed) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut blocker = scene
        .background
        .as_ref()
        .unwrap()
        .pool
        .try_reserve(JobBudget {
            input_bytes: 1,
            output_bytes: 1,
        })
        .unwrap()
        .submit(Priority::Visible, move |_| {
            started.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(5)).unwrap();
            Err(JobError::Cancelled)
        });
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    for x in 3..100 {
        scene.stream_around(SectionPos::new(x, 0, 0), 1).unwrap();
        assert_eq!(
            scene.authority.store().query(BlockPos::new(x * 16, 1, 0)),
            TerrainQuery::NotLoaded
        );
        assert!(scene.background.as_ref().unwrap().pool.usage().jobs <= 2);
        assert!(scene.load_generations.len() <= 1);
    }
    assert!(scene.background.as_ref().unwrap().stats.cancelled >= 96);
    release.send(()).unwrap();
    wait(|| blocker.try_take().is_some());
    drop(blocker);
    drain(&mut scene, SectionPos::ZERO);
    assert_eq!(scene.authority.store().len(), 1);
    assert_eq!(scene.mesh_count(), 1);
    assert_eq!(
        scene.authority.store().query(BlockPos::new(99 * 16, 1, 0)),
        TerrainQuery::NotLoaded
    );
}

#[test]
fn full_executor_defers_before_snapshot_and_old_generation_cannot_replace_a_new_load() {
    let mut scene = Scene::new(0).unwrap();
    scene.enable_background(config(1, 1)).unwrap();
    let held = scene
        .background
        .as_ref()
        .unwrap()
        .pool
        .try_reserve(JobBudget {
            input_bytes: 1,
            output_bytes: 1,
        })
        .unwrap();
    assert!(scene.refresh_visuals(1).unwrap().is_empty());
    assert_eq!(scene.background.as_ref().unwrap().stats.admitted, 0);
    assert_eq!(scene.background.as_ref().unwrap().stats.backpressure, 1);
    drop(held);
    scene.stream_around(SectionPos::new(3, 0, 0), 1).unwrap();
    wait(|| idle(&scene));
    let pos = SectionPos::new(3, 0, 0);
    scene
        .install_generated(pos, Section::empty(16).unwrap())
        .unwrap();
    scene.stream_around(pos, 1).unwrap();
    assert_eq!(scene.background.as_ref().unwrap().stats.stale, 1);
    assert_eq!(
        scene.authority.store().query(BlockPos::new(48, 1, 0)),
        TerrainQuery::Ready(BlockStateId::EMPTY)
    );
}
