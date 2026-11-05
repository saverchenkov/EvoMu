//! Deep branch and mutation-killing unit tests for `fw_gameplay` (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::*;
use crate::services::fw_services::{STATUS_COMMAND, STATUS_FRONTLINE, STATUS_REARGUARD};
use crate::services::insurgency_services::{
    PIRATE_STRONGHOLD_ANGEL_TYPE, PIRATE_STRONGHOLD_GURISTAS_TYPE,
};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry};
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
            25 => 587,
            324 => 11365,
            420 => 16236,
            26 => 620,
            419 => 16227,
            27 => 641,
            28 => 648,
            _ => 587,
        })
}

fn seed_pilot(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    fac_id: Option<i64>,
    ship_id: i64,
    ship_tid: u32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch: CharacterRecord = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.name = format!("FwPilot_{char_id}");
        ch.corporation_id = 1_000_009;
        ch.alliance_id = None;
        ch.war_faction_id = fac_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents(10_000_000);
        ch.loyalty_points.clear();
        ch.standings.clear();
        snap.characters.insert(char_id, ch);
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
    });
}

#[test]
fn militia_lp_corp_frontline_rules_and_pocket_pos_cover_all_branches() {
    let ctx = test_ctx();
    let mut empty_factions_sde = (*ctx.sde).clone();
    empty_factions_sde.factions.clear();

    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_001), 1_000_180);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_002), 1_000_182);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_003), 1_000_179);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_004), 1_000_181);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_010), 1_000_437);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_011), 1_000_436);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, 500_999), 500_999);
    assert_eq!(militia_lp_corp_id(&empty_factions_sde, -77), -77);

    // Pocket pos slot arithmetic: 250_000.0 + (gate_id.unsigned_abs() % 11) * 4_000.0
    assert_eq!(pocket_pos_for_complex(0), DVec3::new(250_000.0, 0.0, 0.0));
    assert_eq!(pocket_pos_for_complex(11), DVec3::new(250_000.0, 0.0, 0.0));
    assert_eq!(pocket_pos_for_complex(3), DVec3::new(262_000.0, 0.0, 0.0));
    assert_eq!(pocket_pos_for_complex(-5), DVec3::new(270_000.0, 0.0, 0.0));

    // Setup Rearguard -> Command -> Frontline topology: sys_a -- sys_b -- sys_c
    let sys_a = 30002505_u32;
    let sys_b = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_a && j.to_solar_system_id != sys_a)
        .map(|j| j.to_solar_system_id)
        .unwrap_or(30002506);
    let sys_c = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_b && j.to_solar_system_id != sys_a && j.to_solar_system_id != sys_b)
        .map(|j| j.to_solar_system_id)
        .unwrap_or(30002507);

    ctx.db.with_mut_snapshot(|snap| {
        for s in [sys_a, sys_b, sys_c] {
            snap.fw_system_occupations.insert(
                s,
                FwSystemOccupationRecord {
                    solar_system_id: s,
                    owner_faction_id: 500_004,
                    occupier_faction_id: 500_004,
                    victory_points: 0,
                    victory_points_threshold: 3_000,
                },
            );
        }
    });
    // All friendly neighbors -> Rearguard (0.50x LP, Novice/Small, 0.50x speed)
    let (st_rear, lp_rear, tiers_rear, spd_rear) = get_system_frontline_rules(&ctx, sys_a);
    assert_eq!(st_rear, STATUS_REARGUARD);
    assert!((lp_rear - 0.50).abs() < 1e-9);
    assert_eq!(tiers_rear, vec!["Novice", "Small"]);
    assert!((spd_rear - 0.50).abs() < 1e-9);

    // Flip sys_c to opposing faction (500_001): sys_b is Frontline, sys_a is Command Operations!
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.get_mut(&sys_c).unwrap().occupier_faction_id = 500_001;
    });
    let (st_front, lp_front, tiers_front, spd_front) = get_system_frontline_rules(&ctx, sys_b);
    assert_eq!(st_front, STATUS_FRONTLINE);
    assert!((lp_front - 1.0).abs() < 1e-9);
    assert_eq!(tiers_front.len(), 6);
    assert!((spd_front - 1.0).abs() < 1e-9);

    let (st_cmd, lp_cmd, tiers_cmd, spd_cmd) = get_system_frontline_rules(&ctx, sys_a);
    assert_eq!(st_cmd, STATUS_COMMAND);
    assert!((lp_cmd - 0.75).abs() < 1e-9);
    assert_eq!(tiers_cmd, vec!["Novice", "Small", "Medium"]);
    assert!((spd_cmd - 0.75).abs() < 1e-9);
}

