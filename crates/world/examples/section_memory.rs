//! Reproduce resident layout sizes without changing exchange formats.
use torse_world::{BlockStateId, Section, SectionStorage};

fn main() {
    for states in [1, 2, 4, 256, 257] {
        let cells = (0..32usize.pow(3))
            .map(|i| BlockStateId((i % states) as u16))
            .collect();
        let section = Section::from_storage(32, SectionStorage::Dense(cells)).unwrap();
        println!(
            "states={states} cell_bytes={} section_bytes={} dense_reference=65536",
            section.allocated_bytes(),
            size_of::<Section>() + section.allocated_bytes()
        );
    }
}
