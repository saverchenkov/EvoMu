#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use crate::services::insurgency_services::{add_system_corruption, init_insurgency_campaign};
use eve_db::{clock::VirtualServerClock, ServerClock};
use eve_destiny::{Ball, MovementMode};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_heist_empire_escort_lp_awards_and_ball_respawn() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(clock.clone());

    // thunderchild_guard_dps_vector fallback with empty SDE
    let empty_ctx = ServerContext::with_sde_and_repository(
        Arc::new(SdeRegistry::default()),
        ctx.db.clone(),
    );
    let fallback_dps = thunderchild_guard_dps_vector(&empty_ctx);
    assert_eq!(fallback_dps, DamageVector::new(570.0, 0.0, 570.0, 0.0));

    let sys_id = 30_045_330_u32;
    // step_ice_heist_system when corruption < 3 is a no-op
    step_ice_heist_system(&ctx, sys_id).await;
    assert!(read_insurgency_runtime(&ctx, |rt| rt.ice_heist_instances.is_empty()));

    let camp_id = 77_101_i64;
    init_insurgency_campaign(&ctx, camp_id, FACTION_ANGEL_CARTEL, 500_002, sys_id, vec![sys_id]);
    add_system_corruption(&ctx, camp_id, sys_id, 450);

    let inst_id = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap();
    // Already spawned -> ensure_ice_heist_balls_spawned returns false
    assert!(!ensure_ice_heist_balls_spawned(&ctx, sys_id, inst_id).await);

    // Remove tanker ball and verify ensure_ice_heist_balls_spawned respawns it and returns true
    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.remove_ball(tanker_id).await;
    assert!(ensure_ice_heist_balls_spawned(&ctx, sys_id, inst_id).await);

    // Seed 4 pilots in sys_id:
    // - 90_000_501: Empire (500_002) in space, on grid -> SHOULD receive 150,000 LP
    // - 90_000_502: Empire (500_002) docked -> NO LP
    // - 90_000_503: Empire (500_002) in space, >250km away -> NO LP
    // - 90_000_504: Pirate (Angel) in space, on grid -> NO LP on empire win
    let ship_on_grid = 1_897_000_001_i64;
    let ship_docked = 1_897_000_002_i64;
    let ship_far = 1_897_000_003_i64;
    let ship_pirate = 1_897_000_004_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).unwrap().clone();
        for (cid, sid, wf, docked) in [
            (90_000_501, ship_on_grid, 500_002, false),
            (90_000_502, ship_docked, 500_002, true),
            (90_000_503, ship_far, 500_002, false),
            (90_000_504, ship_pirate, FACTION_ANGEL_CARTEL, false),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
            ch.war_faction_id = Some(wf);
            ch.station_id = if docked { Some(60_003_760) } else { None };
            ch.structure_id = None;
            ch.loyalty_points.clear();
            snap.characters.insert(cid, ch);
        }
    });
    sys.spawn_ball(make_ship_ball(ship_on_grid, 587, ICE_HEIST_REFINERY_POS + DVec3::new(1_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    sys.spawn_ball(make_ship_ball(ship_far, 587, ICE_HEIST_REFINERY_POS + DVec3::new(500_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    sys.spawn_ball(make_ship_ball(ship_pirate, 587, ICE_HEIST_REFINERY_POS + DVec3::new(2_000.0, 0.0, 0.0)))
        .await
        .unwrap();

    // Escort 10 empire tankers -> wins LP for on-grid empire pilot
    for i in 1..10 {
        let r = escort_ice_heist_tanker(&ctx, 90_000_501, inst_id, false).await.unwrap();
        assert_eq!(r.dict_get("empireTankersSaved"), Some(&PyRep::Integer(i)));
        assert_eq!(r.dict_get("lpAwarded"), Some(&PyRep::Integer(0)));
    }
    let win_r = escort_ice_heist_tanker(&ctx, 90_000_501, inst_id, false).await.unwrap();
    assert_eq!(win_r.dict_get("empireTankersSaved"), Some(&PyRep::Integer(10)));
    assert_eq!(win_r.dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(win_r.dict_get("lpAwarded"), Some(&PyRep::Integer(ICE_HEIST_WIN_LP)));

    assert_eq!(inst_id, 800_001);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&90_000_501].loyalty_points.get(&1_000_182),
            Some(&ICE_HEIST_WIN_LP)
        );
        assert!(snap.characters[&90_000_502].loyalty_points.is_empty());
        assert!(snap.characters[&90_000_503].loyalty_points.is_empty());
        assert!(snap.characters[&90_000_504].loyalty_points.is_empty());
        assert_eq!(
            snap.insurgency_complex_ops
                .ice_heist_respawn_until
                .get(&sys_id)
                .copied(),
            Some(1_800_000_000 + ICE_HEIST_RESPAWN_COOLDOWN_SECS)
        );
    });

    // Immediate respawn is blocked by cooldown; advancing exactly ICE_HEIST_RESPAWN_COOLDOWN_SECS unlocks next spawn
    assert_eq!(
        spawn_ice_heist_checked(&ctx, sys_id).await.unwrap_err().code,
        "IceHeistRespawnCooldown"
    );
    clock.advance_secs(ICE_HEIST_RESPAWN_COOLDOWN_SECS);
    let inst2 = spawn_ice_heist_checked(&ctx, sys_id).await.unwrap();
    assert_eq!(inst2, 800_002);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ice_heist_step_guard_combat_and_tanker_steering() {
    let ctx = test_ctx();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_800_100_000));
    ctx.set_clock(clock.clone());
    let sys_id = 30_045_331_u32;
    let camp_id = 77_102_i64;
    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, sys_id, vec![sys_id]);
    add_system_corruption(&ctx, camp_id, sys_id, 450);

    // Let step_ice_heist_system auto-spawn the instance when none is active
    step_ice_heist_system(&ctx, sys_id).await;
    let inst_id = read_insurgency_runtime(&ctx, |rt| rt.ice_heist_instances[0].instance_id);
    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let guard_pos = ICE_HEIST_REFINERY_POS + DVec3::new(0.0, 0.0, 40_000.0);
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Guard engagement: fragile pirate ship (1 HP -> destroyed), sturdy pirate ship (damaged),
    //    capsule (670 -> ignored), empire ship (ignored)
    let fragile_pirate_ship = 1_897_001_001_i64;
    let capsule_pirate_ship = 1_897_001_002_i64;
    let empire_ship = 1_897_001_003_i64;
    let pirate_escort_ship = 1_897_001_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&90_000_001).unwrap().clone();
        for (cid, sid, wf) in [
            (90_000_601, fragile_pirate_ship, FACTION_GURISTAS),
            (90_000_602, capsule_pirate_ship, FACTION_GURISTAS),
            (90_000_603, empire_ship, 500_001),
            (90_000_604, pirate_escort_ship, FACTION_ANGEL_CARTEL),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
            ch.war_faction_id = Some(wf);
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);
        }
    });

    let mut fragile_cs = CombatState::default();
    fragile_cs.structure_hp = 1.0;
    fragile_cs.max_structure_hp = 1.0;
    sys.spawn_ball_with_combat(
        make_ship_ball(fragile_pirate_ship, 587, guard_pos + DVec3::new(1_000.0, 0.0, 0.0)),
        fragile_cs,
    )
    .await
    .unwrap();
    sys.spawn_ball_with_combat(
        make_ship_ball(capsule_pirate_ship, 670, guard_pos + DVec3::new(500.0, 0.0, 0.0)),
        CombatState::default(),
    )
    .await
    .unwrap();
    sys.spawn_ball_with_combat(
        make_ship_ball(empire_ship, 587, guard_pos + DVec3::new(800.0, 0.0, 0.0)),
        CombatState::default(),
    )
    .await
    .unwrap();

    // Place pirate_escort_ship within 15km of tanker (at refinery) -> steers toward pirate zone
    sys.spawn_ball_with_combat(
        make_ship_ball(pirate_escort_ship, 587, ICE_HEIST_REFINERY_POS + DVec3::new(2_000.0, 0.0, 0.0)),
        CombatState::default(),
    )
    .await
    .unwrap();

    step_ice_heist_system(&ctx, sys_id).await;
    // Fragile pirate ship was destroyed by Thunderchild guard, capsule & empire ship remain
    assert!(sys.get_ball_snapshot(fragile_pirate_ship).await.unwrap().is_none());
    assert!(sys.get_ball_snapshot(capsule_pirate_ship).await.unwrap().is_some());
    assert!(sys.get_ball_snapshot(empire_ship).await.unwrap().is_some());
    assert!(matches!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().unwrap().1.mode,
        MovementMode::GotoDirection { .. }
    ));

    // 2. Closer empire escort ship -> steers toward empire zone
    let empire_escort_ship = 1_897_001_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&90_000_001).unwrap().clone();
        ch.character_id = 90_000_605;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = empire_escort_ship;
        ch.war_faction_id = Some(500_003);
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(90_000_605, ch);
    });
    sys.spawn_ball_with_combat(
        make_ship_ball(empire_escort_ship, 587, ICE_HEIST_REFINERY_POS + DVec3::new(100.0, 0.0, 0.0)),
        CombatState::default(),
    )
    .await
    .unwrap();
    step_ice_heist_system(&ctx, sys_id).await;
    assert!(matches!(
        sys.get_ball_snapshot(tanker_id).await.unwrap().unwrap().1.mode,
        MovementMode::GotoDirection { .. }
    ));

    // 3. Tanker in Empire zone -> scores empire_tankers_saved
    assert!(sys.set_ball_position(tanker_id, ICE_HEIST_EMPIRE_ZONE).await);
    step_ice_heist_system(&ctx, sys_id).await;
    // 4. Tanker in Pirate zone -> scores pirate_tankers_saved
    assert!(sys.set_ball_position(tanker_id, ICE_HEIST_PIRATE_ZONE).await);
    step_ice_heist_system(&ctx, sys_id).await;

    read_insurgency_runtime(&ctx, |rt| {
        let inst = rt.ice_heist_instances.iter().find(|h| h.instance_id == inst_id).unwrap();
        assert_eq!(inst.empire_tankers_saved, 1);
        assert_eq!(inst.pirate_tankers_saved, 1);
    });
}
