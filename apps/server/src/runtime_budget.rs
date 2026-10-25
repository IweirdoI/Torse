//! Cache only immutable tables; dynamic event/receipt state is validated each time.
use super::*;
use torse_codec::wire::WireCost;

#[derive(Default)]
pub(super) struct RuntimeBudget {
    rules: Option<(Arc<Vec<SystemSpec>>, WireCost)>,
    tasks: Option<(Arc<Vec<FrozenTask>>, WireCost)>,
    empty_rules: Arc<Vec<SystemSpec>>,
    empty_tasks: Arc<Vec<FrozenTask>>,
}

fn table_cost<T: wire::Wire>(
    cache: &mut Option<(Arc<T>, WireCost)>,
    value: &Arc<T>,
    limits: ExecutionLimits,
) -> Result<WireCost, Fault> {
    if let Some((old, cost)) = cache
        && Arc::ptr_eq(old, value)
    {
        return Ok(*cost);
    }
    let cost = wire::measure_inner(value, limits.runtime, limits.codec)?;
    *cache = Some((Arc::clone(value), cost));
    Ok(cost)
}

impl RuntimeBudget {
    pub(super) fn check(
        &mut self,
        state: &RuntimeState,
        limits: ExecutionLimits,
    ) -> Result<(), Fault> {
        let rules = table_cost(&mut self.rules, &state.rules, limits)?;
        let empty_rules = wire::measure_inner(&self.empty_rules, limits.runtime, limits.codec)?;
        let mut short = state.clone();
        short.rules = Arc::clone(&self.empty_rules);
        let mut additions = vec![(rules, empty_rules)];
        if let Some(work) = &mut short.work {
            let tasks = table_cost(&mut self.tasks, &work.tasks, limits)?;
            additions.push((
                tasks,
                wire::measure_inner(&self.empty_tasks, limits.runtime, limits.codec)?,
            ));
            work.tasks = Arc::clone(&self.empty_tasks);
        } else {
            self.tasks = None;
        }
        let mut total = wire::measure_inner(&short, limits.runtime, limits.codec)?;
        for (full, empty) in additions {
            total.encoded_bytes = total
                .encoded_bytes
                .checked_sub(empty.encoded_bytes)
                .and_then(|n| n.checked_add(full.encoded_bytes))
                .ok_or_else(overflow)?;
            total.owned_bytes = total
                .owned_bytes
                .checked_sub(empty.owned_bytes)
                .and_then(|n| n.checked_add(full.owned_bytes))
                .ok_or_else(overflow)?;
            total.items = total
                .items
                .checked_sub(empty.items)
                .and_then(|n| n.checked_add(full.items))
                .ok_or_else(overflow)?;
        }
        let encoded = total.encoded_bytes.checked_add(2).ok_or_else(overflow)?;
        let owned = total
            .owned_bytes
            .checked_add(size_of::<RuntimeState>() as u64)
            .ok_or_else(overflow)?;
        let items = total.items.checked_add(1).ok_or_else(overflow)?;
        if encoded
            > limits
                .runtime
                .max_bytes
                .min(u64::from(limits.codec.max_frame_bytes))
            || !limits.runtime.allows(items, owned)
        {
            return Err(Fault::resource_limit("complete runtime budget exceeded"));
        }
        Ok(())
    }
}

fn overflow() -> Fault {
    Fault::resource_limit("runtime cost arithmetic overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_cost_matches_full_wire_validation_at_capacity_boundaries_and_after_cow() {
        let mut state = RuntimeState {
            rules: vec![SystemSpec {
                id: crate::bootstrap::key("cost:rule"),
                phase: Phase::Simulation,
                rule_priority: 0,
                before: vec![],
                after: vec![],
            }]
            .into(),
            next_event: 2,
            events: vec![TimedEvent {
                id: EventId(1),
                event: EventDraft {
                    tick: Tick(3),
                    rule: crate::bootstrap::key("cost:rule"),
                    payload: [(
                        crate::bootstrap::key("cost:text"),
                        torse_world::ComponentValue::Text("x".repeat(120)),
                    )]
                    .into(),
                },
            }],
            windows: vec![],
            work: None,
        };
        let mut cache = RuntimeBudget::default();
        for count in [0, 1, 16, 128] {
            state.work = Some(PhaseWork {
                tasks: (0..count)
                    .map(|_| FrozenTask {
                        transaction: None,
                        request: None,
                        consume: None,
                        schedule: vec![],
                        cancel: vec![],
                        status: ReplyStatus::Committed,
                        output: [(
                            crate::bootstrap::key("cost:value"),
                            torse_world::ComponentValue::Text("z".repeat(40)),
                        )]
                        .into(),
                    })
                    .collect::<Vec<_>>()
                    .into(),
                next: 0,
            });
            for items in [1, 4, 16, 128, 1024, 32000] {
                for bytes in [64, 256, 512, 1024, 4096, 16384, 65536] {
                    let mut limits = crate::bootstrap::execution_limits();
                    limits.runtime = OutputBudget::new(items, bytes);
                    let mut encoded = vec![];
                    let reference =
                        wire::encode(&state, &mut encoded, limits.runtime, limits.codec)
                            .and_then(|_| {
                                wire::decode::<RuntimeState>(&encoded, limits.runtime, limits.codec)
                            })
                            .is_ok();
                    assert_eq!(
                        cache.check(&state, limits).is_ok(),
                        reference,
                        "count={count} items={items} bytes={bytes}"
                    );
                }
            }
            // Retained cache references force copy-on-write and invalidate the key.
            let work = state.work.as_mut().unwrap();
            if !work.tasks.is_empty() {
                Arc::make_mut(&mut work.tasks)[0].output.insert(
                    crate::bootstrap::key("cost:extra"),
                    torse_world::ComponentValue::Text("q".repeat(4096)),
                );
                let mut limits = crate::bootstrap::execution_limits();
                limits.runtime.max_bytes = 4096;
                assert!(cache.check(&state, limits).is_err());
            }
        }
    }
}
