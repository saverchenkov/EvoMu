//! `pve_incursion_lifecycle` — Sansha Incursion Constellation Role Selection, Influence Regen,
//! Withdrawal Timer, Respawn Cadence, System-Wide DBuffs, Multi-Wave Progression & World Event Tales.
//!
// parity: feat:pve.incursion-spawn-lifecycle, feat:pve.incursion-influence, feat:pve.incursion-system-effects, feat:pve.incursion-site-classes, feat:pve.tales

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_incursion_sites::{
    derive_incursion_site_position, make_incursion_site, make_site_npc_ball,
    publish_incursion_tale_notice, seed_incursion_for_constellation, ConstellationId,
    IncursionNpcSpawn, IncursionSiteEntry, IncursionSystemEffects, IncursionTaleRecord,
    INCURSION_TALE_TEMPLATE_ID,
};
use crate::services::pve_special_sites::{next_spec_seq, spec_now_sec, with_pve_special_reg};
use crate::system_actor::SystemRegistry;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const INCURSION_EFFECT_BEACON_VANGUARD: u32 = 3069;
pub const INCURSION_EFFECT_BEACON_ASSAULT: u32 = 3493;
pub const INCURSION_EFFECT_BEACON_HQ: u32 = 3494;

/// 24-hour withdrawal duration after the Kundalini Manifest mothership is defeated.
pub const INCURSION_WITHDRAWAL_DURATION_SEC: u64 = 86_400;
/// 12-hour respawn cadence after an incursion finishes withdrawing before a new constellation spawns.
pub const INCURSION_RESPAWN_CADENCE_SEC: u64 = 43_200;
/// Sansha influence regeneration rate while sites are not run (+2% per hour).
pub const INCURSION_INFLUENCE_REGEN_PER_HOUR: f64 = 0.02;
/// Target concurrent active Sansha incursions across New Eden.
pub const TARGET_ACTIVE_INCURSIONS: usize = 3;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IncursionLifecycleMeta {
    #[serde(default)]
    pub spawned_at_sec: u64,
    #[serde(default)]
    pub last_site_completed_sec: u64,
    #[serde(default)]
    pub last_regen_sec: u64,
    #[serde(default)]
    pub withdraw_until_sec: Option<u64>,
    #[serde(default)]
    pub ended_at_sec: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IncursionSiteWaveMeta {
    #[serde(default = "default_one_u8")]
    pub current_wave: u8,
    #[serde(default = "default_three_u8")]
    pub total_waves: u8,
}

fn default_one_u8() -> u8 {
    1
}
fn default_three_u8() -> u8 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldEventTaleRecord {
    pub tale_id: i64,
    pub template_id: i64,
    pub template_class_id: String,
    pub solar_system_id: SolarSystemId,
    pub constellation_id: ConstellationId,
    pub aggressor_faction_id: i64,
    pub state: String,
    pub influence: f64,
    pub start_unix: u64,
    pub end_unix: u64,
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Returns SDE constellations eligible for Sansha Incursions:
/// - Known-space constellations (`< 21_000_000`) with no Pochven/Jove/Zarzakh/Wormhole systems (`< 30_100_000`)
/// - Excludes major trade hub constellations (`20_000_020`) and starter/school constellations
/// - Has at least 6 solar systems so Staging (1), Vanguard (2+), Assault (2+), and HQ (1) have distinct systems
pub fn eligible_incursion_constellations(ctx: &ServerContext) -> Vec<(ConstellationId, f64)> {
    let trade_hubs: &[ConstellationId] = &[20_000_020];
    let starter_corps: HashSet<i64> = ctx
        .sde
        .schools
        .values()
        .map(|s| s.corporation_id as i64)
        .chain(ctx.sde.bloodlines.values().map(|b| b.corporation_id as i64))
        .collect();
    let starter_systems: HashSet<SolarSystemId> = ctx
        .sde
        .schools
        .values()
        .map(|s| s.solar_system_id)
        .chain(
            ctx.sde
                .stations
                .values()
                .filter(|st| starter_corps.contains(&(st.owner_corporation_id as i64)))
                .map(|st| st.solar_system_id),
        )
        .filter(|&id| id > 0)
        .collect();
    let starter_constellations: HashSet<ConstellationId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| starter_systems.contains(&s.solar_system_id))
        .map(|s| s.constellation_id)
        .collect();

    let mut out: Vec<(ConstellationId, f64)> = ctx
        .sde
        .constellations
        .keys()
        .copied()
        .filter(|&cid| cid < 21_000_000 && !trade_hubs.contains(&cid) && !starter_constellations.contains(&cid))
        .filter_map(|cid| {
            let sys: Vec<_> = ctx
                .sde
                .solar_systems
                .values()
                .filter(|s| s.constellation_id == cid)
                .collect();
            if sys.len() < 6 || sys.iter().any(|s| s.solar_system_id >= 30_100_000) {
                return None;
            }
            let avg_sec = sys.iter().map(|s| s.security_status).sum::<f64>() / (sys.len() as f64);
            if (-1.0..=0.85).contains(&avg_sec) {
                Some((cid, avg_sec))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| {
        let a_empire = (0.1..=0.8).contains(&a.1);
        let b_empire = (0.1..=0.8).contains(&b.1);
        b_empire
            .cmp(&a_empire)
            .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Assigns distinct Staging, Vanguard, Assault, and HQ solar system roles within a constellation.
pub fn assign_constellation_system_roles(
    sys_list: &[SolarSystemId],
) -> (SolarSystemId, Vec<SolarSystemId>, Vec<SolarSystemId>, SolarSystemId) {
    let stg = sys_list.first().copied().unwrap_or(30_000_140);
    if sys_list.len() >= 6 {
        let mut vg = vec![sys_list[1], sys_list[2]];
        if sys_list.len() > 6 {
            vg.extend_from_slice(&sys_list[6..]);
        }
        let as_s = vec![sys_list[3], sys_list[4]];
        let hq = sys_list[5];
        (stg, vg, as_s, hq)
    } else {
        let hq = sys_list.last().copied().unwrap_or(stg);
        let vg = if sys_list.len() > 2 {
            vec![sys_list[1], sys_list.get(2).copied().unwrap_or(sys_list[1])]
        } else {
            vec![stg]
        };
        let as_s = if sys_list.len() > 4 {
            vec![sys_list[3], sys_list[4]]
        } else {
            vec![stg]
        };
        (stg, vg, as_s, hq)
    }
}

/// Validates that a site tier spawns only in a system assigned the matching incursion role.
pub fn validate_site_role_for_system(
    inc: &IncursionTaleRecord,
    solar_system_id: SolarSystemId,
    tier: &str,
) -> Result<(), BridgeRpcError> {
    let valid = match tier {
        "Staging" => solar_system_id == inc.staging_solar_system_id,
        "Vanguard" => inc.vanguard_solar_system_ids.contains(&solar_system_id),
        "Assault" => inc.assault_solar_system_ids.contains(&solar_system_id),
        "Headquarters" | "Mothership" | "Boss" => solar_system_id == inc.hq_solar_system_id,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(rpc_err(
            "InvalidIncursionSiteSystem",
            format!(
                "Incursion site class {tier} cannot spawn in system {solar_system_id} for constellation {}",
                inc.constellation_id
            ),
        ))
    }
}

/// Spawns a new incursion site of `tier` into `solar_system_id` only if `solar_system_id` has the matching role.
pub fn spawn_incursion_site_in_role_system(
    ctx: &ServerContext,
    constellation_id: ConstellationId,
    solar_system_id: SolarSystemId,
    tier: &str,
    dungeon_id: u32,
    dungeon_name: &str,
) -> Result<IncursionSiteEntry, BridgeRpcError> {
    let (min_pilots, max_opt, base_isk, base_lp, inf_delta, locked) = match tier {
        "Staging" => (3, 5, 3_500_000, 400, 0.05, false),
        "Vanguard" => (5, 10, 10_395_000, 1_400, 0.05, false),
        "Assault" => (10, 20, 18_200_000, 3_500, 0.20, false),
        "Headquarters" => (20, 40, 31_500_000, 7_000, 0.25, false),
        "Mothership" => (40, 80, 63_000_000, 14_000, 1.0, true),
        _ => {
            return Err(rpc_err(
                "InvalidIncursionSiteClass",
                format!("Unknown incursion site class: {tier}"),
            ));
        }
    };
    with_pve_special_reg(ctx, |reg| {
        let inc = reg.incursions.get_mut(&constellation_id).ok_or_else(|| {
            rpc_err(
                "IncursionNotFound",
                format!("No active incursion in constellation {constellation_id}"),
            )
        })?;
        validate_site_role_for_system(inc, solar_system_id, tier)?;
        let pos = derive_incursion_site_position(&ctx.sde, solar_system_id, inc.sites.len());
        let entry = make_incursion_site(
            constellation_id,
            dungeon_id,
            dungeon_name,
            solar_system_id,
            tier,
            min_pilots,
            max_opt,
            base_isk,
            base_lp,
            inf_delta,
            locked,
            pos,
        );
        reg.incursion_site_waves.insert(
            entry.site_id.clone(),
            IncursionSiteWaveMeta {
                current_wave: 1,
                total_waves: if tier == "Staging" { 2 } else { 3 },
            },
        );
        inc.sites.push(entry.clone());
        Ok(entry)
    })
}

pub fn build_wave_reinforcements(
    boss_ball_id: i64,
    tier: &str,
    wave: u8,
) -> Vec<IncursionNpcSpawn> {
    let base_id = boss_ball_id * 10 + (wave as i64) * 20;
    match tier {
        "Headquarters" | "Mothership" | "Boss" => vec![
            IncursionNpcSpawn { npc_ball_id: base_id + 1, type_id: 2845, offset: [2_800.0, 900.0, -400.0], destroyed: false },
            IncursionNpcSpawn { npc_ball_id: base_id + 2, type_id: 2933, offset: [-2_400.0, -1_100.0, 600.0], destroyed: false },
            IncursionNpcSpawn { npc_ball_id: base_id + 3, type_id: 2936, offset: [1_400.0, 1_600.0, 350.0], destroyed: false },
        ],
        "Assault" => vec![
            IncursionNpcSpawn { npc_ball_id: base_id + 1, type_id: 2933, offset: [-1_900.0, 1_300.0, 250.0], destroyed: false },
            IncursionNpcSpawn { npc_ball_id: base_id + 2, type_id: 2859, offset: [1_600.0, -900.0, -350.0], destroyed: false },
        ],
        _ => vec![
            IncursionNpcSpawn { npc_ball_id: base_id + 1, type_id: 2936, offset: [1_100.0, 700.0, 200.0], destroyed: false },
            IncursionNpcSpawn { npc_ball_id: base_id + 2, type_id: 2909, offset: [-550.0, -450.0, 350.0], destroyed: false },
        ],
    }
}

pub async fn advance_incursion_wave_on_escort_kill(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    solar_system_id: SolarSystemId,
    site_id: &str,
) {
    let now_sec = db.clock().now_unix_secs();
    let next_wave_spawns = crate::services::pve_special_sites::with_pve_special_reg_db(db, now_sec, |reg| {
        for inc in reg.incursions.values_mut() {
            if inc.state == "Ended" || inc.state == "Withdrawing" {
                continue;
            }
            if let Some(site) = inc.sites.iter_mut().find(|s| s.site_id == site_id && s.solar_system_id == solar_system_id) {
                if site.boss_destroyed {
                    return None;
                }
                let all_escorts_dead = !site.escort_spawns.is_empty()
                    && site.escort_spawns.iter().all(|e| e.destroyed);
                if !all_escorts_dead {
                    return None;
                }
                let wave_meta = reg
                    .incursion_site_waves
                    .entry(site.site_id.clone())
                    .or_insert(IncursionSiteWaveMeta {
                        current_wave: 1,
                        total_waves: if site.tier == "Staging" { 2 } else { 3 },
                    });
                if wave_meta.current_wave >= wave_meta.total_waves {
                    return None;
                }
                wave_meta.current_wave += 1;
                let reinforcements = build_wave_reinforcements(
                    site.boss_npc_ball_id,
                    &site.tier,
                    wave_meta.current_wave,
                );
                let mut to_spawn = Vec::with_capacity(reinforcements.len());
                for r in reinforcements {
                    to_spawn.push((r.npc_ball_id, r.type_id, site.position + DVec3::new(r.offset[0], r.offset[1], r.offset[2])));
                    site.escort_spawns.push(r);
                }
                return Some(to_spawn);
            }
        }
        None
    });

    if let Some(spawns) = next_wave_spawns {
        let sys_handle = systems.get_or_spawn(solar_system_id);
        for (bid, tid, pos) in spawns {
            systems.claim_special_site_ball(solar_system_id, bid);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(bid, tid, pos),
                    crate::services::pve_special_sites::build_special_site_npc_combat_state(sde, tid),
                )
                .await;
        }
    }
}

/// Lock-free computation of active Sansha Incursion system effects directly from `snap`.
pub(crate) fn compute_incursion_system_effects_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    solar_system_id: SolarSystemId,
) -> Option<IncursionSystemEffects> {
    let const_id = sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.constellation_id)?;
    let raw_json = snap
        .service_state_blobs
        .get("incursions_state")
        .map(|s| s.as_str())
        .or_else(|| {
            snap.owner_notes
                .get(&-8_300_000_000_000)
                .map(|n| n.note.as_str())
        })?;
    let reg = serde_json::from_str::<crate::services::pve_special_sites::PveSpecialSitesRegistry>(
        raw_json,
    )
    .ok()?;
    let inc = reg.incursions.get(&const_id)?;
    if inc.influence <= 0.0
        || inc.mothership_defeated
        || inc.state == "Ended"
        || inc.state == "Withdrawing"
    {
        return None;
    }
    let (effect_type_id, base_mag) = if solar_system_id == inc.hq_solar_system_id {
        (INCURSION_EFFECT_BEACON_HQ, 0.50)
    } else if inc.assault_solar_system_ids.contains(&solar_system_id) {
        (INCURSION_EFFECT_BEACON_ASSAULT, 0.25)
    } else if inc.vanguard_solar_system_ids.contains(&solar_system_id)
        || solar_system_id == inc.staging_solar_system_id
    {
        (INCURSION_EFFECT_BEACON_VANGUARD, 0.10)
    } else {
        return None;
    };
    let inf = inc.influence.clamp(0.0, 1.0);
    let mag = base_mag * inf;
    Some(IncursionSystemEffects {
        constellation_id: const_id,
        effect_type_id,
        influence: inf,
        cyno_jammed: inf > 0.0,
        shield_penalty: mag,
        armor_penalty: mag,
        capacitor_penalty: mag,
        damage_penalty: mag,
        resistance_penalty: mag,
        bounty_penalty: mag,
        lp_penalty: mag,
    })
}

/// Computes active Sansha Incursion system effects for `solar_system_id`, scaled by influence.
/// Returns `None` if no incursion is active or once the incursion enters `Withdrawing` / `Ended`.
pub fn compute_incursion_system_effects(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Option<IncursionSystemEffects> {
    ctx.db.with_snapshot(|snap| {
        compute_incursion_system_effects_in_snap(&ctx.sde, snap, solar_system_id)
    })
}

/// Returns the active Sansha system-wide dbuffs `(dbuff_id, value)` for `solar_system_id`,
/// scaled by influence. Returns an empty vector when the incursion withdraws or ends.
pub fn get_incursion_system_wide_dbuffs(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Vec<(u32, f64)> {
    let Some(eff) = compute_incursion_system_effects(ctx, solar_system_id) else {
        return Vec::new();
    };
    let pct = eff.resistance_penalty * 100.0;
    // dbuff 10 = Shield Resistance PostPercent (`[271, 272, 273, 274]`)
    // dbuff 13 = Armor Resistance PostPercent (`[267, 268, 269, 270]`)
    vec![(10, pct), (13, pct)]
}

/// Synchronizes SDE Sansha Incursion System Effect beacons (`3069`, `3493`, `3494`) in `snap.items`
/// for all active incursion systems and removes them from withdrawing/ended systems.
pub fn sync_incursion_system_effect_beacons(ctx: &ServerContext) {
    let desired_by_sys: HashMap<SolarSystemId, u32> = with_pve_special_reg(ctx, |reg| {
        let mut map = HashMap::new();
        for inc in reg.incursions.values() {
            if inc.influence <= 0.0
                || inc.mothership_defeated
                || inc.state == "Ended"
                || inc.state == "Withdrawing"
            {
                continue;
            }
            map.insert(inc.staging_solar_system_id, INCURSION_EFFECT_BEACON_VANGUARD);
            for &s in &inc.vanguard_solar_system_ids {
                map.insert(s, INCURSION_EFFECT_BEACON_VANGUARD);
            }
            for &s in &inc.assault_solar_system_ids {
                map.insert(s, INCURSION_EFFECT_BEACON_ASSAULT);
            }
            map.insert(inc.hq_solar_system_id, INCURSION_EFFECT_BEACON_HQ);
        }
        map
    });

    ctx.db.with_mut_snapshot(|snap| {
        let stale_ids: Vec<i64> = snap
            .items
            .iter()
            .filter_map(|(&iid, item)| {
                if !matches!(
                    item.type_id,
                    INCURSION_EFFECT_BEACON_VANGUARD
                        | INCURSION_EFFECT_BEACON_ASSAULT
                        | INCURSION_EFFECT_BEACON_HQ
                ) {
                    return None;
                }
                let sys_id = u32::try_from(item.location_id).unwrap_or(0);
                if desired_by_sys.get(&sys_id) == Some(&item.type_id) {
                    None
                } else {
                    Some(iid)
                }
            })
            .collect();
        for iid in stale_ids {
            snap.items.remove(&iid);
            snap.item_positions.remove(&iid);
        }

        let mut existing_sys: HashSet<SolarSystemId> = snap
            .items
            .values()
            .filter(|i| {
                matches!(
                    i.type_id,
                    INCURSION_EFFECT_BEACON_VANGUARD
                        | INCURSION_EFFECT_BEACON_ASSAULT
                        | INCURSION_EFFECT_BEACON_HQ
                )
            })
            .filter_map(|i| u32::try_from(i.location_id).ok())
            .collect();

        for (sys_id, beacon_tid) in desired_by_sys {
            if existing_sys.insert(sys_id) {
                let beacon_id = 1_896_900_000 + i64::from(sys_id % 100_000);
                snap.items.insert(
                    beacon_id,
                    InventoryItemRecord {
                        item_id: beacon_id,
                        type_id: beacon_tid,
                        owner_id: 500_019,
                        location_id: i64::from(sys_id),
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                snap.item_positions.insert(beacon_id, [0.0, 0.0, 0.0]);
            }
        }
    });
}

/// Advances incursion influence regeneration, withdrawal timers, and respawn cadence when the
/// simulated PvE clock moves forward.
pub fn advance_incursion_lifecycles(ctx: &ServerContext) {
    let has_state = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.contains_key("incursions_state")
            || snap.owner_notes.contains_key(&-8_300_000_000_000)
    });
    if !has_state {
        return;
    }
    let (started_tales, balls_to_despawn, balls_to_spawn) = with_pve_special_reg(ctx, |reg| {
        if reg.incursions.is_empty() {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let now_sec = spec_now_sec(reg);
        let mut despawn = Vec::new();
        let mut latest_ended_sec: u64 = 0;
        let mut latest_spawned_sec: u64 = 0;

        let cids: Vec<ConstellationId> = reg.incursions.keys().copied().collect();
        for cid in cids {
            let meta = reg
                .incursion_lifecycle
                .entry(cid)
                .or_insert_with(|| IncursionLifecycleMeta {
                    spawned_at_sec: now_sec,
                    last_site_completed_sec: now_sec,
                    last_regen_sec: now_sec,
                    withdraw_until_sec: None,
                    ended_at_sec: None,
                });
            if meta.spawned_at_sec == 0 {
                meta.spawned_at_sec = now_sec;
            }
            latest_spawned_sec = latest_spawned_sec.max(meta.spawned_at_sec);
            if let Some(ended) = meta.ended_at_sec {
                latest_ended_sec = latest_ended_sec.max(ended);
            }

            let Some(inc) = reg.incursions.get_mut(&cid) else {
                continue;
            };
            if inc.state == "Ended" {
                continue;
            }

            if inc.state == "Withdrawing" {
                let withdraw_until = meta
                    .withdraw_until_sec
                    .unwrap_or(meta.last_site_completed_sec + INCURSION_WITHDRAWAL_DURATION_SEC);
                if now_sec >= withdraw_until {
                    inc.state = "Ended".into();
                    meta.ended_at_sec = Some(now_sec);
                    latest_ended_sec = latest_ended_sec.max(now_sec);
                    for s in &inc.sites {
                        despawn.push((s.solar_system_id, s.boss_npc_ball_id));
                        for esc in &s.escort_spawns {
                            despawn.push((s.solar_system_id, esc.npc_ball_id));
                        }
                    }
                }
                continue;
            }

            // Regenerate influence over time while sites are not run (+2% per hour)
            let baseline = meta.last_regen_sec.max(meta.last_site_completed_sec);
            if now_sec > baseline && inc.influence < 1.0 {
                let elapsed = now_sec - baseline;
                let regen = (elapsed as f64) * (INCURSION_INFLUENCE_REGEN_PER_HOUR / 3600.0);
                inc.influence = (inc.influence + regen).min(1.0);
                meta.last_regen_sec = now_sec;
                if inc.influence > 0.10 && inc.mothership_spawned && !inc.mothership_defeated {
                    inc.mothership_spawned = false;
                    inc.state = "Established".into();
                    if let Some(ms) = inc.sites.iter().find(|s| s.tier == "Mothership") {
                        despawn.push((ms.solar_system_id, ms.boss_npc_ball_id));
                        for esc in &ms.escort_spawns {
                            despawn.push((ms.solar_system_id, esc.npc_ball_id));
                        }
                    }
                }
            }
        }

        // Respawn cadence: maintain TARGET_ACTIVE_INCURSIONS once cooldown elapses after an incursion ends
        let mut started = Vec::new();
        let mut spawn_balls = Vec::new();
        let active_cids: HashSet<ConstellationId> = reg
            .incursions
            .iter()
            .filter(|(_, inc)| inc.state != "Ended")
            .map(|(&cid, _)| cid)
            .collect();
        let used_cids: HashSet<ConstellationId> = reg.incursions.keys().copied().collect();
        let deficit = TARGET_ACTIVE_INCURSIONS.saturating_sub(active_cids.len());
        let cadence_ready = latest_ended_sec > 0
            && now_sec >= latest_ended_sec.saturating_add(INCURSION_RESPAWN_CADENCE_SEC)
            && now_sec >= latest_spawned_sec.saturating_add(INCURSION_RESPAWN_CADENCE_SEC);

        if deficit > 0 && cadence_ready {
            let eligible = eligible_incursion_constellations(ctx);
            let candidates: Vec<ConstellationId> = eligible
                .iter()
                .map(|&(cid, _)| cid)
                .filter(|cid| !active_cids.contains(cid) && !used_cids.contains(cid))
                .chain(
                    eligible
                        .iter()
                        .map(|&(cid, _)| cid)
                        .filter(|cid| !active_cids.contains(cid) && used_cids.contains(cid)),
                )
                .take(deficit)
                .collect();

            for cid in candidates {
                seed_incursion_for_constellation(ctx, reg, cid);
                reg.incursion_lifecycle.insert(
                    cid,
                    IncursionLifecycleMeta {
                        spawned_at_sec: now_sec,
                        last_site_completed_sec: now_sec,
                        last_regen_sec: now_sec,
                        withdraw_until_sec: None,
                        ended_at_sec: None,
                    },
                );
                if let Some(inc) = reg.incursions.get(&cid) {
                    started.push((inc.tale_id, inc.staging_solar_system_id));
                    for s in &inc.sites {
                        if s.locked_until_low_influence {
                            continue;
                        }
                        spawn_balls.push((s.solar_system_id, s.boss_npc_ball_id, s.boss_type_id, s.position));
                        for esc in &s.escort_spawns {
                            if !esc.destroyed {
                                let esc_pos = s.position + DVec3::new(esc.offset[0], esc.offset[1], esc.offset[2]);
                                spawn_balls.push((s.solar_system_id, esc.npc_ball_id, esc.type_id, esc_pos));
                            }
                        }
                    }
                }
            }
        }

        (started, despawn, spawn_balls)
    });

    sync_incursion_system_effect_beacons(ctx);

    let now_u = ctx.now_unix();
    for (tale_id, staging) in started_tales {
        publish_incursion_tale_notice(ctx.quasar_gateway.as_ref(), true, tale_id, staging, now_u);
    }

    if (!balls_to_despawn.is_empty() || !balls_to_spawn.is_empty())
        && tokio::runtime::Handle::try_current().is_ok()
    {
        let systems = ctx.systems.clone();
        let sde = ctx.sde.clone();
        tokio::spawn(async move {
            for (sys_id, bid) in balls_to_despawn {
                let h = systems.get_or_spawn(sys_id);
                let _ = h.remove_ball(bid).await;
            }
            for (sys_id, bid, tid, pos) in balls_to_spawn {
                if !systems.claim_special_site_ball(sys_id, bid) {
                    continue;
                }
                let h = systems.get_or_spawn(sys_id);
                if h.get_ball_snapshot(bid).await.ok().flatten().is_none() {
                    let cs = crate::services::pve_special_sites::build_special_site_npc_combat_state(&sde, tid);
                    let _ = h.spawn_ball_with_combat(make_site_npc_ball(bid, tid, pos), cs).await;
                }
            }
        });
    }
}

/// Registers a custom world-event tale (e.g., invasion, campaign event) in the live tale registry.
pub fn register_world_event_tale(ctx: &ServerContext, tale: WorldEventTaleRecord) -> i64 {
    with_pve_special_reg(ctx, |reg| {
        let tid = if tale.tale_id > 0 {
            tale.tale_id
        } else {
            900_000 + next_spec_seq(reg)
        };
        let mut rec = tale;
        rec.tale_id = tid;
        reg.world_event_tales.insert(tid, rec);
        tid
    })
}

fn build_incursion_tale_py(inc: &IncursionTaleRecord) -> PyRep {
    let mut scene_types_map: Vec<(PyRep, PyRep)> = Vec::new();
    let mut system_states: Vec<(PyRep, PyRep)> = Vec::new();
    scene_types_map.push((
        PyRep::Integer(inc.staging_solar_system_id as i64),
        PyRep::String("incursionStaging".into()),
    ));
    system_states.push((
        PyRep::Integer(inc.staging_solar_system_id as i64),
        PyRep::String(inc.state.clone()),
    ));
    for &v_sys in &inc.vanguard_solar_system_ids {
        scene_types_map.push((
            PyRep::Integer(v_sys as i64),
            PyRep::String("incursionLightInfestation".into()),
        ));
        system_states.push((
            PyRep::Integer(v_sys as i64),
            PyRep::String(inc.state.clone()),
        ));
    }
    for &a_sys in &inc.assault_solar_system_ids {
        scene_types_map.push((
            PyRep::Integer(a_sys as i64),
            PyRep::String("incursionMediumInfestation".into()),
        ));
        system_states.push((
            PyRep::Integer(a_sys as i64),
            PyRep::String(inc.state.clone()),
        ));
    }
    scene_types_map.push((
        PyRep::Integer(inc.hq_solar_system_id as i64),
        PyRep::String("incursionHeavyInfestation".into()),
    ));
    system_states.push((
        PyRep::Integer(inc.hq_solar_system_id as i64),
        PyRep::String(inc.state.clone()),
    ));

    let sites_py: Vec<PyRep> = inc
        .sites
        .iter()
        .map(|s| {
            let available = !s.locked_until_low_influence || inc.mothership_spawned;
            PyRep::keyval([
                ("siteID", PyRep::String(s.site_id.clone())),
                ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                ("tier", PyRep::String(s.tier.clone())),
                ("minPilots", PyRep::Integer(s.min_pilots as i64)),
                ("maxOptimalPilots", PyRep::Integer(s.max_optimal_pilots as i64)),
                ("baseIsk", PyRep::Integer(s.base_isk)),
                ("baseConcordLP", PyRep::Integer(s.base_concord_lp)),
                ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                ("available", PyRep::Bool(available)),
            ])
        })
        .collect();

    PyRep::keyval([
        ("taleID", PyRep::Integer(inc.tale_id)),
        ("templateID", PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)),
        ("templateClassID", PyRep::String("worldEvents".into())),
        ("managerSolarSystemID", PyRep::Integer(inc.staging_solar_system_id as i64)),
        ("solar_system_id", PyRep::Integer(inc.staging_solar_system_id as i64)),
        ("locationID", PyRep::Integer(inc.staging_solar_system_id as i64)),
        ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
        ("constellationName", PyRep::String(inc.constellation_name.clone())),
        ("influence", PyRep::Real(inc.influence)),
        ("state", PyRep::String(inc.state.clone())),
        ("systemState", PyRep::String(inc.state.clone())),
        ("systemStates", PyRep::Dict(system_states)),
        ("aggressorFactionID", PyRep::Integer(500_019)),
        ("templateNameID", PyRep::Integer(564035)),
        ("templateDescriptionID", PyRep::Integer(564036)),
        ("endTime", PyRep::Integer(inc.lockout_until_filetime.unwrap_or(0))),
        (
            "hasFinalEncounter",
            PyRep::Bool(inc.mothership_spawned && !inc.mothership_defeated),
        ),
        ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
        ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
        ("sceneTypesByLocationID", PyRep::Dict(scene_types_map)),
        ("sites", PyRep::List(sites_py)),
    ])
}

/// Collects all active world event tales (Sansha Incursions, Triglavian/EDENCOM Invasions,
/// Metaliminal Storms, and registered world events) with their live system state.
pub fn collect_all_world_event_tales(
    ctx: &ServerContext,
    template_filter: Option<i64>,
) -> Vec<PyRep> {
    let now_sec = ctx.now_unix();
    let now_ft = ((now_sec + 11_644_473_600) * 10_000_000) as i64;
    with_pve_special_reg(ctx, |reg| {
        let mut out = Vec::new();

        // 1. Sansha Incursion tales (templateID = 5158)
        if template_filter.is_none() || template_filter == Some(INCURSION_TALE_TEMPLATE_ID as i64) {
            for inc in reg.incursions.values().filter(|i| i.state != "Ended") {
                if template_filter.is_some() {
                    out.push(PyRep::keyval([
                        ("tale_id", PyRep::Integer(inc.tale_id)),
                        ("taleID", PyRep::Integer(inc.tale_id)),
                        ("template_id", PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)),
                        ("templateID", PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)),
                        ("solar_system_id", PyRep::Integer(inc.staging_solar_system_id as i64)),
                        ("solarSystemID", PyRep::Integer(inc.staging_solar_system_id as i64)),
                        ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                        ("state", PyRep::String(inc.state.clone())),
                        ("systemState", PyRep::String(inc.state.clone())),
                        ("influence", PyRep::Real(inc.influence)),
                        ("start_date", PyRep::Integer(now_ft - 864_000_000_000)),
                        (
                            "end_date",
                            PyRep::Integer(
                                inc.lockout_until_filetime
                                    .unwrap_or(now_ft + 864_000_000_000),
                            ),
                        ),
                    ]));
                } else {
                    out.push(build_incursion_tale_py(inc));
                }
            }
        }

        // 2. Triglavian Invasion / Minor Victory world event tales (templateID = 5201)
        if template_filter.is_none() || template_filter == Some(5201) {
            for &sys_id in &reg.triglavian_minor_victory_systems {
                let const_id = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.constellation_id)
                    .unwrap_or(0);
                let active_sites = reg
                    .triglavian_sites
                    .get(&sys_id)
                    .map(|v| v.iter().filter(|s| !s.completed).count())
                    .unwrap_or(0);
                let tale_id = 850_000 + i64::from(sys_id % 100_000);
                let sys_state = if active_sites > 0 {
                    "TriglavianInvasionActive"
                } else {
                    "TriglavianMinorVictory"
                };
                if template_filter.is_some() {
                    out.push(PyRep::keyval([
                        ("tale_id", PyRep::Integer(tale_id)),
                        ("taleID", PyRep::Integer(tale_id)),
                        ("template_id", PyRep::Integer(5201)),
                        ("templateID", PyRep::Integer(5201)),
                        ("solar_system_id", PyRep::Integer(i64::from(sys_id))),
                        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("constellationID", PyRep::Integer(i64::from(const_id))),
                        ("state", PyRep::String(sys_state.into())),
                        ("systemState", PyRep::String(sys_state.into())),
                        ("activeSites", PyRep::Integer(active_sites as i64)),
                        ("start_date", PyRep::Integer(now_ft - 864_000_000_000)),
                        ("end_date", PyRep::Integer(now_ft + 864_000_000_000)),
                    ]));
                } else {
                    out.push(PyRep::keyval([
                        ("taleID", PyRep::Integer(tale_id)),
                        ("templateID", PyRep::Integer(5201)),
                        ("templateClassID", PyRep::String("triglavianInvasion".into())),
                        ("managerSolarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("solar_system_id", PyRep::Integer(i64::from(sys_id))),
                        ("locationID", PyRep::Integer(i64::from(sys_id))),
                        ("constellationID", PyRep::Integer(i64::from(const_id))),
                        ("state", PyRep::String(sys_state.into())),
                        ("systemState", PyRep::String(sys_state.into())),
                        ("activeSites", PyRep::Integer(active_sites as i64)),
                        ("aggressorFactionID", PyRep::Integer(500_026)),
                        (
                            "sceneTypesByLocationID",
                            PyRep::Dict(vec![(
                                PyRep::Integer(i64::from(sys_id)),
                                PyRep::String("triglavianMinorVictory".into()),
                            )]),
                        ),
                    ]));
                }
            }
        }

        // 3. EDENCOM Fortress & Minor Victory defense tales (templateID = 5202)
        if template_filter.is_none() || template_filter == Some(5202) {
            for (&sys_id, is_fortress) in reg
                .edencom_fortress_systems
                .iter()
                .map(|s| (s, true))
                .chain(reg.edencom_minor_victory_systems.iter().map(|s| (s, false)))
            {
                let const_id = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.constellation_id)
                    .unwrap_or(0);
                let tale_id = 860_000 + i64::from(sys_id % 100_000);
                let sys_state = if is_fortress {
                    "EdencomFortress"
                } else {
                    "EdencomMinorVictory"
                };
                if template_filter.is_some() {
                    out.push(PyRep::keyval([
                        ("tale_id", PyRep::Integer(tale_id)),
                        ("taleID", PyRep::Integer(tale_id)),
                        ("template_id", PyRep::Integer(5202)),
                        ("templateID", PyRep::Integer(5202)),
                        ("solar_system_id", PyRep::Integer(i64::from(sys_id))),
                        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("constellationID", PyRep::Integer(i64::from(const_id))),
                        ("state", PyRep::String(sys_state.into())),
                        ("systemState", PyRep::String(sys_state.into())),
                        ("start_date", PyRep::Integer(now_ft - 864_000_000_000)),
                        ("end_date", PyRep::Integer(now_ft + 864_000_000_000)),
                    ]));
                } else {
                    out.push(PyRep::keyval([
                        ("taleID", PyRep::Integer(tale_id)),
                        ("templateID", PyRep::Integer(5202)),
                        ("templateClassID", PyRep::String("edencomDefense".into())),
                        ("managerSolarSystemID", PyRep::Integer(i64::from(sys_id))),
                        ("solar_system_id", PyRep::Integer(i64::from(sys_id))),
                        ("locationID", PyRep::Integer(i64::from(sys_id))),
                        ("constellationID", PyRep::Integer(i64::from(const_id))),
                        ("state", PyRep::String(sys_state.into())),
                        ("systemState", PyRep::String(sys_state.into())),
                        ("aggressorFactionID", PyRep::Integer(500_027)),
                        (
                            "sceneTypesByLocationID",
                            PyRep::Dict(vec![(
                                PyRep::Integer(i64::from(sys_id)),
                                PyRep::String(sys_state.into()),
                            )]),
                        ),
                    ]));
                }
            }
        }

        // 4. Registered custom world-event tales (e.g., active invasion campaigns)
        for wt in reg.world_event_tales.values() {
            if wt.state == "Ended" {
                continue;
            }
            if template_filter.is_some() && template_filter != Some(wt.template_id) {
                continue;
            }
            let start_ft = ((wt.start_unix + 11_644_473_600) * 10_000_000) as i64;
            let end_ft = ((wt.end_unix + 11_644_473_600) * 10_000_000) as i64;
            out.push(PyRep::keyval([
                ("tale_id", PyRep::Integer(wt.tale_id)),
                ("taleID", PyRep::Integer(wt.tale_id)),
                ("template_id", PyRep::Integer(wt.template_id)),
                ("templateID", PyRep::Integer(wt.template_id)),
                ("templateClassID", PyRep::String(wt.template_class_id.clone())),
                ("managerSolarSystemID", PyRep::Integer(i64::from(wt.solar_system_id))),
                ("solar_system_id", PyRep::Integer(i64::from(wt.solar_system_id))),
                ("solarSystemID", PyRep::Integer(i64::from(wt.solar_system_id))),
                ("locationID", PyRep::Integer(i64::from(wt.solar_system_id))),
                ("constellationID", PyRep::Integer(i64::from(wt.constellation_id))),
                ("aggressorFactionID", PyRep::Integer(wt.aggressor_faction_id)),
                ("state", PyRep::String(wt.state.clone())),
                ("systemState", PyRep::String(wt.state.clone())),
                ("influence", PyRep::Real(wt.influence)),
                ("start_date", PyRep::Integer(start_ft)),
                ("end_date", PyRep::Integer(end_ft)),
            ]));
        }

        out
    })
}
