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
