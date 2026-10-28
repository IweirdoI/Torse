mod support;
use support::Directory;
use torse_server::{
    bootstrap as base,
    execution::{ExecutionHost, ExecutionRecovery},
};

#[test]
fn changed_item_limit_rejects_existing_directory_before_recovery() {
    let dir = Directory::new();
    let (world, service) = base::world().unwrap();
    let mut server = base::Files::create_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            ExecutionHost::new(
                binding,
                world,
                service,
                Vec::new(),
                base::execution_limits(),
            )
        },
    )
    .unwrap();
    server.checkpoint().unwrap();
    drop(server);
    let (world, service) = base::world_with_catalog(
        torse_inventory::ItemCatalog::new([(base::key("base:stone"), 63)]).unwrap(),
    )
    .unwrap();
    let result = base::Files::open_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                service,
                Vec::new(),
                base::execution_limits(),
            ))
        },
    );
    assert!(
        result.is_err(),
        "different frozen item rules must not resume the old world"
    );
    drop(result);
    let (world, service) = base::world().unwrap();
    let result = base::Files::open_file(
        dir.acquire(),
        world.coordinator().compatibility(),
        base::Codec::new(base::codec_limits()).unwrap(),
        base::file_policy(),
        |binding| {
            Ok(ExecutionRecovery::new(
                binding,
                world.registry(),
                world.schemas(),
                service,
                Vec::new(),
                base::execution_limits(),
            ))
        },
    );
    assert!(
        result.is_ok(),
        "failed compatibility admission must preserve the directory"
    );
}
