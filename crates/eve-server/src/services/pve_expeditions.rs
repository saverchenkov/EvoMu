//! `pve_expeditions` — Multi-step PvE Escalations and Expeditions (`feat:pve.escalations`, `feat:pve.expeditions`).
//!
//! Provides dynamic multi-step expedition progression in character journal/state with dynamic steps,
//! spawning of destination dungeon instances in destination solar systems, and step progression
//! when destination encounters are defeated.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    build_wave_npc_balls, next_pve_seq, now_sec,
    PveExplorationRegistry, PveSiteInstance,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;

pub type CharacterId = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalatingPathEntry {
    pub instance_id: i64,
    pub character_id: CharacterId,
    pub source_dungeon_id: u32,
    pub expedition_dungeon_id: u32,
    pub expedition_name: String,
    pub destination_solar_system_id: SolarSystemId,
    pub step_index: u8,
    pub total_steps: u8,
    pub journal_text: String,
    pub created_at_sec: u64,
    pub expires_at_sec: u64,
    pub dungeons_entered: Vec<u32>,
}

pub fn find_escalation_destination_system_sde(sde: &eve_sde::SdeRegistry, origin_sys_id: SolarSystemId) -> SolarSystemId {
    let mut candidate = origin_sys_id;
    let adj_systems: Vec<u32> = sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == origin_sys_id)
        .map(|j| j.to_solar_system_id)
        .collect();
    if let Some(&next) = adj_systems.first() {
        candidate = next;
    }
    if candidate == origin_sys_id {
        candidate = match origin_sys_id {
            30_000_142 => 30_000_144, // Jita -> Perimeter
            30_000_144 => 30_000_145, // Perimeter -> Urlen
            30_002_187 => 30_002_188, // Amarr
            30_002_659 => 30_002_660, // Dodixie
            30_002_510 => 30_002_511, // Rens
            other => (other + 1).max(30_000_001),
        };
    }
    candidate
}

pub fn find_escalation_destination_system(ctx: &ServerContext, origin_sys_id: SolarSystemId) -> SolarSystemId {
    find_escalation_destination_system_sde(&ctx.sde, origin_sys_id)
}

pub fn expedition_dungeon_chain(source_dungeon_id: u32) -> (u32, &'static str, u8) {
    match source_dungeon_id {
        // Guristas
        1420 => (1420, "Guristas Scout Outpost", 3),
        1215 | 1421 => (1421, "Guristas Hallucinogen Supply Waypoint", 3),
        1232 | 1422 => (1422, "Chasing the Dragon", 4),
        2283 | 2402 => (2402, "Guristas Fleet Staging Point", 4),
        2134 | 2321 | 2403 => (2403, "The Maze", 4),

        // Blood Raiders
        2141 | 1430 => (1430, "Blood Raider Scout Outpost", 3),
        1218 | 1431 => (1431, "Blood Raider Crimson Hand Supply Depot", 3),
        1229 | 1432 => (1432, "Blood Raider Fleet Staging Point", 4),
        2282 | 2410 => (2410, "Blood Raider Fleet Staging Point 2", 4),
        2319 | 2411 => (2411, "Crimson Gauntlet", 4),

        // Sansha's Nation
        2135 | 1440 => (1440, "Sansha Scout Outpost", 3),
        1216 | 1441 => (1441, "Sansha Supply Yard", 3),
        1230 | 1442 => (1442, "Sansha Fleet Staging Point", 4),
        2290 | 2420 => (2420, "Centus Assembly Complex", 4),
        2327 | 2421 => (2421, "True Creations Research Facility", 4),

        // Serpentis
        2140 | 1450 => (1450, "Serpentis Scout Outpost", 3),
        1217 | 1451 => (1451, "Serpentis Supply Depot", 3),
        1231 | 1452 => (1452, "Serpentis Fleet Staging Point", 4),
        2300 | 2430 => (2430, "Serpentis Fleet Staging Point 2", 4),
        2330 | 2431 => (2431, "Serpentis Corporation Hydroponics Site", 4),

        // Angel Cartel
        2132 | 1410 => (1410, "Angel Scout Outpost", 3),
        1214 | 1411 => (1411, "Angel Cartel Supply Waypoint", 3),
        1228 | 1412 => (1412, "Angel Fleet Staging Point", 4),
        2115 | 2525 => (2525, "Angel Domination Fleet Staging Point 2.", 4),
        2311 | 2396 => (2396, "Angel Cartel Naval Shipyard", 4),

        // Fallbacks based on dungeon ID ranges
        d if d < 1500 => (1420, "Pirate Scout Outpost", 3),
        d if d < 2200 => (1422, "Pirate Outpost Escalation", 4),
        d if d < 2300 => (2402, "Pirate Fleet Staging Point", 4),
        _ => (2403, "The Maze", 4),
    }
}

