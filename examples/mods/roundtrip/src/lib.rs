//! Roundtrip mod: the smallest behavior package used by host tests to prove
//! the component boundary end to end. It behaves like a degenerate machine:
//! each wakeup counts its occupied neighborhood and steps a private counter.

use bindings::exports::torse::behavior::invoke::{
    BlockEdit, ComponentEdit, Guest, Invocation, Plan, WakeupRequest,
};
use bindings::torse::behavior::types::{Atom, Bbox, BlockRef, Cell, EntityId, Field, Vec3i};
use torse_mod_sdk::Rng;

mod bindings {
    wit_bindgen::generate!({
        path: "../../../crates/mod_api/wit/behavior.wit",
        world: "mod-behavior",
    });
}

const SCHEMA_KEY: &str = "roundtrip:counter-state";
const WORK_KEY: &str = "roundtrip:tick";

#[allow(dead_code)]
struct Component;

fn self_position(invocation: &Invocation) -> (i64, i64, i64) {
    match &invocation.self_ {
        Some(object) => (object.pos.x, object.pos.y, object.pos.z),
        None => (0, 64, 0),
    }
}

fn occupied_neighborhood(cx: i64, cy: i64, cz: i64) -> (u32, Option<Cell>) {
    let region = Bbox {
        min: Vec3i {
            x: cx - 1,
            y: cy - 1,
            z: cz - 1,
        },
        max: Vec3i {
            x: cx + 2,
            y: cy + 2,
            z: cz + 2,
        },
    };
    let samples = match bindings::torse::behavior::query::query_blocks(region) {
        Ok(samples) => samples,
        Err(_) => return (0, None),
    };
    let mut occupied = 0_u32;
    let mut above = None;
    for sample in samples {
        if let Cell::Occupied(..) = sample.cell {
            occupied += 1;
        }
        if sample.pos.x == cx && sample.pos.y == cy + 1 && sample.pos.z == cz {
            above = Some(sample.cell);
        }
    }
    (occupied, above)
}

fn current_count(invocation: &Invocation) -> Option<u32> {
    let object = invocation.self_.as_ref()?;
    let state = object.owned.first()?;
    let fields = state.fields.as_ref()?;
    let field = fields.iter().find(|field| field.key == "count")?;
    match field.value {
        Atom::U32(count) => Some(count),
        _ => None,
    }
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
            let (cx, cy, cz) = self_position(&invocation);
            let (occupied, above) = occupied_neighborhood(cx, cy, cz);
            let schema = bindings::torse::behavior::query::resolve_symbol(SCHEMA_KEY)?;

            let mut rng = Rng::new(invocation.rng_seed);
            let _work: u64 = (0..occupied)
                .map(|index| rng.next_u64() ^ (index as u64))
                .sum();

            let previous_count = current_count(&invocation).unwrap_or(0);
            let next_count = previous_count.wrapping_add(1);

            if tick.is_multiple_of(64) {
                bindings::torse::behavior::diagnostics::log(
                    bindings::torse::behavior::diagnostics::LogLevel::Info,
                    &format!("roundtrip count {next_count}"),
                );
            }

            let mut edits = Vec::new();
            if tick.is_multiple_of(8) {
                let pos = Vec3i {
                    x: cx,
                    y: cy + 1,
                    z: cz,
                };
                let (new_cell, expected) = match above {
                    Some(cell @ Cell::Occupied(..)) => (Cell::Empty, Some(cell)),
                    Some(Cell::Empty) => (
                        Cell::Occupied(BlockRef {
                            definition: schema,
                            state_values: Vec::new(),
                        }),
                        Some(Cell::Empty),
                    ),
                    _ => return None,
                };
                edits.push(BlockEdit {
                    pos,
                    expected,
                    new: new_cell,
                });
            }

            Some(Plan {
                edits,
                component_edits: vec![ComponentEdit {
                    entity: EntityId { low: 1, high: 0 },
                    schema,
                    previous: Some(vec![Field {
                        key: "count".to_string(),
                        value: Atom::U32(previous_count),
                    }]),
                    fields: Some(vec![Field {
                        key: "count".to_string(),
                        value: Atom::U32(next_count),
                    }]),
                }],
                transfers: Vec::new(),
                wakeups: vec![WakeupRequest {
                    entity: EntityId { low: 1, high: 0 },
                    tick: tick + 1,
                    key: WORK_KEY.to_string(),
                }],
            })
        }
    }
}

bindings::export!(Component with_types_in bindings);
