//! Unit tests for `fw_gameplay` (complex timers/VP, acceleration gate validation, supply cache/depot & I-Hub damage, warzone site spawning, 1 Hz tick).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn find_ship_type_for_group(sde: &SdeRegistry, group_id: u32, empire_only: bool) -> u32 {
    sde.types
        .iter()
        .filter(|(_, t)| {
            t.published
                && t.group_id == group_id
                && (!empire_only || matches!(t.faction_id, None | Some(500_001..=500_004)))
                && (!empire_only || matches!(t.meta_group_id, None | Some(1) | Some(3) | Some(4)))
        })
        .map(|(&tid, _)| tid)
        .min()
        .unwrap_or(match group_id {
            25 => 587,   // Rifter
            324 => 11365, // Vengeance
            420 => 16236, // Coercer
            26 => 620,   // Osprey
            419 => 16227, // Ferox
            27 => 641,   // Megathron
            _ => 587,
        })
}

fn clone_char_template(snap: &eve_db::WorldStateSnapshot, char_id: i64, fac_id: i64, sys_id: u32, ship_id: i64) -> CharacterRecord {
    let mut c = snap.characters.values().next().unwrap().clone();
    c.character_id = char_id;
    c.name = format!("Pilot_{char_id}");
    c.corporation_id = 1_000_009;
    c.alliance_id = None;
    c.war_faction_id = Some(fac_id);
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.active_ship_id = ship_id;
    c.balance = IskCents(100_000_00);
    c.loyalty_points.clear();
    c
}

fn seed_warzone_and_pilot(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    fac_id: i64,
    occ_fac: i64,
) -> i64 {
    let ship_tid = find_ship_type_for_group(&ctx.sde, 25, true);
    let neighbor_sys = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id && j.to_solar_system_id != sys_id)
        .map(|j| j.to_solar_system_id)
        .unwrap_or(30002506);
    let opp_fac = if occ_fac == 500_004 { 500_001 } else { 500_004 };
    let mut ship_id = 0_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_004,
                occupier_faction_id: occ_fac,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.fw_system_occupations.insert(
            neighbor_sys,
            FwSystemOccupationRecord {
                solar_system_id: neighbor_sys,
                owner_faction_id: opp_fac,
                occupier_faction_id: opp_fac,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        snap.next_entity_id += 1;
        ship_id = snap.next_entity_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_tid,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let ch = clone_char_template(snap, char_id, fac_id, sys_id, ship_id);
        snap.characters.insert(char_id, ch);
    });
    ship_id
}

#[test]
fn complex_capture_timers_vp_and_battlefield_centers_exact_values() {
    assert_eq!(complex_base_capture_duration_seconds("Novice"), 600);
    assert_eq!(complex_base_capture_duration_seconds("Small"), 600);
    assert_eq!(complex_base_capture_duration_seconds("Medium"), 900);
    assert_eq!(complex_base_capture_duration_seconds("Large"), 1200);
    assert_eq!(complex_base_capture_duration_seconds("Open"), 1200);
    assert_eq!(complex_base_capture_duration_seconds("CustomUnknown"), 600);

    assert_eq!(complex_victory_points("Novice"), 50);
    assert_eq!(complex_victory_points("Scout"), 50);
    assert_eq!(complex_victory_points("Small"), 75);
    assert_eq!(complex_victory_points("Medium"), 600);
    assert_eq!(complex_victory_points("Large"), 300);
    assert_eq!(complex_victory_points("Open"), 350);
    assert_eq!(complex_victory_points("Other"), 50);

    let ctx = test_ctx();
    let centers = battlefield_zone_centers_for_system(&ctx, 30002505);
    assert!((centers[1][0] - centers[0][0] - 15_000.0).abs() < 1e-6);
    assert!((centers[2][0] - centers[1][0] - 15_000.0).abs() < 1e-6);
    assert_eq!(centers[0][1], centers[1][1]);
    assert_eq!(centers[1][2], centers[2][2]);

    let lp_corp_gal = militia_lp_corp_id(&ctx.sde, 500_004);
    assert!(lp_corp_gal > 1_000_000);
    assert_eq!(militia_lp_corp_id(&ctx.sde, -42), -42);
    assert_eq!(militia_lp_corp_id(&ctx.sde, 500_010), 1_000_437);
    assert_eq!(militia_lp_corp_id(&ctx.sde, 500_011), 1_000_436);
}

