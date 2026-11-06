//! Exploit and security regression tests for Round 6 Track 1 P0 issues (#355-#362).
#![forbid(unsafe_code)]

use crate::router::{ActiveScannerProbe, BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::combat_fighters::{
    apply_damage_to_fighter_squadron, handle_fighters,
};
use crate::services::combat_crimewatch::combat_npcs_drones::handle_npcs_drones;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, DroneSpaceState, SafetyLevel};
use crate::services::inventory_stacking::handle_stack_ops;
use crate::services::pve_exploration::handle_pve_exploration;
use crate::services::pve_special_sites::{with_pve_special_reg, HomefrontSiteRecord};
use crate::services::r6_t1_p0_helpers::{
    enforce_legacy_pvp_safety_and_concord, execute_lancer_beam_attack,
};
use crate::services::space_probe_scanning::{
    expire_overdue_probes_for_char, handle_scan_and_wormhole_extras,
};
use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, ServerClock, UpwellStructureRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        for p in [
            SdeRegistry::default_db_path(),
            std::path::PathBuf::from("../../data/evomu_sde.sqlite"),
            std::path::PathBuf::from("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite"),
        ] {
            if let Ok(sde) = SdeRegistry::open(&p) {
                return Arc::new(sde);
            }
        }
        Arc::new(SdeRegistry::default())
    }))
}

fn test_ctx_at(base_unix: u64) -> (Arc<ServerContext>, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(shared_sde(), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);
    (ctx, clock)
}

fn make_session(char_id: i64, corp_id: i64, sys_id: u32, ship_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn make_item(id: i64, owner: i64, loc: i64, flag: InventoryFlag, type_id: u32, qty: i32) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        owner_id: owner,
        location_id: loc,
        flag,
        type_id,
        quantity: qty,
        singleton: false,
        ..InventoryItemRecord::default()
    }
}

fn make_ship_ball(id: i64, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_char(
    character_id: i64, corporation_id: i64, active_ship_id: i64, solar_system_id: u32,
) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        activity_status: None, character_id, user_id: character_id as u64,
        name: format!("Pilot {character_id}"), corporation_id, alliance_id: None, war_faction_id: None,
        station_id: None, structure_id: None, solar_system_id, constellation_id: 20_000_020, region_id: 10_000_002,
        active_ship_id, balance: eve_db::IskCents(0), plex_balance: 0, loyalty_points: HashMap::new(), standings: HashMap::new(),
        security_status: 0.0, clone_grade: 2, home_station_id: 60_003_760, race_id: 1, bloodline_id: 1,
        ancestry_id: 1, school_id: 11, neural: eve_db::NeuralAttributes::default(), skill_points: 0.0,
        free_skill_points: 0.0, last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0, description: String::new(), title: String::new(),
        gender: 1, create_date_unix: 0, fleet_id: None, autopilot_waypoints: Vec::new(), version: 1, system_epoch: 1,
    }
}

fn make_drone_state(
    id: i64,
    owner: i64,
    ship: i64,
    sys: u32,
    range: f64,
    dmg: DamageVector,
    arm_rep: f64,
    tp_pct: f64,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id: id,
        drone_item_id: id,
        type_id: 2454,
        group_id: 100,
        owner_char_id: owner,
        original_owner_char_id: owner,
        parent_ship_id: ship,
        solar_system_id: sys,
        target_id: None,
        activity_state: 1,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 1000.0,
        optimal_range: range,
        damage_vector: dmg,
        shield_repair_amount: 0.0,
        armor_repair_amount: arm_rep,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: tp_pct,
    }
}

