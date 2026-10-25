//! `pve_exploration_seeding` — Cosmic anomalies, signatures, and DED complexes seeding.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    wormhole_class_for_system, with_pve_exploration_reg,
    ItemId, PveSiteInstance,
};
use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PirateFaction {
    Guristas,
    BloodRaiders,
    Sansha,
    Serpentis,
    AngelCartel,
}

pub fn pirate_faction_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> PirateFaction {
    let Some(sys) = sde.solar_systems.get(&solar_system_id) else {
        return PirateFaction::Guristas;
    };
    let con_fac = sde.constellations.get(&sys.constellation_id).and_then(|c| c.faction_id);
    let reg = sde.regions.get(&sys.region_id);
    let reg_fac = reg.and_then(|r| r.faction_id);
    let fac = con_fac.or(reg_fac).unwrap_or(0);
    let reg_name = reg.map(|r| r.name.as_str()).unwrap_or("");

    match fac {
        500001 | 500010 => PirateFaction::Guristas,
        500003 | 500012 | 500019 | 500007 => {
            if reg_name == "Derelik" || reg_name == "Tash-Murkon" || reg_name == "Stain" || reg_name == "Catch" {
                PirateFaction::Sansha
            } else {
                PirateFaction::BloodRaiders
            }
        }
        500004 | 500011 => PirateFaction::Serpentis,
        500002 | 500015 => PirateFaction::AngelCartel,
        _ => {
            if reg_name.contains("Forge") || reg_name.contains("Lonetrek") || reg_name.contains("Citadel") || reg_name.contains("Black Rise") {
                PirateFaction::Guristas
            } else if reg_name.contains("Sinq") || reg_name.contains("Everyshore") || reg_name.contains("Essence") || reg_name.contains("Placid") || reg_name.contains("Verge") {
                PirateFaction::Serpentis
            } else if reg_name.contains("Domain") || reg_name.contains("Kador") || reg_name.contains("Kor-Azor") || reg_name.contains("Devoid") || reg_name.contains("Bleak") {
                PirateFaction::BloodRaiders
            } else {
                PirateFaction::AngelCartel
            }
        }
    }
}

// parity: feat:pve.combat-anomalies, feat:pve.wormhole-sites
pub fn anomaly_dungeons_for_system(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: SolarSystemId,
    true_sec: f64,
) -> Vec<(u32, &'static str, &'static str, u8, Option<u8>)> {
    if let Some(wh_class) = wormhole_class_for_system(solar_system_id) {
        return match wh_class {
            1 | 2 => vec![
                (3707, "Perimeter Ambush Point", "WormholeCombat", 3, Some(wh_class)),
                (3708, "Perimeter Camp", "WormholeCombat", 3, Some(wh_class)),
            ],
            3 | 4 => vec![
                (3710, "Fortification Frontier Stronghold", "WormholeCombat", 3, Some(wh_class)),
                (3713, "Outpost Frontier Stronghold", "WormholeCombat", 3, Some(wh_class)),
            ],
            _ => vec![
                (3704, "Core Citadel", "WormholeCombat", 4, Some(wh_class)),
                (3716, "Core Garrison", "WormholeCombat", 4, Some(wh_class)),
            ],
        };
    }
    match pirate_faction_for_system(sde, solar_system_id) {
        PirateFaction::Guristas => if true_sec >= 0.5 {
            vec![(2134, "Guristas Hideaway", "CombatAnomaly", 2, None), (1215, "Guristas Watch", "CombatAnomaly", 3, None)]
        } else if true_sec > 0.0 {
            vec![(1232, "Guristas Vigil", "CombatAnomaly", 3, None), (2283, "Guristas Haven", "CombatAnomaly", 3, None)]
        } else {
            vec![(2283, "Guristas Haven", "CombatAnomaly", 3, None), (2321, "Guristas Sanctum", "CombatAnomaly", 4, None)]
        },
        PirateFaction::BloodRaiders => if true_sec >= 0.5 {
            vec![(2141, "Blood Hideaway", "CombatAnomaly", 2, None), (1218, "Blood Watch", "CombatAnomaly", 3, None)]
        } else if true_sec > 0.0 {
            vec![(1229, "Blood Vigil", "CombatAnomaly", 3, None), (2282, "Blood Haven", "CombatAnomaly", 3, None)]
        } else {
            vec![(2282, "Blood Haven", "CombatAnomaly", 3, None), (2319, "Blood Sanctum", "CombatAnomaly", 4, None)]
        },
        PirateFaction::Sansha => if true_sec >= 0.5 {
            vec![(2135, "Sansha Hideaway", "CombatAnomaly", 2, None), (1216, "Sansha Watch", "CombatAnomaly", 3, None)]
        } else if true_sec > 0.0 {
            vec![(1230, "Sansha Vigil", "CombatAnomaly", 3, None), (2290, "Sansha Haven", "CombatAnomaly", 3, None)]
        } else {
            vec![(2290, "Sansha Haven", "CombatAnomaly", 3, None), (2327, "Sansha Sanctum", "CombatAnomaly", 4, None)]
        },
        PirateFaction::Serpentis => if true_sec >= 0.5 {
            vec![(2140, "Serpentis Hideaway", "CombatAnomaly", 2, None), (1217, "Serpentis Watch", "CombatAnomaly", 3, None)]
        } else if true_sec > 0.0 {
            vec![(1231, "Serpentis Vigil", "CombatAnomaly", 3, None), (2300, "Serpentis Haven", "CombatAnomaly", 3, None)]
        } else {
            vec![(2300, "Serpentis Haven", "CombatAnomaly", 3, None), (2330, "Serpentis Sanctum", "CombatAnomaly", 4, None)]
        },
        PirateFaction::AngelCartel => if true_sec >= 0.5 {
            vec![(2132, "Angel Hideaway", "CombatAnomaly", 2, None), (1214, "Angel Watch", "CombatAnomaly", 3, None)]
        } else if true_sec > 0.0 {
            vec![(1228, "Angel Vigil", "CombatAnomaly", 3, None), (2115, "Angel Haven", "CombatAnomaly", 3, None)]
        } else {
            vec![(2115, "Angel Haven", "CombatAnomaly", 3, None), (2311, "Angel Sanctum", "CombatAnomaly", 4, None)]
        },
    }
}

