//! Reproducible CPU-only reference/run-mesher comparison.
use std::hint::black_box;
use std::time::Instant;
use torse_render::{MeshSample, RenderBlock, build_mesh_optimized_bounded, build_mesh_reference};
use torse_types::{BlockPos, SectionPos};

fn main() {
    let sample = |p: BlockPos| {
        if [p.x, p.y, p.z].iter().all(|n| (0..16).contains(n)) {
            MeshSample::Ready(RenderBlock::uniform(1))
        } else {
            MeshSample::NotLoaded
        }
    };
    println!("variant,iterations,faces,bytes,elapsed_us");
    for optimized in [false, true] {
        let start = Instant::now();
        let mut faces = 0;
        let mut bytes = 0;
        for _ in 0..100 {
            let mesh = if optimized {
                build_mesh_optimized_bounded(
                    16,
                    SectionPos::ZERO,
                    sample,
                    || false,
                    16usize.pow(3) * 6,
                )
                .unwrap()
            } else {
                build_mesh_reference(16, SectionPos::ZERO, sample)
            };
            faces = mesh.face_count();
            bytes = mesh.byte_size();
            black_box(mesh);
        }
        println!(
            "{},100,{faces},{bytes},{}",
            if optimized {
                "occupancy_runs"
            } else {
                "reference"
            },
            start.elapsed().as_micros()
        );
    }
}