#[tokio::test]
async fn r6_t1_01_fighter_tube_merge_and_launch_hp_inflation_blocked() {
    let (ctx, _) = test_ctx_at(1_800_000_000);
    let (sys_id, char_id, carrier_id) = (30_000_142, 91_000_001, 81_000_001);
    let fighter_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 87 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(23_057);
    let mut sess = make_session(char_id, 98_000_001, sys_id, Some(carrier_id));
    let kw = HashMap::new();
    let tube0 = InventoryFlag::from_u16(159);
    let tube1 = InventoryFlag::from_u16(160);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(carrier_id, make_item(carrier_id, char_id, sys_id as i64, InventoryFlag::Cargo, 23_911, 1));
        snap.items.insert(700_001, make_item(700_001, char_id, carrier_id, tube0, fighter_type, 9));
        snap.items.insert(700_004, make_item(700_004, char_id, carrier_id, tube0, fighter_type, 5));
        snap.items.insert(700_002, make_item(700_002, char_id, carrier_id, InventoryFlag::FighterBay, fighter_type, 50));
    });
    ctx.fighter_tube_loads.write().unwrap().insert((carrier_id, 0), 700_001);

    let inv_bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: carrier_id,
        oid_str: format!("N=1:{carrier_id}"),
        ref_id: 1,
    };
    let merge_err = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&inv_bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Integer(700_002)]), PyRep::Integer(700_001)],
        &kw,
    )
    .unwrap();
    assert!(merge_err.is_err(), "merging into an occupied fighter tube must fail");

    let stack_rep = handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&inv_bound),
        "invbroker",
        "StackAll",
        &[PyRep::Integer(159)],
        &kw,
    )
    .unwrap()
    .expect("StackAll completes without inflating loaded tube item");
    assert_eq!(stack_rep.dict_get("mergedCount").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items[&700_001].quantity), 9);

    // Oversized stack in tube: launch splits reserve back to bay & caps squadron count.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(700_003, make_item(700_003, char_id, carrier_id, tube1, fighter_type, 45));
    });
    ctx.fighter_tube_loads.write().unwrap().insert((carrier_id, 1), 700_003);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(make_ship_ball(carrier_id, DVec3::ZERO), CombatState::default())
        .await;
    let _ = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(160)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("LaunchFightersFromTubes should split reserve safely");

    let sq = ctx
        .active_fighters
        .read()
        .unwrap()
        .values()
        .find(|s| s.parent_ship_id == carrier_id)
        .cloned()
        .expect("launched squadron registered");
    assert!(sq.squadron_size <= 9, "launched squadron count must be capped at max_squadron_size");

    let bay_total: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == carrier_id && i.flag == InventoryFlag::FighterBay && i.type_id == fighter_type)
            .map(|i| i64::from(i.quantity))
            .sum()
    });
    assert_eq!(bay_total, 50 + (45 - i64::from(sq.squadron_size)));

    let _ = apply_damage_to_fighter_squadron(&ctx, sys_id, sq.ball_id, DamageVector::new(0.0, 0.0, 5_000.0, 0.0)).await;
    let post_bay: i64 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == carrier_id && i.flag == InventoryFlag::FighterBay && i.type_id == fighter_type)
            .map(|i| i64::from(i.quantity))
            .sum()
    });
    assert_eq!(post_bay, bay_total);
}

#[tokio::test]
async fn r6_t1_02_legacy_pvp_safety_concord_wreck_and_limited_engagement() {
    let now_u = 1_800_000_000_u64;
    let (ctx, _) = test_ctx_at(now_u);
    let sys_id = 30_000_142; // 0.95 high-sec
    let (att_char, vic_char, att_ship, vic_ship) = (91_000_010, 91_000_011, 81_000_010, 81_000_011);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut sess = make_session(att_char, 98_000_010, sys_id, Some(att_ship));
    let b_att = make_ship_ball(att_ship, DVec3::ZERO);
    let b_vic = make_ship_ball(vic_ship, DVec3::new(5_000.0, 0.0, 0.0));
    let _ = sys.spawn_ball_with_combat(b_att.clone(), CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(b_vic.clone(), CombatState::default()).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(att_ship, make_item(att_ship, att_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(vic_ship, make_item(vic_ship, vic_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(81_000_099, make_item(81_000_099, vic_char, vic_ship, InventoryFlag::Cargo, 34, 250));
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Green);
    });
    let err = enforce_legacy_pvp_safety_and_concord(
        &ctx, &mut sess, &sys, att_char, att_ship, &b_att, sys_id, 0.95, vic_ship, &b_vic, vic_char, now_u,
    )
    .await
    .expect_err("Green safety must block high-sec aggression");
    assert_eq!(err.code, "CantDoInCurrentSafetyLevel");

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(att_char, SafetyLevel::Red);
        rt.add_limited_engagement(att_char, vic_char, now_u);
    });
    let (_, le_concord) = enforce_legacy_pvp_safety_and_concord(
        &ctx, &mut sess, &sys, att_char, att_ship, &b_att, sys_id, 0.95, vic_ship, &b_vic, vic_char, now_u,
    )
    .await
    .expect("Limited engagement allowed");
    assert!(le_concord.is_none());

    with_mut_combat_runtime(&ctx, |rt| rt.limited_engagements.clear());
    let (_, concord_rep) = enforce_legacy_pvp_safety_and_concord(
        &ctx, &mut sess, &sys, att_char, att_ship, &b_att, sys_id, 0.95, vic_ship, &b_vic, vic_char, now_u,
    )
    .await
    .expect("CONCORD triggers on illegal high-sec aggression");
    assert!(concord_rep.is_some());
    assert!(ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&att_char)
        .is_some_and(|s| s.is_criminal(now_u)));
}