// parity: feat:pve.ded-complexes
pub fn ded_complex_for_rating(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: SolarSystemId,
    ded_rating: u8,
) -> (u32, &'static str) {
    match pirate_faction_for_system(sde, solar_system_id) {
        PirateFaction::Guristas => match ded_rating {
            1 => (1012, "Guristas Pithi Invaded Station"), 2 | 3 => (5302, "Guristas Guerilla Grounds"),
            4 => (98, "Guristas Scout Outpost"), 5 => (167, "Guristas Troop Reinvigoration Camp"), _ => (140, "The Maze"),
        },
        PirateFaction::BloodRaiders => match ded_rating {
            1 => (1012, "Blood Raider Scouting Station"), 2 | 3 => (5317, "Blood Raider Human Farm"),
            4 => (106, "Mul-Zatah Monastery"), 5 => (5221, "Blood Raider Prison Camp"), _ => (2260, "Blood Raider Naval Shipyard"),
        },
        PirateFaction::Sansha => match ded_rating {
            1 => (1012, "Sansha Outpost"), 2 | 3 => (110, "Sansha's Command Relay Outpost"),
            4 => (111, "Sansha Military Outpost"), 5 => (49, "Sansha War Supply Complex"), _ => (249, "Sansha's Fortified Outpost"),
        },
        PirateFaction::Serpentis => match ded_rating {
            1 => (1012, "Serpentis Base"), 2 => (116, "Serpentis Drug Outlet"), 3 => (51, "Serpentis Narcotic Warehouses"),
            4 => (119, "Serpentis Phi-Outpost"), 5 => (5269, "Serpentis Corporation Hydroponics Site"), _ => (144, "Serpentis Fleet Shipyard"),
        },
        PirateFaction::AngelCartel => match ded_rating {
            1 => (1012, "Knaaninn Aranuri"), 2 | 3 => (150, "Angel Creo-Corp Mining"), 4 | 5 => (168, "Angel's Red Light District"),
            6 => (47, "Crimson Hand Supply Depot"), 7 => (59, "Angel Military Operations Complex"), 8 => (5203, "Angel Naval Shipyard"),
            9 => (2519, "Angel Domination Fleet Staging Point"), _ => (141, "Angel Cartel Naval Shipyard"),
        },
    }
}

