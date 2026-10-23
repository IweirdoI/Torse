//! Item-port planning; both inventory endpoints publish in one transaction.

use crate::{Inventory, ItemCatalog, ItemStack, key, owner, schema};
use std::sync::Arc;
use torse_sim::action::{
    ActionHandler, ActionOutput, ActionRequest, ActionTarget, ActionView, NativeHandler,
};
use torse_sim::{OrderingKey, Phase, Transaction};
use torse_types::{EntityId, RejectCode, TransactionId};
use torse_world::{ComponentEdit, ComponentFields, ComponentValue};

/// Bind query and transfer handlers to one immutable, game-supplied item catalog.
pub fn handlers(catalog: ItemCatalog) -> Vec<NativeHandler> {
    let catalog = Arc::new(catalog);
    [false, true]
        .into_iter()
        .map(|transfer| NativeHandler {
            handler: key(if transfer {
                "inventory:handler/transfer"
            } else {
                "inventory:handler/inspect"
            }),
            owner: owner(),
            implementation: Box::new(PortHandler {
                catalog: catalog.clone(),
                transfer,
            }),
        })
        .collect()
}

struct PortHandler {
    catalog: Arc<ItemCatalog>,
    transfer: bool,
}

fn read<'a>(view: &'a ActionView<'_>, id: EntityId) -> Result<&'a ComponentFields, RejectCode> {
    view.component(id, &schema())
}

fn number(fields: &ComponentFields, name: &str) -> Result<u32, RejectCode> {
    match fields.get(&key(&format!("inventory:field/{name}"))) {
        Some(ComponentValue::U32(value)) => Ok(*value),
        _ => Err(RejectCode::InvalidData),
    }
}

/// Typed item-port operation shared by interactive actions and scheduled machines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transfer {
    pub source: EntityId,
    pub source_slot: usize,
    pub destination: EntityId,
    pub destination_slot: usize,
    pub count: u32,
}

impl ActionHandler for PortHandler {
    fn plan(
        &self,
        view: &ActionView<'_>,
        request: &ActionRequest,
    ) -> Result<ActionOutput, RejectCode> {
        let ActionTarget::Entity(source) = request.target else {
            return Err(RejectCode::InvalidData);
        };
        if !self.transfer {
            return Ok(ActionOutput::Query(
                Inventory::from_fields(read(view, source)?, &self.catalog)?.fields(),
            ));
        }
        let Some(ComponentValue::Entity(destination)) =
            request.parameters.get(&key("inventory:field/destination"))
        else {
            return Err(RejectCode::InvalidData);
        };
        plan_transfer(
            view,
            &self.catalog,
            Transfer {
                source,
                destination: *destination,
                source_slot: number(&request.parameters, "source_slot")? as usize,
                destination_slot: number(&request.parameters, "destination_slot")? as usize,
                count: number(&request.parameters, "count")?,
            },
            request.transaction,
            request.ordering,
        )
        .map(ActionOutput::Mutation)
    }
}

/// Plan a bounded transfer at the interaction or simulation phase.
///
/// A native machine uses a host-issued view and typed arguments directly; it
/// does not format a command or bypass permission, capacity, or commit checks.
pub fn plan_transfer(
    view: &ActionView<'_>,
    catalog: &ItemCatalog,
    transfer: Transfer,
    transaction: TransactionId,
    ordering: OrderingKey,
) -> Result<Transaction, RejectCode> {
    if !view.actor().permits(&key("inventory:permission/transfer"))
        || !view.actor().allows(ActionTarget::Entity(transfer.source))
        || !view
            .actor()
            .allows(ActionTarget::Entity(transfer.destination))
    {
        return Err(RejectCode::PermissionDenied);
    }
    if !matches!(ordering.phase, Phase::Interaction | Phase::Simulation) {
        return Err(RejectCode::InvalidData);
    }
    let Transfer {
        source,
        source_slot: from,
        destination,
        destination_slot: to,
        count,
    } = transfer;
    if count == 0 || (source == destination && from == to) {
        return Err(RejectCode::InvalidData);
    }
    let original = read(view, source)?;
    let mut input = Inventory::from_fields(original, catalog)?;
    let stack = input
        .slots()
        .get(from)
        .ok_or(RejectCode::InvalidData)?
        .as_ref()
        .ok_or(RejectCode::Conflict)?
        .clone();
    if stack.count < count {
        return Err(RejectCode::Conflict);
    }
    let destination_fields = read(view, destination)?;
    let mut output = Inventory::from_fields(destination_fields, catalog)?;
    let target = output.slots().get(to).ok_or(RejectCode::InvalidData)?;
    let target_count = match target {
        None => 0,
        Some(existing) if existing.item == stack.item => existing.count,
        Some(_) => return Err(RejectCode::Conflict),
    };
    let next_count = target_count
        .checked_add(count)
        .ok_or(RejectCode::Conflict)?;
    if next_count > catalog.limit(&stack.item)? {
        return Err(RejectCode::Conflict);
    }
    let remaining = if stack.count == count {
        None
    } else {
        Some(ItemStack {
            item: stack.item.clone(),
            count: stack.count - count,
        })
    };
    let added = Some(ItemStack {
        item: stack.item,
        count: next_count,
    });
    input.set(from, remaining, catalog)?;
    let mut tx = Transaction::new(transaction, owner(), ordering);
    if source == destination {
        input.set(to, added, catalog)?;
    } else {
        output.set(to, added, catalog)?;
        tx.components.push(ComponentEdit {
            id: destination,
            schema: schema(),
            previous: Some(destination_fields.clone()),
            fields: Some(output.fields()),
        });
    }
    tx.components.push(ComponentEdit {
        id: source,
        schema: schema(),
        previous: Some(original.clone()),
        fields: Some(input.fields()),
    });
    Ok(tx)
}
