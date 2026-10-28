use sha2::{Digest as _, Sha256};
use torse_assets::{
    AssetAlias, AssetGuid,
    cooked::{CookedResources, LoadLimits},
};
use torse_content::{CookImage, CookedLocale, compile, cook, m0_fixture_pack, write_cooked};
use torse_types::{ContentKey, Digest, MemoryBudget, MemoryClass, MemoryPool};

const PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../games/base/assets/textures/atlas.png"
));
fn key(s: &str) -> ContentKey {
    ContentKey::parse(s).unwrap()
}
fn images() -> Vec<CookImage> {
    vec![CookImage {
        guid: AssetGuid(1),
        source_digest: Digest(Sha256::digest(PNG).into()),
        key: key("test:image/atlas"),
        path: "textures/test.png".into(),
    }]
}
fn bytes(images: &[CookImage]) -> Vec<u8> {
    let tables = cook(
        &compile(m0_fixture_pack()).unwrap(),
        images,
        &[CookedLocale {
            language: "en".into(),
            entries: [("hello".into(), "Hello".into())].into(),
        }],
    )
    .unwrap();
    write_cooked(&tables).unwrap()
}
fn limits() -> LoadLimits {
    LoadLimits {
        max_source_bytes: 4096,
        ..LoadLimits::default()
    }
}
fn source(_path: &str, max: usize) -> Result<Vec<u8>, String> {
    assert!(PNG.len() <= max);
    Ok(PNG.to_vec())
}

#[test]
fn runtime_rows_aliases_and_dependency_lifetimes_use_verified_cpu_images() {
    let memory = MemoryPool::new(MemoryBudget::new(4 * 1024 * 1024));
    let encoded = bytes(&images());
    let mut resources =
        CookedResources::load(&encoded, memory.clone(), limits(), &mut source).unwrap();
    let (info, pixels) = resources.image(AssetGuid(1)).unwrap();
    assert_eq!(pixels.len(), info.width as usize * info.height as usize * 4);
    assert_eq!(
        resources.catalog().guid_for_key(&key("test:image/atlas")),
        Some(AssetGuid(1))
    );
    assert_eq!(
        resources.catalog().guid_for_path("textures/test.png"),
        Some(AssetGuid(1))
    );
    assert_eq!(resources.message("en", "hello"), Some("Hello"));
    let state = resources.state(&key("base:stone"), 0).unwrap();
    assert!(resources.material(state.materials[0]).is_some());
    let alias = AssetAlias::path("derived/sample.rgba").unwrap();
    let first = resources
        .load_derived(AssetGuid(2), &[1, 2, 3, 4], &[alias], &[AssetGuid(1)])
        .unwrap();
    let second = resources
        .load_derived(AssetGuid(3), &[5, 6, 7, 8], &[], &[AssetGuid(1)])
        .unwrap();
    // A dependency graph is immutable while resident. Neither a self-edge
    // nor an attempted back-edge can introduce a cycle.
    let before = memory.usage().total;
    assert!(
        resources
            .load_derived(AssetGuid(4), &[1], &[], &[AssetGuid(4)])
            .is_err()
    );
    assert!(
        resources
            .load_derived(AssetGuid(2), &[1, 2, 3, 4], &[], &[AssetGuid(3)])
            .is_err()
    );
    assert!(
        resources
            .load_derived(AssetGuid(2), &[9], &[], &[AssetGuid(1)])
            .is_err()
    );
    assert_eq!(memory.usage().total, before);
    assert!(resources.unload(info.handle).is_err());
    resources.unload(first).unwrap();
    assert!(resources.unload(info.handle).is_err());
    resources.unload(second).unwrap();
    resources.unload(info.handle).unwrap();
    assert!(resources.catalog().get(info.handle).is_none());
    assert!(resources.image(AssetGuid(1)).is_none());
    let reloaded = resources.load_image(AssetGuid(1), &mut source).unwrap();
    assert_ne!(reloaded.handle, info.handle);
    assert!(resources.unload(info.handle).is_err());
    assert!(resources.image(AssetGuid(1)).is_some());
    assert!(
        resources
            .load_derived(AssetGuid(1), &[1], &[], &[])
            .is_err()
    );
    assert!(
        resources
            .load_derived(AssetGuid(4), &[1], &[], &[AssetGuid(99)])
            .is_err()
    );
    let before = memory.usage().total;
    assert!(
        resources
            .load_image(AssetGuid(1), &mut |_: &str, _| Ok(vec![0]))
            .is_err()
    );
    assert_eq!(memory.usage().total, before);
    assert!(resources.image(AssetGuid(1)).is_some());
    drop(resources);
    assert_eq!(memory.usage().total, 0);
}

#[test]
fn later_source_failure_or_budget_denial_releases_the_entire_initial_load() {
    let mut rows = images();
    let mut second = rows[0].clone();
    second.guid = AssetGuid(2);
    second.key = key("test:image/other");
    second.path = "textures/other.png".into();
    rows.push(second);
    let memory = MemoryPool::new(MemoryBudget::new(4 * 1024 * 1024));
    let mut calls = 0;
    let result = CookedResources::load(
        &bytes(&rows),
        memory.clone(),
        limits(),
        &mut |_: &str, _| {
            calls += 1;
            Ok(if calls == 1 { PNG.to_vec() } else { vec![0] })
        },
    );
    assert!(result.is_err());
    assert_eq!(calls, 2);
    assert_eq!(memory.usage().total, 0);
    for class in [MemoryClass::Resource, MemoryClass::Codec] {
        let pool = MemoryPool::new(MemoryBudget::new(4 * 1024 * 1024).with_class_limit(class, 1));
        let mut calls = 0;
        assert!(
            CookedResources::load(
                &bytes(&images()),
                pool.clone(),
                limits(),
                &mut |_: &str, _| {
                    calls += 1;
                    Ok(PNG.to_vec())
                }
            )
            .is_err()
        );
        assert_eq!(calls, 0);
        assert_eq!(pool.usage().total, 0);
    }
}

#[test]
fn huge_png_dimensions_are_rejected_before_native_decode_and_leave_no_charge() {
    let mut png = PNG.to_vec();
    png[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    png[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut rows = images();
    rows[0].source_digest = Digest(Sha256::digest(&png).into());
    let pool = MemoryPool::new(MemoryBudget::new(4 * 1024 * 1024));
    assert!(
        CookedResources::load(&bytes(&rows), pool.clone(), limits(), &mut |_: &str, _| Ok(
            png.clone()
        ))
        .is_err()
    );
    assert_eq!(pool.usage().total, 0);
}
