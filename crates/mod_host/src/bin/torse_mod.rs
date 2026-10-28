//! `torse-mod`: the Mod platform toolchain (P3).
//!
//! Commands:
//! ```text
//!   torse-mod new <dir>                       scaffold a new package
//!   torse-mod check <pkg-dir>... [--lock F]   validate packages and content
//!   torse-mod pack <pkg-dir>... -o F          build entries and write a lock
//!   torse-mod test <pkg-dir>... --scenario F  run a headless fixture scenario
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use torse_mod_host::package::{resolve, verify_lock, write_lock};
use torse_mod_host::scenario::{self, GrantPolicy, Scenario};

const TEMPLATE_MANIFEST: &str = r#"id = "{id}"
version = "0.1.0"
api = "0.3"
side = "server"
permissions = []

[server]
entry = "logic.wasm"

[build]
crate = "{crate_name}"
workspace = "../../../"

[dependencies]
"#;

const TEMPLATE_CONTENT: &str = r#"[[component]]
id = "{id}:schema/state"
version = 1
owner = "{id}:module"
persistence = "always"
replication = "server-private"

[[component.field]]
id = "{id}:field/progress"
type = "u32"
required = true

[[state]]
id = "{id}:state/machine"
version = 1

[[state.component]]
id = "{id}:schema/state"
"#;

const TEMPLATE_SCENARIO: &str = r#"[world]
side = 16
min-y = 0
max-y = 32

[[entity]]
id = "machine"
at = [2.0, 2.0, 2.0]

[entity.components."{id}:schema/state"]
"{id}:field/progress" = 0

[[wakeup]]
tick = 1
package = "{id}"
entity = "machine"
key = "{id}:tick"

[[step]]
ticks = 4

[[assert]]
entity = "machine"
component = "{id}:schema/state"
field = "{id}:field/progress"
equals = 4
"#;

const TEMPLATE_CARGO: &str = r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
torse-mod-sdk = { path = "{sdk_path}" }
wit-bindgen = "=0.63.0"
"#;

const TEMPLATE_LIB: &str = r#"//! Behavior entry for the {id} package.

use bindings::exports::torse::behavior::invoke::{
    ComponentEdit, Guest, Invocation, Plan, WakeupRequest,
};
use bindings::torse::behavior::types::{Atom, EntityId, Field};

mod bindings {
    wit_bindgen::generate!({
        path: "{wit_path}",
        world: "mod-behavior",
    });
}

const SCHEMA_KEY: &str = "{id}:schema/state";
const PROGRESS_FIELD: &str = "{id}:field/progress";
const WORK_KEY: &str = "{id}:tick";

#[allow(dead_code)]
struct Component;

impl Guest for Component {
    fn handle_event(invocation: Invocation) -> Option<Plan> {
        let tick = invocation.tick;
        let self_ref = invocation.self_.as_ref()?;
        let machine = self_ref.entity;
        let previous: Vec<Field> = self_ref
            .owned
            .first()
            .and_then(|state| state.fields.clone())
            .unwrap_or_default();
        let progress = previous
            .iter()
            .find(|field| field.key == PROGRESS_FIELD)
            .and_then(|field| match field.value {
                Atom::U32(value) => Some(value),
                _ => None,
            })
            .unwrap_or(0);
        let schema = bindings::torse::behavior::query::resolve_symbol(SCHEMA_KEY)?;
        let next = progress.wrapping_add(1);
        let mut fields = previous.clone();
        for field in &mut fields {
            if field.key == PROGRESS_FIELD {
                field.value = Atom::U32(next);
            }
        }
        Some(Plan {
            spawns: Vec::new(),
            despawns: Vec::new(),
            edits: Vec::new(),
            component_edits: vec![ComponentEdit {
                entity: machine,
                schema,
                previous: Some(previous),
                fields: Some(fields),
            }],
            transfers: Vec::new(),
            wakeups: vec![WakeupRequest {
                entity: machine,
                tick: tick + 1,
                key: WORK_KEY.to_string(),
            }],
        })
    }
}

