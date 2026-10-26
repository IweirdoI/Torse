//! Resolve declared bodies; undeclared entities retain the legacy caller bounds.
use torse_types::RejectCode;
use torse_world::{ComponentEdit, ComponentFields, Entity, EntityCollider};

pub(crate) fn half(
    fields: Option<&ComponentFields>,
    attached: bool,
    legacy: [f64; 3],
) -> Result<Option<[f64; 3]>, RejectCode> {
    if let Some(fields) = fields {
        return EntityCollider::from_fields(fields)
            .map(EntityCollider::half)
            .map_err(|_| RejectCode::InvalidData);
    }
    Ok((!attached).then_some(legacy))
}
pub(crate) fn of(
    entity: &Entity,
    edits: &[ComponentEdit],
    legacy: [f64; 3],
) -> Result<Option<[f64; 3]>, RejectCode> {
    let schema = torse_world::collider_schema();
    let fields = edits
        .iter()
        .find(|e| e.id == entity.id && e.schema == schema)
        .map_or_else(
            || entity.components.component(&schema),
            |e| e.fields.as_ref(),
        );
    half(
        fields,
        entity
            .components
            .component(&torse_world::block_instance_schema())
            .is_some(),
        legacy,
    )
}
