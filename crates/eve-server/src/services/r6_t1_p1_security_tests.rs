//! Exploit and security regression tests for Round 6 Track 1 P1 issues (#363-#365).
#![forbid(unsafe_code)]

use crate::router::{ActiveFighterSquadron, ServerContext};
use crate::services::combat_crimewatch::combat_fighters::handle_fighters;
use crate::services::combat_crimewatch::combat_npcs_drones::handle_npcs_drones;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{FighterAbilitySlot, SdeRegistry};
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

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
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
    character_id: i64,
    corporation_id: i64,
    active_ship_id: i64,
    solar_system_id: u32,
) -> eve_db::CharacterRecord {
    eve_db::CharacterRecord {
        activity_status: None,
        character_id,
        user_id: character_id as u64,
        name: format!("Pilot {character_id}"),
        corporation_id,
        alliance_id: None,
        war_faction_id: None,
        station_id: None,
        structure_id: None,
        solar_system_id,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id,
        balance: eve_db::IskCents(0),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 11,
        neural: eve_db::NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 0,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

#[tokio::test]
async fn issue_363_zero_drone_bay_ship_rejects_scoop_to_bay() {
    let (ctx, _) = test_ctx_at(1_700_001_000);
    let char_id = 91_001_001;
    let corp_id = 98_001_001;
    let sys_id = 30_000_142;
    let capsule_id = 1_100_001_001;
    let drone_id = 1_100_001_002;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(char_id, make_char(char_id, corp_id, capsule_id, sys_id));
        snap.items.insert(
            capsule_id,
            make_item(capsule_id, char_id, sys_id as i64, InventoryFlag::None, 670, 1),
        );
        snap.items.insert(
            drone_id,
            make_item(drone_id, char_id, sys_id as i64, InventoryFlag::None, 2488, 1),
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ship_ball(capsule_id, 670, DVec3::ZERO)).await;
    let _ = sys.spawn_ball(make_ship_ball(drone_id, 2488, DVec3::new(200.0, 0.0, 0.0))).await;

    let mut sess = make_session(char_id, corp_id, sys_id, Some(capsule_id));
    let kwargs = HashMap::new();
    let res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(drone_id)],
        &kwargs,
    )
    .await
    .expect("handled");

    let err = res.expect_err("ship with 0.0 m3 drone bay must reject drone scoop");
    assert_eq!(err.code, "InsufficientDroneBayCapacity");
    assert!(sys.get_ball_snapshot(drone_id).await.unwrap().is_some());
}

#[tokio::test]
async fn issue_364_fighter_mwd_enforces_sde_ability_cooldown_and_charges() {
    let mut sde = (*shared_sde()).clone();
    sde.fighter_abilities_by_type.insert(
        23_055,
        vec![FighterAbilitySlot {
            ability_id: 9,
            slot_index: 1,
            cooldown_seconds: Some(30.0),
            charges: Some(eve_sde::FighterAbilityCharges {
                charge_count: 2,
                rearm_time_seconds: 60.0,
            }),
        }],
    );
    sde.fighter_abilities_by_type.insert(23_061, vec![]);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::new(sde), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_002_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let char_id = 91_002_001;
    let corp_id = 98_002_001;
    let sys_id = 30_000_142;
    let carrier_id = 1_100_002_001;
    let no_mwd_fighter = 1_100_002_002;
    let light_fighter = 1_100_002_003;

    {
        let mut fighters = ctx.active_fighters.write().unwrap();
        fighters.insert(
            no_mwd_fighter,
            ActiveFighterSquadron {
                ball_id: no_mwd_fighter,
                item_id: no_mwd_fighter,
                type_id: 23_061,
                owner_char_id: char_id,
                parent_ship_id: carrier_id,
                tube_index: 0,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        fighters.insert(
            light_fighter,
            ActiveFighterSquadron {
                ball_id: light_fighter,
                item_id: light_fighter,
                type_id: 23_055,
                owner_char_id: char_id,
                parent_ship_id: carrier_id,
                tube_index: 1,
                squadron_size: 9,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.spawn_ball(make_ship_ball(no_mwd_fighter, 23_061, DVec3::ZERO)).await;
    let _ = sys.spawn_ball(make_ship_ball(light_fighter, 23_055, DVec3::new(1000.0, 0.0, 0.0))).await;

    let mut sess = make_session(char_id, corp_id, sys_id, Some(carrier_id));
    let kwargs = HashMap::new();

    let err_unsupported = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(no_mwd_fighter)]), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect_err("fighter without MWD ability must fail");
    assert_eq!(err_unsupported.code, "UnsupportedFighterAbility");

    let ok1 = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(light_fighter)]), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .expect("handled");
    assert!(ok1.is_ok(), "first valid MWD activation should succeed: {ok1:?}");

    let err_cd = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(light_fighter)]), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect_err("MWD on cooldown must be rejected");
    assert_eq!(err_cd.code, "AbilityOnCooldown");

    clock.advance_secs(31);
    let ok2 = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(light_fighter)]), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .expect("handled");
    assert!(ok2.is_ok());

    clock.advance_secs(31);
    let err_charges = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "ExecuteMovementCommandOnFighters",
        &[PyRep::List(vec![PyRep::Integer(light_fighter)]), PyRep::String("MWD".into())],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect_err("MWD out of charges must be rejected");
    assert_eq!(err_charges.code, "OutOfCharges");
}

