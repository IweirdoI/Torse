//! Torse client.
//!
//! Current scope: build the authoritative scene, mesh it, and render one frame
//! into a PNG from a fixed camera. The windowed surface, input, and continuous
//! loop come next; the pipeline and buffers here are the same ones they will
//! drive.

mod camera;
mod renderer;
mod scene;

use std::process::ExitCode;
use std::time::Instant;

use camera::Camera;
use renderer::Renderer;
use scene::Scene;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut capture = None;
    let mut width = 960u32;
    let mut height = 540u32;
    let mut radius = 2i64;
    let mut view = String::from("overview");
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--capture" => {
                index += 1;
                capture = arguments.get(index).cloned();
            }
            "--width" => {
                index += 1;
                width = arguments
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(width);
            }
            "--height" => {
                index += 1;
                height = arguments
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(height);
            }
            "--radius" => {
                index += 1;
                radius = arguments
                    .get(index)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(radius);
            }
            "--view" => {
                index += 1;
                if let Some(value) = arguments.get(index) {
                    view = value.clone();
                }
            }
            "--help" | "-h" => {
                println!(
                    "usage: torse-client --capture <png> [--width N] [--height N] [--radius N]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::FAILURE;
            }
        }
        index += 1;
    }
    let Some(path) = capture else {
        println!("usage: torse-client --capture <png> [--width N] [--height N] [--radius N]");
        return ExitCode::SUCCESS;
    };

    match run(&path, width, height, radius, &view) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("client failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(path: &str, width: u32, height: u32, radius: i64, view: &str) -> Result<String, String> {
    let start = Instant::now();
    let mut scene = Scene::new(radius)?;
    scene.refresh_meshes();
    let (vertices, indices, faces) = scene.geometry();
    if indices.is_empty() {
        return Err("scene produced no geometry".to_owned());
    }
    let vertex_bytes = Scene::vertex_bytes(&vertices);

    let mut renderer = Renderer::new(width, height)?;
    renderer.upload(&vertex_bytes, &indices);

    let (eye, target) = match view {
        "player" => {
            let eye = scene.eye();
            (eye, [eye[0], eye[1] - 1.5, eye[2] - 12.0])
        }
        _ => ([40.0, 18.0, 40.0], [0.0, 6.0, 0.0]),
    };
    let camera = Camera::new(eye, target);
    renderer.render(camera.view_projection(width as f32 / height as f32));
    renderer.save_png(path)?;

    let elapsed = start.elapsed();
    Ok(format!(
        "sections={} meshes={} entities={} seq={} vertices={} indices={} faces={} eye=({:.1},{:.1},{:.1}) elapsed_ms={}",
        scene.sections().len(),
        scene.mesh_count(),
        scene.authority().entities().len(),
        scene.authority().seq().0,
        vertices.len(),
        indices.len(),
        faces,
        eye[0],
        eye[1],
        eye[2],
        elapsed.as_millis()
    ))
}
