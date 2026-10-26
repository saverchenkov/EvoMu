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

    // Candidate SDE archetype 70 dungeons for Empire Homefront Operations
    let candidate_dids: &[u32] = &[
        10346, // Abyssal Artifact Recovery
        10347, // Raid: Hall of Sacrifice
        10238, // Emergency Aid: Holy Mission
        10309, // Suspicious Signal: Secure the Intel
        10368, // Suspicious Signal: Stop the Sabotage
        10369, // Suspicious Signal: Stop the Siphon
        10373, // Emergency Aid: Convoy
        10381, // Dread Assault: Occupied Industry
    ];

    let base_inst = (sys_id as i64) * 1000;
    let did1 = candidate_dids[(sys_id as usize) % candidate_dids.len()];
    let did2 = candidate_dids[((sys_id as usize) + 3) % candidate_dids.len()];
    let d1_name = ctx
        .sde
        .dungeons
        .get(&did1)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "Homefront Operation".into());
    let d2_name = ctx
        .sde
        .dungeons
        .get(&did2)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "Homefront Operation".into());

    // DERIVED: EVE University Wiki "Homefront Operations" highsec optimal 5-pilot payout
    // (17,230,000 ISK per pilot for Empire Raid / Abyssal Artifact Recovery; 15,500,000 ISK for secondary operations).
    // Hostile wave boss NPC types use real SDE Abyssal/Triglavian combat entities (48090 Starving Damavik, 48092 Striking Damavik).
    reg.homefront_sites.insert(
        sys_id,
        vec![
            HomefrontSiteRecord {
                instance_id: base_inst + 1,
                dungeon_id: did1,
                dungeon_name: d1_name,
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: 17_230_000,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 1),
                boss_type_id: 48_090,
            },
            HomefrontSiteRecord {
                instance_id: base_inst + 2,
                dungeon_id: did2,
                dungeon_name: d2_name,
                solar_system_id: sys_id,
                optimal_fleet_size: 5,
                base_pilot_isk: 15_500_000,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 2),
                boss_type_id: 48_092,
            },
        ],
    );
}

pub fn init_homefront_sites(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    if !reg.homefront_sites.is_empty() {
        return;
    }
    for &sys_id in &[30_000_142_u32, 30_002_187, 30_002_659, 30_002_510] {
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

    let sys_id = match args.first().and_then(|a| a.as_i64()).map(|v| v as SolarSystemId) {
        Some(s) => s,
        None => session
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
            })?,
    };

    let reply = with_pve_special_reg(ctx, |reg| {
        ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
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