#[tokio::test]
async fn r6_t1_03_drone_engage_mixed_squadron_range_and_tp_no_compound() {
    let now_u = 1_800_000_000_u64;
    let (ctx, _) = test_ctx_at(now_u);
    let sys_id = 30_000_142;
    let (att_char, friendly_char, suspect_char) = (91_000_020, 91_000_021, 91_000_022);
    let (att_ship, target_ship, suspect_ship) = (81_000_020, 81_000_021, 81_000_022);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    let _ = sys.spawn_ball_with_combat(make_ship_ball(att_ship, DVec3::ZERO), CombatState::default()).await;
    let mut cs = CombatState::default();
    cs.signature_radius = 100.0;
    cs.base_signature_radius = Some(100.0);
    let _ = sys.spawn_ball_with_combat(make_ship_ball(target_ship, DVec3::new(15_000.0, 0.0, 0.0)), cs.clone()).await;
    let _ = sys.spawn_ball_with_combat(make_ship_ball(suspect_ship, DVec3::new(12_000.0, 0.0, 0.0)), CombatState::default()).await;
    for did in [901_001, 901_010, 901_002, 901_003] {
        let _ = sys.spawn_ball(make_ship_ball(did, DVec3::new(1_000.0, 0.0, 0.0))).await;
    }
    for tid in [target_ship, suspect_ship] {
        let _ = sys
            .add_target_lock(
                att_ship,
                eve_destiny::TargetLock {
                    target_id: tid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(suspect_char)
        .or_default()
        .suspect_flag_until_unix = now_u + 900;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, shid) in [(att_char, att_ship), (friendly_char, target_ship), (suspect_char, suspect_ship)] {
            snap.characters.insert(cid, make_char(cid, 98_000_000 + (cid % 1_000), shid, sys_id));
        }
        snap.items.insert(att_ship, make_item(att_ship, att_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(target_ship, make_item(target_ship, friendly_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(suspect_ship, make_item(suspect_ship, suspect_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
    });

    // 1. Out-of-range drone must not deal damage.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            901_001,
            make_drone_state(901_001, att_char, att_ship, sys_id, 5_000.0, DamageVector::new(0.0, 0.0, 100.0, 0.0), 0.0, 0.0),
        );
    });
    let mut sess = make_session(att_char, 98_000_020, sys_id, Some(att_ship));
    let _ = handle_npcs_drones(
        &ctx, &mut sess, "entity", "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(901_001)]), PyRep::Integer(target_ship)],
        &kw,
    )
    .await
    .unwrap()
    .expect("out-of-range drone call");
    assert_eq!(sys.get_combat_state(target_ship).await.unwrap().unwrap().shield_hp, cs.shield_hp);

    // 2. Mixed offensive + logistics squadron assisting a Suspect must trigger Suspect.
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.clear();
        rt.active_drones_in_space.insert(
            901_010,
            make_drone_state(901_010, att_char, att_ship, sys_id, 60_000.0, DamageVector::new(0.0, 0.0, 10.0, 0.0), 0.0, 0.0),
        );
        let mut logi = make_drone_state(901_002, att_char, att_ship, sys_id, 60_000.0, DamageVector::default(), 50.0, 0.0);
        logi.group_id = 640;
        rt.active_drones_in_space.insert(901_002, logi);
        rt.safety_levels.insert(att_char, SafetyLevel::Yellow);
    });
    let _ = handle_npcs_drones(
        &ctx, &mut sess, "entity", "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(901_010), PyRep::Integer(901_002)]), PyRep::Integer(suspect_ship)],
        &kw,
    )
    .await
    .unwrap()
    .expect("mixed offensive+logi squadron");
    assert!(
        ctx.crimewatch.read().unwrap().states.get(&att_char).is_some_and(|s| s.is_suspect(now_u)),
        "assisting a Suspect with a logistics drone in a mixed squadron must flag helper Suspect"
    );

    // 3. Repeated TP engagement must anchor to base_signature_radius (never compound > 1.25x).
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.clear();
        rt.active_drones_in_space.insert(
            901_003,
            make_drone_state(901_003, att_char, att_ship, sys_id, 60_000.0, DamageVector::default(), 0.0, 25.0),
        );
        rt.safety_levels.insert(att_char, SafetyLevel::Red);
    });
    for _ in 0..5 {
        let _ = handle_npcs_drones(
            &ctx, &mut sess, "entity", "CmdEngage",
            &[PyRep::List(vec![PyRep::Integer(901_003)]), PyRep::Integer(target_ship)],
            &kw,
        )
        .await;
    }
    let post_sig = sys.get_combat_state(target_ship).await.unwrap().unwrap().signature_radius;
    assert!(post_sig <= 125.1, "TP sig radius must not compound across calls: {post_sig}");
}

