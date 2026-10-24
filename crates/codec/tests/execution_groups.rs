//! Frozen multi-source task compatibility, corruption, and budget checks.
use torse_codec::{
    CodecLimits, Encoder,
    wire::{self, Wire},
};
use torse_sim::durable::{
    FrozenTask, ReplyStatus, StableCell, StoredTransaction, StoredTransactionGroup,
};
use torse_types::{BlockPos, ContentKey, OutputBudget, TransactionId};
use torse_world::{CellContent, ComponentFields};

const LIMITS: CodecLimits = CodecLimits {
    max_frame_bytes: 65536,
    max_field_bytes: 4096,
    max_depth: 32,
};
const BUDGET: OutputBudget = OutputBudget {
    max_items: 4096,
    max_bytes: 65536,
};

fn member(source: &str, x: i64) -> StoredTransaction {
    StoredTransaction {
        id: TransactionId(7),
        source: ContentKey::parse(source).unwrap(),
        phase: 3,
        priority: 0,
        task_key: 42,
        local: 0,
        reads: Vec::new(),
        writes: vec![StableCell {
            pos: BlockPos::new(x, 0, 0),
            content: CellContent::Empty,
        }],
        moves: Vec::new(),
        components: Vec::new(),
        spawns: Vec::new(),
        despawns: Vec::new(),
    }
}
fn task(members: Vec<StoredTransaction>) -> FrozenTask {
    FrozenTask {
        transaction: Some(StoredTransactionGroup {
            id: TransactionId(7),
            members,
        }),
        request: None,
        consume: None,
        schedule: Vec::new(),
        cancel: Vec::new(),
        status: ReplyStatus::Committed,
        output: ComponentFields::new(),
    }
}
fn raw_group(members: &[StoredTransaction]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut e = Encoder::new(&mut bytes, BUDGET, LIMITS);
    1u16.write(&mut e).unwrap();
    2u8.write(&mut e).unwrap();
    TransactionId(7).write(&mut e).unwrap();
    members.to_vec().write(&mut e).unwrap();
    // request, consume, schedule, cancel, status, output in their v1 order.
    0u8.write(&mut e).unwrap();
    0u8.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    0u8.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    e.finish();
    bytes
}

#[test]
fn legacy_single_member_bytes_remain_readable_and_unchanged() {
    let t = task(vec![member("machine:module", 0)]);
    let mut old = Vec::new();
    let mut e = Encoder::new(&mut old, BUDGET, LIMITS);
    1u16.write(&mut e).unwrap();
    1u8.write(&mut e).unwrap();
    t.transaction.as_ref().unwrap().members[0]
        .write(&mut e)
        .unwrap();
    0u8.write(&mut e).unwrap();
    0u8.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    0u8.write(&mut e).unwrap();
    0u32.write(&mut e).unwrap();
    e.finish();
    assert_eq!(wire::decode::<FrozenTask>(&old, BUDGET, LIMITS).unwrap(), t);
    let mut encoded = Vec::new();
    wire::encode(&t, &mut encoded, BUDGET, LIMITS).unwrap();
    assert_eq!(encoded, old);
}

#[test]
fn group_roundtrip_truncation_and_budget_rollback() {
    let t = task(vec![
        member("inventory:module", 0),
        member("machine:module", 1),
    ]);
    let mut bytes = Vec::new();
    wire::encode(&t, &mut bytes, BUDGET, LIMITS).unwrap();
    assert_eq!(bytes[2], 2);
    assert_eq!(
        wire::decode::<FrozenTask>(&bytes, BUDGET, LIMITS).unwrap(),
        t
    );
    for end in 0..bytes.len() {
        assert!(wire::decode::<FrozenTask>(&bytes[..end], BUDGET, LIMITS).is_err());
    }
    let mut out = vec![9];
    assert!(
        wire::encode(
            &t,
            &mut out,
            OutputBudget {
                max_bytes: 12,
                ..BUDGET
            },
            LIMITS
        )
        .is_err()
    );
    assert_eq!(out, [9]);
    assert!(
        wire::decode::<FrozenTask>(
            &bytes,
            OutputBudget {
                max_items: 1,
                ..BUDGET
            },
            LIMITS
        )
        .is_err()
    );
}

#[test]
fn malformed_groups_reject_before_recovery() {
    let inventory = member("inventory:module", 0);
    let machine = member("machine:module", 1);
    let mut overlap = machine.clone();
    overlap.writes[0].pos = BlockPos::new(0, 0, 0);
    let mut ordering = machine.clone();
    ordering.local = 3;
    for members in [
        vec![],
        vec![inventory.clone(); 5],
        vec![machine, inventory.clone()],
        vec![inventory.clone(), inventory.clone()],
        vec![inventory.clone(), overlap],
        vec![inventory, ordering],
    ] {
        let bytes = raw_group(&members);
        assert!(wire::decode::<FrozenTask>(&bytes, BUDGET, LIMITS).is_err());
        let mut out = vec![9];
        assert!(wire::encode(&task(members), &mut out, BUDGET, LIMITS).is_err());
        assert_eq!(out, [9]);
    }
}
