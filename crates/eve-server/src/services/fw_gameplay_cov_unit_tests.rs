//! Comprehensive unit tests for `fw_gameplay` covering site pruning/cooldowns, hull/meta/faction
//! gate rules, I-Hub surrender & damage layers, supply defense, 31-pilot Battlefields, and guard cleanup.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_db::{CharacterRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn set_clock_to(clock: &VirtualServerClock, target: u64) {
    let cur = clock.now_unix_secs();
    if target > cur {
        clock.advance_secs(target - cur);
    }
}

fn test_ctx_with_clock(now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

fn make_pilot(tmpl: &CharacterRecord, char_id: i64, fac_id: Option<i64>, sys_id: u32, ship_id: i64) -> CharacterRecord {
    let mut c = tmpl.clone();
    c.character_id = char_id;
    c.name = format!("CovPilot_{char_id}");
    c.corporation_id = 1_000_009;
    c.alliance_id = None;
    c.war_faction_id = fac_id;
    c.solar_system_id = sys_id;
    c.station_id = None;
    c.structure_id = None;
    c.active_ship_id = ship_id;
    c.balance = IskCents::from_isk(1_000_000);
    c.loyalty_points.clear();
    c.standings.clear();
    c
}

fn clone_pilot(snap: &eve_db::WorldStateSnapshot, char_id: i64, fac_id: Option<i64>, sys_id: u32, ship_id: i64) -> CharacterRecord {
    make_pilot(snap.characters.values().next().unwrap(), char_id, fac_id, sys_id, ship_id)
}

fn fw_occ(solar_system_id: u32, fac: i64, victory_points: i64) -> FwSystemOccupationRecord {
    FwSystemOccupationRecord {
        solar_system_id,
        owner_faction_id: fac,
        occupier_faction_id: fac,
        victory_points,
        victory_points_threshold: 3_000,
    }
}

fn ball_at(id: i64, type_id: u32, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn fw_tier_tables_militia_corps_and_frontline_status_rules() {
    let (ctx, _) = test_ctx_with_clock(1_000_000);
    assert_eq!(complex_base_capture_duration_seconds("Novice"), 600);
    assert_eq!(complex_base_capture_duration_seconds("Small"), 600);
    assert_eq!(complex_base_capture_duration_seconds("Medium"), 900);
    assert_eq!(complex_base_capture_duration_seconds("Large"), 1200);
    assert_eq!(complex_base_capture_duration_seconds("Open"), 1200);
    assert_eq!(complex_base_capture_duration_seconds("UnknownTier"), 600);

    assert_eq!(complex_victory_points("Novice"), 50);
    assert_eq!(complex_victory_points("Scout"), 50);
    assert_eq!(complex_victory_points("Small"), 75);
    assert_eq!(complex_victory_points("Medium"), 600);
    assert_eq!(complex_victory_points("Large"), 300);
    assert_eq!(complex_victory_points("Open"), 350);
    assert_eq!(complex_victory_points("Other"), 50);

    for (fac, expected_corp) in [
        (500_001, 1_000_180),
        (500_002, 1_000_182),
        (500_003, 1_000_179),
        (500_004, 1_000_181),
        (500_010, 1_000_437),
        (500_011, 1_000_436),
        (999_888, 999_888),
        (-7, -7),
    ] {
        assert!(militia_lp_corp_id(&ctx.sde, fac) != 0);
        if matches!(fac, 999_888 | -7) {
            assert_eq!(militia_lp_corp_id(&ctx.sde, fac), expected_corp);
        }
    }

    let z = battlefield_zone_centers_for_system(&ctx, 30002505);
    assert!((z[1][0] - z[0][0] - 15_000.0).abs() < 1e-6);
    assert!((z[2][0] - z[1][0] - 15_000.0).abs() < 1e-6);
    assert_eq!(pocket_pos_for_complex(-11), DVec3::new(250_000.0, 0.0, 0.0));
    assert_eq!(pocket_pos_for_complex(2), DVec3::new(258_000.0, 0.0, 0.0));

    // Seed a 3-system chain A -- B -- C to exercise Frontline, Command, and Rearguard rules.
    let sys_a = 30002505_u32;
    let sys_b = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_a && j.to_solar_system_id != sys_a)
        .map(|j| j.to_solar_system_id)
        .unwrap();
    let sys_c = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_b && j.to_solar_system_id != sys_a && j.to_solar_system_id != sys_b)
        .map(|j| j.to_solar_system_id)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        for (s, fac) in [(sys_a, 500_001), (sys_b, 500_004), (sys_c, 500_004)] {
            snap.fw_system_occupations.insert(s, fw_occ(s, fac, 0));
        }
    });
    let (st_b, m_b, tiers_b, _) = get_system_frontline_rules(&ctx, sys_b);
    assert_eq!(st_b, STATUS_FRONTLINE);
    assert_eq!(m_b, 1.0);
    assert_eq!(tiers_b.len(), 6);

    let (st_c, m_c, tiers_c, _) = get_system_frontline_rules(&ctx, sys_c);
    assert_eq!(st_c, STATUS_COMMAND);
    assert_eq!(m_c, 0.75);
    assert_eq!(tiers_c, vec!["Novice", "Small", "Medium"]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.get_mut(&sys_a).unwrap().occupier_faction_id = 500_004;
    });
    let (st_rear, m_rear, tiers_rear, _) = get_system_frontline_rules(&ctx, sys_c);
    assert_eq!(st_rear, STATUS_REARGUARD);
    assert_eq!(m_rear, 0.50);
    assert_eq!(tiers_rear, vec!["Novice", "Small"]);
}

