//! Invocation-local host work and store-wide guest resource limits.

use crate::component::CallBudget;
use wasmtime::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

#[derive(Clone, Copy, Debug)]
pub(crate) struct BudgetExceeded;

/// A failed reservation poisons the invocation, including plans that ignore
/// an import's budget error. Counters reset only at an invocation boundary.
pub(crate) struct HostBudget {
    pub limits: CallBudget,
    calls: u32,
    work: u64,
    input: u64,
    output: u64,
    pub logs: u32,
    exceeded: bool,
}

impl HostBudget {
    pub fn new(limits: CallBudget) -> Self {
        Self {
            limits,
            calls: 0,
            work: 0,
            input: 0,
            output: 0,
            logs: 0,
            exceeded: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.limits);
    }

    pub fn exceeded(&self) -> bool {
        self.exceeded
    }

    pub fn reject<T>(&mut self) -> Result<T, BudgetExceeded> {
        self.exceeded = true;
        Err(BudgetExceeded)
    }

    pub fn enter(&mut self, bytes: u64) -> Result<(), BudgetExceeded> {
        if self.calls >= self.limits.max_host_calls {
            return self.reject();
        }
        self.calls += 1;
        self.input(bytes)
    }

    pub fn work(&mut self, units: u64) -> Result<(), BudgetExceeded> {
        match self.work.checked_add(units) {
            Some(total) if !self.exceeded && total <= self.limits.max_host_work => {
                self.work = total;
                Ok(())
            }
            _ => self.reject(),
        }
    }

    pub fn input(&mut self, bytes: u64) -> Result<(), BudgetExceeded> {
        match self.input.checked_add(bytes) {
            Some(total) if !self.exceeded && total <= self.limits.max_input_bytes => {
                self.input = total;
                Ok(())
            }
            _ => self.reject(),
        }
    }

    pub fn output(&mut self, bytes: u64) -> Result<(), BudgetExceeded> {
        match self.output.checked_add(bytes) {
            Some(total) if !self.exceeded && total <= self.limits.max_output_bytes => {
                self.output = total;
                Ok(())
            }
            _ => self.reject(),
        }
    }

    pub fn region(
        &mut self,
        min: [i64; 3],
        max: [i64; 3],
        cap: u32,
    ) -> Result<u32, BudgetExceeded> {
        self.enter(48)?;
        // Test all axes before subtracting or entering even the outer loop.
        if (0..3).any(|axis| max[axis] <= min[axis]) {
            return Ok(0);
        }
        let mut volume = 1_u64;
        for axis in 0..3 {
            volume = match volume.checked_mul(max[axis].abs_diff(min[axis])) {
                Some(value) if value <= u64::from(cap.min(self.limits.max_cells_per_query)) => {
                    value
                }
                _ => return self.reject(),
            };
        }
        self.work(volume)?;
        Ok(volume as u32)
    }
}

/// StoreLimits caps object counts; this limiter additionally caps the sum of
/// all linear memories and tables in a store, including component helpers.
pub(crate) struct GuestLimits {
    counts: StoreLimits,
    memory: usize,
    tables: usize,
    budget: CallBudget,
}

impl GuestLimits {
    pub fn new(budget: CallBudget) -> Self {
        Self {
            counts: StoreLimitsBuilder::new()
                .instances(budget.max_guest_instances)
                .memories(budget.max_guest_memories)
                .tables(budget.max_guest_tables)
                .build(),
            memory: 0,
            tables: 0,
            budget,
        }
    }
}

impl ResourceLimiter for GuestLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = self.memory.checked_add(desired.saturating_sub(current));
        match total {
            Some(total)
                if total <= self.budget.max_guest_memory_bytes
                    && maximum.is_none_or(|max| desired <= max) =>
            {
                self.memory = total;
                Ok(true)
            }
            _ => Err(wasmtime::Error::msg("guest memory budget exceeded")),
        }
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = self.tables.checked_add(desired.saturating_sub(current));
        match total {
            Some(total)
                if total <= self.budget.max_guest_table_elements
                    && maximum.is_none_or(|max| desired <= max) =>
            {
                self.tables = total;
                Ok(true)
            }
            _ => Err(wasmtime::Error::msg("guest table budget exceeded")),
        }
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        Err(error)
    }
    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        Err(error)
    }
    fn instances(&self) -> usize {
        self.counts.instances()
    }
    fn memories(&self) -> usize {
        self.counts.memories()
    }
    fn tables(&self) -> usize {
        self.counts.tables()
    }
}