#[tokio::test]
async fn r6_t1_04_lancer_respects_tether_invuln_and_lowsec_crimewatch() {
    let now_u = 1_800_000_000_u64;
    let (ctx, _) = test_ctx_at(now_u);
    let sys_id = 30_002_813; // Tama 0.3 low-sec
    with_mut_combat_runtime(&ctx, |rt| { rt.modified_system_securities.insert(sys_id, 0.3); });
    let (att_char, vic_char, tethered_char, invuln_char) = (91_000_030, 91_000_031, 91_000_032, 91_000_033);
    let (att_ship, vic_ship, tethered_ship, invuln_ship) = (81_000_030, 81_000_031, 81_000_032, 81_000_033);
    let sys = ctx.systems.get_or_spawn(sys_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(att_char, make_char(att_char, 98_000_030, att_ship, sys_id));
        snap.characters.insert(vic_char, make_char(vic_char, 98_000_031, vic_ship, sys_id));
        snap.items.insert(att_ship, make_item(att_ship, att_char, sys_id as i64, InventoryFlag::Cargo, 77_281, 1));
        snap.items.insert(vic_ship, make_item(vic_ship, vic_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(tethered_ship, make_item(tethered_ship, tethered_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(invuln_ship, make_item(invuln_ship, invuln_char, sys_id as i64, InventoryFlag::Cargo, 587, 1));
    });

    let mut tethered_cs = CombatState::default();
    tethered_cs.is_tethered = true;
    let mut invuln_cs = CombatState::default();
    invuln_cs.gate_cloak_remaining_ticks = 20;

    let b_att = make_ship_ball(att_ship, DVec3::ZERO);
    let b_teth = make_ship_ball(tethered_ship, DVec3::new(10_000.0, 0.0, 0.0));
    let b_inv = make_ship_ball(invuln_ship, DVec3::new(15_000.0, 0.0, 0.0));
    let b_vic = make_ship_ball(vic_ship, DVec3::new(20_000.0, 0.0, 0.0));

    let _ = sys.spawn_ball_with_combat(b_att, CombatState::default()).await;
    let _ = sys.spawn_ball_with_combat(b_teth.clone(), tethered_cs).await;
    let _ = sys.spawn_ball_with_combat(b_inv.clone(), invuln_cs).await;
    let _ = sys.spawn_ball_with_combat(b_vic.clone(), CombatState::default()).await;

    let hits = execute_lancer_beam_attack(
        &ctx, &sys, sys_id, att_char, att_ship, 77_281, 77_398,
        &[b_teth, b_inv, b_vic], DVec3::ZERO, DVec3::X, 100_000.0, 5_000.0, 5_000.0,
        DamageVector::new(1_000.0, 0.0, 0.0, 0.0), 100.0, 60, now_u,
    )
    .await
    .expect("lance fires in lowsec");

    assert!(!hits.contains(&tethered_ship), "tethered ship must not be hit by lance");
    assert!(!hits.contains(&invuln_ship), "invulnerable ship must not be hit by lance");
    assert!(hits.contains(&vic_ship), "open ship in cylinder must be hit");
    assert!(sys.get_combat_state(tethered_ship).await.unwrap().unwrap().is_tethered, "tether must remain intact");
    assert!(
        ctx.crimewatch.read().unwrap().states.get(&att_char).is_some_and(|s| s.is_suspect(now_u)),
        "low-sec lance hit must trigger Crimewatch Suspect flag on shooter"
    );
}

#[tokio::test]
async fn r6_t1_05_and_06_scan_state_auth_redaction_and_atomic_probe_expiry() {
    let now_u = 1_800_000_000_u64;
    let (ctx, clock) = test_ctx_at(now_u);
    let (my_sys, other_sys, char_id) = (30_000_142_u32, 30_002_187_u32, 91_000_050_i64);
    let mut sess = make_session(char_id, 98_000_050, my_sys, Some(81_000_050));
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(my_sys).or_default().push(CosmicSignatureRecord {
            signature_id: "ABC-123".to_string(), solar_system_id: my_sys, sig_group: "Relic".to_string(),
            type_name: "Unknown".to_string(), position: [123_456_789.0, -987_654_321.0, 42_000.0],
            signal_strength: 0.25, wormhole_type_id: None, destination_system_id: None, created_unix: now_u,
            lifetime_seconds: 86_400, max_mass_kg: 0.0, remaining_mass_kg: 0.0, max_jump_mass_kg: 0.0, collapsed: false,
        });
        snap.items.insert(81_000_050, make_item(81_000_050, char_id, my_sys as i64, InventoryFlag::Cargo, 587, 1));
    });

    let remote_bound = BoundObjectEntry {
        bound_id: 1, session_id: sess.session_id, user_id: char_id as u64, character_id: char_id,
        service: "scanMgr".into(), object_id: other_sys as i64, oid_str: format!("N=1:{other_sys}"), ref_id: 1,
    };
    let cross_err = handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&remote_bound), "scanMgr", "GetFullState", &[], &kw).await.unwrap();
    assert!(cross_err.is_err(), "cross-system GetFullState must be blocked for non-GM");

    let local_bound = BoundObjectEntry { object_id: my_sys as i64, ..remote_bound };
    let state_rep = handle_scan_and_wormhole_extras(&ctx, &mut sess, Some(&local_bound), "scanMgr", "GetFullState", &[], &kw)
        .await.unwrap().expect("in-system GetFullState");
    assert!(!format!("{state_rep:?}").contains("123456789"), "raw unscanned signature X coordinate must not leak");

    ctx.active_scanner_probes.write().unwrap().insert(555_001, ActiveScannerProbe {
        probe_id: 555_001, probe_type_id: 30_013, owner_char_id: char_id, ship_id: 81_000_050,
        launcher_module_id: 666_001, solar_system_id: my_sys, position: [1.0, 2.0, 3.0], destination: [1.0, 2.0, 3.0],
        range_step: 4, scan_range_m: 4.0 * 149_597_870_700.0, base_sensor_strength: 40.0,
        max_flight_time_s: 10.0, launched_at_unix: now_u, state: 0, warp_end_ft: None,
    });
    clock.advance_secs(20);
    let (ret1, _) = expire_overdue_probes_for_char(&ctx, char_id, now_u + 20).await;
    let (ret2, _) = expire_overdue_probes_for_char(&ctx, char_id, now_u + 20).await;
    assert_eq!(ret1.len(), 1);
    assert!(ret2.is_empty(), "expired probe must not be claimed or returned twice");

    let probe_count: i64 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == 81_000_050 && i.type_id == 30_013).map(|i| i64::from(i.quantity)).sum()
    });
    assert_eq!(probe_count, 1, "expired probe must be returned once, never duplicated");
}

