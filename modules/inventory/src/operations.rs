//! Explicit, owner-authored plans for external item ingress, withdrawal and consumption.
use crate::{Inventory, ItemCatalog, ItemStack, key, owner, schema};
use std::collections::{BTreeMap, BTreeSet};
use torse_sim::action::{ActionTarget, ActionView};
use torse_sim::{OrderingKey, Phase, Transaction};
use torse_types::{EntityId, RejectCode, TransactionId};
use torse_world::{ComponentEdit, ComponentFields, component_size};

pub const MAX_OPERATIONS: usize = 64;

/// One operation over exactly observed containers. External ingress requires
/// its own trusted permission; transfer alone never grants item creation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InventoryOperation {
    Transfer(crate::Transfer),
    Insert {
        target: EntityId,
        slot: usize,
        stack: ItemStack,
    },
    Extract {
        target: EntityId,
        slot: usize,
        stack: ItemStack,
    },
    Consume {
        target: EntityId,
        slot: usize,
        stack: ItemStack,
    },
    Swap {
        left: EntityId,
        left_slot: usize,
        right: EntityId,
        right_slot: usize,
    },
}

/// No authoritative state changes until the returned transaction is committed.
/// Extracted stacks are proposed results, usable only after that commit succeeds.
pub struct InventoryPlan {
    transaction: Transaction,
    extracted: Vec<ItemStack>,
}
impl InventoryPlan {
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    pub fn extracted(&self) -> &[ItemStack] {
        &self.extracted
    }
    pub fn into_parts(self) -> (Transaction, Vec<ItemStack>) {
        (self.transaction, self.extracted)
    }
}