bindings::export!(Component with_types_in bindings);
"#;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprintln!("usage: torse-mod <new|check|pack|test> ...");
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    let result = match command.as_str() {
        "new" => command_new(rest),
        "check" => command_check(rest),
        "pack" => command_pack(rest),
        "test" => command_test(rest),
        other => {
            eprintln!("unknown command `{other}`");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            for error in errors {
                eprintln!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn command_new(args: &[String]) -> Result<(), Vec<String>> {
    let Some(dir) = args.first() else {
        return Err(vec!["usage: torse-mod new <dir>".to_string()]);
    };
    let dir = PathBuf::from(dir);
    if dir.exists() {
        return Err(vec![format!("{} already exists", dir.display())]);
    }
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| vec!["directory name is not valid".to_string()])?
        .to_string();
    let crate_name = format!("torse-mod-{id}");
    let sdk_path =
        std::env::var("TORSE_SDK_PATH").unwrap_or_else(|_| "../../crates/mod_sdk".to_string());
    let wit_path =
        std::env::var("TORSE_WIT_PATH").unwrap_or_else(|_| "../../crates/mod_api/wit".to_string());
    let fill = |template: &str| {
        template
            .replace("{id}", &id)
            .replace("{crate_name}", &crate_name)
            .replace("{sdk_path}", &sdk_path)
            .replace("{wit_path}", &wit_path)
    };
    let files = [
        ("mod.toml", fill(TEMPLATE_MANIFEST)),
        ("content/machine.toml", fill(TEMPLATE_CONTENT)),
        ("scenario.toml", fill(TEMPLATE_SCENARIO)),
        ("Cargo.toml", fill(TEMPLATE_CARGO)),
        ("src/lib.rs", fill(TEMPLATE_LIB)),
    ];
    for (path, text) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| vec![format!("create {}: {error}", parent.display())])?;
        }
        std::fs::write(&path, text)
            .map_err(|error| vec![format!("write {}: {error}", path.display())])?;
    }
    println!("created package {id} at {}", dir.display());
    println!(
        "build and test with: torse-mod test {} --scenario {}/scenario.toml",
        dir.display(),
        dir.display()
    );
    Ok(())
}

fn parse_dirs(args: &[String]) -> Result<Vec<PathBuf>, Vec<String>> {
    const VALUE_FLAGS: [&str; 3] = ["--scenario", "--lock", "-o"];
    let mut dirs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if VALUE_FLAGS.contains(&arg.as_str()) {
            // The flag and its value are not package directories.
            index += 2;
            continue;
        }
        if !arg.starts_with("--") {
            dirs.push(PathBuf::from(arg));
        }
        index += 1;
    }
    if dirs.is_empty() {
        return Err(vec![
            "at least one package directory is required".to_string(),
        ]);
    }
    Ok(dirs)
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|arg| arg == flag).and_then(|index| {
        args.get(index + 1)
            .filter(|value| !value.starts_with("--"))
            .cloned()
    })
}

fn command_check(args: &[String]) -> Result<(), Vec<String>> {
    let dirs = parse_dirs(args)?;
    let set = resolve(&dirs, &scenario::engine_base()).map_err(|errors: Vec<_>| {
        errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
    })?;
    for package in &set.packages {
        println!(
            "ok: {} {} ({} files, merkle {})",
            package.manifest.id,
            package.manifest.version,
            package.files.len(),
            &package.merkle[..12]
        );
    }
    if let Some(lock_path) = flag_value(args, "--lock") {
        let text = std::fs::read_to_string(&lock_path)
            .map_err(|error| vec![format!("read lock: {error}")])?;
        verify_lock(&set, &text).map_err(|errors: Vec<_>| {
            errors
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        })?;
        println!("lock verified: {lock_path}");
    }
    Ok(())
}

fn command_pack(args: &[String]) -> Result<(), Vec<String>> {
    let dirs = parse_dirs(args)?;
    let out = flag_value(args, "-o")
        .ok_or_else(|| vec!["usage: torse-mod pack <pkg-dir>... -o <lock-file>".to_string()])?;
    let set = resolve(&dirs, &scenario::engine_base()).map_err(|errors: Vec<_>| {
        errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
    })?;
    let lock = write_lock(&set);
    std::fs::write(&out, lock).map_err(|error| vec![format!("write lock: {error}")])?;
    println!("lock written: {out}");
    Ok(())
}

fn command_test(args: &[String]) -> Result<(), Vec<String>> {
    let dirs = parse_dirs(args)?;
    let scenario_path = flag_value(args, "--scenario")
        .ok_or_else(|| vec!["--scenario <file> is required".to_string()])?;
    let text = std::fs::read_to_string(&scenario_path)
        .map_err(|error| vec![format!("read scenario: {error}")])?;
    let scenario = Scenario::parse(&text).map_err(|error| vec![error])?;
    let set = resolve(&dirs, &scenario::engine_base()).map_err(|errors: Vec<_>| {
        errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
    })?;
    let components = scenario::build_components(&set).map_err(|error| vec![error])?;
    let policy = GrantPolicy::AllRequested;
    let report = scenario::run_scenario(&set, &scenario, &policy, components)
        .map_err(|error| vec![error])?;

    for (package, stats) in &report.packages {
        println!(
            "package {package}: dispatches={} committed={} rejected={} trapped={} no-plan={} deactivated={} wall={:?}",
            stats.dispatches,
            stats.committed,
            stats.rejected,
            stats.trapped,
            stats.no_plan,
            stats.deactivated,
            stats.wall
        );
    }
    println!("commit-seq: {}", report.seq);
    for assert in &report.asserts {
        let verdict = if assert.ok { "ok" } else { "FAIL" };
        println!("assert {verdict}: {}", assert.detail);
    }
    if report.passed {
        println!("scenario passed");
        Ok(())
    } else {
        Err(vec!["scenario assertions failed".to_string()])
    }
}