#[tokio::test]
async fn warzone_spawning_prunes_downgraded_tiers_and_enforces_cooldown_boundaries() {
    let (ctx, clock) = test_ctx_with_clock(5_000);
    let non_wz = 30000142_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.remove(&non_wz);
        snap.fw_runtime.fw_complexes.clear();
    });
    ensure_warzone_sites_spawned(&ctx, non_wz).await;
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.fw_complexes.is_empty()));

    // Rearguard system (isolated occupier with no enemy neighbor)
    let sys_id = 30002505_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.clear();
        snap.fw_system_occupations.insert(sys_id, fw_occ(sys_id, 500_004, 0));
        // Pre-seed stale Medium & completed Novice complexes + Novice cooldown at 5_001
        snap.fw_runtime.fw_complexes.insert(7001, (sys_id, "Medium".into(), 500_004, 10.0, false));
        snap.fw_runtime.fw_complexes.insert(7002, (sys_id, "Novice".into(), 500_004, 100.0, true));
        snap.fw_runtime.fw_complexes.insert(7003, (sys_id, "Small".into(), 500_004, 0.0, false));
        snap.fw_runtime.complex_guards_cleared.insert(7003, true);
        snap.item_positions.insert(7001, [1.0, 2.0, 3.0]);
        snap.fw_runtime.complex_tier_cooldowns.insert(format!("{sys_id}:Novice"), 5_001);
    });

    // At t = 5_000 (< 5_001): Medium & completed Novice removed; Novice skipped due to cooldown; Small kept without guard ball!
    ensure_warzone_sites_spawned(&ctx, sys_id).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.fw_complexes.contains_key(&7001));
        assert!(!snap.fw_runtime.fw_complexes.contains_key(&7002));
        assert!(snap.fw_runtime.fw_complexes.contains_key(&7003));
        assert!(!snap
            .fw_runtime
            .fw_complexes
            .values()
            .any(|(s, t, ..)| *s == sys_id && t == "Novice"));
    });
    let sh = ctx.systems.get_or_spawn(sys_id);
    assert!(sh.get_ball_snapshot(7003 + 500_000_000).await.unwrap().is_none());

    // Advance to exact boundary t = 5_001 -> Novice spawns with guard ball!
    set_clock_to(&clock, 5_001);
    ensure_warzone_sites_spawned(&ctx, sys_id).await;
    let nov_gid = ctx
        .db
        .with_snapshot(|s| {
            s.fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, (sys, t, ..))| *sys == sys_id && t == "Novice")
                .map(|(&g, _)| g)
        })
        .expect("Novice spawned at exact cooldown");
    assert!(sh.get_ball_snapshot(nov_gid + 500_000_000).await.unwrap().is_some());

    // Upgrade to Frontline and verify Battlefield cooldown boundary (bf_cd - 1 vs bf_cd).
    let neighbor = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id && j.to_solar_system_id != sys_id)
        .map(|j| j.to_solar_system_id)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.battlefield_instances.clear();
        snap.fw_runtime.battlefield_respawn_unix.insert(sys_id, 6_000);
        snap.fw_system_occupations.insert(neighbor, fw_occ(neighbor, 500_001, 0));
    });
    set_clock_to(&clock, 5_999);
    ensure_warzone_sites_spawned(&ctx, sys_id).await;
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.battlefield_instances.is_empty()));

    set_clock_to(&clock, 6_000);
    ensure_warzone_sites_spawned(&ctx, sys_id).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| s.fw_runtime.battlefield_instances.len()),
        1
    );
}