#[tokio::test]
async fn issue_365_fighter_point_bomb_parses_ints_rejects_nan_and_activates_all_fighters() {
    let mut sde = (*shared_sde()).clone();
    sde.fighter_abilities.entry(7).and_modify(|fa| {
        fa.target_mode = "pointTargeted".into();
        fa.disallow_in_high_sec = true;
    });
    sde.fighter_abilities_by_type.insert(
        40_358,
        vec![FighterAbilitySlot {
            ability_id: 7,
            slot_index: 2,
            cooldown_seconds: Some(10.0),
            charges: Some(eve_sde::FighterAbilityCharges {
                charge_count: 3,
                rearm_time_seconds: 60.0,
            }),
        }],
    );

    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(Arc::new(sde), db));
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_003_000));
    ctx.set_clock(Arc::clone(&clock) as Arc<dyn ServerClock>);

    let char_id = 91_003_001;
    let corp_id = 98_003_001;
    let null_sys_id = 30_000_842;
    let carrier_id = 1_100_003_001;
    let fid1 = 1_100_003_002;
    let fid2 = 1_100_003_003;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.modified_system_securities.insert(null_sys_id, -0.5);
    });
    for (idx, fid) in [fid1, fid2].into_iter().enumerate() {
        ctx.active_fighters.write().unwrap().insert(
            fid,
            ActiveFighterSquadron {
                ball_id: fid,
                item_id: fid,
                type_id: 40_358,
                owner_char_id: char_id,
                parent_ship_id: carrier_id,
                tube_index: idx,
                squadron_size: 6,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    let sys = ctx.systems.get_or_spawn(null_sys_id);
    let _ = sys.spawn_ball(make_ship_ball(fid1, 40_358, DVec3::ZERO)).await;
    let _ = sys.spawn_ball(make_ship_ball(fid2, 40_358, DVec3::new(100.0, 0.0, 0.0))).await;

    let mut sess = make_session(char_id, corp_id, null_sys_id, Some(carrier_id));
    let kwargs = HashMap::new();

    let err_nan = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::List(vec![PyRep::Integer(fid1)]),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect_err("NaN coordinates must be rejected");
    assert_eq!(err_nan.code, "InvalidTargetPoint");

    let ok_multi = handle_fighters(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[
            PyRep::List(vec![PyRep::Integer(fid1), PyRep::Integer(fid2)]),
            PyRep::Integer(2),
            PyRep::Integer(30_000),
            PyRep::Integer(250),
            PyRep::Integer(-100),
        ],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect("multi-fighter integer coordinates must succeed");

    match ok_multi {
        PyRep::List(items) => assert_eq!(items.len(), 2, "both fighter squadrons must activate"),
        other => panic!("expected list of 2 activation results, got {other:?}"),
    }
    assert_eq!(ctx.fighter_charge_state.read().unwrap().get(&(fid1, 2)).copied(), Some(1));
    assert_eq!(ctx.fighter_charge_state.read().unwrap().get(&(fid2, 2)).copied(), Some(1));
}