pub fn expedition_dungeon_for_step(base_dungeon_id: u32, step_index: u8) -> u32 {
    base_dungeon_id + (step_index.saturating_sub(1) as u32)
}

pub fn roll_escalation_chance(ctx: &ServerContext, reg: &PveExplorationRegistry) -> bool {
    if reg.force_escalate {
        return true;
    }
    // 20% escalation roll on completion of combat anomaly/signature/DED complex
    ctx.mechanic_rng.roll(0.20)
}

pub async fn create_escalation_expedition_core(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    char_id: CharacterId,
    solar_system_id: SolarSystemId,
    source_dungeon_id: u32,
    dungeon_name: &str,
    pos: [f64; 3],
) -> Option<EscalatingPathEntry> {
    let (exp_base_did, exp_default_name, total_steps) = expedition_dungeon_chain(source_dungeon_id);
    let exp_name = sde
        .dungeons
        .get(&exp_base_did)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| exp_default_name.to_string());
    let dest_sys_id = find_escalation_destination_system_sde(sde, solar_system_id);

    let (entry, _created_ts) = crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let ts = now_sec(reg);
        let inst_id = 910_000 + next_pve_seq(reg);
        let e = EscalatingPathEntry {
            instance_id: inst_id,
            character_id: char_id,
            source_dungeon_id,
            expedition_dungeon_id: exp_base_did,
            expedition_name: exp_name.clone(),
            destination_solar_system_id: dest_sys_id,
            step_index: 1,
            total_steps,
            journal_text: format!(
                "Logs recovered from {dungeon_name} point to coordinates for '{exp_name}' (Step 1/{total_steps}) in solar system {dest_sys_id}."
            ),
            created_at_sec: ts,
            expires_at_sec: ts + 86_400,
            dungeons_entered: vec![source_dungeon_id],
        };
        reg.escalations_by_char.entry(char_id).or_default().push(e.clone());
        (e, ts)
    });

    db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bm_id = snap.next_entity_id;
        snap.bookmarks.insert(
            bm_id,
            eve_db::BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: format!("Escalation: {} [Step 1/{total_steps}]", entry.expedition_name),
                solar_system_id: dest_sys_id,
                x: pos[0],
                y: pos[1],
                z: pos[2],
                item_id: Some(entry.instance_id),
                memo: entry.journal_text.clone(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    spawn_expedition_site_instance_core(sde, systems, db, &entry).await;
    Some(entry)
}

pub async fn create_escalation_expedition(
    ctx: &ServerContext,
    char_id: CharacterId,
    solar_system_id: SolarSystemId,
    source_dungeon_id: u32,
    dungeon_name: &str,
    pos: [f64; 3],
) -> Option<EscalatingPathEntry> {
    let entry = create_escalation_expedition_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, solar_system_id, source_dungeon_id, dungeon_name, pos).await?;
    let _ = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnEscalatingPathMessage",
        "charid",
        vec![
            PyRep::String(format!(
                "Escalation discovered: {} in system {}",
                entry.expedition_name, entry.destination_solar_system_id
            )),
            PyRep::Integer(entry.instance_id),
        ],
    );
    Some(entry)
}