// parity: feat:pve.ded-complexes
pub fn ded_ship_restriction_for_rating(ded_rating: u8) -> (f64, &'static str, &'static [u32]) {
    match ded_rating {
        1 => (5_000_000.0, "Frigate Hulls Only (1/10)", &[25, 324, 830, 831, 834, 1283, 1527]),
        2 => (10_000_000.0, "Frigate and Destroyer Hulls Only (2/10)", &[25, 324, 420, 541, 830, 831, 834, 1283, 1305, 1527, 1534]),
        3 => (25_000_000.0, "Up to Cruiser Hulls (3/10)", &[25, 26, 324, 358, 420, 541, 830, 831, 832, 833, 834, 894, 906, 963, 1283, 1305, 1527, 1534, 1972]),
        4 => (75_000_000.0, "Up to Battlecruiser Hulls (4/10)", &[25, 26, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972]),
        _ => (250_000_000.0, "Up to Battleship Hulls (5/10-10/10, No Capitals)", &[25, 26, 27, 324, 358, 419, 420, 540, 541, 830, 831, 832, 833, 834, 894, 898, 900, 906, 963, 1201, 1283, 1305, 1527, 1534, 1972]),
    }
}

fn make_sig_record(
    signature_id: String,
    solar_system_id: SolarSystemId,
    sig_group: &str,
    type_name: String,
    position: [f64; 3],
    created_unix: u64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: sig_group.into(),
        type_name,
        position,
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    }
}

fn make_can_record(can_id: ItemId, type_id: TypeId, solar_system_id: SolarSystemId) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: can_id,
        type_id,
        owner_id: 1_000_125,
        location_id: solar_system_id as i64,
        flag: InventoryFlag::None,
        quantity: 1,
        singleton: true,
        ..Default::default()
    }
}