#[tokio::test]
async fn activate_fw_complex_gate_hull_classes_boundary_distance_and_errors() {
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let char_id = 911_001_i64;
    let ship_id = 88_100_001_i64;
    let destroyer_tid = find_ship_type_for_group(&ctx.sde, 420, true);
    let cruiser_tid = find_ship_type_for_group(&ctx.sde, 26, false);
    let bc_tid = find_ship_type_for_group(&ctx.sde, 419, false);
    let bs_tid = find_ship_type_for_group(&ctx.sde, 27, false);
    let indy_tid = find_ship_type_for_group(&ctx.sde, 28, false);

    seed_pilot(&ctx, sys_id, char_id, Some(500_004), ship_id, destroyer_tid);

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let respawn_hull = |tid: u32, pos: DVec3| {
        let h = sys_h.clone();
        async move {
            let _ = h.remove_ball(ship_id).await;
            h.spawn_ball(Ball {
                id: ship_id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await
            .unwrap();
        }
    };

    // 1. ShipNotInSpace first, then ComplexNotFound
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, 999_999_999)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    respawn_hull(destroyer_tid, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let err = activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, 999_999_999)
        .await
        .unwrap_err();
    assert_eq!(err.code, "ComplexNotFound");

    let g_small = 78_001_i64;
    let g_med = 78_002_i64;
    let g_large = 78_003_i64;
    let g_open = 78_004_i64;
    let g_done = 78_005_i64;
    let g_no_pos = 78_006_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.fw_complexes.insert(g_small, (sys_id, "Small".into(), 500_004, 0.0, false));
        snap.fw_runtime.fw_complexes.insert(g_med, (sys_id, "Medium".into(), 500_004, 0.0, false));
        snap.fw_runtime.fw_complexes.insert(g_large, (sys_id, "Large".into(), 500_004, 0.0, false));
        snap.fw_runtime.fw_complexes.insert(g_open, (sys_id, "Open".into(), 500_004, 0.0, false));
        snap.fw_runtime.fw_complexes.insert(g_done, (sys_id, "Small".into(), 500_004, 100.0, true));
        snap.fw_runtime.fw_complexes.insert(g_no_pos, (sys_id, "Small".into(), 500_004, 0.0, false));
        for gid in [g_small, g_med, g_large, g_open] {
            snap.item_positions.insert(gid, [10_000.0, 0.0, 0.0]);
        }
    });

    // 2. GateNotInSystem & ComplexAlreadyCompleted & GateNotInSpace
    ctx.systems
        .get_or_spawn(sys_id + 1)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: destroyer_tid,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .unwrap();
    for (s, g, exp) in [
        (sys_id + 1, g_small, "GateNotInSystem"),
        (sys_id, g_done, "ComplexAlreadyCompleted"),
        (sys_id, g_no_pos, "GateNotInSpace"),
    ] {
        assert_eq!(
            activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, s, g).await.unwrap_err().code,
            exp
        );
    }

    // 3. Surface distance boundary: center distance 2790.1 m (surface 2500.1 m) fails; 2790.0 m (surface 2500.0 m) passes!
    respawn_hull(destroyer_tid, DVec3::new(12_790.1, 0.0, 0.0)).await;
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_small).await.unwrap_err().code,
        "OutsideGateActivationRange"
    );

    sys_h.set_ball_position(ship_id, DVec3::new(12_790.0, 0.0, 0.0)).await;
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_small).await.is_ok());
    sys_h.set_ball_position(ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_small).await.is_ok());
    assert_eq!(ctx.db.with_snapshot(|s| s.fw_runtime.complex_occupants[&g_small].len()), 1);

    // 4. Medium gate: Cruiser allowed, Battlecruiser rejected (but allowed in Large gate!)
    respawn_hull(bc_tid, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_med).await.unwrap_err().code,
        "ShipTooLargeForGate"
    );
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_large).await.is_ok());

    respawn_hull(cruiser_tid, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_med).await.is_ok());

    // 5. Large vs Open gate: Battleship allowed at Large; Industrial (group 28) rejected at Large, allowed at Open
    respawn_hull(bs_tid, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_large).await.is_ok());

    respawn_hull(indy_tid, DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_large).await.unwrap_err().code,
        "ShipTooLargeForGate"
    );
    assert!(activate_fw_complex_gate_in_space(&ctx, char_id, ship_id, sys_id, g_open).await.is_ok());
}

