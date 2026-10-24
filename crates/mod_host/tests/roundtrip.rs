//! P1 component roundtrip tests. All tests in this file require the
//! `component` feature and build the roundtrip guest for
//! `wasm32-unknown-unknown` through cargo, so they are skipped in default
//! workspace runs.

#![cfg(feature = "component")]

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// Guest builds share one artifact path, so builds must not overlap and each
/// test must copy its bytes out before another feature combination rebuilds.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

use torse_mod_host::component::{
    CallBudget, EntityId, InvokeEvent, WakeupTarget, encode_component, engine_with, instantiate,
    invoke, machine_invocation, run_native, run_roundtrip,
};
use wasmtime::Engine;
use wasmtime::component::Component;

fn workspace_root() -> PathBuf {
    let mut dir = env::current_dir().expect("current directory readable");
    // crates/mod_host -> workspace root
    for _ in 0..2 {
        dir.pop();
    }
    dir
}

fn guest_artifact_path() -> PathBuf {
    workspace_root()
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("torse_mod_roundtrip.wasm")
}

/// Builds the roundtrip guest and returns the core module bytes.
fn build_guest(features: &[&str]) -> Vec<u8> {
    let _guard = BUILD_LOCK.lock().expect("build lock");
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("--release")
        .arg("--target")
        .arg("wasm32-unknown-unknown")
        .arg("-p")
        .arg("torse-mod-roundtrip")
        .current_dir(workspace_root());
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    let status = command
        .status()
        .expect("cargo command can be spawned from tests");
    assert!(
        status.success(),
        "guest build failed with features {features:?}"
    );
    std::fs::read(guest_artifact_path()).expect("guest wasm artifact readable")
}

fn error_chain_contains(error: &wasmtime::Error, needle: &str) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains(needle))
}

fn wakeup_event() -> InvokeEvent {
    InvokeEvent::Wakeup(WakeupTarget {
        entity: EntityId { low: 1, high: 0 },
        key: "roundtrip:tick".to_string(),
    })
}

fn component_from(engine: &Engine, core: &[u8]) -> Component {
    let bytes = encode_component(core).expect("component encoding succeeds");
    Component::new(engine, &bytes).expect("component compiles")
}

#[test]
fn roundtrip_correctness() {
    let core = build_guest(&[]);
    let engine = engine_with(false).expect("engine");
    let component = component_from(&engine, &core);

    let report = run_roundtrip(&engine, &component, CallBudget::prototype(), 100)
        .expect("roundtrip run completes");
    assert_eq!(report.calls, 100);
    assert_eq!(report.plans_returned, 100);
    assert_eq!(report.committed_count, 100, "each plan steps the counter");
    assert_eq!(report.wakeups, 100, "each plan schedules the next wakeup");
    assert_eq!(report.meters.query_block_calls, 100);
    assert_eq!(report.meters.cells_returned, 2_700);
    assert_eq!(report.meters.resolve_calls, 100);
    assert_eq!(report.meters.log_entries, 1, "exactly tick 64 logs");
}

#[test]
fn epoch_kills_runaway_guest_and_leaves_state_unchanged() {
    let core = build_guest(&["spin"]);
    let engine = engine_with(true).expect("engine with epoch");
    let component = component_from(&engine, &core);

    let (bindings, mut store) =
        instantiate(&engine, &component, CallBudget::prototype()).expect("instance");
    store.set_epoch_deadline(1);

    let stop = Arc::new(AtomicBool::new(false));
    let ticker_engine = engine.clone();
    let ticker_stop = Arc::clone(&stop);
    let ticker = thread::spawn(move || {
        while !ticker_stop.load(Ordering::Relaxed) {
            ticker_engine.increment_epoch();
            thread::sleep(Duration::from_millis(1));
        }
    });

    let invocation = machine_invocation(&store, 1, wakeup_event());
    let result = invoke(&bindings).call_handle_event(&mut store, &invocation);
    stop.store(true, Ordering::Relaxed);
    ticker.join().expect("ticker joins");

    let error = result.expect_err("spin guest must be interrupted");
    assert!(
        error_chain_contains(&error, "interrupt"),
        "unexpected trap: {error:#}"
    );
    let host = store.data();
    assert_eq!(host.committed_count, 0, "failed call must not commit");
    assert!(host.wakeups.is_empty());
}

#[test]
fn fuel_kills_runaway_guest() {
    let core = build_guest(&["spin"]);
    let engine = engine_with(false).expect("engine");
    let component = component_from(&engine, &core);

    let budget = CallBudget {
        fuel: 50_000_000,
        ..CallBudget::prototype()
    };
    let (bindings, mut store) = instantiate(&engine, &component, budget).expect("instance");
    let invocation = machine_invocation(&store, 1, wakeup_event());
    let result = invoke(&bindings).call_handle_event(&mut store, &invocation);
    let error = result.expect_err("spin guest must exhaust fuel");
    assert!(
        error_chain_contains(&error, "fuel"),
        "unexpected trap: {error:#}"
    );
}

#[test]
#[ignore = "measurement harness, run in release with --ignored"]
fn component_cost_versus_native() {
    let core = build_guest(&[]);
    let engine = engine_with(false).expect("engine");
    let component = component_from(&engine, &core);

    let warmup = run_roundtrip(&engine, &component, CallBudget::prototype(), 1_000)
        .expect("warmup completes");
    println!("instantiate: {:?}", warmup.instantiate);

    for calls in [10_000_u32, 50_000] {
        let component_report = run_roundtrip(&engine, &component, CallBudget::prototype(), calls)
            .expect("component run completes");
        let native_elapsed = run_native(calls);
        println!(
            "calls={calls} component={:?}/call native={:?}/call ratio={:.1}x plans={}",
            component_report.per_call,
            native_elapsed / calls,
            component_report.per_call.as_secs_f64() / (native_elapsed / calls).as_secs_f64(),
            component_report.plans_returned,
        );
    }
}