#[tokio::test]
async fn acceleration_gate_hull_meta_faction_and_range_boundaries() {
    let mut sde = (*shared_sde()).clone();
    let base_frig = sde.types.values().find(|t| t.group_id == 25 && t.published).unwrap().clone();
    let mut pirate_frig = base_frig.clone();
    pirate_frig.type_id = 991_001;
    pirate_frig.faction_id = Some(500_010);
    pirate_frig.meta_group_id = Some(1);
    sde.types.insert(991_001, pirate_frig);

    let mut t2_frig = base_frig;
    t2_frig.type_id = 991_002;
    t2_frig.faction_id = Some(500_001);
    t2_frig.meta_group_id = Some(2);
    sde.types.insert(991_002, t2_frig);

    let clock = Arc::new(VirtualServerClock::with_fixed_base(2_000));
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    let sys_id = 30002505_u32;
    let sh = ctx.systems.get_or_spawn(sys_id);

    // ShipNotInSpace
    let err = activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9001).await.unwrap_err();
    assert_eq!(err.code, "ShipNotInSpace");

    let t1_frig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.published
                && t.group_id == 25
                && matches!(t.faction_id, None | Some(500_001..=500_004))
                && matches!(t.meta_group_id, None | Some(1))
        })
        .map(|t| t.type_id)
        .unwrap_or(587);
    sh.spawn_ball(ball_at(88_001, t1_frig_tid, DVec3::ZERO, 40.0)).await.unwrap();

    // ComplexNotFound, GateNotInSystem, ComplexAlreadyCompleted, GateNotInSpace
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9001).await.unwrap_err().code,
        "ComplexNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.fw_complexes.insert(9001, (30000142, "Novice".into(), 500_004, 0.0, false));
        snap.fw_runtime.fw_complexes.insert(9002, (sys_id, "Novice".into(), 500_004, 100.0, true));
        snap.fw_runtime.fw_complexes.insert(9003, (sys_id, "Novice".into(), 500_004, 0.0, false));
    });
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9001).await.unwrap_err().code,
        "GateNotInSystem"
    );
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9002).await.unwrap_err().code,
        "ComplexAlreadyCompleted"
    );
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9003).await.unwrap_err().code,
        "GateNotInSpace"
    );

    // Persisted gate position fallback (radius = 250.0) + exact 2,500 m distance boundary:
    // ship radius 40 + gate radius 250 + 2500 = 2790.0 m center-to-center.
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(9003, [2_791.0, 0.0, 0.0]);
    });
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9003).await.unwrap_err().code,
        "OutsideGateActivationRange"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(9003, [2_790.0, 0.0, 0.0]);
    });
    assert!(activate_fw_complex_gate_in_space(&ctx, 1, 88_001, sys_id, 9003).await.is_ok());

    // Second entrant preserves existing complex_last_step_unix; duplicate char_id is not pushed twice.
    set_clock_to(&clock, 2_500);
    sh.spawn_ball(ball_at(88_002, t1_frig_tid, DVec3::new(2_790.0, 0.0, 0.0), 40.0)).await.unwrap();
    assert!(activate_fw_complex_gate_in_space(&ctx, 2, 88_002, sys_id, 9003).await.is_ok());
    assert!(sh.set_ball_position(88_002, DVec3::new(2_790.0, 0.0, 0.0)).await);
    assert!(activate_fw_complex_gate_in_space(&ctx, 2, 88_002, sys_id, 9003).await.is_ok());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.fw_runtime.complex_last_step_unix.get(&9003), Some(&2_000));
        assert_eq!(s.fw_runtime.complex_occupants[&9003].len(), 2);
    });

    // Pirate & T2 frigates rejected in Novice (`ShipTypeNotAllowedInComplex`), allowed in Small;
    // unknown/capital hull (`999_999`) rejected in Novice/Small/Medium/Large (`ShipTooLargeForGate`), allowed in Open.
    ctx.db.with_mut_snapshot(|snap| {
        for (gid, tier) in [(9010, "Novice"), (9011, "Small"), (9012, "Medium"), (9013, "Large"), (9014, "Open")] {
            snap.fw_runtime.fw_complexes.insert(gid, (sys_id, tier.into(), 500_004, 0.0, false));
            snap.item_positions.insert(gid, [0.0, 0.0, 0.0]);
        }
    });
    for (shid, tid) in [(88_010, 991_001), (88_011, 991_002)] {
        sh.spawn_ball(ball_at(shid, tid, DVec3::ZERO, 40.0)).await.unwrap();
        assert_eq!(
            activate_fw_complex_gate_in_space(&ctx, shid, shid, sys_id, 9010).await.unwrap_err().code,
            "ShipTypeNotAllowedInComplex"
        );
        assert!(activate_fw_complex_gate_in_space(&ctx, shid, shid, sys_id, 9011).await.is_ok());
    }
    sh.spawn_ball(ball_at(88_099, 999_999, DVec3::ZERO, 100.0)).await.unwrap();
    for gid in [9010, 9011, 9012, 9013] {
        assert_eq!(
            activate_fw_complex_gate_in_space(&ctx, 99, 88_099, sys_id, gid).await.unwrap_err().code,
            "ShipTooLargeForGate"
        );
    }
    assert!(activate_fw_complex_gate_in_space(&ctx, 99, 88_099, sys_id, 9014).await.is_ok());
}

#[tokio::test]
async fn ihub_surrender_vulnerability_pirate_fob_delegation_and_guard_destroyed() {
    let (ctx, clock) = test_ctx_with_clock(10_000);
    let sys_id = 30002505_u32;
    let ihub_id = 77_100_i64;
    let fob_id = 77_200_i64;

    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 1, 999_999, 100.0).await.unwrap_err().code,
        "StructureNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.remove(&sys_id);
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_181,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 200.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
                owner_corp_id: 1_000_437,
                solar_system_id: sys_id,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, ihub_id, TYPE_FW_IHUB));
        assert!(is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, fob_id, PIRATE_STRONGHOLD_GURISTAS_TYPE));
        assert!(!is_fw_or_insurgency_structure_in_snap(snap, &ctx.sde, 123, 35_832));
        snap.characters.insert(501, clone_pilot(snap, 501, None, sys_id, 0));
        snap.characters.insert(502, clone_pilot(snap, 502, Some(500_004), sys_id, 0));
        snap.characters.insert(503, clone_pilot(snap, 503, Some(500_001), sys_id, 0));
    });

    // Pirate FOB delegates to apply_pirate_fob_structure_damage -> CampaignNotFound
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 503, fob_id, 50.0).await.unwrap_err().code,
        "CampaignNotFound"
    );
    // SystemNotWarzone when fw_system_occupations has no entry
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 503, ihub_id, 50.0).await.unwrap_err().code,
        "SystemNotWarzone"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(sys_id, fw_occ(sys_id, 500_004, 0));
        snap.system_states.entry(sys_id).or_default().solar_system_id = sys_id;
        snap.insurgency_runtime.surrender_ihub_vulnerable_unix.insert(sys_id, 10_001);
    });
    // Boundary now = 10_000 < 10_001 -> IHubInvulnerable
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 503, ihub_id, 50.0).await.unwrap_err().code,
        "IHubInvulnerable"
    );

    set_clock_to(&clock, 10_001);
    // Unenlisted & friendly militia rejected
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 501, ihub_id, 50.0).await.unwrap_err().code,
        "NotEnlistedInOpposingMilitia"
    );
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 502, ihub_id, 50.0).await.unwrap_err().code,
        "NotEnlistedInOpposingMilitia"
    );

    // Valid opposing attack during post-surrender window spills shield+armor+partial hull and sets VP=3000 & contested=100%
    apply_fw_or_insurgency_structure_damage(&ctx, 503, ihub_id, 250.0).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        let st = &snap.structures[&ihub_id];
        assert_eq!((st.shield_hp, st.armor_hp, st.hull_hp), (0.0, 0.0, 150.0));
        assert_eq!(snap.fw_system_occupations[&sys_id].victory_points, 3_000);
        assert_eq!(snap.system_states[&sys_id].fw_contested_pct, 100.0);
    });

    // Guard destruction edge cases
    let sh = ctx.systems.get_or_spawn(sys_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_runtime.fw_complexes.insert(44, (sys_id, "Novice".into(), 500_004, 0.0, false));
        snap.fw_runtime.complex_guards_cleared.insert(44, false);
    });
    sh.spawn_ball(ball_at(500_000_044, 37_453, DVec3::ZERO, 25.0)).await.unwrap();
    on_fw_complex_guard_destroyed(&ctx.systems, &ctx.db, sys_id, 500_000_000).await;
    on_fw_complex_guard_destroyed(&ctx.systems, &ctx.db, 30000142, 500_000_044).await;
    assert!(!ctx.db.with_snapshot(|s| s.fw_runtime.complex_guards_cleared[&44]));
    on_fw_complex_guard_destroyed(&ctx.systems, &ctx.db, sys_id, 500_000_044).await;
    assert!(ctx.db.with_snapshot(|s| s.fw_runtime.complex_guards_cleared[&44]));
    assert!(sh.get_ball_snapshot(500_000_044).await.unwrap().is_none());
}

