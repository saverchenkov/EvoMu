//! `pve_homefront_sites` — Homefront Operations seeding, registry management,
//! and V24 dungeonInstanceCacheMgr RPCs (`feat:pve.homefront-ops`, `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    with_pve_special_reg, HomefrontSiteRecord, PveSpecialSitesRegistry,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use std::collections::BTreeMap;

pub fn ensure_homefront_sites_for_system_locked(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
) {
    if reg.homefront_sites.contains_key(&sys_id) {
        return;
    }
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec < 0.45 || sys_id >= 31_000_000 {
        // Homefront operations only spawn in High-Security empire space
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    // Dynamically query SDE archetype 70 dungeons and operations.fsdbinary for Homefront Operations
    let templates = crate::services::pve_special_sites_ops::resolve_sde_homefront_templates(&ctx.sde);
    if templates.is_empty() {
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    let rng = crate::rng::MechanicRng::seeded(
        (sys_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ctx.now_unix(),
    );
    let idx1 = (rng.try_next_u64().unwrap_or(sys_id as u64) as usize) % templates.len();
    let idx2 = if templates.len() > 1 {
        let step = 1 + ((rng.try_next_u64().unwrap_or(1) as usize) % (templates.len() - 1));
        (idx1 + step) % templates.len()
    } else {
        idx1
    };

    let base_inst = (sys_id as i64) * 1000;
    let t1 = &templates[idx1];
    let t2 = &templates[idx2];
    let (boss1, boss2) =
        crate::services::pve_special_sites_ops::resolve_homefront_faction_boss_types(
            &ctx.sde, sys_id,
        );
    let isk1 = crate::services::pve_special_sites_ops::compute_homefront_base_pilot_isk(
        &ctx.sde,
        sys_id,
        t1.difficulty,
        0,
    );
    let isk2 = crate::services::pve_special_sites_ops::compute_homefront_base_pilot_isk(
        &ctx.sde,
        sys_id,
        t2.difficulty,
        1,
    );

    reg.homefront_sites.insert(
        sys_id,
        vec![
            HomefrontSiteRecord {
                instance_id: base_inst + 1,
                dungeon_id: t1.dungeon_id,
                dungeon_name: t1.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: isk1,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 1),
                boss_type_id: boss1,
            },
            HomefrontSiteRecord {
                instance_id: base_inst + 2,
                dungeon_id: t2.dungeon_id,
                dungeon_name: t2.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: isk2,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 2),
                boss_type_id: boss2,
            },
        ],
    );
}

pub const HOMEFRONT_SYSTEMS: &[SolarSystemId] =
    &[30_000_142_u32, 30_002_187, 30_002_659, 30_002_510];

pub fn init_homefront_sites(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    for &sys_id in HOMEFRONT_SYSTEMS {
        ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
    }
}

pub fn handle_get_homefront_site_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);

    let sys_id = if let Some(arg0) = args.first().filter(|a| !matches!(a, PyRep::None)) {
        let raw = arg0.as_i64().ok_or_else(|| BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: "GetHomefrontSiteInstances requires an integer solarSystemID".into(),
        })?;
        let sid = u32::try_from(raw).ok().filter(|id| ctx.sde.solar_systems.contains_key(id)).ok_or_else(|| BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: format!("Unknown solar system ID {raw}"),
        })?;
        sid
    } else {
        session
            .solar_system_id
            .or_else(|| {
                session.character_id.and_then(|cid| {
                    ctx.db
                        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                })
            })
            .ok_or_else(|| BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Action requires an active solar system session".into(),
            })?
    };

    let reply = with_pve_special_reg(ctx, |reg| {
        let mut by_dungeon: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
        if let Some(list) = reg.homefront_sites.get(&sys_id) {
            for s in list {
                if !s.completed {
                    by_dungeon.entry(s.dungeon_id).or_default().push(s.instance_id);
                }
            }
        }
        let inner: Vec<(PyRep, PyRep)> = by_dungeon
            .into_iter()
            .map(|(did, mut ids)| {
                ids.sort_unstable();
                (
                    PyRep::Integer(did as i64),
                    PyRep::List(ids.into_iter().map(PyRep::Integer).collect()),
                )
            })
            .collect();
        PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::Dict(inner))])
    });

    Ok(reply)
}

#[cfg(test)]
#[path = "pve_homefront_sites_tests.rs"]
mod pve_homefront_sites_tests;