pub async fn spawn_expedition_site_instance_core(
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    db: &std::sync::Arc<eve_db::WorldRepository>,
    entry: &EscalatingPathEntry,
) {
    let dest_sys = entry.destination_solar_system_id;
    let site_id = format!("EXP-{}-{}", entry.instance_id, entry.step_index);
    let pos = [
        -45_000_000.0 + (entry.step_index as f64) * 5_000_000.0,
        15_000_000.0,
        30_000_000.0,
    ];
    let initial_balls = build_wave_npc_balls(sde, dest_sys, entry.expedition_dungeon_id, 1, pos, None);
    let mut spawned_ids = Vec::new();
    let sys_handle = systems.get_or_spawn(dest_sys);
    for b in initial_balls {
        spawned_ids.push(b.id);
        let cs = crate::services::pve_exploration_seeding::build_type_combat_state_from_sde(sde, b.type_id);
        let _ = sys_handle.spawn_ball_with_combat(b, cs).await;
    }

    crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let sites = reg.sites_by_system.entry(dest_sys).or_default();
        sites.retain(|s| s.site_id != site_id);
        sites.push(PveSiteInstance {
            site_id,
            dungeon_id: entry.expedition_dungeon_id,
            dungeon_name: format!(
                "{} [Step {}/{}]",
                entry.expedition_name, entry.step_index, entry.total_steps
            ),
            solar_system_id: dest_sys,
            site_category: "Expedition".into(),
            ded_rating: None,
            wormhole_class: None,
            position: pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: 2,
            active_npc_ball_ids: spawned_ids,
            hackable_can_ids: Vec::new(),
            npc_guards_spawned: true,
            ghost_timer_started_at: None,
            ghost_detonate_after_sec: 0,
            ghost_detonated: false,
            sleeper_rift_stabilized: true,
            sleeper_sentry_disabled: true,
            sleeper_gas_cloud_active: false,
            cleared: false,
            created_unix: entry.created_at_sec,
            lifetime_seconds: entry.expires_at_sec.saturating_sub(entry.created_at_sec),
            expedition_instance_id: Some(entry.instance_id),
            gate_ball_id: None,
        });
    });
}

pub async fn spawn_expedition_site_instance(
    ctx: &ServerContext,
    entry: &EscalatingPathEntry,
) {
    spawn_expedition_site_instance_core(&ctx.sde, &ctx.systems, &ctx.db, entry).await;
}

pub async fn advance_expedition_step_core(
    db: &std::sync::Arc<eve_db::WorldRepository>,
    sde: &std::sync::Arc<eve_sde::SdeRegistry>,
    systems: &std::sync::Arc<crate::system_actor::SystemRegistry>,
    char_id: CharacterId,
    instance_id: i64,
) -> Result<Option<EscalatingPathEntry>, BridgeRpcError> {
    let mut updated_entry = None;
    let mut completed = false;

    crate::services::pve_exploration::with_pve_exploration_reg_db(db, db.clock().now_unix_secs(), |reg| {
        let ts = now_sec(reg);
        let Some(list) = reg.escalations_by_char.get_mut(&char_id) else {
            return;
        };
        let Some(entry) = list.iter_mut().find(|e| e.instance_id == instance_id) else {
            return;
        };

        entry.dungeons_entered.push(entry.expedition_dungeon_id);

        if entry.step_index < entry.total_steps {
            entry.step_index += 1;
            let next_dest = find_escalation_destination_system_sde(sde, entry.destination_solar_system_id);
            entry.destination_solar_system_id = next_dest;
            let (exp_base_did, _, _) = expedition_dungeon_chain(entry.source_dungeon_id);
            entry.expedition_dungeon_id = expedition_dungeon_for_step(exp_base_did, entry.step_index);
            entry.expires_at_sec = ts + 86_400;
            entry.journal_text = format!(
                "Intelligence confirmed at previous encounter. Target coordinates updated for '{}' (Step {}/{}) in solar system {}.",
                entry.expedition_name, entry.step_index, entry.total_steps, next_dest
            );
            updated_entry = Some(entry.clone());
        } else {
            completed = true;
            let mut done_entry = entry.clone();
            done_entry.journal_text = format!(
                "Expedition '{}' has been fully completed! All pirate overseers neutralized.",
                done_entry.expedition_name
            );
            list.retain(|e| e.instance_id != instance_id);
            updated_entry = Some(done_entry);
        }
    });

    if let Some(ref entry) = updated_entry {
        if !completed {
            spawn_expedition_site_instance_core(sde, systems, db, entry).await;

            db.with_mut_snapshot(|snap| {
                if let Some(bm) = snap
                    .bookmarks
                    .values_mut()
                    .find(|b| b.owner_id == char_id && b.item_id == Some(instance_id))
                {
                    bm.solar_system_id = entry.destination_solar_system_id;
                    bm.label = format!(
                        "Escalation: {} [Step {}/{}]",
                        entry.expedition_name, entry.step_index, entry.total_steps
                    );
                    bm.memo = entry.journal_text.clone();
                }
            });
        } else {
            let dest_sys = entry.destination_solar_system_id;
            let pos = [-45_000_000.0, 15_000_000.0, 30_000_000.0];
            crate::services::pve_exploration::drop_site_loot_into_space_core(db, systems, dest_sys, char_id, pos, 19419, 1).await;

            db.with_mut_snapshot(|snap| {
                snap.bookmarks.retain(|_, b| !(b.owner_id == char_id && b.item_id == Some(instance_id)));
            });
        }
    }

    Ok(updated_entry)
}