#[tokio::test]
async fn supply_site_variants_fob_detection_and_ihub_surrender_window_damage() {
    let mut ctx = test_ctx();
    let fob_group_tid = 919_444_u32;
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut t: InvType = sde.types.values().next().unwrap().clone();
        t.type_id = fob_group_tid;
        t.group_id = 4644;
        sde.types.insert(fob_group_tid, t);
    }

    let sys_id = 30002505_u32;
    let unenlisted_id = 912_000_i64;
    let gal_pilot = 912_001_i64;
    let cal_pilot = 912_002_i64;
    seed_pilot(&ctx, sys_id, unenlisted_id, None, 88_200_000, 587);
    seed_pilot(&ctx, sys_id, gal_pilot, Some(500_004), 88_200_001, 587);
    seed_pilot(&ctx, sys_id, cal_pilot, Some(500_001), 88_200_002, 587);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, 1, PIRATE_STRONGHOLD_ANGEL_TYPE));
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, 1, PIRATE_STRONGHOLD_GURISTAS_TYPE));
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, 1, fob_group_tid));
    });

    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, cal_pilot, 999_888_777, 100.0)
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // Test SupplyCache, SupplyDepot, Propaganda across factions (500_003, 500_002, 500_001)
    for (idx, (kind, fac, expected_insignia_tid, expected_qty)) in [
        ("SupplyCache", 500_003_i64, 15_607_u32, 2_i32),
        ("SupplyDepot", 500_002_i64, 15_625_u32, 5_i32),
        ("Propaganda", 500_001_i64, 15_596_u32, 2_i32),
    ]
    .into_iter()
    .enumerate()
    {
        let sid = 79_100_i64 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.fw_runtime.supply_sites.insert(sid, (sys_id, kind.into(), fac));
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    solar_system_id: sys_id,
                    type_id: TYPE_FW_SUPPLY_SITE,
                    owner_corp_id: 1_000_180,
                    state: "ShieldVulnerable".into(),
                    shield_hp: 100.0,
                    armor_hp: 100.0,
                    hull_hp: 100.0,
                    position: [1_000.0, 0.0, 0.0],
                    ..Default::default()
                },
            );
        });

        if idx == 0 {
            assert_eq!(
                apply_fw_or_insurgency_structure_damage(&ctx, unenlisted_id, sid, 50.0)
                    .await
                    .unwrap_err()
                    .code,
                "NotEnlistedInMilitia"
            );
        }
        let attacker = if fac == 500_001 { gal_pilot } else { cal_pilot };
        if fac == 500_001 {
            assert_eq!(
                apply_fw_or_insurgency_structure_damage(&ctx, cal_pilot, sid, 50.0)
                    .await
                    .unwrap_err()
                    .code,
                "CannotAttackFriendlyStructure"
            );
        }
        // Spill damage across shield (100) + armor (50) -> armor_hp = 50
        let mid = apply_fw_or_insurgency_structure_damage(&ctx, attacker, sid, 150.0)
            .await
            .unwrap();
        assert_eq!(mid.dict_get("shieldHP").and_then(|v| v.as_f64()), Some(0.0));
        assert_eq!(mid.dict_get("armorHP").and_then(|v| v.as_f64()), Some(50.0));

        // Finish destroying structure
        let fin = apply_fw_or_insurgency_structure_damage(&ctx, attacker, sid, 500.0)
            .await
            .unwrap();
        let can_id = fin.dict_get("lootContainerID").and_then(|v| v.as_i64()).unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let insig = snap
                .items
                .values()
                .find(|i| i.location_id == can_id && i.type_id == expected_insignia_tid)
                .expect("insignia drop");
            assert_eq!(insig.quantity, expected_qty);
            snap.fw_runtime.supply_sites.insert(sid, (sys_id, kind.into(), fac));
        });
        assert_eq!(
            apply_fw_or_insurgency_structure_damage(&ctx, attacker, sid, 10.0)
                .await
                .unwrap_err()
                .code,
            "StructureDestroyed"
        );
    }

    // I-Hub: surrender_ihub_vulnerable_unix window auto-maxes VP, friendly militia blocked, partial damage works
    let ihub_id = 79_200_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, false);
        snap.insurgency_runtime.surrender_ihub_vulnerable_unix.insert(sys_id, now_u);
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                solar_system_id: sys_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_181,
                state: "ShieldVulnerable".into(),
                shield_hp: 500.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, gal_pilot, ihub_id, 100.0)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInOpposingMilitia"
    );
    let ihub_partial = apply_fw_or_insurgency_structure_damage(&ctx, cal_pilot, ihub_id, 700.0)
        .await
        .unwrap();
    assert_eq!(ihub_partial.dict_get("state").and_then(|v| v.as_str()), Some("ShieldVulnerable"));
    assert_eq!(ihub_partial.dict_get("armorHP").and_then(|v| v.as_f64()), Some(300.0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_system_occupations[&sys_id].victory_points, 3_000);
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&sys_id).copied(), Some(false));
    });
}