// Bindgen creates distinct Rust types per world. Keep the size accounting
// identical in the fixture, authority, and durable snapshot bridges.
macro_rules! boundary_sizes {
    () => {
        fn fields_size(fields: &[Field]) -> u64 {
            fields.iter().fold(0_u64, |size, field| {
                size.saturating_add(std::mem::size_of::<Field>() as u64)
                    .saturating_add(field.key.len() as u64)
                    .saturating_add(match &field.value {
                        Atom::Text(text) => text.len() as u64,
                        _ => 0,
                    })
            })
        }

        fn cell_size(cell: &Cell) -> u64 {
            (std::mem::size_of::<Cell>() as u64).saturating_add(match cell {
                Cell::Occupied(block) => (block.state_values.len() as u64).saturating_mul(2),
                _ => 0,
            })
        }

        fn plan_size(plan: &Plan) -> u64 {
            let mut bytes = std::mem::size_of::<Plan>() as u64;
            for edit in &plan.edits {
                bytes = bytes
                    .saturating_add(64)
                    .saturating_add(cell_size(&edit.new))
                    .saturating_add(edit.expected.as_ref().map(cell_size).unwrap_or(0));
            }
            for edit in &plan.component_edits {
                bytes = bytes
                    .saturating_add(96)
                    .saturating_add(edit.previous.as_deref().map(fields_size).unwrap_or(0))
                    .saturating_add(edit.fields.as_deref().map(fields_size).unwrap_or(0));
            }
            bytes = bytes.saturating_add((plan.transfers.len() as u64).saturating_mul(64));
            for wakeup in &plan.wakeups {
                bytes = bytes
                    .saturating_add(48)
                    .saturating_add(wakeup.key.len() as u64);
            }
            bytes
        }

        fn invocation_size(invocation: &Invocation) -> u64 {
            let mut bytes = 128_u64;
            match &invocation.event {
                Event::Wakeup(target) => bytes = bytes.saturating_add(target.key.len() as u64),
                Event::BlockBroken(cell) => bytes = bytes.saturating_add(cell_size(cell)),
                _ => {}
            }
            if let Some(object) = &invocation.self_ {
                for owned in &object.owned {
                    bytes = bytes
                        .saturating_add(32)
                        .saturating_add(owned.fields.as_deref().map(fields_size).unwrap_or(0));
                }
            }
            bytes
        }
    };
}

pub(crate) use boundary_sizes;

