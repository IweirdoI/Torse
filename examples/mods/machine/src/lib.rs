//! Machine mod: the P2 processing-machine example. Every wakeup advances a
//! persistent progress counter; at the fourth step it moves one stone from the
//! input chest to the output chest and resets, all in one plan that either
//! commits completely or not at all.

use bindings::exports::torse::behavior::invoke::{
    ComponentEdit, Guest, Invocation, Plan, TransferRequest, WakeupRequest,
};
use bindings::torse::behavior::types::{Atom, EntityId, Field};
use torse_mod_sdk::Rng;

mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/mod_api/wit/behavior.wit",
        world: "mod-behavior",
    });
}

const SCHEMA_KEY: &str = "machine:schema/state";
const FOREIGN_SCHEMA_KEY: &str = "inventory:schema/container";
const PROGRESS_FIELD: &str = "machine:field/progress";
const INPUT_FIELD: &str = "machine:field/input";
const OUTPUT_FIELD: &str = "machine:field/output";
const WORK_KEY: &str = "machine:tick";
const RECIPES_PER_STEP: u32 = 4;

#[allow(dead_code)]
struct Component;

fn owned_fields(invocation: &Invocation) -> Vec<Field> {
    invocation
        .self_
        .as_ref()
        .and_then(|object| object.owned.first())
        .and_then(|state| state.fields.as_ref())
        .cloned()
        .unwrap_or_default()
}

fn field_u32(fields: &[Field], key: &str) -> Option<u32> {
    fields
        .iter()
        .find(|field| field.key == key)
        .and_then(|field| match field.value {
            Atom::U32(value) => Some(value),
            _ => None,
        })
}

fn field_entity(fields: &[Field], key: &str) -> Option<EntityId> {
    fields
        .iter()
        .find(|field| field.key == key)
        .and_then(|field| match field.value {
            Atom::Entity(entity) => Some(entity),
            _ => None,
        })
}

impl Guest for Component {
    fn handle_event(invocation: Invocation) -> Option<Plan> {
        #[cfg(feature = "spin")]
        {
            let mut sink = 0_u64;
            loop {
                sink = sink.wrapping_add(std::hint::black_box(1_u64));
                std::hint::black_box(&mut sink);
            }
        }

        #[cfg(not(feature = "spin"))]
        {
            let tick = invocation.tick;
            let self_ref = invocation.self_.as_ref()?;
            let machine = self_ref.entity;
            let previous = owned_fields(&invocation);
            let progress = field_u32(&previous, PROGRESS_FIELD)?;
            let input = field_entity(&previous, INPUT_FIELD)?;
            let output = field_entity(&previous, OUTPUT_FIELD)?;
            let schema = bindings::torse::behavior::query::resolve_symbol(SCHEMA_KEY)?;

            let mut rng = Rng::new(invocation.rng_seed);
            let _work = rng.next_u64() ^ u64::from(progress);

            let (next_progress, transfers) = if progress + 1 >= RECIPES_PER_STEP {
                (
                    0,
                    vec![TransferRequest {
                        source: input,
                        source_slot: 0,
                        destination: output,
                        destination_slot: 0,
                        count: 1,
                    }],
                )
            } else {
                (progress + 1, Vec::new())
            };

            let mut next_fields = previous.clone();
            for field in &mut next_fields {
                if field.key == PROGRESS_FIELD {
                    field.value = Atom::U32(next_progress);
                }
            }

            let machine_edit = ComponentEdit {
                entity: machine,
                schema,
                previous: Some(previous),
                fields: Some(next_fields),
            };
            let mut component_edits = vec![machine_edit];
            if cfg!(feature = "intrude") {
                let foreign = bindings::torse::behavior::query::resolve_symbol(FOREIGN_SCHEMA_KEY)?;
                component_edits.push(ComponentEdit {
                    entity: input,
                    schema: foreign,
                    previous: None,
                    fields: None,
                });
            }

            Some(Plan {
                edits: Vec::new(),
                component_edits,
                transfers,
                wakeups: vec![WakeupRequest {
                    entity: machine,
                    tick: tick + 1,
                    key: WORK_KEY.to_string(),
                }],
            })
        }
    }
}

bindings::export!(Component with_types_in bindings);
