//! Child helpers for [`super::pve_dungeon_runtime`] keeping module size bounded
//! while preserving exact deadspace & special-site warp target resolution.

#![forbid(unsafe_code)]

use super::*;

pub(super) fn resolve_fw_or_special_warp_target(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    target_id: i64,
    target_str_opt: Option<&str>,
) -> Option<DVec3> {
    let fw_pos = ctx.db.with_snapshot(|snap| {
        let has_fw = snap
            .fw_runtime
            .fw_complexes
            .iter()
            .any(|(&gid, (csys, _, _, _, done))| gid == target_id && *csys == sys_id && !*done)
            || snap
                .insurgency_runtime
                .insurgency_instances
                .iter()
                .any(|s| s.instance_id == target_id && s.solar_system_id == sys_id && !s.completed);
        has_fw
            .then(|| {
                snap.system_states
                    .get(&sys_id)
                    .and_then(|st| st.fw_complex_position)
                    .map(DVec3::from_array)
            })
            .flatten()
    });
    if fw_pos.is_some() {
        return fw_pos;
    }
    crate::services::pve_special_sites::read_pve_special_reg(ctx, |reg| {
        if let Some(hf) = reg.homefront_sites.get(&sys_id).and_then(|v| {
            v.iter()
                .find(|s| !s.completed && (s.instance_id == target_id || i64::from(s.dungeon_id) == target_id))
        }) {
            return Some(DVec3::new(
                10_000.0 + ((hf.instance_id % 100) as f64) * 5_000.0,
                ((hf.instance_id % 10) as f64) * 1_000.0,
                0.0,
            ));
        }
        if let Some(tr) = reg.triglavian_sites.get(&sys_id).and_then(|v| {
            v.iter()
                .find(|s| !s.completed && (s.instance_id == target_id || i64::from(s.dungeon_id) == target_id))
        }) {
            return Some(DVec3::new(
                10_000.0 + ((tr.instance_id % 100) as f64) * 2_500.0,
                0.0,
                0.0,
            ));
        }
        for inc in reg
            .incursions
            .values()
            .filter(|i| i.state != "Ended" && i.state != "Withdrawing")
        {
            if let Some(s) = inc.sites.iter().find(|s| {
                s.solar_system_id == sys_id
                    && (target_str_opt == Some(s.site_id.as_str())
                        || i64::from(s.dungeon_id) == target_id)
            }) {
                return Some(DVec3::from_array(s.position));
            }
        }
        if reg.drifter_hives.get(&sys_id).is_some_and(|h| !h.cleared) {
            return Some(DVec3::new(10_000.0, 0.0, 0.0));
        }
        None
    })
}

pub(super) fn find_active_dungeon_id_at_position(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    pos: [f64; 3],
) -> Option<u32> {
    let runtime_did = with_dungeon_runtime_store(ctx, |store| {
        store.instances.values().find_map(|inst| {
            if inst.completed || inst.despawned || inst.solar_system_id != sys_id {
                return None;
            }
            let near_entry = inst
                .entry_gate_pos
                .is_some_and(|ep| dist3(pos, ep) <= 150_000.0);
            let near_room = inst
                .rooms
                .iter()
                .any(|r| dist3(pos, r.position) <= 150_000.0);
            (near_entry || near_room).then_some(inst.dungeon_id)
        })
    });
    if runtime_did.is_some() {
        return runtime_did;
    }

    with_pve_exploration_reg(ctx, |reg| {
        reg.sites_by_system.get(&sys_id).and_then(|sites| {
            sites
                .iter()
                .find(|s| !s.cleared && dist3(pos, s.position) <= 150_000.0)
                .map(|s| s.dungeon_id)
        })
    })
}
