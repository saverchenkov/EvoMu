//! Integration tests closing `crimewatch.*` and `drones.*` parity rows:
//! - `feat:crimewatch.concord-response-scaling`
//! - `feat:crimewatch.limited-engagement-duel`
//! - `feat:crimewatch.safety-setting-action-block`
//! - `feat:crimewatch.suspect-timer-looting-ess`
//! - `feat:drones.bandwidth-and-bay-constraints`
//! - `feat:drones.sentry-drone-stationary-tracking`
//! - `feat:drones.drone-npc-aggro-switching`
//! - `feat:drones.carrier-launch-tubes-and-flight-control`

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::combat::crimewatch::{AggressionEvaluation, CrimeWatchDirector};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn call_service(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
    kw: HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(ctx, sess, svc, method, &args, &kw)
        .await
}

fn make_session(
    sid: u64,
    user_id: u32,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: Option<i64>,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = station_id;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = ship_id;
    sess
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

async fn spawn_ship_ball(ctx: &ServerContext, sys_id: u32, ship_id: i64, type_id: u32, owner_id: i64, x: f64) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(type_id),
            SlimItemMeta {
                group_id: 26,
                category_id: 6,
                owner_id,
                char_id: Some(owner_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        )
        .await;
}

#[tokio::test]
async fn test_crimewatch_concord_response_scaling_and_invulnerable_enforcement() {

    let ctx = ServerContext::new();
    let attacker_char = 90_000_001;
    let victim_char = 90_000_002;
    let attacker_ship = 910_001;
    let victim_ship = 910_002;
    let jita_sys = 30_000_142;

    // 1. Verify exact CONCORD response scaling table across 0.5–1.0 security ratings
    for (sec, expected_delay_s) in [
        (0.5, 19.0),
        (0.6, 14.0),
        (0.7, 11.0),
        (0.8, 8.0),
        (0.9, 6.0),
        (1.0, 6.0),
    ] {
        let mut cw = CrimeWatchDirector::new();
        let eval = cw.handle_combat_aggression(attacker_char, victim_char, attacker_ship, jita_sys, sec, false, 1_000);
        match eval {
            AggressionEvaluation::CriminalFlagTriggered { concord_delay_sec, arrival_unix } => {
                assert!((concord_delay_sec - expected_delay_s).abs() < 1e-6);
                assert_eq!(arrival_unix, 1_000 + expected_delay_s as u64);
            }
            other => panic!("Expected CriminalFlagTriggered at sec {sec}, got {other:?}"),
        }
    }

    // 2. Negative check: low-sec (0.4) triggers Suspect Flag without CONCORD
    {
        let mut cw = CrimeWatchDirector::new();
        let low = cw.handle_combat_aggression(attacker_char, victim_char, attacker_ship, jita_sys, 0.4, false, 1_000);
        assert_eq!(low, AggressionEvaluation::SuspectFlagTriggered);
    }

    // 3. End-to-end CONCORD arrival in Jita (sec 0.9 -> 6s):
    //    Spawns invulnerable CONCORD battleship (0.0 resonances), applies 100% web, warp scramble,
    //    cap neut (0 cap), ECM jam, and executes criminal ship into a capsule without destroying the pod.
    seed_item(&ctx, attacker_ship, 621, attacker_char, jita_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, victim_ship, 587, victim_char, jita_sys as i64, InventoryFlag::None, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&attacker_char) {
            c.solar_system_id = jita_sys;
            c.active_ship_id = attacker_ship;
            c.station_id = None;
        }
    });
    spawn_ship_ball(&ctx, jita_sys, attacker_ship, 621, attacker_char, 10_000.0).await;

    let mut cw = CrimeWatchDirector::new();
    let _ = cw.handle_combat_aggression(attacker_char, victim_char, attacker_ship, jita_sys, 0.9, false, 2_000);
    // At t=2003 (3s elapsed < 6s): CONCORD has not arrived yet
    let early = cw.step_concord(&ctx, 2_003, 42).await;
    assert!(early.is_empty());
    // At t=2006 (6s elapsed): CONCORD arrives, neutralizes & destroys criminal ship, leaves pod intact
    let reports = cw.step_concord(&ctx, 2_006, 42).await;
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].criminal_char_id, attacker_char);

    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    let concord_cs = sys_h
        .get_combat_state(reports[0].concord_ball_id)
        .await
        .unwrap()
        .expect("CONCORD ship combat state must exist on grid");
    assert_eq!(concord_cs.shield_resonance, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(concord_cs.armor_resonance, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(concord_cs.structure_resonance, [0.0, 0.0, 0.0, 0.0]);

    let new_active_ship = ctx
        .db
        .with_snapshot(|s| s.characters.get(&attacker_char).map(|c| c.active_ship_id).unwrap_or(0));
    let new_ship_tid = ctx
        .db
        .with_snapshot(|s| s.items.get(&new_active_ship).map(|i| i.type_id).unwrap_or(0));
    assert_eq!(new_ship_tid, 670, "CONCORD must destroy the criminal ship and leave the capsule intact");
}

#[tokio::test]
async fn test_crimewatch_safety_setting_block_duel_and_suspect_looting_ess() {

    let ctx = new_ctx();
    let c1 = 90_000_001;
    let c2 = 90_000_002;
    let ship1 = 920_001;
    let ship2 = 920_002;
    let mod_scram = 920_010;
    let mod_burst = 920_011;
    let jita_sys = 30_000_142;

    let burst_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 80 && t.category_id == 7)
        .map(|t| t.type_id)
        .expect("Burst Jammer in SDE");
    seed_item(&ctx, ship1, 587, c1, jita_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, ship2, 587, c2, jita_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, mod_scram, 447, c1, ship1, InventoryFlag::MedSlot0, 1, true);
    seed_item(&ctx, mod_burst, burst_tid, c1, ship1, InventoryFlag::MedSlot1, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(mod_scram, true);
        snap.module_online_states.insert(mod_burst, true);
        for (cid, sid) in [(c1, ship1), (c2, ship2)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.solar_system_id = jita_sys;
                ch.active_ship_id = sid;
                ch.station_id = None;
            }
        }
    });
    spawn_ship_ball(&ctx, jita_sys, ship1, 587, c1, 0.0).await;
    spawn_ship_ball(&ctx, jita_sys, ship2, 587, c2, 2_000.0).await;
    let sys_h = ctx.systems.get_or_spawn(jita_sys);
    let _ = sys_h
        .add_target_lock(
            ship1,
            TargetLock {
                target_id: ship2,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(20_000.0),
            },
        )
        .await;

    let mut sess1 = make_session(1, 1, c1, 1000009, None, jita_sys, Some(ship1));
    let mut sess2 = make_session(2, 2, c2, 1000010, None, jita_sys, Some(ship2));

    // 1. Safety Green (2) in high-sec blocks illegal EWAR and AoE Burst Jammer with CantDoInCurrentSafetyLevel
    let _ = call_service(&ctx, &mut sess1, "crimewatch", "SetSafetyLevel", vec![PyRep::Integer(2)], HashMap::new()).await.unwrap();
    let err_ewar = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(mod_scram), PyRep::String("warpScramble".into()), PyRep::Integer(ship2)], HashMap::new()).await.expect_err("Safety Green must block illegal high-sec EWAR");
    assert_eq!(err_ewar.code, "SafetyLevelRestricted");
    assert!(err_ewar.message.contains("CantDoInCurrentSafetyLevel"));

    let err_burst = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(mod_burst), PyRep::String("ecmBurst".into())], HashMap::new()).await.expect_err("Safety Green must block high-sec Burst Jammer");
    assert_eq!(err_burst.code, "SafetyLevelRestricted");
    assert!(err_burst.message.contains("CantDoInCurrentSafetyLevel"));

    // 2. Duel challenge -> accept creates 300s Limited Engagement, allowing legal engagement on Safety Green
    //    and refreshing the LE back to 300s on subsequent EWAR/combat hits!
    let _ = call_service(&ctx, &mut sess1, "crimewatch", "StartDuelChallenge", vec![PyRep::Integer(c2)], HashMap::new()).await.unwrap();
    let accept_rep = call_service(&ctx, &mut sess2, "crimewatch", "RespondToDuelChallenge", vec![PyRep::Integer(c1), PyRep::Bool(true)], HashMap::new()).await.unwrap();
    assert_eq!(accept_rep.dict_get("limitedEngagementSeconds").and_then(|v| v.as_i64()), Some(300));

    let ewar_ok = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(mod_scram), PyRep::String("warpScramble".into()), PyRep::Integer(ship2)], HashMap::new()).await.expect("Limited Engagement must allow EWAR under Safety Green without CONCORD");
    assert_eq!(ewar_ok.dict_get("ewarAssistRecorded").and_then(|v| v.as_bool()), Some(true));
    let has_le = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.has_limited_engagement_with(c1, c2, ctx.now_unix_secs())
    });
    assert!(has_le, "Limited Engagement must be active between duelists");

    // Advance virtual clock by 100s, attack again, assert timer refreshes to now + 300s
    ctx.advance_virtual_clock(100);
    let now_refresh = ctx.now_unix_secs();
    let _ = call_service(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(mod_scram), PyRep::String("warpScramble".into()), PyRep::Integer(ship2)], HashMap::new()).await.expect("Duel attack refreshes timer");
    let (le1, le2) = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let e1 = rt.limited_engagements.get(&c1).and_then(|m| m.get(&c2)).copied().unwrap_or(0);
        let e2 = rt.limited_engagements.get(&c2).and_then(|m| m.get(&c1)).copied().unwrap_or(0);
        (e1, e2)
    });
    assert!(le1 >= now_refresh + 300 && le2 >= now_refresh + 300, "Hostile action must refresh Limited Engagement timer to now + 300s");

    // 3. Suspect timer (900s) from looting non-owned MTU (type 33475, group 1250) and linking ESS Main Bank
    let mtu_id = 920_050;
    let loot_id = 920_051;
    let other_owner = 90_000_003;
    seed_item(&ctx, mtu_id, 33475, other_owner, jita_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, loot_id, 34, other_owner, mtu_id, InventoryFlag::Cargo, 10, false);
    spawn_ship_ball(&ctx, jita_sys, mtu_id, 33475, other_owner, 500.0).await;

    let loot_green_err = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(loot_id), PyRep::Integer(ship1), PyRep::Integer(InventoryFlag::Cargo as i64)], HashMap::new()).await.expect_err("Safety Green must block looting another player's MTU");
    assert_eq!(loot_green_err.code, "SafetyLevelRestricted");

    let _ = call_service(&ctx, &mut sess1, "crimewatch", "SetSafetyLevel", vec![PyRep::Integer(1)], HashMap::new()).await.unwrap();
    let now_loot = ctx.now_unix_secs();
    let _ = call_service(&ctx, &mut sess1, "invbroker", "Add", vec![PyRep::Integer(loot_id), PyRep::Integer(ship1), PyRep::Integer(InventoryFlag::Cargo as i64)], HashMap::new()).await.expect("Safety Yellow must allow looting MTU and apply 900s Suspect timer");
    let suspect_until = ctx.crimewatch.read().unwrap().states.get(&c1).map(|s| s.suspect_flag_until_unix).unwrap_or(0);
    assert!(suspect_until >= now_loot + 900);

    // ESS Main Bank link blocks on Safety Green and applies 900s Suspect timer on Safety Yellow
    let null_sys = 30_004_759;
    let ess_ball_id = 820_000_000 + null_sys as i64;
    sess2.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c2) { ch.solar_system_id = null_sys; }
        if let Some(sh) = snap.items.get_mut(&ship2) { sh.location_id = null_sys as i64; }
        let st = snap.system_states.entry(null_sys).or_default();
        st.ess_main_bank = IskCents::from_isk(50_000_000);
    });
    spawn_ship_ball(&ctx, null_sys, ship2, 587, c2, 0.0).await;
    spawn_ship_ball(&ctx, null_sys, ess_ball_id, 32882, 0, 500.0).await;
    let _ = call_service(&ctx, &mut sess2, "crimewatch", "SetSafetyLevel", vec![PyRep::Integer(2)], HashMap::new()).await.unwrap();
    let ess_green_err = call_service(&ctx, &mut sess2, "essMgr", "AttemptLinkToMainBank", vec![PyRep::Integer(ess_ball_id)], HashMap::new()).await.expect_err("Safety Green must block ESS Main Bank theft link");
    assert_eq!(ess_green_err.code, "SafetyLevelRestricted");

    let _ = call_service(&ctx, &mut sess2, "crimewatch", "SetSafetyLevel", vec![PyRep::Integer(1)], HashMap::new()).await.unwrap();
    let ess_ok = call_service(&ctx, &mut sess2, "essMgr", "AttemptLinkToMainBank", vec![PyRep::Integer(ess_ball_id)], HashMap::new()).await.expect("Safety Yellow must allow ESS Main Bank link with 900s Suspect timer");
    assert!(ess_ok.dict_get("suspectTimerUntilUnix").and_then(|v| v.as_i64()).unwrap_or(0) >= (ctx.now_unix_secs() + 900) as i64);

    // Concurrent link attempt by another character is rejected with EssAlreadyLinked (R-ESS-REGRESS-01)
    let mut sess1_null = make_session(1, 1, c1, 1000009, None, null_sys, Some(ship1));
    let _ = call_service(&ctx, &mut sess1_null, "crimewatch", "SetSafetyLevel", vec![PyRep::Integer(1)], HashMap::new()).await.unwrap();
    spawn_ship_ball(&ctx, null_sys, ship1, 587, c1, 100.0).await;
    let ess_concurrent_err = call_service(&ctx, &mut sess1_null, "essMgr", "AttemptLinkToMainBank", vec![PyRep::Integer(ess_ball_id)], HashMap::new()).await.expect_err("Concurrent ESS link must fail with EssAlreadyLinked");
    assert_eq!(ess_concurrent_err.code, "EssAlreadyLinked");
}

