//! Reproducible developer probes for the M0 contracts.

use std::process::ExitCode;

use torse_content::{compile, m0_fixture_pack};
use torse_modules::resolve_modules;
use torse_types::Digest;

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "content-fixture" => content_fixture(),
        "ecs-probe" => ecs_probe(),
        "wasm-probe" => wasm_probe(),
        "gpu-probe" => gpu_probe(),
        "help" | "--help" | "-h" => {
            println!(
                "usage: torse-tools <content-fixture|ecs-probe|wasm-probe|gpu-probe>\n\
                 experimental probes require their matching cargo feature"
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "ecs-probe")]
fn ecs_probe() -> ExitCode {
    let report = torse_runtime::run_ecs_probe(10_000, 100);
    println!("entities={}", report.entities);
    println!("ticks={}", report.ticks);
    println!("elapsed={:?}", report.elapsed);
    println!("checksum={}", report.checksum);
    ExitCode::SUCCESS
}

#[cfg(not(feature = "ecs-probe"))]
fn ecs_probe() -> ExitCode {
    missing_feature("ecs-probe")
}

#[cfg(feature = "wasm-probe")]
fn wasm_probe() -> ExitCode {
    match torse_mod_host::run_wasm_probe(10_000, 100) {
        Ok(report) => {
            println!("calls={}", report.calls);
            println!("iterations_per_call={}", report.iterations_per_call);
            println!("elapsed={:?}", report.elapsed);
            println!("checksum={}", report.checksum);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("wasm probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "wasm-probe"))]
fn wasm_probe() -> ExitCode {
    missing_feature("wasm-probe")
}

#[cfg(feature = "gpu-probe")]
fn gpu_probe() -> ExitCode {
    match torse_render::run_gpu_probe() {
        Ok(report) => {
            println!("adapter={}", report.adapter_name);
            println!("backend={}", report.backend);
            println!("device_type={}", report.device_type);
            println!("surface_format={}", report.surface_format);
            println!("elapsed={:?}", report.elapsed);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gpu probe failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "gpu-probe"))]
fn gpu_probe() -> ExitCode {
    missing_feature("gpu-probe")
}

#[cfg(any(
    not(feature = "ecs-probe"),
    not(feature = "wasm-probe"),
    not(feature = "gpu-probe")
))]
fn missing_feature(feature: &str) -> ExitCode {
    eprintln!("experiment disabled; rebuild torse-tools with --features {feature}");
    ExitCode::FAILURE
}

fn content_fixture() -> ExitCode {
    match compile(m0_fixture_pack()) {
        Ok(content) => {
            println!("blocks={}", content.blocks.len());
            println!("state_schemas={}", content.state_schemas.len());
            println!("capabilities={}", content.capabilities.len());
            println!("actions={}", content.actions.len());
            println!("modules={}", content.modules.len());
            let modules = match resolve_modules(content.modules.values().cloned().collect()) {
                Ok(modules) => modules,
                Err(errors) => {
                    for error in errors {
                        eprintln!("{error}");
                    }
                    return ExitCode::FAILURE;
                }
            };
            println!(
                "module_order={}",
                modules
                    .order()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            println!("authority_digest={}", hex(&content.authority_digest));
            println!("presentation_digest={}", hex(&content.presentation_digest));
            ExitCode::SUCCESS
        }
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn hex(digest: &Digest) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.0 {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}