pub async fn advance_expedition_step(
    ctx: &ServerContext,
    char_id: CharacterId,
    instance_id: i64,
) -> Result<Option<EscalatingPathEntry>, BridgeRpcError> {
    let updated = advance_expedition_step_core(&ctx.db, &ctx.sde, &ctx.systems, char_id, instance_id).await?;
    if let Some(ref entry) = updated {
        let is_completed = entry.step_index >= entry.total_steps;
        let msg = if is_completed {
            format!("Expedition '{}' successfully completed!", entry.expedition_name)
        } else {
            format!(
                "Expedition progressed to Step {}/{}: {} in system {}",
                entry.step_index, entry.total_steps, entry.expedition_name, entry.destination_solar_system_id
            )
        };
        let _ = ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnEscalatingPathMessage",
            "charid",
            vec![
                PyRep::String(msg),
                PyRep::Integer(entry.instance_id),
            ],
        );
    }
    Ok(updated)
}

pub fn get_my_escalating_path_details(reg: &PveExplorationRegistry, char_id: CharacterId) -> Vec<PyRep> {
    let now = now_sec(reg);
    reg.escalations_by_char
        .get(&char_id)
        .map(|list| {
            list.iter()
                .filter(|e| now < e.expires_at_sec)
                .map(|e| {
                    PyRep::keyval([
                        ("instanceID", PyRep::Integer(e.instance_id)),
                        ("sourceDungeonID", PyRep::Integer(e.source_dungeon_id as i64)),
                        ("dungeonID", PyRep::Integer(e.expedition_dungeon_id as i64)),
                        ("dungeonName", PyRep::String(e.expedition_name.clone())),
                        ("destinationSolarSystemID", PyRep::Integer(e.destination_solar_system_id as i64)),
                        ("stepIndex", PyRep::Integer(e.step_index as i64)),
                        ("totalSteps", PyRep::Integer(e.total_steps as i64)),
                        ("journalText", PyRep::String(e.journal_text.clone())),
                        ("createdAt", PyRep::Integer(e.created_at_sec as i64)),
                        ("expiresAt", PyRep::Integer(e.expires_at_sec as i64)),
                    ])
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn get_escalating_path_dungeons_entered(reg: &PveExplorationRegistry, char_id: CharacterId) -> Vec<PyRep> {
    reg.escalations_by_char
        .get(&char_id)
        .map(|list| {
            list.iter()
                .flat_map(|e| e.dungeons_entered.iter().copied())
                .map(|d| PyRep::Integer(d as i64))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}
