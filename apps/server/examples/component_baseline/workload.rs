//! Shared component workload for preparation, durable-path measurements and regression.
use std::error::Error;
use torse_content::{
    ComponentField, ComponentFieldType, ComponentSchema, PersistencePolicy, compile,
    m0_fixture_pack,
};
use torse_server::{bootstrap as base, execution::*};
use torse_sim::action::register_action_module;
use torse_sim::{OrderingKey, Phase, SystemSpec, Transaction, TransactionGroup, WorldAuthority};
use torse_types::*;
use torse_world::*;
pub fn payload(records: usize, value: u32) -> ComponentFields {
    let list = if records == 0 {
        vec![ComponentValue::U32(value)]
    } else {
        (0..records)
            .map(|i| {
                ComponentValue::Record(
                    [
                        (base::key("c:a"), ComponentValue::U32(value)),
                        (base::key("c:b"), ComponentValue::I64(i as i64)),
                        (base::key("c:c"), ComponentValue::Bool(true)),
                        (
                            base::key("c:t"),
                            ComponentValue::Text("abcdefghijklmnop".into()),
                        ),
                    ]
                    .into(),
                )
            })
            .collect()
    };
    [(base::key("c:data"), ComponentValue::List(list))].into()
}
pub fn fixture(
    tasks: usize,
    records: usize,
) -> Result<(WorldAuthority, Vec<TransactionGroup>), Box<dyn Error>> {
    fixture_members(tasks, records, 1)
}
pub fn payload_for(owner: &str, records: usize, value: u32) -> ComponentFields {
    let mut fields = payload(records, value);
    let data = fields.remove(&base::key("c:data")).unwrap();
    [(base::key(&format!("{owner}:data")), data)].into()
}
pub fn fixture_members(
    tasks: usize,
    records: usize,
    members: usize,
) -> Result<(WorldAuthority, Vec<TransactionGroup>), Box<dyn Error>> {
    assert!((1..=2).contains(&members));
    let mut pack = m0_fixture_pack();
    for owner in ["c", "d"].into_iter().take(members) {
        register_action_module(
            &mut pack,
            base::key(&format!("{owner}:owner")),
            base::key(&format!("{owner}:state")),
            vec![ComponentSchema {
                id: base::key(&format!("{owner}:payload")),
                schema_version: 1,
                owner_module: base::key(&format!("{owner}:owner")),
                fields: vec![ComponentField {
                    id: base::key(&format!("{owner}:data")),
                    field_type: ComponentFieldType::List { max_items: 4096 },
                    required: true,
                }],
                persistence: PersistencePolicy::Always,
                replication: ReplicationDeclaration {
                    authority: AuthorityMode::Server,
                    visibility: ReplicationVisibility::Public,
                    update_mode: UpdateMode::CompleteTransaction,
                    presentation_hint: PresentationHint::Discrete,
                },
                migration_binding: None,
            }],
            vec![],
            vec![],
        );
    }
    let compiled = compile(pack).unwrap();
    let registry = BlockStateRegistry::build(&compiled)?;
    let schemas = ComponentSchemas::build(&compiled);
    let mut world = WorldAuthority::new(
        base::WORLD,
        registry,
        schemas.clone(),
        16,
        WorldBounds::new(0, 32)?,
        Compatibility {
            authority_schema: 4,
            rules_digest: compiled.authority_digest,
            content_digest: compiled.presentation_digest,
        },
    )?;
    for _ in 0..tasks {
        let handle = world.entities_mut().spawn(Position::new(4.0, 4.0, 4.0))?;
        for owner in ["c", "d"].into_iter().take(members) {
            world
                .entities_mut()
                .components_mut(handle)
                .unwrap()
                .insert(
                    &schemas,
                    &base::key(&format!("{owner}:payload")),
                    payload_for(owner, records, 1),
                )?;
        }
    }
    let load = Load {
        tasks,
        records,
        members,
        contended: false,
        deny_plan: false,
    };
    let groups = load.groups(&world, Tick(0));
    Ok((world, groups))
}
#[derive(Clone, Copy)]
pub struct Load {
    pub tasks: usize,
    pub records: usize,
    pub members: usize,
    pub contended: bool,
    pub deny_plan: bool,
}
impl Load {
    pub fn world(self) -> WorldAuthority {
        fixture_members(self.tasks, self.records, self.members)
            .unwrap()
            .0
    }
    pub fn rules(self) -> Vec<Box<dyn DurableRule>> {
        vec![Box::new(self)]
    }
    fn groups(self, world: &WorldAuthority, tick: Tick) -> Vec<TransactionGroup> {
        let entities: Vec<_> = world.entities().iter().collect();
        (0..self.tasks)
            .map(|index| {
                let (_, entity) = entities[if self.contended { index / 2 } else { index }];
                let ordering = OrderingKey {
                    phase: Phase::Simulation,
                    rule_priority: 0,
                    stable_task_key: index as u64,
                    local_sequence: 0,
                };
                let members = ["c", "d"]
                    .into_iter()
                    .take(self.members)
                    .enumerate()
                    .map(|(member, owner)| {
                        let schema = base::key(&format!("{owner}:payload"));
                        let mut tx = Transaction::new(
                            TransactionId(
                                u128::from(tick.0) * 1024 + index as u128 * 4 + member as u128 + 1,
                            ),
                            base::key(&format!("{owner}:owner")),
                            ordering,
                        );
                        tx.components.push(ComponentEdit {
                            id: entity.id,
                            schema: schema.clone(),
                            previous: entity.components.component(&schema).cloned(),
                            fields: Some(payload_for(owner, self.records, tick.0 as u32 + 2)),
                        });
                        tx
                    })
                    .collect();
                TransactionGroup::new(
                    TransactionId(u128::from(tick.0) * 256 + index as u128 + 1),
                    ordering,
                    members,
                )
                .unwrap()
            })
            .collect()
    }
}
impl DurableRule for Load {
    fn spec(&self) -> SystemSpec {
        SystemSpec {
            id: base::key("c:rule"),
            phase: Phase::Simulation,
            rule_priority: 0,
            before: vec![],
            after: vec![],
        }
    }
    fn plan(&self, world: &WorldAuthority, tick: Tick) -> Result<Vec<WorkPlan>, Fault> {
        assert!(!self.deny_plan, "recovery must consume frozen work");
        Ok(self
            .groups(world, tick)
            .into_iter()
            .map(|group| WorkPlan {
                transaction: Some(group),
                schedule: vec![],
                cancel: vec![],
            })
            .collect())
    }
}
// Deliberately large, explicit benchmark limits; production defaults are unchanged.
pub fn limits() -> ExecutionLimits {
    let mut limits = base::execution_limits();
    limits.runtime = OutputBudget::new(4_000_000, 268_435_456);
    limits.codec.max_frame_bytes = 67_108_864;
    limits
}
pub fn budget() -> OutputBudget {
    OutputBudget::new(8_000_000, 536_870_912)
}
pub fn config(workers: usize) -> PrevalidationConfig {
    PrevalidationConfig {
        workers,
        components: true,
        max_job_bytes: 64 * 1024 * 1024,
        ..Default::default()
    }
}