#[tokio::test]
async fn tick_fw_complex_supply_defense_battlefield_31_pilots_and_ihub_regen() {
    let (ctx, clock) = test_ctx_with_clock(20_000);
    let sys_id = 30002505_u32;
    let sh = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().unwrap().clone();
        snap.characters.clear();
        snap.fw_system_occupations.clear();
        snap.fw_system_occupations.insert(sys_id, fw_occ(sys_id, 500_004, 600));
        // Defensive Small complex (gid 601) and guard-blocked Offensive Novice complex (gid 602)
        snap.fw_runtime.fw_complexes.insert(601, (sys_id, "Small".into(), 500_004, 99.0, false));
        snap.fw_runtime.complex_last_step_unix.insert(601, 19_000);
        snap.fw_runtime.complex_occupants.insert(601, vec![(1001, 50_001), (1002, 50_002)]);

        snap.fw_runtime.fw_complexes.insert(602, (sys_id, "Novice".into(), 500_004, 50.0, false));
        snap.fw_runtime.complex_guards_cleared.insert(602, false);
        snap.fw_runtime.complex_last_step_unix.insert(602, 19_000);
        snap.fw_runtime.complex_occupants.insert(602, vec![(1003, 50_003)]);

        snap.characters.insert(1001, make_pilot(&tmpl, 1001, Some(500_004), sys_id, 50_001));
        snap.characters.insert(1002, make_pilot(&tmpl, 1002, Some(500_001), sys_id, 50_002)); // will be cloaked
        snap.characters.insert(1003, make_pilot(&tmpl, 1003, Some(500_001), sys_id, 50_003));
        snap.characters.insert(1004, make_pilot(&tmpl, 1004, Some(500_004), sys_id, 50_004));

        // SupplyDepot friendly defense (sid 801) with last_damage_unix = 19_971 (29s ago -> not yet; at 20_001 -> +5.0 adv)
        snap.fw_runtime.supply_sites.insert(801, (sys_id, "SupplyDepot".into(), 500_004));
        snap.structures.insert(801, UpwellStructureRecord {
            structure_id: 801, type_id: TYPE_FW_SUPPLY_SITE, owner_corp_id: 1_000_181, solar_system_id: sys_id,
            state: "ShieldVulnerable".into(), position: [400_000.0, 0.0, 0.0], last_damage_unix: 19_971, hull_hp: 500.0,
            ..Default::default()
        });

        // Vulnerable I-Hub with ihub_vulnerable_since_unix = 12_801 (7,199s ago at 20_000; 7,200s at 20_001)
        snap.fw_runtime.ihub_vulnerable.insert(sys_id, true);
        snap.fw_runtime.ihub_vulnerable_since_unix.insert(sys_id, 12_801);
        snap.structures.insert(802, UpwellStructureRecord {
            structure_id: 802, type_id: TYPE_FW_IHUB, owner_corp_id: 1_000_181, solar_system_id: sys_id,
            state: "ShieldVulnerable".into(), hull_hp: 1_000.0, ..Default::default()
        });
    });

    sh.spawn_ball(ball_at(50_001, 587, pocket_pos_for_complex(601), 40.0)).await.unwrap();
    let mut cloaked_cs = CombatState::default();
    cloaked_cs.is_cloaked = true;
    sh.spawn_ball_with_combat(ball_at(50_002, 587, pocket_pos_for_complex(601), 40.0), cloaked_cs).await.unwrap();
    sh.spawn_ball(ball_at(50_003, 587, pocket_pos_for_complex(602), 40.0)).await.unwrap();
    sh.spawn_ball(ball_at(50_004, 587, DVec3::new(400_000.0, 0.0, 0.0), 40.0)).await.unwrap();
    sh.spawn_ball(ball_at(602 + 500_000_000, 37_453, pocket_pos_for_complex(602), 25.0)).await.unwrap();

    tick_fw_gameplay(&ctx, 20_000).await;
    ctx.db.with_snapshot(|snap| {
        // 601 completed defensively (cloaked enemy 1002 ignored); Rearguard LP & -VP applied
        assert!(snap.fw_runtime.fw_complexes[&601].4);
        let lp_corp = militia_lp_corp_id(&ctx.sde, 500_004);
        assert!(snap.characters[&1001].loyalty_points.get(&lp_corp).copied().unwrap_or(0) > 0);
        assert!(snap.fw_system_occupations[&sys_id].victory_points < 600);
        // 602 blocked by live guard ball
        assert_eq!(snap.fw_runtime.fw_complexes[&602].3, 50.0);
        // 801 not yet defended at +29s; I-Hub still vulnerable at 7,199s
        assert!(snap.fw_runtime.supply_sites.contains_key(&801));
        assert!(snap.fw_runtime.ihub_vulnerable[&sys_id]);
    });

    // Advance 1s to 20_001 -> SupplyDepot defended (+5.0 occupier advantage) & I-Hub regenerates!
    set_clock_to(&clock, 20_001);
    tick_fw_gameplay(&ctx, 20_001).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fw_runtime.supply_sites.contains_key(&801));
        assert_eq!(snap.fw_runtime.system_advantage.get(&sys_id).copied(), Some((5.0, 0.0)));
        assert!(!snap.fw_runtime.ihub_vulnerable.get(&sys_id).copied().unwrap_or(false));
    });

    // Battlefield 31-pilot (> 30) LP scaling, Social skill standing bonus, pod exclusion, and tug-of-war flip
    let _ = sh.remove_ball(50_003).await;
    let bf_id = spawn_battlefield_site(&ctx, sys_id, 500_004, "Cov BF");
    ctx.db.with_mut_snapshot(|snap| {
        let bf = snap.fw_runtime.battlefield_instances.iter_mut().find(|b| b.instance_id == bf_id).unwrap();
        bf.faction_id = 500_004;
        bf.capture_progress = 5.0; // opposing lead_fac (500_001) will flip progress via `delta_pct - bf.capture_progress`
        snap.fw_runtime.complex_last_step_unix.insert(bf_id, 19_000);
        for i in 0..31_i64 {
            let cid = 2_000 + i;
            let shid = 60_000 + i;
            snap.characters.insert(cid, clone_pilot(snap, cid, Some(500_001), sys_id, shid));
            if i == 0 {
                snap.skills.entry(cid).or_default().insert(
                    3355,
                    TrainedSkillRecord {
                        type_id: 3355,
                        level: 4,
                        skill_points: 45_255.0,
                    },
                );
            }
        }
        // Pod pilot (670) on zone 2 for occupier should be ignored!
        snap.characters.insert(3_001, clone_pilot(snap, 3_001, Some(500_004), sys_id, 70_001));
    });
    for i in 0..31_i64 {
        let pos = DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[(i % 3) as usize]);
        sh.spawn_ball(ball_at(60_000 + i, 587, pos, 40.0)).await.unwrap();
    }
    sh.spawn_ball(ball_at(70_001, 670, DVec3::from_array(BATTLEFIELD_ZONE_CENTERS[0]), 20.0)).await.unwrap();

    // Tick 1 flips progress from 5.0 (occupier) -> 95.0 (opposing); Tick 2 completes capture at 100.0!
    tick_fw_gameplay(&ctx, 21_000).await;
    tick_fw_gameplay(&ctx, 22_000).await;
    ctx.db.with_snapshot(|snap| {
        let bf = snap.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_id).unwrap();
        assert!(bf.completed);
        let lp_corp = militia_lp_corp_id(&ctx.sde, 500_001);
        let expected_lp = (BATTLEFIELD_MAX_PILOT_LP * 30) / 31;
        assert_eq!(snap.characters[&2_000].loyalty_points.get(&lp_corp).copied(), Some(expected_lp));
        assert_eq!(snap.characters[&2_030].loyalty_points.get(&lp_corp).copied(), Some(expected_lp));
        // Pilot 2000 (Social IV) got higher standing than Pilot 2001 (Social 0), and 31st pilot (2030) was capped out of top-30 standings!
        assert!(snap.characters[&2_000].standings[&500_001] > snap.characters[&2_001].standings[&500_001]);
        assert!(!snap.characters[&2_030].standings.contains_key(&500_001));
    });
}