/// Validate all operations against exact expected components, then produce one
/// replacement per touched container. Repeated operations share private draft
/// state; partial failure discards those drafts without touching the world.
pub fn plan_inventory(
    view: &ActionView<'_>,
    catalog: &ItemCatalog,
    operations: &[InventoryOperation],
    expected: &BTreeMap<EntityId, ComponentFields>,
    transaction: TransactionId,
    ordering: OrderingKey,
) -> Result<InventoryPlan, RejectCode> {
    if operations.is_empty() || operations.len() > MAX_OPERATIONS {
        return Err(RejectCode::QuotaExceeded);
    }
    if !matches!(ordering.phase, Phase::Interaction | Phase::Simulation) {
        return Err(RejectCode::InvalidData);
    }
    let mut targets = BTreeSet::new();
    for operation in operations {
        let permission = match operation {
            InventoryOperation::Transfer(transfer) => {
                targets.insert(transfer.source);
                targets.insert(transfer.destination);
                "transfer"
            }
            InventoryOperation::Insert { target, .. } => {
                targets.insert(*target);
                "insert"
            }
            InventoryOperation::Extract { target, .. } => {
                targets.insert(*target);
                "extract"
            }
            InventoryOperation::Consume { target, .. } => {
                targets.insert(*target);
                "consume"
            }
            InventoryOperation::Swap { left, right, .. } => {
                targets.insert(*left);
                targets.insert(*right);
                "swap"
            }
        };
        if !view
            .actor()
            .permits(&key(&format!("inventory:permission/{permission}")))
        {
            return Err(RejectCode::PermissionDenied);
        }
    }
    if targets.len() > MAX_SLOTS_TOUCHED || expected.len() != targets.len() {
        return Err(RejectCode::InvalidData);
    }
    let mut drafts = BTreeMap::new();
    for target in targets {
        if !view.actor().allows(ActionTarget::Entity(target)) {
            return Err(RejectCode::PermissionDenied);
        }
        let observed = view.component(target, &schema())?;
        let expected = expected.get(&target).ok_or(RejectCode::InvalidData)?;
        component_size(expected).map_err(|_| RejectCode::QuotaExceeded)?;
        if observed != expected {
            return Err(RejectCode::Conflict);
        }
        drafts.insert(
            target,
            (observed, Inventory::from_fields(observed, catalog)?),
        );
    }
    let mut extracted = Vec::new();
    for operation in operations {
        match operation {
            InventoryOperation::Transfer(transfer) => {
                let crate::Transfer {
                    source,
                    source_slot,
                    destination,
                    destination_slot,
                    count,
                } = *transfer;
                if count == 0 || (source == destination && source_slot == destination_slot) {
                    return Err(RejectCode::InvalidData);
                }
                let stack = drafts[&source]
                    .1
                    .slots()
                    .get(source_slot)
                    .ok_or(RejectCode::InvalidData)?
                    .as_ref()
                    .ok_or(RejectCode::Conflict)?
                    .clone();
                if stack.count < count {
                    return Err(RejectCode::Conflict);
                }
                let existing = drafts[&destination]
                    .1
                    .slots()
                    .get(destination_slot)
                    .ok_or(RejectCode::InvalidData)?;
                let old = match existing {
                    None => 0,
                    Some(other) if other.item == stack.item => other.count,
                    Some(_) => return Err(RejectCode::Conflict),
                };
                let limit = catalog.limit(&stack.item)?;
                let new = old
                    .checked_add(count)
                    .filter(|total| *total <= limit)
                    .ok_or(RejectCode::Conflict)?;
                drafts.get_mut(&source).expect("observed source").1.set(
                    source_slot,
                    (stack.count > count).then(|| ItemStack {
                        item: stack.item.clone(),
                        count: stack.count - count,
                    }),
                    catalog,
                )?;
                drafts
                    .get_mut(&destination)
                    .expect("observed destination")
                    .1
                    .set(
                        destination_slot,
                        Some(ItemStack {
                            item: stack.item,
                            count: new,
                        }),
                        catalog,
                    )?;
            }
            InventoryOperation::Insert {
                target,
                slot,
                stack,
            } => {
                if stack.count == 0 {
                    return Err(RejectCode::InvalidData);
                }
                let limit = catalog.limit(&stack.item)?;
                let inventory = &mut drafts.get_mut(target).expect("observed target").1;
                let count = match inventory
                    .slots()
                    .get(*slot)
                    .ok_or(RejectCode::InvalidData)?
                {
                    None => 0,
                    Some(old) if old.item == stack.item => old.count,
                    _ => return Err(RejectCode::Conflict),
                };
                let count = count
                    .checked_add(stack.count)
                    .filter(|count| *count <= limit)
                    .ok_or(RejectCode::Conflict)?;
                inventory.set(
                    *slot,
                    Some(ItemStack {
                        item: stack.item.clone(),
                        count,
                    }),
                    catalog,
                )?;
            }
            InventoryOperation::Extract {
                target,
                slot,
                stack,
            }
            | InventoryOperation::Consume {
                target,
                slot,
                stack,
            } => {
                if stack.count == 0 {
                    return Err(RejectCode::InvalidData);
                }
                catalog.limit(&stack.item)?;
                let inventory = &mut drafts.get_mut(target).expect("observed target").1;
                let old = inventory
                    .slots()
                    .get(*slot)
                    .ok_or(RejectCode::InvalidData)?
                    .as_ref()
                    .ok_or(RejectCode::Conflict)?;
                if old.item != stack.item || old.count < stack.count {
                    return Err(RejectCode::Conflict);
                }
                let remaining = old.count - stack.count;
                inventory.set(
                    *slot,
                    (remaining > 0).then(|| ItemStack {
                        item: stack.item.clone(),
                        count: remaining,
                    }),
                    catalog,
                )?;
                if matches!(operation, InventoryOperation::Extract { .. }) {
                    extracted.push(stack.clone());
                }
            }
            InventoryOperation::Swap {
                left,
                left_slot,
                right,
                right_slot,
            } => {
                if left == right && left_slot == right_slot {
                    return Err(RejectCode::InvalidData);
                }
                let a = drafts[left]
                    .1
                    .slots()
                    .get(*left_slot)
                    .ok_or(RejectCode::InvalidData)?
                    .clone();
                let b = drafts[right]
                    .1
                    .slots()
                    .get(*right_slot)
                    .ok_or(RejectCode::InvalidData)?
                    .clone();
                drafts
                    .get_mut(left)
                    .expect("observed left")
                    .1
                    .set(*left_slot, b, catalog)?;
                drafts
                    .get_mut(right)
                    .expect("observed right")
                    .1
                    .set(*right_slot, a, catalog)?;
            }
        }
    }
    let mut transaction = Transaction::new(transaction, owner(), ordering);
    for (id, (previous, inventory)) in drafts {
        let fields = inventory.fields();
        if &fields != previous {
            transaction.components.push(ComponentEdit {
                id,
                schema: schema(),
                previous: Some(previous.clone()),
                fields: Some(fields),
            });
        }
    }
    Ok(InventoryPlan {
        transaction,
        extracted,
    })
}
const MAX_SLOTS_TOUCHED: usize = 64;
