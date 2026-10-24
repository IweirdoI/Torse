//! Stable, bounded execution vocabulary shared with the journal and protocol.
//! Frozen tasks contain stable content/identity references, never runtime handles.
use crate::action::ActionTarget;
use crate::{OrderingKey, Phase, Transaction, WorldAuthority};
use std::collections::BTreeMap;
use torse_types::*;
use torse_world::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Call {
    pub action: ContentKey,
    pub version: u32,
    pub target: ActionTarget,
    pub parameters: ComponentFields,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyStatus {
    Committed,
    ReadOnly,
    Rejected(RejectCode),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub status: ReplyStatus,
    pub fields: ComponentFields,
    pub at: CommitCursor,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub id: RequestId,
    pub input: Call,
    pub reply: Reply,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    pub epoch: WorldEpoch,
    pub session: SessionId,
    pub high: u64,
    pub receipts: Vec<Receipt>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDraft {
    pub tick: Tick,
    pub rule: ContentKey,
    pub payload: ComponentFields,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimedEvent {
    pub id: EventId,
    pub event: EventDraft,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableCell {
    pub pos: BlockPos,
    pub content: CellContent,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableMove {
    pub id: EntityId,
    pub previous: [u64; 3],
    pub delta: [u64; 3],
    pub half: [u64; 3],
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StableSpawn {
    pub position: [u64; 3],
    pub components: BTreeMap<ContentKey, ComponentFields>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredTransaction {
    pub id: TransactionId,
    pub source: ContentKey,
    pub phase: u16,
    pub priority: i32,
    pub task_key: u64,
    pub local: u32,
    pub reads: Vec<StableCell>,
    pub writes: Vec<StableCell>,
    pub moves: Vec<StableMove>,
    pub components: Vec<ComponentEdit>,
    pub spawns: Vec<StableSpawn>,
    pub despawns: Vec<EntityState>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Accepted {
    pub id: RequestId,
    pub input: Call,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenTask {
    pub transaction: Option<StoredTransaction>,
    pub request: Option<Accepted>,
    pub consume: Option<EventId>,
    pub schedule: Vec<EventDraft>,
    pub cancel: Vec<EventId>,
    pub status: ReplyStatus,
    pub output: ComponentFields,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseWork {
    pub tasks: Vec<FrozenTask>,
    pub next: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeState {
    pub rules: Vec<crate::SystemSpec>,
    pub next_event: u128,
    pub events: Vec<TimedEvent>,
    pub windows: Vec<Window>,
    pub work: Option<PhaseWork>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionSnapshot {
    pub world: Snapshot,
    pub runtime: RuntimeState,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionChanges {
    pub world: ChangeSet,
    pub runtime: RuntimeState,
    pub outcome: Option<ReplyStatus>,
    pub cancelled: Option<Accepted>,
}

fn stable(registry: &BlockStateRegistry, id: BlockStateId) -> Result<CellContent, Fault> {
    if id.is_empty() {
        return Ok(CellContent::Empty);
    }
    Ok(CellContent::Block {
        block: registry
            .definition_key(id)
            .ok_or_else(|| Fault::invalid_data("unknown task state"))?
            .clone(),
        values: registry.state_values(id).unwrap_or(&[]).to_vec(),
    })
}
fn resolve(registry: &BlockStateRegistry, value: &CellContent) -> Result<BlockStateId, RejectCode> {
    match value {
        CellContent::Empty => Ok(BlockStateId::EMPTY),
        CellContent::Block { block, values } => registry
            .state_of(block, values)
            .ok_or(RejectCode::InvalidData),
    }
}
impl StoredTransaction {
    pub fn capture(world: &WorldAuthority, tx: &Transaction) -> Result<Self, Fault> {
        Ok(Self {
            id: tx.id,
            source: tx.source.clone(),
            phase: tx.ordering.phase.index_u16(),
            priority: tx.ordering.rule_priority,
            task_key: tx.ordering.stable_task_key,
            local: tx.ordering.local_sequence,
            reads: tx
                .preconditions
                .iter()
                .map(|r| {
                    Ok(StableCell {
                        pos: r.pos,
                        content: stable(world.registry(), r.expected)?,
                    })
                })
                .collect::<Result<_, Fault>>()?,
            writes: tx
                .mutations
                .iter()
                .map(|r| {
                    Ok(StableCell {
                        pos: r.pos,
                        content: stable(world.registry(), r.state)?,
                    })
                })
                .collect::<Result<_, Fault>>()?,
            moves: tx
                .movements
                .iter()
                .map(|m| StableMove {
                    id: m.id,
                    previous: m.previous_bits,
                    delta: m.delta_bits,
                    half: m.half_bits,
                })
                .collect(),
            components: tx.components.clone(),
            spawns: tx
                .spawns
                .iter()
                .map(|s| StableSpawn {
                    position: s.position_bits,
                    components: s.components.clone(),
                })
                .collect(),
            despawns: tx.despawns.iter().map(|s| s.previous.clone()).collect(),
        })
    }
    pub fn restore(&self, world: &WorldAuthority) -> Result<Transaction, RejectCode> {
        let mut tx = Transaction::new(
            self.id,
            self.source.clone(),
            OrderingKey {
                phase: Phase::from_index(self.phase).ok_or(RejectCode::InvalidData)?,
                rule_priority: self.priority,
                stable_task_key: self.task_key,
                local_sequence: self.local,
            },
        );
        for r in &self.reads {
            tx.read_block(r.pos, resolve(world.registry(), &r.content)?);
        }
        for r in &self.writes {
            tx.write_block(r.pos, resolve(world.registry(), &r.content)?);
        }
        for m in &self.moves {
            let handle = world
                .entities()
                .handle_of(m.id)
                .ok_or(RejectCode::MissingTarget)?;
            tx.move_entity(
                handle,
                m.id,
                Position::from_bits(m.previous),
                m.delta.map(f64::from_bits),
                m.half.map(f64::from_bits),
            );
        }
        tx.components = self.components.clone();
        for s in &self.spawns {
            tx.spawn_entity(Position::from_bits(s.position), s.components.clone());
        }
        for s in &self.despawns {
            let handle = world
                .entities()
                .handle_of(s.id)
                .ok_or(RejectCode::MissingTarget)?;
            tx.despawns.push(crate::transaction::Despawn {
                handle,
                previous: s.clone(),
            });
        }
        Ok(tx)
    }
}