// parity: feat:pve.combat-anomalies, feat:pve.combat-signatures, feat:pve.ded-complexes
// parity: feat:pve.data-sites, feat:pve.relic-sites, feat:pve.ghost-sites, feat:pve.sleeper-caches, feat:pve.wormhole-sites
pub async fn ensure_system_pve_sites_seeded(ctx: &ServerContext, solar_system_id: SolarSystemId) {
    let already_seeded = with_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.get(&solar_system_id).copied().unwrap_or(false)
    });
    if already_seeded {
        return;
    }

    let true_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let wh_class = wormhole_class_for_system(solar_system_id);

    let now = ctx.now_unix();
    let mut sites = Vec::new();
    let mut new_anoms = Vec::new();
    let mut new_sigs = Vec::new();
    let mut cans_to_insert = Vec::new();
    let mut balls_to_spawn = Vec::new();

    // 1. True-Sec Combat Anomalies (or Wormhole Sleeper Combat Anomalies)
    for (idx, (did, default_name, cat, waves, wh_c)) in
        anomaly_dungeons_for_system(&ctx.sde, solar_system_id, true_sec).into_iter().enumerate()
    {
        let d_name = ctx
            .sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .unwrap_or_else(|| default_name.to_string());
        let site_id = format!("PVE-ANOM-{solar_system_id}-{}", idx + 1);
        let pos = [25_000_000.0 + (idx as f64) * 5_000_000.0, 12_000_000.0, -8_000_000.0];
        let wave1_npcs = build_wave_npc_balls(&ctx.sde, solar_system_id, did, 1, pos, wh_c);
        let npc_ids: Vec<i64> = wave1_npcs.iter().map(|b| b.id).collect();
        balls_to_spawn.extend(wave1_npcs);

        new_anoms.push(CosmicAnomalyRecord {
            anomaly_id: site_id.clone(),
            solar_system_id,
            anomaly_type: "Combat".into(),
            name: d_name.clone(),
            position: pos,
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: now,
            lifetime_seconds: 86_400,
            cleared: false,
        });
        sites.push(PveSiteInstance {
            site_id,
            dungeon_id: did,
            dungeon_name: d_name,
            solar_system_id,
            site_category: cat.into(),
            wormhole_class: wh_c,
            position: pos,
            probe_required: false,
            signal_strength: 1.0,
            current_room: 1,
            total_rooms: 1,
            current_wave: 1,
            total_waves: waves,
            active_npc_ball_ids: npc_ids,
            npc_guards_spawned: true,
            created_unix: now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    }

    // 2. Scan-only Rated DED Complex Signature
    let default_ded_rating: u8 = if true_sec >= 0.5 { 3 } else if true_sec > 0.0 { 5 } else { 10 };
    let (ded_did, ded_fallback_name) = ded_complex_for_rating(&ctx.sde, solar_system_id, default_ded_rating);
    let ded_name = ctx
        .sde
        .dungeons
        .get(&ded_did)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_else(|| ded_fallback_name.to_string());
    let ded_sig_id = format!("DED-{solar_system_id}-{default_ded_rating}");
    let ded_pos = [45_000_000.0, -20_000_000.0, 15_000_000.0];
    let ded_room1_npcs = build_wave_npc_balls(&ctx.sde, solar_system_id, ded_did, 1, ded_pos, None);
    let ded_npc_ids: Vec<i64> = ded_room1_npcs.iter().map(|b| b.id).collect();
    balls_to_spawn.extend(ded_room1_npcs);
    new_sigs.push(make_sig_record(ded_sig_id.clone(), solar_system_id, "Combat", ded_name.clone(), ded_pos, now));
    sites.push(PveSiteInstance {
        site_id: ded_sig_id,
        dungeon_id: ded_did,
        dungeon_name: ded_name,
        solar_system_id,
        site_category: "DED".into(),
        ded_rating: Some(default_ded_rating),
        position: ded_pos,
        probe_required: true,
        current_room: 1,
        total_rooms: 3,
        current_wave: 1,
        total_waves: 3,
        active_npc_ball_ids: ded_npc_ids,
        npc_guards_spawned: true,
        created_unix: now,
        lifetime_seconds: 86_400,
        ..Default::default()
    });

    // 3. Data, Relic, Ghost & Sleeper Cache Signatures
    let guarded = wh_class.is_some() || true_sec <= 0.0;
    let sig_specs = [
        (format!("DAT-{solar_system_id}"), if guarded { 3729 } else { 1133 }, if guarded { "Unsecured Frontier Enclave Relay" } else { "Digital Plexus" }, "Data", "Data", [-32_000_000.0, 28_000_000.0, -10_000_000.0], 1_870_000_000, 21_740, guarded),
        (format!("REL-{solar_system_id}"), if guarded { 3722 } else { 1286 }, if guarded { "Forgotten Core Data Field" } else { "Ancient Ruins" }, "Relic", "Relic", [-38_000_000.0, -30_000_000.0, 22_000_000.0], 1_871_000_000, 21_741, guarded),
        (format!("GHO-{solar_system_id}"), if wh_class.is_some() { 5536 } else if true_sec <= 0.0 { 5535 } else if true_sec < 0.5 { 5534 } else { 5533 }, "Standard Angel Covert Research Facility", "Data", "Ghost", [52_000_000.0, 42_000_000.0, -18_000_000.0], 1_872_000_000, 21_740, false),
        (format!("SLC-{solar_system_id}"), if true_sec <= 0.0 { 5632 } else if true_sec < 0.5 { 5609 } else { 5604 }, "Standard Sleeper Cache", "Data", "SleeperCache", [-58_000_000.0, 14_000_000.0, 35_000_000.0], 1_873_000_000, 21_740, true),
    ];

    for (sig_id, did, fallback_name, grp, cat, pos, base_can_id, can_tid, has_guards) in sig_specs {
        let name = ctx.sde.dungeons.get(&did).map(|d| d.name.trim().to_string()).unwrap_or_else(|| fallback_name.to_string());
        let mut can_ids = Vec::new();
        for c_idx in 0..3_i64 {
            let cid = base_can_id + (solar_system_id as i64 % 100_000) * 100 + c_idx;
            can_ids.push(cid);
            cans_to_insert.push(make_can_record(cid, can_tid, solar_system_id));
        }
        let guard_balls = if has_guards {
            build_wave_npc_balls(&ctx.sde, solar_system_id, did, 1, pos, wh_class.or(Some(3)))
        } else {
            Vec::new()
        };
        let guard_ids: Vec<i64> = guard_balls.iter().map(|b| b.id).collect();
        balls_to_spawn.extend(guard_balls);

        let is_ghost = cat == "Ghost";
        let ghost_detonate_after_sec = if is_ghost { 90 } else { 0 };

        new_sigs.push(make_sig_record(sig_id.clone(), solar_system_id, grp, name.clone(), pos, now));
        sites.push(PveSiteInstance {
            site_id: sig_id,
            dungeon_id: did,
            dungeon_name: name,
            solar_system_id,
            site_category: cat.into(),
            wormhole_class: wh_class,
            position: pos,
            probe_required: true,
            current_room: 1,
            total_rooms: if cat == "SleeperCache" { 3 } else { 1 },
            current_wave: 1,
            total_waves: if cat == "SleeperCache" { 2 } else { 1 },
            active_npc_ball_ids: guard_ids,
            hackable_can_ids: can_ids,
            npc_guards_spawned: has_guards,
            ghost_detonate_after_sec,
            sleeper_gas_cloud_active: cat == "SleeperCache",
            created_unix: now,
            lifetime_seconds: 86_400,
            ..Default::default()
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for ball in balls_to_spawn {
        let cs = ctx.build_type_combat_state(ball.type_id);
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let sys_anoms = snap.cosmic_anomalies.entry(solar_system_id).or_default();
        for a in new_anoms {
            if !sys_anoms.iter().any(|x| x.anomaly_id == a.anomaly_id) {
                sys_anoms.push(a);
            }
        }
        let sys_sigs = snap.cosmic_signatures.entry(solar_system_id).or_default();
        for s in new_sigs {
            if !sys_sigs.iter().any(|x| x.signature_id == s.signature_id) {
                sys_sigs.push(s);
            }
        }
        for item in cans_to_insert {
            snap.items.insert(item.item_id, item);
        }
    });

    with_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.insert(solar_system_id, true);
        reg.sites_by_system.insert(solar_system_id, sites);
    });
}

pub fn pirate_npc_type_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> TypeId {
    match pirate_faction_for_system(sde, solar_system_id) {
        PirateFaction::Guristas => 10_962,
        PirateFaction::BloodRaiders => 10_960,
        PirateFaction::Sansha => 10_961,
        PirateFaction::Serpentis => 10_963,
        PirateFaction::AngelCartel => 10_959,
    }
}

pub fn pirate_commander_loot_for_system(sde: &eve_sde::SdeRegistry, solar_system_id: SolarSystemId) -> (TypeId, &'static str) {
    match pirate_faction_for_system(sde, solar_system_id) {
        PirateFaction::Guristas => (19_403, "Dread Guristas Modified Module"),
        PirateFaction::BloodRaiders => (19_404, "Dark Blood Modified Module"),
        PirateFaction::Sansha => (19_405, "True Sansha Modified Module"),
        PirateFaction::Serpentis => (19_406, "Shadow Serpentis Modified Module"),
        PirateFaction::AngelCartel => (19_407, "Domination Modified Module"),
    }
}

pub fn build_wave_npc_balls(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: SolarSystemId,
    dungeon_id: u32,
    wave_number: u8,
    center: [f64; 3],
    wormhole_class: Option<u8>,
) -> Vec<Ball> {
    let npc_type_id: TypeId = match wormhole_class {
        Some(1 | 2) => 30_192,
        Some(3 | 4) => 30_195,
        Some(_) => 30_198,
        None => pirate_npc_type_for_system(sde, solar_system_id),
    };
    let count = (wave_number as usize).clamp(2, 4);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let ball_id = 1_860_000_000
            + (solar_system_id as i64 % 10_000) * 100_000
            + (dungeon_id as i64 % 1_000) * 100
            + (wave_number as i64) * 10
            + (i as i64)
            + 1;
        out.push(Ball {
            id: ball_id,
            type_id: npc_type_id,
            position: DVec3::new(
                center[0] + (i as f64) * 2_500.0,
                center[1] + (wave_number as f64) * 1_500.0,
                center[2],
            ),
            velocity: DVec3::ZERO,
            max_velocity: 420.0,
            inertia: 1.0,
            mass: 2_500_000.0,
            radius: 180.0,
            speed_fraction: 0.75,
            mode: MovementMode::Stop,
        });
    }
    out
}