#[test]
fn warzone_site_spawning_frontline_rules_and_gate_validation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let char_id = 910_001_i64;
    let ship_id = seed_warzone_and_pilot(&ctx, sys_id, char_id, 500_004, 500_004);

    let (def_status, _, def_tiers, _) = get_system_frontline_rules(&ctx, 30000142);
    assert_eq!(def_status, STATUS_REARGUARD);
    assert_eq!(def_tiers, vec!["Novice", "Small"]);

    let (status_val, mult, allowed_tiers, _) = get_system_frontline_rules(&ctx, sys_id);
    assert_eq!(status_val, STATUS_FRONTLINE);
    assert_eq!(mult, 1.0);
    assert!(allowed_tiers.contains(&"Battlefield"));

    rt.block_on(ensure_warzone_sites_spawned(&ctx, sys_id));
    let (gate_ids, supply_ids, has_ihub) = ctx.db.with_snapshot(|snap| {
        let gids: Vec<i64> = snap
            .fw_runtime
            .fw_complexes
            .iter()
            .filter(|(_, (s, _, _, _, _))| *s == sys_id)
            .map(|(&id, _)| id)
            .collect();
        let sids: Vec<i64> = snap
            .fw_runtime
            .supply_sites
            .iter()
            .filter(|(_, (s, _, _))| *s == sys_id)
            .map(|(&id, _)| id)
            .collect();
        let ihub = snap
            .structures
            .values()
            .any(|st| st.solar_system_id == sys_id && st.type_id == TYPE_FW_IHUB);
        (gids, sids, ihub)
    });
    assert!(!gate_ids.is_empty());
    assert!(!supply_ids.is_empty());
    assert!(has_ihub);

    let novice_gate_id = ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .fw_complexes
            .iter()
            .find(|(_, (s, t, _, _, _))| *s == sys_id && t == "Novice")
            .map(|(&id, _)| id)
            .unwrap_or(gate_ids[0])
    });

    // Unspawned ship ball -> ShipNotInSpace
    let err_no_ship = rt
        .block_on(activate_fw_complex_gate_in_space(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            novice_gate_id,
        ))
        .unwrap_err();
    assert_eq!(err_no_ship.code, "ShipNotInSpace");

    // Ship too far away -> OutsideGateActivationRange
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let gate_pos = rt
        .block_on(sys_handle.get_ball_snapshot(novice_gate_id))
        .unwrap()
        .unwrap()
        .1
        .position;
    let frigate_tid = find_ship_type_for_group(&ctx.sde, 25, true);
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: frigate_tid,
        position: gate_pos + DVec3::new(15_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();
    let err_far = rt
        .block_on(activate_fw_complex_gate_in_space(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            novice_gate_id,
        ))
        .unwrap_err();
    assert_eq!(err_far.code, "OutsideGateActivationRange");

    // Cruiser at Novice gate -> ShipTooLargeForGate
    let cruiser_tid = find_ship_type_for_group(&ctx.sde, 26, false);
    rt.block_on(sys_handle.remove_ball(ship_id)).unwrap();
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: cruiser_tid,
        position: gate_pos + DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1.0e7,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();
    let err_large = rt
        .block_on(activate_fw_complex_gate_in_space(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            novice_gate_id,
        ))
        .unwrap_err();
    assert_eq!(err_large.code, "ShipTooLargeForGate");

    // T2 frigate at Novice gate -> ShipTypeNotAllowedInComplex
    let t2_frig_tid = find_ship_type_for_group(&ctx.sde, 324, false);
    rt.block_on(sys_handle.remove_ball(ship_id)).unwrap();
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: t2_frig_tid,
        position: gate_pos + DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();
    let err_t2 = rt
        .block_on(activate_fw_complex_gate_in_space(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            novice_gate_id,
        ))
        .unwrap_err();
    assert_eq!(err_t2.code, "ShipTypeNotAllowedInComplex");

    // T1 Frigate at Novice gate -> succeeds and records occupant in complex_occupants
    rt.block_on(sys_handle.remove_ball(ship_id)).unwrap();
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: frigate_tid,
        position: gate_pos + DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();
    let warp_dest = rt
        .block_on(activate_fw_complex_gate_in_space(
            &ctx,
            char_id,
            ship_id,
            sys_id,
            novice_gate_id,
        ))
        .expect("frigate activates Novice gate");
    assert_ne!(warp_dest, PyRep::None);
    assert!(ctx.db.with_snapshot(|snap| {
        snap.fw_runtime
            .complex_occupants
            .get(&novice_gate_id)
            .is_some_and(|occ| occ.iter().any(|&(cid, shid)| cid == char_id && shid == ship_id))
    }));
}