#[cfg(feature = "authority")]
pub(crate) fn flat_source_size(fields: &torse_world::ComponentFields) -> Option<u64> {
    if fields.values().any(|value| {
        matches!(
            value,
            torse_world::ComponentValue::List(_) | torse_world::ComponentValue::Record(_)
        )
    }) {
        return None;
    }
    torse_world::component_size(fields)
        .ok()?
        .checked_add((fields.len() as u64).checked_mul(128)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmtime::{Instance, Module, Store};

    #[test]
    fn extreme_and_empty_regions_are_bounded_before_iteration() {
        let mut budget = HostBudget::new(CallBudget::prototype());
        assert!(
            budget
                .region([0, 0, 0], [1_i64 << 32, 1_i64 << 32, 1], 4096)
                .is_err()
        );
        budget.reset();
        assert!(
            budget
                .region([i64::MIN, 0, 0], [i64::MAX, 1, 1], 4096)
                .is_err()
        );
        budget.reset();
        assert_eq!(
            budget
                .region([0, 0, 0], [0, i64::MAX, i64::MAX], 4096)
                .unwrap(),
            0
        );
        assert_eq!(
            budget
                .region([0, 1, 0], [i64::MAX, 0, i64::MAX], 4096)
                .unwrap(),
            0
        );
        assert_eq!(
            budget
                .region([i64::MIN, 0, 0], [i64::MIN + 1, 1, 1], 4096)
                .unwrap(),
            1
        );
    }

    #[test]
    fn repeated_small_queries_exhaust_the_invocation_and_reset_recovers() {
        let mut budget = HostBudget::new(CallBudget {
            max_host_work: 2,
            ..CallBudget::prototype()
        });
        assert_eq!(budget.region([0; 3], [1; 3], 4096).unwrap(), 1);
        assert_eq!(budget.region([0; 3], [1; 3], 4096).unwrap(), 1);
        assert!(budget.region([0; 3], [1; 3], 4096).is_err());
        assert!(
            budget.input(0).is_err(),
            "ignoring an import error must not admit a plan"
        );
        budget.reset();
        assert_eq!(budget.region([0; 3], [1; 3], 4096).unwrap(), 1);
    }

    #[test]
    fn cumulative_bytes_and_calls_do_not_wrap() {
        let mut budget = HostBudget::new(CallBudget {
            max_host_calls: 1,
            max_input_bytes: 4,
            max_output_bytes: 4,
            ..CallBudget::prototype()
        });
        budget.enter(4).unwrap();
        assert!(budget.enter(0).is_err());
        budget.reset();
        assert!(budget.input(u64::MAX).is_err());
        budget.reset();
        budget.output(4).unwrap();
        assert!(budget.output(1).is_err());
        budget.reset();
        assert!(budget.input(5).is_err());
    }

    #[test]
    fn actual_guest_memory_and_table_growth_trap_at_store_totals() {
        let engine = crate::component::engine_with(false).unwrap();
        let module = Module::new(
            &engine,
            r#"(module
            (memory (export "first") 1)
            (memory (export "second") 1)
            (table (export "a") 1 funcref)
            (table (export "b") 1 funcref)
            (func (export "grow-memory") (param i32) (result i32)
                local.get 0 memory.grow 0)
            (func (export "grow-table") (param i32) (result i32)
                ref.null func local.get 0 table.grow 0))"#,
        )
        .unwrap();
        let mut store = Store::new(
            &engine,
            GuestLimits::new(CallBudget {
                max_guest_memory_bytes: 3 * 65536,
                max_guest_table_elements: 3,
                ..CallBudget::prototype()
            }),
        );
        store.limiter(|limits| limits);
        store.set_fuel(10000).unwrap();
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let grow_memory = instance
            .get_typed_func::<i32, i32>(&mut store, "grow-memory")
            .unwrap();
        assert_eq!(grow_memory.call(&mut store, 1).unwrap(), 1);
        assert!(grow_memory.call(&mut store, 1).is_err());
        let grow_table = instance
            .get_typed_func::<i32, i32>(&mut store, "grow-table")
            .unwrap();
        assert_eq!(grow_table.call(&mut store, 1).unwrap(), 1);
        assert!(grow_table.call(&mut store, 1).is_err());
        assert_eq!(
            instance
                .get_memory(&mut store, "second")
                .unwrap()
                .size(&store),
            1
        );
    }

    #[test]
    fn initial_resources_and_instance_counts_are_limited() {
        let engine = crate::component::engine_with(false).unwrap();
        let module = Module::new(&engine, "(module)").unwrap();
        let mut store = Store::new(
            &engine,
            GuestLimits::new(CallBudget {
                max_guest_instances: 1,
                ..CallBudget::prototype()
            }),
        );
        store.limiter(|limits| limits);
        Instance::new(&mut store, &module, &[]).unwrap();
        assert!(Instance::new(&mut store, &module, &[]).is_err());
        let module = Module::new(&engine, "(module (memory 2))").unwrap();
        let mut store = Store::new(
            &engine,
            GuestLimits::new(CallBudget {
                max_guest_memory_bytes: 65536,
                ..CallBudget::prototype()
            }),
        );
        store.limiter(|limits| limits);
        assert!(Instance::new(&mut store, &module, &[]).is_err());
    }

    #[test]
    fn canonical_lifting_counts_aliased_strings_before_copying() {
        let engine = crate::component::engine_with(false).unwrap();
        let text = "x".repeat(512);
        let wat = format!(
            r#"(component
            (core module $m
                (memory (export "memory") 1)
                (data (i32.const 0) "{text}")
                (data (i32.const 1024) "\00\00\00\00\00\02\00\00\00\00\00\00\00\02\00\00")
                (data (i32.const 1056) "\00\04\00\00\02\00\00\00")
                (func (export "get") (result i32) i32.const 1056))
            (core instance $i (instantiate $m))
            (func (export "aliased") (result (list string))
                (canon lift (core func $i "get") (memory (core memory $i "memory")))))"#
        );
        let component = wasmtime::component::Component::new(&engine, wat).unwrap();
        let budget = CallBudget {
            max_input_bytes: 800,
            ..CallBudget::prototype()
        };
        let mut store = Store::new(&engine, GuestLimits::new(budget));
        store.limiter(|limits| limits);
        store.set_hostcall_fuel(budget.copy_fuel());
        store.set_fuel(10000).unwrap();
        let instance = wasmtime::component::Linker::new(&engine)
            .instantiate(&mut store, &component)
            .unwrap();
        let get = instance
            .get_typed_func::<(), (Vec<String>,)>(&mut store, "aliased")
            .unwrap();
        let error = get
            .call(&mut store, ())
            .expect_err("aliased pointers must still consume copy fuel");
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains("fuel allocated for hostcalls")),
            "{error:#}"
        );
    }
}