#[tokio::test]
async fn test_drones_bandwidth_sentry_stationary_npc_aggro_and_carrier_tubes() {
    // parity: feat:drones.bandwidth-and-bay-constraints
    // parity: feat:drones.sentry-drone-stationary-tracking
    eve_testkit::attest(&[
        "feat:drones.bandwidth-and-bay-constraints",
        "feat:drones.sentry-drone-stationary-tracking",
    ]);

    let ctx = new_ctx();
    let c1 = 90_000_001;
    let sys_id = 30_000_142;
    let vexor_id = 930_001;
    let ogre_1 = 930_010;
    let ogre_2 = 930_012;
    let ogre_3 = 930_013;
    let garde_id = 930_011;

    seed_item(&ctx, vexor_id, 626, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, ogre_1, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, ogre_2, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, ogre_3, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, garde_id, 23561, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = vexor_id;
            ch.station_id = None;
        }
        snap.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 });
    });
    spawn_ship_ball(&ctx, sys_id, vexor_id, 626, c1, 0.0).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    let mut sess = make_session(10, 1, c1, 1000009, None, sys_id, Some(vexor_id));

    // 0. Drone bay capacity (125 m3 on Vexor) and maxActiveDrones skill limit (skill 3436 level 1 -> max 1 drone)
    let extra_full_ogre = 930_020;
    let excess_cargo_ogre = 930_021;
    seed_item(&ctx, extra_full_ogre, 2444, c1, vexor_id, InventoryFlag::DroneBay, 1, true);
    seed_item(&ctx, excess_cargo_ogre, 2444, c1, vexor_id, InventoryFlag::Cargo, 1, true);
    let bay_cap_err = call_service(&ctx, &mut sess, "invbroker", "Add", vec![PyRep::Integer(excess_cargo_ogre), PyRep::Integer(vexor_id), PyRep::Integer(87)], HashMap::new()).await.expect_err("Exceeding 125 m3 drone bay capacity must fail");
    assert_eq!(bay_cap_err.code, "CapacityExceeded");

    // Untrained pilot with no Drones skill record gets 0 drone capacity and NoDroneManagementAbilities (R-DRONE-NOSKILL-5)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().remove(&3436);
    });
    ctx.invalidate_ship_fitting(vexor_id);
    let no_skill_rec_err = call_service(&ctx, &mut sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(ogre_1)])], HashMap::new()).await.expect_err("Launching drones with no Drones skill record must fail");
    assert_eq!(no_skill_rec_err.code, "NoDroneManagementAbilities");

    // Launching when drone bay volume exceeds capacity must fail
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 0, skill_points: 0.0 });
    });
    ctx.invalidate_ship_fitting(vexor_id);
    let no_drone_err = call_service(&ctx, &mut sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(ogre_1)])], HashMap::new()).await.expect_err("Launching drones with level 0 Drones skill must fail");
    assert_eq!(no_drone_err.code, "NoDroneManagementAbilities");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 1, skill_points: 250.0 });
    });
    ctx.invalidate_ship_fitting(vexor_id);
    let max_drone_err = call_service(&ctx, &mut sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2)])], HashMap::new()).await.expect_err("Launching 2 drones with level 1 Drones skill must fail");
    assert_eq!(max_drone_err.code, "NoDroneManagementAbilitiesLeft");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(c1).or_default().insert(3436, TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 });
    });
    ctx.invalidate_ship_fitting(vexor_id);

    // 1. Launch 1 Garde I (25 Mbit/s); launching 3x Ogre I (75 Mbit/s) alongside it exceeds 75 Mbit/s bandwidth!
    let _ = call_service(&ctx, &mut sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(garde_id)])], HashMap::new()).await.expect("Launching Garde I must succeed");
    let bw_err = call_service(&ctx, &mut sess, "ship", "LaunchDrones", vec![PyRep::List(vec![PyRep::Integer(ogre_1), PyRep::Integer(ogre_2), PyRep::Integer(ogre_3)])], HashMap::new()).await.expect_err("Launching 3x Ogre I alongside 1x Garde I must exceed 75 Mbit/s bandwidth");
    assert_eq!(bw_err.code, "MaxBandwidthExceeded");

    // 2. Sentry drone stationary tracking: Garde I remains stationary (max_velocity == 0), CmdReturnBay fails > 2500m
    let active_garde_ball = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.values().find(|d| d.type_id == 23561).map(|d| d.ball_id)
    }).expect("Active Garde I ball");
    let (_, garde_ball_snap) = sys_h.get_ball_snapshot(active_garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_ball_snap.max_velocity, 0.0, "Sentry drone must have max_velocity == 0");

    let _ = call_service(&ctx, &mut sess, "entity", "CmdReturnAndOrbit", vec![PyRep::List(vec![PyRep::Integer(active_garde_ball)])], HashMap::new()).await.expect("CmdReturnAndOrbit on sentry drone succeeds");
    let (_, garde_after_snap) = sys_h.get_ball_snapshot(active_garde_ball).await.unwrap().unwrap();
    assert_eq!(garde_after_snap.max_velocity, 0.0, "Sentry drone remains stationary at 0 m/s");

    let _ = sys_h.set_ball_position(vexor_id, DVec3::new(4_000.0, 0.0, 0.0)).await;
    let ret_err = call_service(&ctx, &mut sess, "entity", "CmdReturnBay", vec![PyRep::List(vec![PyRep::Integer(active_garde_ball)])], HashMap::new()).await.expect_err("CmdReturnBay beyond 2500m must fail");
    assert_eq!(ret_err.code, "SentryDroneOutOfScoopRange");
    let scoop_err = call_service(&ctx, &mut sess, "ship", "ScoopToDroneBay", vec![PyRep::Integer(active_garde_ball)], HashMap::new()).await.expect_err("Scooping sentry drone beyond 2500m must fail");
    assert_eq!(scoop_err.code, "OutOfScoopRange");
    let _ = sys_h.set_ball_position(vexor_id, DVec3::ZERO).await;

    // 3. NPC aggro switching onto player drones based on threat/sig/proximity/DPS and back to ship on recall
    let npc_id = eve_server::services::combat_crimewatch::spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        10004,
        "Pirate",
        1,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    let _ = sys_h.add_target_lock(vexor_id, TargetLock { target_id: npc_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(60_000.0) }).await;
    let engage_rep = call_service(&ctx, &mut sess, "entity", "CmdEngage", vec![PyRep::List(vec![PyRep::Integer(active_garde_ball)]), PyRep::Integer(npc_id)], HashMap::new()).await.expect("Garde I engages NPC");
    assert!(engage_rep.dict_get("totalDamageDealt").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    // Sentry drone optimal, falloff, and tracking turret formula hit-chance evaluation
    let (hit_opt, hit_falloff, hit_transversal) = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let d = rt.active_drones_in_space.get(&active_garde_ball).unwrap();
        let d_ball = Ball { id: active_garde_ball, type_id: 23561, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1000.0, radius: 10.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let falloff = d.effective_falloff_range(&ctx);
        let t_opt = Ball { id: npc_id, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_falloff = Ball { id: npc_id, type_id: 10004, position: DVec3::new(d.optimal_range + falloff, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 1000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
        let t_trans = Ball { id: npc_id, type_id: 10004, position: DVec3::new(d.optimal_range, 0.0, 0.0), velocity: DVec3::new(0.0, 5000.0, 0.0), max_velocity: 5000.0, inertia: 1.0, mass: 1000.0, radius: 25.0, speed_fraction: 1.0, mode: MovementMode::Stop };
        (
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_opt, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_falloff, 50.0),
            d.compute_combat_hit_multiplier(&ctx, &d_ball, &t_trans, 25.0),
        )
    });
    assert!((hit_opt - 1.0).abs() < 1e-3, "Stationary target at optimal has 1.0 hit chance");
    assert!((hit_falloff - 0.5).abs() < 0.05, "Target at optimal + falloff has ~0.5 hit chance");
    assert!(hit_transversal < 0.25, "High transversal tracking penalty drops hit chance");

    eve_server::services::combat_crimewatch::step_extended_npc_ai(&ctx, sys_id).await;
    let (npc_target, threat_dbg) = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let n = rt.npc_entities.get(&npc_id).unwrap();
        (n.target_id, n.aggression_threat.clone())
    });
    assert_eq!(npc_target, Some(active_garde_ball), "NPC must switch aggro onto high-threat player drone (threats={threat_dbg:?})");

    let _ = call_service(&ctx, &mut sess, "ship", "ScoopToDroneBay", vec![PyRep::Integer(active_garde_ball)], HashMap::new()).await.expect("Scooping Garde I within 2500m succeeds");
    let garde_flag = ctx.db.with_snapshot(|s| s.items.get(&garde_id).map(|i| i.flag));
    assert_eq!(garde_flag, Some(InventoryFlag::DroneBay), "Scooped drone must return to DroneBay");
    eve_server::services::combat_crimewatch::record_npc_threat_event(&ctx, npc_id, vexor_id, 50.0, false, false);
    eve_server::services::combat_crimewatch::step_extended_npc_ai(&ctx, sys_id).await;
    let npc_target_after = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.get(&npc_id).and_then(|n| n.target_id)
    });
    assert_eq!(npc_target_after, Some(vexor_id), "NPC must switch aggro back to player ship after drone recall");

    // 4. Carrier launch tubes & flight control (`Archon` 24483 + `Templar I` 23057):
    let archon_id = 930_100;
    let templar_item = 930_101;
    let templar_oversize = 930_102;
    seed_item(&ctx, archon_id, 24483, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, templar_item, 23057, c1, archon_id, InventoryFlag::FighterBay, 9, false);
    seed_item(&ctx, templar_oversize, 23057, c1, archon_id, InventoryFlag::FighterBay, 12, false);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1) { ch.active_ship_id = archon_id; }
    });
    sess.ship_id = Some(archon_id);
    spawn_ship_ball(&ctx, sys_id, archon_id, 24483, c1, 0.0).await;

    // Carrier has 4 tubes (0..3); tube 4 is invalid
    let bad_tube = call_service(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", vec![PyRep::Integer(4), PyRep::Integer(templar_item)], HashMap::new()).await.expect_err("Carrier max 4 tubes");
    assert_eq!(bad_tube.code, "InvalidFighterTube");

    // Supercarrier (Aeon 23919) has 5 tubes (0..4); tube 4 is valid, tube 5 is invalid
    let aeon_id = 930_200;
    let aeon_templar = 930_201;
    seed_item(&ctx, aeon_id, 23919, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, aeon_templar, 23057, c1, aeon_id, InventoryFlag::FighterBay, 9, false);
    sess.ship_id = Some(aeon_id);
    let aeon_load4 = call_service(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", vec![PyRep::Integer(4), PyRep::Integer(aeon_templar)], HashMap::new()).await.expect("Supercarrier tube 4 valid");
    assert_eq!(aeon_load4.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    let aeon_load5 = call_service(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", vec![PyRep::Integer(5), PyRep::Integer(aeon_templar)], HashMap::new()).await.expect_err("Supercarrier max 5 tubes");
    assert_eq!(aeon_load5.code, "InvalidFighterTube");
    sess.ship_id = Some(archon_id);

    // Oversized squadron size check (max_squadron_size attr 2215 == 6 for Dragonfly I): loading 12 caps to 6
    let load_rep = call_service(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", vec![PyRep::Integer(0), PyRep::Integer(templar_oversize)], HashMap::new()).await.expect("LoadFightersToTube must succeed");
    assert_eq!(load_rep.dict_get("success").and_then(|v| v.as_bool()), Some(true));
    let loaded_qty = ctx.db.with_snapshot(|s| s.items.get(&templar_oversize).map(|i| i.quantity)).unwrap();
    assert_eq!(loaded_qty, 6, "Loaded squadron size must be capped to max_squadron_size (6)");

    // Cannot launch fighters in warp:
    let _ = sys_h.set_movement_mode(archon_id, eve_destiny::MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000_000.0, 0.0, 0.0), 3.0)).await;
    let warp_launch_err = call_service(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", vec![PyRep::List(vec![PyRep::Integer(0)])], HashMap::new()).await.expect_err("Cannot launch fighters in warp");
    assert_eq!(warp_launch_err.code, "CannotLaunchFightersInWarp");
    let _ = sys_h.set_movement_mode(archon_id, eve_destiny::MovementMode::Stop).await;

    let launch_rep = call_service(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", vec![PyRep::List(vec![PyRep::Integer(0)])], HashMap::new()).await.expect("LaunchFightersFromTubes must succeed");
    let fighter_ball_id = match launch_rep {
        PyRep::List(ref items) => items.first().and_then(|v| v.as_i64()).expect("launched fighter ball ID"),
        _ => panic!("Expected list of launched fighter ball IDs"),
    };
    let move_rep = call_service(&ctx, &mut sess, "fighterMgr", "ExecuteMovementCommandOnFighters", vec![PyRep::List(vec![PyRep::Integer(fighter_ball_id)]), PyRep::String("ORBIT".into()), PyRep::Integer(npc_id)], HashMap::new()).await.expect("ExecuteMovementCommandOnFighters must succeed");
    assert_eq!(move_rep.as_bool(), Some(true));
    let recall_rep = call_service(&ctx, &mut sess, "fighterMgr", "RecallFightersToTubes", vec![PyRep::List(vec![PyRep::Integer(fighter_ball_id)])], HashMap::new()).await.expect("RecallFightersToTubes must succeed");
    assert!(matches!(recall_rep, PyRep::List(ref items) if items.len() == 1));
    let rearm_active = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until.contains_key(&(archon_id, 0))
    });
    assert!(rearm_active, "Recalled carrier fighter tube must enter refuel/rearm state");
}