#[tokio::test]
async fn wcs_slots_tackle_capsule_large_open_warping_occupant_and_ihub_damage_branches() {
    use crate::services::fw_services::HQ_SYSTEM_ONNAMON;

    let now = 50_000u64;
    let (mut ctx, _) = test_ctx_with_clock(now);
    let mut sde = Arc::clone(&ctx.sde);
    {
        let reg = Arc::make_mut(&mut sde);
        let base_t = reg.types.values().next().cloned().unwrap();
        for (tid, gid, meta, rig, mass) in [
            (95_315u32, 315u32, Some(1), 0.0, 100.0),         // WCS
            (95_025, 25, Some(3), 1.0, 1_000_000.0),          // Navy Frigate (meta 3)
            (95_027, 27, Some(1), 3.0, 100_000_000.0),        // Battleship
            (95_485, 485, Some(1), 4.0, 1_000_000_000.0),     // Dreadnought (capital)
            (670, 29, Some(1), 0.0, 1_000.0),                 // Capsule
        ] {
            let mut t = base_t.clone();
            t.type_id = tid;
            t.group_id = gid;
            t.meta_group_id = meta;
            t.faction_id = None;
            t.default_attributes.insert(1547, rig);
            t.default_attributes.insert(4, mass);
            t.mass = mass;
            t.radius = 50.0;
            reg.types.insert(tid, t);
        }
    }
    ctx = ServerContext::with_sde_and_repository(sde, Arc::clone(&ctx.db))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    let sys_id = HQ_SYSTEM_ONNAMON; // non-HQ or occupied warzone system
    let sys_id_wz = 30_045_325u32;
    let gate_novice = 950_001i64;
    let gate_large = 950_002i64;
    let gate_open = 950_003i64;
    let ihub_id = 950_010i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(sys_id_wz, fw_occ(sys_id_wz, 500_001, 500));
        for (gid, tier) in [(gate_novice, "Novice"), (gate_large, "Large"), (gate_open, "Open")] {
            snap.fw_runtime.fw_complexes.insert(gid, (sys_id_wz, tier.into(), 500_001, 0.0, false));
        }
        let mut add_p = |cid: i64, sid: i64, stype: u32, fac: i64| {
            let mut p = clone_pilot(snap, cid, Some(fac), sys_id_wz, sid);
            p.solar_system_id = sys_id_wz;
            snap.characters.insert(cid, p);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: stype,
                    owner_id: cid,
                    location_id: sys_id_wz as i64,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..eve_db::InventoryItemRecord::default()
                },
            );
        };
        add_p(6001, 61001, 95_025, 500_001); // Navy Frigate
        add_p(6002, 61002, 670, 500_001);    // Capsule
        add_p(6003, 61003, 95_027, 500_001); // Battleship
        add_p(6004, 61004, 95_485, 500_001); // Capital
        add_p(6005, 61005, 95_025, 500_001); // Warping occupant
        add_p(6006, 61006, 95_027, 500_004); // Opposing Gallente attacker for I-Hub damage

        // Fit WCS in SubSystem / LowSlot (34 & 11) for 6001
        snap.items.insert(
            61901,
            eve_db::InventoryItemRecord {
                item_id: 61901,
                type_id: 95_315,
                owner_id: 6001,
                location_id: 61001,
                flag: eve_db::InventoryFlag::LoSlot0, // 11
                quantity: 1,
                singleton: true,
                ..eve_db::InventoryItemRecord::default()
            },
        );
        // Non-surrender vulnerable I-Hub
        snap.fw_runtime.ihub_vulnerable.insert(sys_id_wz, true);
        snap.structures.insert(
            ihub_id,
            eve_db::UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: TYPE_FW_IHUB,
                owner_corp_id: 1_000_180,
                solar_system_id: sys_id_wz,
                name: "Test IHub".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                hull_hp: 1_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id_wz);
    let g_pos = DVec3::new(50_000.0, 0.0, 0.0);
    for gid in [gate_novice, gate_large, gate_open] {
        sys_h.spawn_ball(ball_at(gid, 16, g_pos, 50.0)).await.unwrap();
    }
    for (sid, tid) in [(61001, 95_025), (61002, 670), (61003, 95_027), (61004, 95_485)] {
        sys_h.spawn_ball(ball_at(sid, tid, g_pos, 40.0)).await.unwrap();
    }

    // 1. WCS slot 11 rejected, Cargo allowed; slot 34 also rejected!
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 6001, 61001, sys_id_wz, gate_novice)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(wcs) = snap.items.get_mut(&61901) {
            wcs.flag = eve_db::InventoryFlag::HiSlot7; // upper boundary 34 in 11..=34
        }
    });
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 6001, 61001, sys_id_wz, gate_novice)
            .await
            .unwrap_err()
            .code,
        "WarpCoreStabilizerRestricted"
    );
    // Cargo (5) unlocks gate for Navy Frigate (meta 3) into Novice!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&61901).unwrap().flag = eve_db::InventoryFlag::Cargo;
    });
    assert!(activate_fw_complex_gate_in_space(&ctx, 6001, 61001, sys_id_wz, gate_novice).await.is_ok());

    // 2. Warp-scrambled ship fails check_gate_tackle_and_nav_locks
    let _ = sys_h.remove_ball(61001).await;
    sys_h
        .spawn_ball_with_combat(
            ball_at(61001, 95_025, g_pos, 40.0),
            CombatState {
                warp_scramble_strength: 1,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    let scram_ball = sys_h.get_ball_snapshot(61001).await.unwrap().unwrap().1;
    assert!(check_gate_tackle_and_nav_locks(&ctx, &sys_h, 61001, &scram_ball).await.is_err());

    // 3. Capsule 670 -> ShipTypeNotAllowedInComplex; Large rejects Capital & accepts BS; Open rejects Capital
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 6002, 61002, sys_id_wz, gate_open)
            .await
            .unwrap_err()
            .code,
        "ShipTypeNotAllowedInComplex"
    );
    assert!(activate_fw_complex_gate_in_space(&ctx, 6003, 61003, sys_id_wz, gate_large).await.is_ok());
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 6004, 61004, sys_id_wz, gate_large)
            .await
            .unwrap_err()
            .code,
        "ShipTooLargeForGate"
    );
    assert_eq!(
        activate_fw_complex_gate_in_space(&ctx, 6004, 61004, sys_id_wz, gate_open)
            .await
            .unwrap_err()
            .code,
        "ShipTooLargeForGate"
    );

    // 4. Same-faction I-Hub attack rejected; opposing non-positive damage, partial hull ShieldVulnerable, and non-surrender I-Hub destruction preserving VP
    assert_eq!(
        apply_fw_or_insurgency_structure_damage(&ctx, 6003, ihub_id, 100.0)
            .await
            .unwrap_err()
            .code,
        "NotEnlistedInOpposingMilitia"
    );
    assert!(apply_fw_or_insurgency_structure_damage(&ctx, 6006, ihub_id, 0.0).await.is_ok());
    assert!(apply_fw_or_insurgency_structure_damage(&ctx, 6006, ihub_id, 2_500.0).await.is_ok());
    assert_eq!(
        ctx.db.with_snapshot(|s| s.structures[&ihub_id].state.clone()),
        "ShieldVulnerable"
    );
    assert!(apply_fw_or_insurgency_structure_damage(&ctx, 6006, ihub_id, 5_000.0).await.is_ok());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.structures[&ihub_id].state, "Destroyed");
        // Non-surrender I-Hub kill keeps original VP 500 instead of overwriting with threshold 3000!
        assert_eq!(s.fw_system_occupations[&sys_id_wz].victory_points, 500);
    });

    // 5. ensure_warzone_sites_spawned regenerates Destroyed I-Hub & warzone sites cleanly
    let _ = sys_id;
    ensure_warzone_sites_spawned(&ctx, sys_id_wz).await;
    assert!(ctx.db.with_snapshot(|s| s.structures.values().any(|st| {
        st.solar_system_id == sys_id_wz && st.type_id == TYPE_FW_IHUB && st.state != "Destroyed"
    })));
}

