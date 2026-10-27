//! Validate public snapshot values before projection, cloning or restoration.
use crate::{
    ExchangeError, MAX_COMPONENT_BYTES, MAX_COMPONENT_INSTANCES, MAX_COMPONENT_NODES,
    MAX_SECTION_SIDE, MAX_VALUE_DEPTH, Position, Snapshot, SnapshotCells, SnapshotValue,
};
use torse_types::ContentKey;

impl Snapshot {
    /// Check canonical identities, cell references and bounded component values.
    /// This does not resolve content or require every referenced module to be installed.
    pub fn validate_structure(&self) -> Result<(), ExchangeError> {
        if self.side == 0 || self.side > MAX_SECTION_SIDE || self.bounds.min_y >= self.bounds.max_y
        {
            return Err(invalid("invalid world extent"));
        }
        self.validate_entity_allocator()?;
        ordered(self.states.iter().map(|entry| &entry.id))?;
        ordered(self.sections.iter().map(|entry| &entry.pos))?;
        ordered(self.entities.iter().map(|entry| &entry.id))?;
        let mut known = [0u64; 1024];
        known[0] = 1;
        for state in &self.states {
            if state.id.is_empty() {
                return Err(invalid("state table redefines empty"));
            }
            known[state.id.0 as usize / 64] |= 1u64 << (state.id.0 % 64);
        }
        let check_cell = |state: crate::BlockStateId| {
            if known[state.0 as usize / 64] & (1u64 << (state.0 % 64)) == 0 {
                Err(invalid("cell references an undefined state"))
            } else {
                Ok(())
            }
        };
        let cells = (self.side as usize).pow(3);
        for section in &self.sections {
            match &section.cells {
                SnapshotCells::Uniform(state) => check_cell(*state)?,
                SnapshotCells::Dense(values) => {
                    if values.len() != cells {
                        return Err(invalid("dense cell count differs from section extent"));
                    }
                    for state in values {
                        check_cell(*state)?;
                    }
                }
            }
        }
        for entity in &self.entities {
            if entity.id.0 == 0
                || Position::from_bits(entity.position_bits)
                    .section(self.side)
                    .is_none()
            {
                return Err(invalid("invalid entity identity or position"));
            }
            if entity.components.len() > MAX_COMPONENT_INSTANCES {
                return Err(invalid("component count exceeds limit"));
            }
            ordered(entity.components.iter().map(|entry| &entry.schema))?;
            for component in &entity.components {
                let mut budget = ValueBudget { nodes: 0, bytes: 8 };
                budget.entries(&component.fields, 0)?;
            }
        }
        Ok(())
    }
}

fn ordered<'a, T: Ord + 'a>(mut keys: impl Iterator<Item = &'a T>) -> Result<(), ExchangeError> {
    let Some(mut previous) = keys.next() else {
        return Ok(());
    };
    for key in keys {
        if previous >= key {
            return Err(invalid("identities must be strictly ascending"));
        }
        previous = key;
    }
    Ok(())
}
struct ValueBudget {
    nodes: usize,
    bytes: u64,
}
impl ValueBudget {
    fn bytes(&mut self, amount: u64) -> Result<(), ExchangeError> {
        self.bytes = self
            .bytes
            .checked_add(amount)
            .ok_or_else(|| invalid("component byte count overflow"))?;
        if self.bytes > MAX_COMPONENT_BYTES {
            return Err(invalid("component byte limit exceeded"));
        }
        Ok(())
    }
    fn key(&mut self, key: &ContentKey) -> Result<(), ExchangeError> {
        self.bytes((key.namespace().len() + key.path().len() + 1) as u64 + 8)
    }
    fn entries(
        &mut self,
        entries: &[(ContentKey, SnapshotValue)],
        depth: u32,
    ) -> Result<(), ExchangeError> {
        if entries.len() > MAX_COMPONENT_NODES {
            return Err(invalid("component entry limit exceeded"));
        }
        ordered(entries.iter().map(|(key, _)| key))?;
        for (key, value) in entries {
            self.key(key)?;
            self.value(value, depth)?;
        }
        Ok(())
    }
    fn value(&mut self, value: &SnapshotValue, depth: u32) -> Result<(), ExchangeError> {
        if depth > MAX_VALUE_DEPTH || self.nodes == MAX_COMPONENT_NODES {
            return Err(invalid("component node or depth limit exceeded"));
        }
        self.nodes += 1;
        self.bytes(9)?;
        match value {
            SnapshotValue::Text(text) => self.bytes(text.len() as u64)?,
            SnapshotValue::Content(key) => self.key(key)?,
            SnapshotValue::Entity(_) => self.bytes(16)?,
            SnapshotValue::List(values) => {
                if values.len() > MAX_COMPONENT_NODES {
                    return Err(invalid("component list limit exceeded"));
                }
                for value in values {
                    self.value(value, depth + 1)?;
                }
            }
            SnapshotValue::Record(entries) => self.entries(entries, depth + 1)?,
            _ => {}
        }
        Ok(())
    }
}
fn invalid(detail: &str) -> ExchangeError {
    ExchangeError::Store {
        detail: format!("invalid snapshot: {detail}"),
    }
}