#[tokio::test]
async fn r6_t1_07_and_08_hacking_lockout_difficulty_and_keeper_on_grid_checks() {
    let now_u = 1_800_000_000_u64;
    let (ctx, _) = test_ctx_at(now_u);
    let (sys_id, char_id, ship_id, st_id) = (30_000_142, 91_000_070, 81_000_070, 600_999_001);
    let mut sess = make_session(char_id, 98_000_070, sys_id, Some(ship_id));
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = UpwellStructureRecord::default();
        st.structure_id = st_id;
        st.owner_corp_id = 98_000_099;
        st.solar_system_id = sys_id;
        st.type_id = 35_835;
        st.position = [10_000.0, 0.0, 0.0];
        snap.structures.insert(st_id, st);
        snap.characters.insert(char_id, make_char(char_id, 98_000_070, ship_id, sys_id));
        snap.items.insert(ship_id, make_item(ship_id, char_id, sys_id as i64, InventoryFlag::Cargo, 587, 1));
        snap.items.insert(82_000_070, make_item(82_000_070, char_id, ship_id, InventoryFlag::MedSlot0, 22_175, 1));
        snap.items.insert(82_000_071, make_item(82_000_071, char_id, ship_id, InventoryFlag::MedSlot1, 22_177, 1));
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, DVec3::new(10_000.0, 0.0, 0.0)),
            CombatState::default(),
        )
        .await;

    for attempt in 1..=2 {
        let s = handle_pve_exploration(
            &ctx, &mut sess, "hackingMgr", "StartNewGameInstance",
            &[PyRep::Integer(st_id), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)], &kw,
        ).await.unwrap();
        assert!(s.is_ok(), "attempt {attempt} should start: {s:?}");
        handle_pve_exploration(&ctx, &mut sess, "hackingMgr", "QuitHackingAttempt", &[], &kw)
            .await.unwrap().expect("quit succeeds");
    }
    let locked = handle_pve_exploration(
        &ctx, &mut sess, "hackingMgr", "StartNewGameInstance",
        &[PyRep::Integer(st_id), PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(1)], &kw,
    ).await.unwrap();
    assert!(locked.is_err(), "2-strike QuitHackingAttempt must enforce Upwell structure lockout");

    sys.try_set_ball_position(ship_id, DVec3::new(500_000.0, 0.0, 0.0));
    with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.entry(sys_id).or_default().push(HomefrontSiteRecord {
            instance_id: 710_700, dungeon_id: 900_001, dungeon_name: "Test Homefront Raid".to_string(),
            solar_system_id: sys_id, optimal_fleet_size: 5, base_pilot_isk: 1_000_000,
            completed: false, reward_paid_isk: 0, boss_npc_ball_id: 9_888_777_666, boss_type_id: 0,
        });
    });

    let offgrid = handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(710_700)], &kw).await.unwrap();
    assert!(offgrid.is_err(), "off-grid caller (>100km) must not complete special site via keeper");

    let _ = sys.remove_ball(9_888_777_666).await;
    sys.try_set_ball_position(ship_id, DVec3::new(15_000.0, 0.0, 0.0));
    let ongrid = handle_pve_exploration(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(710_700)], &kw).await.unwrap();
    assert!(ongrid.is_ok(), "on-grid caller should complete special site via keeper: {ongrid:?}");
    let done = with_pve_special_reg(&ctx, |reg| reg.homefront_sites[&sys_id].iter().any(|s| s.instance_id == 710_700 && s.completed));
    assert!(done, "on-grid gate activation must mark special site completed");
}