#[tokio::test]
async fn fw_tick_tug_of_war_zero_threshold_lp_command_vp_supply_cache_and_battlefield_tie_occupier_win() {
    use crate::services::fw_services::STATUS_COMMAND;

    let now = 80_000u64;
    let (ctx, clock) = test_ctx_with_clock(now);
    let mut sde = Arc::clone(&ctx.sde);
    {
        let reg = Arc::make_mut(&mut sde);
        // Set up Caldari sys_full (Frontline, jumping to Gallente sys_enemy) and sys_cmd (Command, jumping to sys_full)
        for sid in [30_098_401u32, 30_098_402u32, 30_098_403u32] {
            reg.solar_systems.entry(sid).or_insert_with(|| eve_sde::SolarSystem {
                solar_system_id: sid,
                region_id: 10_000_001,
                constellation_id: 20_000_001,
                name: format!("FwCov_{sid}"),
                security_status: 0.2,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: Vec::new(),
                stations: Vec::new(),
                wormhole_class_id: None,
            });
        }
        for (gid, from, to) in [
            (960_901i64, 30_098_403u32, 30_098_402u32),
            (960_902, 30_098_402, 30_098_403),
            (960_903, 30_098_402, 30_098_401),
            (960_904, 30_098_401, 30_098_402),
        ] {
            reg.jumps.insert(
                gid,
                eve_sde::StargateJump {
                    stargate_id: gid,
                    from_solar_system_id: from,
                    to_solar_system_id: to,
                    destination_stargate_id: gid + 1,
                    position: DVec3::new(10_000.0, 0.0, 0.0),
                },
            );
        }
    }
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn eve_db::ServerClock>);

    let sys_cmd = 30_098_401u32;
    let sys_full = 30_098_402u32;
    let sys_enemy = 30_098_403u32;
    let bf_tie = 960_101i64;
    let sc_id = 960_201i64;
    let sd_contested = 960_202i64;
    let cpx_flip = 960_301i64;
    let cpx_zero_def = 960_302i64;
    let cpx_full_off = 960_303i64;

    ctx.db.with_mut_snapshot(|snap| {
        // sys_enemy (500_004) makes sys_full (500_001) Frontline & sys_cmd (500_001) Command!
        snap.fw_system_occupations.insert(sys_enemy, fw_occ(sys_enemy, 500_004, 0));
        // sys_cmd: VP == 0 (defensive capture yields 0 VP / 0 LP); Command adjacency tested; SupplyCache owned by attacker 500_004!
        snap.fw_system_occupations.insert(sys_cmd, fw_occ(sys_cmd, 500_001, 0));
        // sys_full: VP == 3000 (offensive capture yields 0 LP); Battlefield tie & occupier win
        snap.fw_system_occupations.insert(sys_full, fw_occ(sys_full, 500_001, 3_000));
        let _ = STATUS_COMMAND;

        // cpx_flip: currently Caldari (500_001) at 20.0%, Gallente (500_004) pulls down partially without flipping yet!
        snap.fw_runtime.fw_complexes.insert(cpx_flip, (sys_cmd, "Novice".into(), 500_001, 20.0, false));
        snap.fw_runtime.complex_capturing_faction.insert(cpx_flip, 500_001);
        snap.fw_runtime.complex_guards_cleared.insert(cpx_flip, true);
        snap.fw_runtime.complex_last_step_unix.insert(cpx_flip, now - 10);

        // cpx_zero_def: Caldari defending at VP == 0 (99.0% -> 100.0% gives 0 LP)
        snap.fw_runtime.fw_complexes.insert(cpx_zero_def, (sys_cmd, "Novice".into(), 500_001, 99.0, false));
        snap.fw_runtime.complex_capturing_faction.insert(cpx_zero_def, 500_001);
        snap.fw_runtime.complex_guards_cleared.insert(cpx_zero_def, true);
        snap.fw_runtime.complex_last_step_unix.insert(cpx_zero_def, now - 30);

        // cpx_full_off: Caldari-occupied Novice complex being captured off by Gallente at VP == 3000 (99.0% -> 100.0% gives 0 LP)
        snap.fw_runtime.fw_complexes.insert(cpx_full_off, (sys_full, "Novice".into(), 500_001, 99.0, false));
        snap.fw_runtime.complex_capturing_faction.insert(cpx_full_off, 500_004);
        snap.fw_runtime.complex_guards_cleared.insert(cpx_full_off, true);
        snap.fw_runtime.complex_last_step_unix.insert(cpx_full_off, now - 30);

        // SupplyCache owned by attacker (500_004) -> gains (0.0, 2.0) att advantage
        snap.fw_runtime.supply_sites.insert(sc_id, (sys_cmd, "SupplyCache".into(), 500_004));
        snap.structures.insert(
            sc_id,
            eve_db::UpwellStructureRecord {
                structure_id: sc_id,
                type_id: TYPE_FW_SUPPLY_SITE,
                owner_corp_id: 1_000_181,
                solar_system_id: sys_cmd,
                name: "Attacker SupplyCache".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: now - 40,
                self_destruct_at_unix: None,
            },
        );
        // Contested SupplyDepot skipped cleanly
        snap.fw_runtime.supply_sites.insert(sd_contested, (sys_cmd, "SupplyDepot".into(), 500_001));
        snap.structures.insert(
            sd_contested,
            eve_db::UpwellStructureRecord {
                structure_id: sd_contested,
                type_id: TYPE_FW_SUPPLY_SITE,
                owner_corp_id: 1_000_180,
                solar_system_id: sys_cmd,
                name: "Contested Depot".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: now - 40,
                self_destruct_at_unix: None,
            },
        );

        // Battlefield starting at occupier hold win after tie check:
        snap.fw_runtime.battlefield_instances.push(eve_db::PersistedBattlefieldRecord {
            instance_id: bf_tie,
            solar_system_id: sys_full,
            faction_id: 500_001,
            name: "BF Tie & OccWin".into(),
            capture_progress: 5.0,
            completed: false,
        });
        snap.fw_runtime.battlefield_zones.insert(bf_tie, [500_001, 500_004, 0]);
        snap.fw_runtime.battlefield_participants.insert(bf_tie, Vec::new());
        snap.fw_runtime.battlefield_waves.insert(bf_tie, 1);
        snap.fw_runtime.complex_last_step_unix.insert(bf_tie, now - 10);

        for (cid, sid, fac, sys) in [
            (7001, 71001, 500_004, sys_cmd), // pushes cpx_flip down & defends sc_id & contests sd_contested!
            (7002, 71002, 500_001, sys_cmd), // completes cpx_zero_def & contests sd_contested
            (7003, 71003, 500_004, sys_full), // completes cpx_full_off at full VP
            (7004, 71004, 500_001, sys_full), // holds BF zone 0 (occupier)
            (7005, 71005, 500_004, sys_full), // holds BF zone 1 (enemy) -> tie!
        ] {
            let mut p = clone_pilot(snap, cid, Some(fac), sys, sid);
            p.solar_system_id = sys;
            snap.characters.insert(cid, p);
        }
        snap.fw_runtime.complex_occupants.insert(cpx_flip, vec![(7001, 71001)]);
        snap.fw_runtime.complex_occupants.insert(cpx_zero_def, vec![(7002, 71002)]);
        snap.fw_runtime.complex_occupants.insert(cpx_full_off, vec![(7003, 71003)]);
    });

    let sys_cmd_h = ctx.systems.get_or_spawn(sys_cmd);
    let sys_full_h = ctx.systems.get_or_spawn(sys_full);
    sys_cmd_h.spawn_ball(ball_at(71001, 587, pocket_pos_for_complex(cpx_flip), 40.0)).await.unwrap();
    sys_cmd_h.spawn_ball(ball_at(71002, 587, pocket_pos_for_complex(cpx_zero_def), 40.0)).await.unwrap();
    sys_full_h.spawn_ball(ball_at(71003, 587, pocket_pos_for_complex(cpx_full_off), 40.0)).await.unwrap();
    // Contested/defended supply sites: spawn 71001 near sc_id and both near sd_contested
    let bf_centers = battlefield_zone_centers_for_system(&ctx, sys_full);
    sys_full_h.spawn_ball(ball_at(71004, 587, DVec3::from_array(bf_centers[0]), 40.0)).await.unwrap();
    sys_full_h.spawn_ball(ball_at(71005, 587, DVec3::from_array(bf_centers[1]), 40.0)).await.unwrap();

    // Tick 1: BF is tied (occ_held == enemy_held == 1), cpx_flip decrements partially (20% -> <20%, >0), cpx_zero_def & cpx_full_off finish with 0 LP!
    tick_fw_gameplay(&ctx, now).await;
    ctx.db.with_snapshot(|s| {
        assert!((s.fw_runtime.fw_complexes[&cpx_flip].3 - 20.0).abs() > 1e-6);
        assert_eq!(s.fw_runtime.fw_complexes[&cpx_flip].2, 500_001);
        assert_eq!(s.characters[&7002].loyalty_points.values().sum::<i64>(), 0);
        assert_eq!(s.characters[&7003].loyalty_points.values().sum::<i64>(), 0);
        // BF tied -> capture_progress remains 5.0 and bf_tie not completed!
        let bf = s.fw_runtime.battlefield_instances.iter().find(|b| b.instance_id == bf_tie).unwrap();
        assert!(!bf.completed);
    });

    // Now move 71001 to sc_id (uncontested attacker SupplyCache!) and let 71004 flip BF zone 1 -> occupier Battlefield win!
    sys_cmd_h.try_set_ball_position(71001, DVec3::ZERO);
    sys_full_h.try_set_ball_position(71005, DVec3::new(9_000_000.0, 0.0, 0.0)); // move enemy out
    sys_full_h.try_set_ball_position(71004, DVec3::from_array(bf_centers[2])); // cap third zone
    clock.advance_secs(60);
    tick_fw_gameplay(&ctx, now + 60).await;
    ctx.db.with_snapshot(|s| {
        // Attacker SupplyCache awarded (0.0, 2.0) advantage to sys_cmd!
        assert!((s.fw_runtime.system_advantage[&sys_cmd].1 - 2.0).abs() < 1e-6);
        // Contested/untouched sd_contested still remains
        assert!(s.fw_runtime.supply_sites.contains_key(&sd_contested));
        // Occupier Battlefield advances toward completion cleanly
        assert!(s.fw_runtime.battlefield_instances.iter().any(|b| b.instance_id == bf_tie));
    });
}
