#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use crate::services::insurgency_services::{add_system_corruption, init_insurgency_campaign};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_heist_lifecycle_and_escort_mutations() {
    assert_eq!(militia_corp_for_faction(Some(500_001)), Some(1_000_180));
    assert_eq!(militia_corp_for_faction(Some(500_002)), Some(1_000_182));
    assert_eq!(militia_corp_for_faction(Some(500_003)), Some(1_000_179));
    assert_eq!(militia_corp_for_faction(Some(500_004)), Some(1_000_181));
    assert_eq!(militia_corp_for_faction(Some(FACTION_ANGEL_CARTEL)), Some(1_000_436));
    assert_eq!(militia_corp_for_faction(Some(FACTION_GURISTAS)), Some(1_000_437));
    assert_eq!(militia_corp_for_faction(Some(999)), None);

    assert!(is_winning_side_faction(Some(FACTION_ANGEL_CARTEL), true));
    assert!(is_winning_side_faction(Some(FACTION_GURISTAS), true));
    assert!(!is_winning_side_faction(Some(500_001), true));
    assert!(is_winning_side_faction(Some(500_001), false));
    assert!(!is_winning_side_faction(Some(FACTION_GURISTAS), false));

    let ctx = test_ctx();
    let dps = thunderchild_guard_dps_vector(&ctx);
    assert!(dps.total() > 0.0);

    let sys_id = 30045329_u32;
    // Before corruption stage 3..=4, spawn_ice_heist_checked fails
    let err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(err.code, "CorruptionStageOutOfRange");

    // Start campaign and advance corruption to stage 3 (450 pts)
    let camp_id = 77_001_i64;
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, sys_id, vec![sys_id]);
    let (_, stg) = add_system_corruption(&ctx, camp_id, sys_id, 450);
    assert_eq!(stg, 3);

    let inst_id = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap();
    assert!(inst_id >= 800_001);

    // Calling spawn_ice_heist_checked again returns the existing active instance
    assert_eq!(spawn_ice_heist_checked(&ctx, sys_id).await.unwrap(), inst_id);

    // Unknown instance fails escort
    assert!(escort_ice_heist_tanker(&ctx, 90_000_001, 999_999, true).await.is_err());

    // Step system tick
    step_ice_heist_system(&ctx, sys_id).await;

    // Escort 9 tankers on pirate side
    for i in 1..10 {
        let rep = escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true)
            .await
            .unwrap();
        assert_eq!(rep.dict_get("pirateTankersSaved"), Some(&PyRep::Integer(i)));
        assert_eq!(rep.dict_get("completed"), Some(&PyRep::Bool(false)));
    }
    // 10th tanker completes the Ice Heist and sets 4h cooldown
    let win_rep = escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true)
        .await
        .unwrap();
    assert_eq!(win_rep.dict_get("pirateTankersSaved"), Some(&PyRep::Integer(10)));
    assert_eq!(win_rep.dict_get("completed"), Some(&PyRep::Bool(true)));

    // Escorting completed instance fails
    assert!(escort_ice_heist_tanker(&ctx, 90_000_001, inst_id, true).await.is_err());

    // Respawning during cooldown fails
    let cd_err = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err();
    assert_eq!(cd_err.code, "IceHeistRespawnCooldown");
}