pub fn build_type_combat_state_from_sde(sde: &eve_sde::SdeRegistry, type_id: u32) -> eve_destiny::CombatState {
    if let Some(t) = sde.types.get(&type_id) {
        let a = &t.default_attributes;
        let shield = a.get(&eve_sde::attr_ids::SHIELD_CAPACITY).copied().unwrap_or(50.0);
        let armor = a.get(&eve_sde::attr_ids::ARMOR_HP).copied().unwrap_or(50.0);
        let hull = a.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(50.0);
        let cap = a.get(&eve_sde::attr_ids::CAPACITOR_CAPACITY).copied().unwrap_or(100.0);
        let sig = a.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied().unwrap_or(t.radius.max(25.0));
        let shield_rech = a.get(&eve_sde::attr_ids::SHIELD_RECHARGE_RATE).copied().unwrap_or(120_000.0);
        let cap_rech = a.get(&eve_sde::attr_ids::RECHARGE_RATE).copied().unwrap_or(120_000.0);
        eve_destiny::CombatState {
            shield_hp: shield,
            max_shield_hp: shield,
            armor_hp: armor,
            max_armor_hp: armor,
            structure_hp: hull,
            max_structure_hp: hull,
            capacitor: cap,
            max_capacitor: cap,
            signature_radius: sig,
            shield_recharge_time_ms: shield_rech,
            capacitor_recharge_time_ms: cap_rech,
            ..eve_destiny::CombatState::default()
        }
    } else {
        eve_destiny::CombatState::default()
    }
}