#[tokio::test]
async fn tick_fw_gameplay_contested_pause_guards_supply_defense_battlefield_and_ihub_regen() {
    let ctx = test_ctx();
    let sys_id = 30002505_u32;
    let gal_pilot = 913_001_i64;
    let cal_pilot = 913_002_i64;
    let gal_ship = 88_300_001_i64;
    let cal_ship = 88_300_002_i64;
    seed_pilot(&ctx, sys_id, gal_pilot, Some(500_004), gal_ship, 587);
    seed_pilot(&ctx, sys_id, cal_pilot, Some(500_001), cal_ship, 587);

    let g_contested = 80_001_i64;
    let g_guarded = 80_002_i64;
    let g_def_uncontested = 80_003_i64;
    let supply_def_id = 80_004_i64;
    let ihub_regen_id = 80_005_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            sys_id,
            FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_004,
                occupier_faction_id: 500_004,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );
        // 1. Contested complex (both Gal & Cal in range) -> progress stays at 25.0
        snap.fw_runtime.fw_complexes.insert(g_contested, (sys_id, "Novice".into(), 500_004, 25.0, false));
        snap.fw_runtime.complex_guards_cleared.insert(g_contested, true);
        snap.fw_runtime.complex_occupants.insert(g_contested, vec![(gal_pilot, gal_ship), (cal_pilot, cal_ship)]);

        // 2. Un-cleared guard blocks offensive capture (Cal pilot alone, guards_cleared = false)
        snap.fw_runtime.fw_complexes.insert(g_guarded, (sys_id, "Novice".into(), 500_004, 10.0, false));
        snap.fw_runtime.complex_guards_cleared.insert(g_guarded, false);
        snap.fw_runtime.complex_occupants.insert(g_guarded, vec![(cal_pilot, cal_ship)]);

        // 3. Defensive capture in uncontested system (victory_points == 0) gives 0 LP
        snap.fw_runtime.fw_complexes.insert(g_def_uncontested, (sys_id, "Novice".into(), 500_004, 99.9, false));
        snap.fw_runtime.complex_guards_cleared.insert(g_def_uncontested, true);
        snap.fw_runtime.complex_occupants.insert(g_def_uncontested, vec![(gal_pilot, gal_ship)]);

        // 4. Supply site attacked 35s ago with friendly pilot nearby and 0 enemies -> defended!
        snap.fw_runtime.supply_sites.insert(supply_def_id, (sys_id, "SupplyDepot".into(), 500_004));
        snap.structures.insert(
            supply_def_id,
            UpwellStructureRecord {
                structure_id: supply_def_id,
                solar_system_id: sys_id,
                type_id: TYPE_FW_SUPPLY_SITE,
                owner_corp_id: 1_000_181,
                state: "ShieldVulnerable".into(),
                shield_hp: 400.0,
                armor_hp: 500.0,
                hull_hp: 500.0,
                position: [250_000.0 + ((g_def_uncontested.unsigned_abs() % 11) as f64) * 4_000.0, 0.0, 0.0],
                last_damage_unix: now.saturating_sub(35),
                ..Default::default()
            },
        );

        // 5. Vulnerable I-Hub unattacked for > 2 hours (7200s) regenerates shield & reduces VP to 50%
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, true);
        snap.fw_runtime
            .last_ihub_attacked_unix
            .insert(sys_id, now.saturating_sub(7_300));
        snap.structures.insert(
            ihub_regen_id,
            UpwellStructureRecord {
                structure_id: ihub_regen_id,
                solar_system_id: sys_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_181,
                state: "ShieldVulnerable".into(),
                shield_hp: 200.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                last_damage_unix: now.saturating_sub(7_300),
                ..Default::default()
            },
        );

        // 6. Battlefield at 99.95% capture with 1 pilot in Zone 0
        snap.fw_runtime.battlefield_instances.push(
            eve_db::durable_domain_types::PersistedBattlefieldRecord {
                instance_id: 80_099,
                solar_system_id: sys_id,
                faction_id: 500_001,
                name: "Test Battlefield".into(),
                capture_progress: 99.95,
                completed: false,
            },
        );
        snap.fw_runtime.complex_last_step_unix.insert(80_099, now.saturating_sub(10));
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let centers = battlefield_zone_centers_for_system(&ctx, sys_id);
    // Place gal_ship at g_def_uncontested pocket (also right on supply_def_id)
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: gal_ship,
                type_id: 587,
                position: pocket_pos_for_complex(g_def_uncontested),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(gal_pilot),
                owner_id: gal_pilot,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Place cal_ship at Battlefield zone 0
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: cal_ship,
                type_id: 587,
                position: DVec3::from_array(centers[0]),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            CombatState::default(),
            SlimItemMeta {
                char_id: Some(cal_pilot),
                owner_id: cal_pilot,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    tick_fw_gameplay(&ctx, now).await;

    ctx.db.with_snapshot(|snap| {
        // g_guarded stayed at 10.0 because guard NPCs were not cleared
        assert!((snap.fw_runtime.fw_complexes[&g_guarded].3 - 10.0).abs() < 1e-6);
        // g_def_uncontested completed, but gal_pilot got 0 LP from defensive capture at 0 VP
        assert!(snap.fw_runtime.fw_complexes[&g_def_uncontested].4);
        // Supply site was defended and despawned, shifting occupier advantage by +5.0%
        assert!(!snap.fw_runtime.supply_sites.contains_key(&supply_def_id));
        assert!(snap.fw_runtime.system_advantage.get(&sys_id).map(|a| a.0).unwrap_or(0.0) >= 5.0);
        // Battlefield completed and awarded LP + standing to cal_pilot, setting 4h respawn cooldown
        assert!(snap.fw_runtime.battlefield_instances[0].completed);
        assert_eq!(
            snap.fw_runtime.battlefield_respawn_unix.get(&sys_id).copied(),
            Some(now + BATTLEFIELD_RESPAWN_COOLDOWN_SECS)
        );
        // I-Hub regenerated after 2h unattacked
        assert_eq!(snap.fw_runtime.ihub_vulnerable.get(&sys_id).copied(), Some(false));
    });
}