#[test]
fn supply_site_and_ihub_combat_damage_and_destruction() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let attacker_id = 910_002_i64;
    let _ship_id = seed_warzone_and_pilot(&ctx, sys_id, attacker_id, 500_001, 500_004);

    let supply_id = 77_000_101_i64;
    let ihub_id = 77_000_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .supply_sites
            .insert(supply_id, (sys_id, "SupplyDepot".into(), 500_004));
        snap.structures.insert(
            supply_id,
            UpwellStructureRecord {
                structure_id: supply_id,
                solar_system_id: sys_id,
                type_id: TYPE_FW_SUPPLY_SITE,
                owner_corp_id: 1_000_180,
                state: "ShieldVulnerable".into(),
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                position: [1_000.0, 2_000.0, 3_000.0],
                ..Default::default()
            },
        );
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                solar_system_id: sys_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_180,
                state: "ShieldVulnerable".into(),
                shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                hull_hp: 1_000.0,
                position: [4_000.0, 5_000.0, 6_000.0],
                ..Default::default()
            },
        );
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, supply_id, TYPE_FW_SUPPLY_SITE));
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, ihub_id, TYPE_FW_IHUB));
        assert!(!is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, 999_999, 0));
    });

    let partial = rt
        .block_on(apply_fw_or_insurgency_structure_damage(
            &ctx,
            attacker_id,
            supply_id,
            200.0,
        ))
        .expect("partial damage");
    assert_eq!(
        partial.dict_get("shieldHP").and_then(|v| match v {
            PyRep::Real(x) => Some(*x),
            _ => None,
        }),
        Some(300.0)
    );

    let destroyed = rt
        .block_on(apply_fw_or_insurgency_structure_damage(
            &ctx,
            attacker_id,
            supply_id,
            2_000.0,
        ))
        .expect("destroy supply depot");
    assert_eq!(
        destroyed.dict_get("state").and_then(|v| v.as_str()),
        Some("Destroyed")
    );
    let can_id = destroyed
        .dict_get("lootContainerID")
        .and_then(|v| v.as_i64())
        .expect("loot container ID");
    assert!(ctx.db.with_snapshot(|snap| snap.items.contains_key(&can_id)));

    let invuln_err = rt
        .block_on(apply_fw_or_insurgency_structure_damage(
            &ctx,
            attacker_id,
            ihub_id,
            500.0,
        ))
        .unwrap_err();
    assert_eq!(invuln_err.code, "IHubInvulnerable");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, true);
    });
    let ihub_res = rt
        .block_on(apply_fw_or_insurgency_structure_damage(
            &ctx,
            attacker_id,
            ihub_id,
            5_000.0,
        ))
        .expect("opposing pilot destroys vulnerable I-Hub");
    assert_eq!(
        ihub_res.dict_get("state").and_then(|v| v.as_str()),
        Some("Destroyed")
    );
    assert_eq!(
        ihub_res
            .dict_get("pendingOccupierFactionID")
            .and_then(|v| v.as_i64()),
        Some(500_001)
    );
}

#[test]
fn fw_complex_completion_and_tick_progression() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let char_id = 910_003_i64;
    let ship_id = seed_warzone_and_pilot(&ctx, sys_id, char_id, 500_001, 500_004);
    let gate_id = 77_000_201_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .fw_complexes
            .insert(gate_id, (sys_id, "Novice".into(), 500_004_i64, 99.9_f64, false));
        snap.fw_runtime.complex_guards_cleared.insert(gate_id, true);
        snap.fw_runtime
            .complex_occupants
            .insert(gate_id, vec![(char_id, ship_id)]);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: pocket_pos_for_complex(gate_id),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();

    let now = ctx.now_unix();
    rt.block_on(tick_fw_gameplay(&ctx, now));

    let lp_corp = militia_lp_corp_id(&ctx.sde, 500_001);
    let (lp, vp, done) = ctx.db.with_snapshot(|snap| {
        let lp = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.loyalty_points.get(&lp_corp))
            .copied()
            .unwrap_or(0);
        let vp = snap
            .fw_system_occupations
            .get(&sys_id)
            .map(|o| o.victory_points)
            .unwrap_or(0);
        let done = snap
            .fw_runtime
            .fw_complexes
            .get(&gate_id)
            .map(|c| c.4)
            .unwrap_or(false);
        (lp, vp, done)
    });
    assert!(done);
    assert!(lp > 0);
    assert_eq!(vp, 50);

    let guard_ball_id = 500_000_000 + gate_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime
            .fw_complexes
            .insert(gate_id, (sys_id, "Small".into(), 500_004_i64, 0.0_f64, false));
    });
    rt.block_on(sys_handle.spawn_ball(Ball {
        id: guard_ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }))
    .unwrap();
    rt.block_on(on_fw_complex_guard_destroyed(&ctx.systems, &ctx.db, sys_id, guard_ball_id));
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.complex_guards_cleared.get(&gate_id).copied() == Some(true)));
}
