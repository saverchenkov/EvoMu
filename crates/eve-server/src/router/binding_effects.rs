//! Module effect ID resolution helpers for bound dogma objects.

/// Helper to determine the effect ID for a module given its type ID and optional effect name.
pub fn effect_id_for_module_sde(
    sde: &eve_sde::SdeRegistry,
    type_id: u32,
    effect_name: Option<&str>,
) -> Option<i64> {
    let ty = sde.types.get(&type_id);
    if let Some(name) = effect_name {
        let in_type = ty.and_then(|t| {
            t.effect_ids
                .iter()
                .find(|eid| sde.effects.get(eid).is_some_and(|e| e.name == name))
                .copied()
        });
        if let Some(eid) = in_type {
            return Some(eid as i64);
        }
        if let Some(e) = sde.effects.values().find(|e| e.name == name) {
            return Some(e.effect_id as i64);
        }
    }
    ty.and_then(|t| t.default_effect_id).map(|e| e as i64)
}
