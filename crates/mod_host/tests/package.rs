//! P3 toolchain tests: package loading, resolution, locking, and the
//! headless scenario runner. Requires the `package` feature.

#![cfg(feature = "package")]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use torse_mod_host::package::{resolve, verify_lock, write_lock};
use torse_mod_host::scenario::{self, GrantPolicy, Scenario};

/// Resolving the machine package shells out to cargo for the wasm build, so
/// resolutions that build must not overlap.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn workspace_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("current directory readable");
    for _ in 0..2 {
        dir.pop();
    }
    dir
}

fn machine_package() -> PathBuf {
    workspace_root()
        .join("examples")
        .join("mods")
        .join("machine")
}

fn temp_package(name: &str, manifest: &str, content: Option<String>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("torse-mod-pkg-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("content")).expect("temp content dir");
    std::fs::write(dir.join("mod.toml"), manifest).expect("temp manifest");
    if let Some(content) = content {
        std::fs::write(dir.join("content").join("main.toml"), content).expect("temp content");
    }
    dir
}

const VALID_MANIFEST: &str =
    "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n";

const VALID_CONTENT: &str = r#"
[[component]]
id = "probe:schema/state"
version = 1

[[component.field]]
id = "probe:field/count"
type = "u32"
required = true

[[state]]
id = "probe:state/thing"

[[state.component]]
id = "probe:schema/state"

[[block]]
id = "probe:thing"
schema = "probe:state/thing"

[block.render]
shape = "full-cube"
material = "base:material/default"
alpha = "opaque"
"#;

#[test]
fn manifest_and_content_validation() {
    let base = scenario::engine_base();

    let bad_api = temp_package(
        "bad-api",
        "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.1\"\nside = \"server\"\npermissions = []\n",
        None,
    );
    let errors = resolve(std::slice::from_ref(&bad_api), &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("api (0, 1) is not supported")),
        "{errors:?}"
    );

    let foreign_namespace = temp_package(
        "foreign-ns",
        VALID_MANIFEST,
        Some(
            r#"
[[component]]
id = "elsewhere:schema/state"

[[component.field]]
id = "elsewhere:field/count"
type = "u32"

[[state]]
id = "elsewhere:state/thing"

[[state.component]]
id = "elsewhere:schema/state"
"#
            .to_string(),
        ),
    );
    let errors = resolve(std::slice::from_ref(&foreign_namespace), &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("outside the package namespace")),
        "{errors:?}"
    );

    let orphan = temp_package(
        "orphan",
        VALID_MANIFEST,
        Some(
            r#"
[[component]]
id = "probe:schema/state"

[[component.field]]
id = "probe:field/count"
type = "u32"
"#
            .to_string(),
        ),
    );
    let errors = resolve(std::slice::from_ref(&orphan), &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("not attached to any state schema")),
        "{errors:?}"
    );

    for dir in [bad_api, foreign_namespace, orphan] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn dependency_resolution_and_conflicts() {
    let base = scenario::engine_base();

    let dep_content = r#"
[[component]]
id = "dep:schema/state"

[[state]]
id = "dep:state/thing"

[[state.component]]
id = "dep:schema/state"
"#;
    let leaf = temp_package(
        "dep-leaf",
        "id = \"dep\"\nversion = \"0.2.3\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n",
        Some(dep_content.to_string()),
    );
    let root_ok = temp_package(
        "dep-root",
        "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n\n[dependencies]\ndep = \"0.2\"\n",
        Some(VALID_CONTENT.to_string()),
    );
    let set = resolve(&[root_ok.clone(), leaf.clone()], &base).expect("resolution succeeds");
    // Dependency order: dep first.
    assert_eq!(set.packages[0].manifest.id, "dep");
    assert_eq!(set.packages[1].manifest.id, "probe");

    let root_wrong = temp_package(
        "dep-wrong",
        "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n\n[dependencies]\ndep = \"0.3\"\n",
        Some(VALID_CONTENT.to_string()),
    );
    let errors = resolve(&[root_wrong.clone(), leaf.clone()], &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("dependency `dep` requires")),
        "{errors:?}"
    );

    let root_missing = temp_package(
        "dep-missing",
        "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n\n[dependencies]\ndep = \"0.2\"\n",
        Some(VALID_CONTENT.to_string()),
    );
    let errors = resolve(std::slice::from_ref(&root_missing), &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("missing dependency")),
        "{errors:?}"
    );

    let cyclic_a = temp_package(
        "cyc-a",
        "id = \"appa\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n\n[dependencies]\nappb = \"0.1\"\n",
        Some(VALID_CONTENT.replace("probe", "appa")),
    );
    let cyclic_b = temp_package(
        "cyc-b",
        "id = \"appb\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n\n[dependencies]\nappa = \"0.1\"\n",
        Some(VALID_CONTENT.replace("probe", "appb")),
    );
    let errors = resolve(&[cyclic_a.clone(), cyclic_b.clone()], &base).unwrap_err();
    assert!(
        errors.iter().any(|error| error.detail.contains("cycle")),
        "{errors:?}"
    );

    // Namespace enforcement makes cross-package duplicates impossible; the
    // remaining duplicate case is two content files inside one package.
    let duplicated = temp_package(
        "duplicate",
        "id = \"probe\"\nversion = \"0.1.0\"\napi = \"0.3\"\nside = \"server\"\npermissions = []\n",
        Some(VALID_CONTENT.to_string()),
    );
    std::fs::write(duplicated.join("content").join("again.toml"), VALID_CONTENT)
        .expect("second content file");
    let errors = resolve(std::slice::from_ref(&duplicated), &base).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("duplicate component schema")),
        "{errors:?}"
    );

    for dir in [
        leaf,
        root_ok,
        root_wrong,
        root_missing,
        cyclic_a,
        cyclic_b,
        duplicated,
    ] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn lock_roundtrip_detects_tampering() {
    let base = scenario::engine_base();
    let dir = temp_package("lock", VALID_MANIFEST, Some(VALID_CONTENT.to_string()));
    let set = resolve(std::slice::from_ref(&dir), &base).expect("resolves");
    let lock = write_lock(&set);
    verify_lock(&set, &lock).expect("lock verifies against itself");

    // Tamper with a file; the merkle digest must change.
    std::fs::write(dir.join("content").join("main.toml"), "# tampered\n").expect("tamper");
    let tampered = resolve(std::slice::from_ref(&dir), &base).expect("resolves again");
    let errors = verify_lock(&tampered, &lock).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.detail.contains("merkle mismatch")),
        "{errors:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn machine_package_runs_the_scenario() {
    let _guard = BUILD_LOCK.lock().expect("build lock");
    let base = scenario::engine_base();
    let set = resolve(&[machine_package()], &base)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>())
        .expect("machine package resolves");
    assert_eq!(set.packages.len(), 1);
    assert_eq!(
        set.owned_schemas
            .get(&set.packages[0].manifest.principal())
            .map(|schemas| schemas.len()),
        Some(1),
        "machine package owns its component schema"
    );

    let components = scenario::build_components(&set).expect("machine wasm builds");
    let scenario_text = std::fs::read_to_string(machine_package().join("scenario.toml"))
        .expect("scenario readable");
    let scenario = Scenario::parse(&scenario_text).expect("scenario parses");
    let policy = GrantPolicy::AllRequested;
    let report =
        scenario::run_scenario(&set, &scenario, &policy, components).expect("scenario runs");

    assert!(report.passed, "assertions: {:?}", report.asserts);
    let machine = report.packages.get("machine").expect("machine stats");
    assert_eq!(machine.dispatches, 6);
    assert_eq!(machine.committed, 6);
    assert_eq!(machine.rejected, 0);
    assert_eq!(machine.trapped, 0);
    assert!(report.seq >= 6, "commit seq advanced: {}", report.seq);
}

#[test]
fn new_template_passes_check() {
    // The CLI binary is built alongside the tests when the feature is on.
    let exe = env!("CARGO_BIN_EXE_torse-mod");
    let dir = std::env::temp_dir().join(format!("torse-mod-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let status = Command::new(exe)
        .arg("new")
        .arg(&dir)
        .status()
        .expect("cli spawns");
    assert!(status.success(), "new failed");

    // The template's sdk/wit defaults only matter for building; check works
    // against the manifest and content alone.
    let status = Command::new(exe)
        .arg("check")
        .arg(&dir)
        .status()
        .expect("cli spawns");
    assert!(status.success(), "check failed for the template");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_types_roundtrip() {
    use torse_mod_host::package::{Version, VersionReq};
    let version = Version::parse("1.2.3").expect("version");
    assert_eq!(version.as_str(), "1.2.3");
    assert!(VersionReq::parse("1.2").unwrap().accepts(version));
    assert!(!VersionReq::parse("1.2.4").unwrap().accepts(version));
}