fn make_ship_type(type_id: u32, group_id: u32, name: &str) -> eve_sde::InvType {
    eve_sde::InvType {
        type_id,
        group_id,
        category_id: 6,
        name: name.into(),
        mass: 1_000_000.0,
        volume: 20_000.0,
        capacity: 400.0,
        radius: 40.0,
        base_price: 500_000.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: std::collections::HashMap::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_heist_empire_win_lp_guards_steering_scoring_and_thunderchild_combat() {
    let base_ctx = test_ctx();
    let mut sde = (*base_ctx.sde).clone();
    // 1) Custom Thunderchild SDE attributes -> thunderchild_guard_dps_vector uses attr 64/114/116/117/118
    let mut guard_type = make_ship_type(THUNDERCHILD_GUARD_TYPE_ID, 25, "Upwell Security Thunderchild");
    guard_type.default_attributes = std::collections::HashMap::from([
        (64, 2.0),
        (114, 100.0),
        (116, 50.0),
        (117, 25.0),
        (118, 10.0),
    ]);
    sde.types.insert(THUNDERCHILD_GUARD_TYPE_ID, guard_type);
    sde.types.insert(587, make_ship_type(587, 25, "Rifter"));
    sde.types.insert(670, make_ship_type(670, 29, "Capsule"));
    let ctx_sde = ServerContext::with_sde_and_repository(
        std::sync::Arc::new(sde),
        std::sync::Arc::clone(&base_ctx.db),
    );
    let custom_dps = thunderchild_guard_dps_vector(&ctx_sde);
    assert!((custom_dps.em - 200.0).abs() < 1e-6);
    assert!((custom_dps.explosive - 100.0).abs() < 1e-6);
    assert!((custom_dps.kinetic - 50.0).abs() < 1e-6);
    assert!((custom_dps.thermal - 20.0).abs() < 1e-6);

    // Fallback DPS when guard type is missing from SDE
    let mut empty_sde = (*base_ctx.sde).clone();
    empty_sde.types.remove(&THUNDERCHILD_GUARD_TYPE_ID);
    let ctx_no_guard = ServerContext::with_sde_and_repository(
        std::sync::Arc::new(empty_sde),
        std::sync::Arc::clone(&base_ctx.db),
    );
    let fallback_dps = thunderchild_guard_dps_vector(&ctx_no_guard);
    assert!((fallback_dps.em - 570.0).abs() < 1e-6);
    assert!((fallback_dps.kinetic - 570.0).abs() < 1e-6);

    // 2) Setup insurgency in sys_id at stage 3 and test step_ice_heist_system auto-spawn + ball properties
    let sys_id = 30_045_330_u32;
    let camp_id = 77_002_i64;
    init_insurgency_campaign(&ctx_sde, camp_id, FACTION_ANGEL_CARTEL, 500_003, sys_id, vec![sys_id]);
    add_system_corruption(&ctx_sde, camp_id, sys_id, 500);

    step_ice_heist_system(&ctx_sde, sys_id).await;
    let inst_id = read_insurgency_runtime(&ctx_sde, |rt| {
        rt.ice_heist_instances
            .iter()
            .find(|h| h.solar_system_id == sys_id && !h.completed)
            .map(|h| h.instance_id)
    })
    .expect("step_ice_heist_system should auto-spawn instance");

    // Second ensure_ice_heist_balls_spawned returns false (idempotent)
    assert!(!ensure_ice_heist_balls_spawned(&ctx_sde, sys_id, inst_id).await);

    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let guard_id = 1_896_000_000 + inst_id * 10 + 2;
    let sys = ctx_sde.systems.get_or_spawn(sys_id);
    assert_eq!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().map(|(_, b)| b.max_velocity),
        Some(2_500.0)
    );
    assert!(
        sys.get_combat_state(tanker_id)
            .await
            .unwrap()
            .is_some_and(|c| c.is_web_immune && c.warp_core_strength == 100)
    );

    // 3) Spawn ships to test steering + Thunderchild guard engagement:
    // - Pirate Rifter near guard (<25km) -> takes damage
    // - Pirate Rifter near tanker (<15km) & Empire Rifter slightly farther (<15km) -> pirate closer, steers to PIRATE_ZONE
    // - Pirate Capsule near guard -> NOT attacked
    // - Empire Rifter near guard -> NOT attacked
    // - Out-of-range Pirate Rifter (>25km from guard) -> NOT attacked
    let pirate_close_guard_ship = 9_100_001_i64;
    let pirate_capsule_ship = 9_100_002_i64;
    let empire_escort_ship = 9_100_003_i64;
    let pirate_far_ship = 9_100_004_i64;
    let docked_empire_ship = 9_100_005_i64;
    let offgrid_empire_ship = 9_100_006_i64;

    let guard_pos = ICE_HEIST_REFINERY_POS + DVec3::new(0.0, 0.0, 40_000.0);
    ctx_sde.db.with_mut_snapshot(|snap| {
        let base_pilot = snap.characters.values().next().unwrap().clone();
        let make_pilot = |cid: i64, sid: i64, wf: i64, station: Option<i64>| {
            let mut ch = base_pilot.clone();
            ch.character_id = cid;
            ch.name = format!("Pilot-{cid}");
            ch.corporation_id = 1_000_180;
            ch.solar_system_id = sys_id;
            ch.station_id = station;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.war_faction_id = Some(wf);
            ch.loyalty_points.clear();
            ch
        };
        snap.characters.insert(91_001, make_pilot(91_001, pirate_close_guard_ship, FACTION_ANGEL_CARTEL, None));
        snap.characters.insert(91_002, make_pilot(91_002, pirate_capsule_ship, FACTION_GURISTAS, None));
        snap.characters.insert(91_003, make_pilot(91_003, empire_escort_ship, 500_003, None));
        snap.characters.insert(91_004, make_pilot(91_004, pirate_far_ship, FACTION_ANGEL_CARTEL, None));
        snap.characters.insert(91_005, make_pilot(91_005, docked_empire_ship, 500_003, Some(60_003_760)));
        snap.characters.insert(91_006, make_pilot(91_006, offgrid_empire_ship, 500_003, None));
    });

    let spawn_ship = |sid: i64, tid: u32, pos: DVec3, hp: f64| {
        let mut cs = CombatState::default();
        cs.shield_hp = hp;
        cs.max_shield_hp = hp;
        cs.armor_hp = hp;
        cs.max_armor_hp = hp;
        cs.structure_hp = hp;
        cs.max_structure_hp = hp;
        let ball = make_site_npc_ball(sid, tid, pos);
        (ball, cs)
    };
    let (b1, c1) = spawn_ship(pirate_close_guard_ship, 587, guard_pos + DVec3::new(1_000.0, 0.0, 0.0), 5_000.0);
    let (b2, c2) = spawn_ship(pirate_capsule_ship, 670, guard_pos + DVec3::new(500.0, 0.0, 0.0), 500.0);
    let (b3, c3) = spawn_ship(empire_escort_ship, 587, ICE_HEIST_REFINERY_POS + DVec3::new(10_000.0, 0.0, 0.0), 5_000.0);
    let (b4, c4) = spawn_ship(pirate_far_ship, 587, guard_pos + DVec3::new(30_000.0, 0.0, 0.0), 5_000.0);
    let (b5, c5) = spawn_ship(docked_empire_ship, 587, ICE_HEIST_REFINERY_POS, 5_000.0);
    let (b6, c6) = spawn_ship(offgrid_empire_ship, 587, ICE_HEIST_REFINERY_POS + DVec3::new(300_000.0, 0.0, 0.0), 5_000.0);
    for (b, c) in [(b1, c1), (b2, c2), (b3, c3), (b4, c4), (b5, c5), (b6, c6)] {
        let _ = sys.spawn_ball_with_combat(b, c).await;
    }

    step_ice_heist_system(&ctx_sde, sys_id).await;

    // Pirate close to guard took damage; capsule, empire ship, and far pirate ship did NOT
    assert!(sys.get_combat_state(pirate_close_guard_ship).await.unwrap().unwrap().shield_hp < 5_000.0);
    assert!((sys.get_combat_state(pirate_capsule_ship).await.unwrap().unwrap().shield_hp - 500.0).abs() < 1e-6);
    assert!((sys.get_combat_state(empire_escort_ship).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6);
    assert!((sys.get_combat_state(pirate_far_ship).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6);
    // Tanker steered toward empire zone (since empire_escort_ship is within 15km and pirate_close_guard_ship is 40km away)
    assert!(matches!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().unwrap().1.mode,
        eve_destiny::MovementMode::GotoDirection { .. }
    ));

    // Move pirate ship closer to tanker than empire ship -> steers to pirate zone
    let _ = sys.set_ball_position(pirate_far_ship, ICE_HEIST_REFINERY_POS + DVec3::new(1_000.0, 0.0, 0.0)).await;
    step_ice_heist_system(&ctx_sde, sys_id).await;
    assert!(matches!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().unwrap().1.mode,
        eve_destiny::MovementMode::GotoDirection { .. }
    ));

    // Move ships away from tanker -> tanker stops
    let _ = sys.set_ball_position(empire_escort_ship, ICE_HEIST_REFINERY_POS + DVec3::new(25_000.0, 0.0, 0.0)).await;
    let _ = sys.set_ball_position(pirate_far_ship, ICE_HEIST_REFINERY_POS + DVec3::new(25_000.0, 0.0, 0.0)).await;
    step_ice_heist_system(&ctx_sde, sys_id).await;
    assert_eq!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().unwrap().1.mode,
        eve_destiny::MovementMode::Stop
    );

    // 4) Tanker inside ICE_HEIST_PIRATE_ZONE scores pirate_tankers_saved = 1
    let _ = sys.set_ball_position(tanker_id, ICE_HEIST_PIRATE_ZONE + DVec3::new(100.0, 0.0, 0.0)).await;
    step_ice_heist_system(&ctx_sde, sys_id).await;
    let pirate_cnt = read_insurgency_runtime(&ctx_sde, |rt| {
        rt.ice_heist_instances.iter().find(|h| h.instance_id == inst_id).unwrap().pirate_tankers_saved
    });
    assert_eq!(pirate_cnt, 1);

    // 5) Tanker inside ICE_HEIST_EMPIRE_ZONE scores empire_tankers_saved = 1..=9, then 10th wins LP ONLY for on-grid undocked empire pilot 91_003
    for expected in 1..10 {
        let _ = sys.set_ball_position(tanker_id, ICE_HEIST_EMPIRE_ZONE + DVec3::new(100.0, 0.0, 0.0)).await;
        step_ice_heist_system(&ctx_sde, sys_id).await;
        let emp_cnt = read_insurgency_runtime(&ctx_sde, |rt| {
            rt.ice_heist_instances.iter().find(|h| h.instance_id == inst_id).unwrap().empire_tankers_saved
        });
        assert_eq!(emp_cnt, expected);
    }

    let win_rep = escort_ice_heist_tanker(&ctx_sde, 91_003, inst_id, false).await.unwrap();
    assert_eq!(win_rep.dict_get("empireTankersSaved"), Some(&PyRep::Integer(10)));
    assert_eq!(win_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(win_rep.dict_get("lpAwarded"), Some(&PyRep::Integer(ICE_HEIST_WIN_LP)));

    // Tanker & guard removed after win
    assert!(sys.get_ball_snapshot(tanker_id).await.unwrap().is_none());
    assert!(sys.get_ball_snapshot(guard_id).await.unwrap().is_none());

    // LP checks: 91_003 (on-grid undocked empire) got 150k LP; 91_005 (docked), 91_006 (off-grid >250km), 91_001 (pirate) did NOT
    ctx_sde.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&91_003].loyalty_points.get(&1_000_179).copied(), Some(ICE_HEIST_WIN_LP));
        assert!(snap.characters[&91_005].loyalty_points.is_empty());
        assert!(snap.characters[&91_006].loyalty_points.is_empty());
        assert!(snap.characters[&91_001].loyalty_points.is_empty());
    });
}
