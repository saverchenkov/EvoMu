//! Unit tests for `space_nav_travel` (structure tethering, jump drive planning/fuel/fatigue, jump timers, destiny movement commands, and cloak/MJFG sync).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::types::UpwellStructureRecord;
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
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

fn put_item(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
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
            ..Default::default()
        },
    );
    item_id
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(char_id: i64, corp_id: i64, sys_id: Option<u32>, ship_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(740);
    s.user_id = Some(50);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

fn bound_sys(sys_id: u32) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 740,
        user_id: 50,
        character_id: 96_201,
        service: "beyonce".to_string(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    }
}

fn make_ship_ball(ship_id: i64, type_id: u32, _corp_id: i64, pos: DVec3) -> Ball {
    Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::new(50.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn sync_tether_repairs_ship_and_breaks_on_pvp_or_scram_or_range() {
    let mut ctx = test_ctx();
    let sys_id = 30000142;
    let corp_id = 98_333_001;
    let char_id = 96_201;
    let ship_id = 78_201;
    let struct_id = 1_040_333_001;
    let struct_tid = 35832;

    if let Some(tid) = attr_id_by_name(&ctx, "tetheringRange") {
        clone_type_with(&mut ctx.sde, struct_tid, struct_tid, 1657, 65, 800_000.0, 0.0, &[(tid, 15_000.0)]);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 588, char_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: struct_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Astrahus".to_string(),
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                position: [0.0, 0.0, 0.0],
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let ball = make_ship_ball(ship_id, 588, corp_id, DVec3::new(3_000.0, 0.0, 0.0));
    let _ = sys_h.spawn_ball(ball).await;
    if let Ok(Some(mut cs)) = sys_h.get_combat_state(ship_id).await {
        cs.shield_hp = 200.0;
        cs.max_shield_hp = 1000.0;
        cs.armor_hp = 200.0;
        cs.max_armor_hp = 1000.0;
        cs.structure_hp = 200.0;
        cs.max_structure_hp = 1000.0;
        cs.capacitor = 100.0;
        cs.max_capacitor = 400.0;
        let _ = sys_h.set_combat_state(ship_id, cs).await;
    }

    // 1. Eligible tether heals 25% of each HP pool
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let healed = sys_h.get_combat_state(ship_id).await.expect("snap").expect("cs");
    assert!(healed.is_tethered);
    assert!((healed.shield_hp - 450.0).abs() < 1e-3);
    assert!((healed.armor_hp - 450.0).abs() < 1e-3);
    assert!((healed.structure_hp - 450.0).abs() < 1e-3);
    assert!((healed.capacitor - 200.0).abs() < 1e-3);

    // 2. Active jump cooldown timer breaks tether
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = now_u + 45;
    });
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let unteth = sys_h.get_combat_state(ship_id).await.expect("snap").expect("cs");
    assert!(!unteth.is_tethered);

    // Clear jump timer, move out of range -> untethered
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().jump_cooldown_until_unix = 0;
    });
    sys_h.try_remove_ball(ship_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 588, corp_id, DVec3::new(500_000.0, 0.0, 0.0)),
            healed,
        )
        .await;
    sync_tether(&ctx, char_id, sys_id, ship_id).await;
    let unteth2 = sys_h.get_combat_state(ship_id).await.expect("snap").expect("cs");
    assert!(!unteth2.is_tethered);
}

#[test]
fn jump_drive_planning_fuel_fatigue_and_timers() {
    let mut ctx = test_ctx();
    let mut low_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    low_systems.sort_unstable();
    let cur_sys = low_systems[0];
    let dst_sys = low_systems
        .iter()
        .copied()
        .find(|&sid| sid != cur_sys && distance_ly(&ctx, cur_sys, sid) > 0.2 && distance_ly(&ctx, cur_sys, sid) <= 6.0)
        .expect("nearby lowsec system");
    let high_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status >= HIGH_SEC_THRESHOLD && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .expect("highsec system");

    let iso_tid = 17887;
    let blops_tid = 99_801;
    let jf_tid = 99_802;
    let carrier_tid = 99_803;
    let can_jump = attr_id_by_name(&ctx, "canJump").unwrap_or(866);
    let jd_range = attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
    let jd_amount = attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
    let jd_type = attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
    let no_high = attr_id_by_name(&ctx, "disallowInHighSec").unwrap_or(1228);

    clone_type_with(
        &mut ctx.sde,
        22474,
        blops_tid,
        898,
        6,
        15_000.0,
        500.0,
        &[
            (can_jump, 1.0),
            (jd_range, 8.0),
            (jd_amount, 20.0),
            (jd_type, f64::from(iso_tid)),
            (no_high, 1.0),
        ],
    );
    clone_type_with(
        &mut ctx.sde,
        28844,
        jf_tid,
        902,
        6,
        500_000.0,
        50_000.0,
        &[(can_jump, 1.0), (jd_range, 10.0), (jd_amount, 50.0), (jd_type, f64::from(iso_tid))],
    );
    clone_type_with(
        &mut ctx.sde,
        23757,
        carrier_tid,
        547,
        6,
        1_000_000.0,
        10_000.0,
        &[(can_jump, 1.0), (jd_range, 7.0), (jd_amount, 100.0), (jd_type, f64::from(iso_tid))],
    );
    clone_type_with(&mut ctx.sde, 34, iso_tid, 423, 4, 0.05, 0.0, &[]);

    assert!((ship_fatigue_reduction_bonus(&ctx, blops_tid) - 0.75).abs() < 1e-6);
    assert!((ship_fatigue_reduction_bonus(&ctx, jf_tid) - 0.90).abs() < 1e-6);
    assert!((ship_fatigue_reduction_bonus(&ctx, carrier_tid) - 0.0).abs() < 1e-6);

    let char_id = 96_301;
    let ship_id = 78_301;

    // Wormhole & Zarzakh & HighSec & NoJumpDrive checks
    assert_eq!(plan_jump_drive(&ctx, char_id, blops_tid, cur_sys, 31000001).unwrap_err().code, "JumpDriveForbiddenInWormholeSpace");
    assert_eq!(plan_jump_drive(&ctx, char_id, blops_tid, cur_sys, 30100000).unwrap_err().code, "JumpDriveForbiddenInZarzakh");
    assert_eq!(plan_jump_drive(&ctx, char_id, 588, cur_sys, dst_sys).unwrap_err().code, "ShipHasNoJumpDrive");
    assert_eq!(plan_jump_drive(&ctx, char_id, blops_tid, cur_sys, high_sys).unwrap_err().code, "JumpDriveHighSecForbidden");

    // Valid jump plan & fuel consumption
    let plan = plan_jump_drive(&ctx, char_id, blops_tid, cur_sys, dst_sys).expect("plan ok");
    assert!(plan.fuel_units > 0);
    assert!((plan.fatigue_multiplier - 0.25).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, blops_tid, char_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(
            snap,
            78_302,
            iso_tid,
            char_id,
            ship_id,
            InventoryFlag::SpecializedFuelBay,
            plan.fuel_units + 50,
            false,
        );
    });
    consume_jump_fuel(&ctx, ship_id, &plan).expect("fuel consumed");
    let rem_fuel: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == iso_tid)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(rem_fuel, 50);

    // Fatigue & timers RPC
    assert!(check_jump_activation(&ctx, char_id).is_ok());
    let (cd, fat) = apply_jump_fatigue(&ctx, char_id, plan.distance_ly);
    assert!(cd > 0.0 && fat >= cd);
    assert!(check_jump_activation(&ctx, char_id).is_err());

    let sess = make_session(char_id, 98000001, Some(cur_sys), Some(ship_id));
    let timers = handle_get_jump_timers(&ctx, &sess, &[PyRep::Integer(char_id)]).expect("timers ok");
    let PyRep::KeyVal(kv) = timers else {
        panic!("expected KeyVal");
    };
    assert!(kv.contains_key("jumpActivation"));
    assert!(kv.contains_key("jumpFatigue"));
    assert!(kv.contains_key("lastUpdated"));
}

#[tokio::test]
async fn destiny_movement_commands_update_ball_state_and_resolve_targets() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let char_id = 96_401;
    let ship_id = 78_401;
    let target_id = 78_402;
    let bm_id = 5_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "SafeSpot".into(),
                solar_system_id: sys_id,
                x: 12_000.0,
                y: -4_000.0,
                z: 8_000.0,
                item_id: None,
                memo: "SafeSpot".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(make_ship_ball(ship_id, 588, 98000001, DVec3::ZERO))
        .await;
    let _ = sys_h
        .spawn_ball(make_ship_ball(
            target_id,
            588,
            98000001,
            DVec3::new(30_000.0, 0.0, 0.0),
        ))
        .await;

    let bm_pos = resolve_system_target_position(&ctx, sys_id, char_id, bm_id)
        .await
        .expect("bookmark position resolved");
    assert!((bm_pos.x - 12_000.0).abs() < 1e-6);

    let sess = make_session(char_id, 98000001, Some(sys_id), Some(ship_id));
    let bound = bound_sys(sys_id);

    handle_cmd_goto_point(&ctx, &sess, Some(&bound), &[PyRep::Real(1000.0), PyRep::Real(2000.0), PyRep::Real(3000.0)])
        .await
        .expect("goto point ok");
    handle_cmd_goto_direction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)])
        .await
        .expect("goto direction ok");
    handle_cmd_set_speed_fraction(&ctx, &sess, Some(&bound), &[PyRep::Real(0.75)])
        .await
        .expect("set speed ok");
    handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdOrbit", &[PyRep::Integer(target_id), PyRep::Real(2500.0)])
        .await
        .expect("orbit ok");
    handle_cmd_orbit_or_follow(&ctx, &sess, Some(&bound), "CmdFollowBall", &[PyRep::Integer(target_id), PyRep::Real(500.0)])
        .await
        .expect("follow ok");
    handle_cmd_align_to(&ctx, &sess, Some(&bound), &[PyRep::Integer(target_id)], &HashMap::new())
        .await
        .expect("align ok");
    handle_cmd_stop(&ctx, &sess, Some(&bound))
        .await
        .expect("stop ok");

    let (_, final_ball) = sys_h.get_ball_snapshot(ship_id).await.expect("snap").expect("ball");
    assert!(matches!(final_ball.mode, MovementMode::Stop));
}

#[tokio::test]
async fn sync_ship_cloak_and_mjfg_from_fitting_updates_stealth_and_spool() {
    let mut ctx = test_ctx();
    let sys_id = 30000142;
    let char_id = 96_501;
    let ship_id = 78_501;
    let covert_cloak_tid = 11_578;
    let mjfg_tid = 83_485;

    clone_type_with(
        &mut ctx.sde,
        11_578,
        covert_cloak_tid,
        330,
        7,
        5.0,
        0.0,
        &[(20, 1.0), (1034, 1.0)], // covert ops cloak
    );
    clone_type_with(
        &mut ctx.sde,
        37_792,
        mjfg_tid,
        1533,
        7,
        10.0,
        0.0,
        &[(2066, 150.0)], // signatureRadiusBonus = +150% (2.5x)
    );

    let (cloak_item_id, mjfg_item_id) = ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, ship_id, 588, char_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
        let c_id = put_item(
            snap,
            78_502,
            covert_cloak_tid,
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            1,
            true,
        );
        let m_id = put_item(
            snap,
            78_503,
            mjfg_tid,
            char_id,
            ship_id,
            InventoryFlag::HiSlot1,
            1,
            true,
        );
        snap.module_active_states.insert(c_id);
        snap.module_active_states.insert(m_id);
        (c_id, m_id)
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(make_ship_ball(ship_id, 588, 98000001, DVec3::ZERO))
        .await;

    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, sys_id, ship_id).await;
    let cs = sys_h
        .get_combat_state(ship_id)
        .await
        .expect("ok")
        .expect("state");
    assert!(cs.is_cloaked);
    assert!(cs.mjfg_spool_remaining_s > 1.0);
    assert!(cs.signature_radius > 1.0);

    // Abyssal force-decloak clears active cloak state
    let abyss_sys = 32_000_001;
    let abyss_h = ctx.systems.get_or_spawn(abyss_sys);
    let _ = abyss_h
        .spawn_ball(make_ship_ball(ship_id, 588, 98000001, DVec3::ZERO))
        .await;
    sync_ship_cloak_and_mjfg_from_fitting(&ctx, char_id, abyss_sys, ship_id).await;
    let still_active = ctx.db.with_snapshot(|snap| {
        snap.module_active_states.contains(&cloak_item_id)
    });
    assert!(!still_active);
    let _ = mjfg_item_id;
}

#[tokio::test]
async fn cmd_stop_omits_duplicate_do_destiny_updates_on_live_tcp_session_only() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 96_801;
    let ship_id = 78_801;

    ctx.db.with_mut_snapshot(|snap| {
        put_item(
            snap,
            ship_id,
            587,
            char_id,
            i64::from(sys_id),
            InventoryFlag::None,
            1,
            true,
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(make_ship_ball(ship_id, 587, 98_000_001, DVec3::ZERO))
        .await;
    let _ = sys_h.drain_observer_updates(ship_id).await;
    let _ = ctx.drain_notifications_for_char(char_id);

    let bound = bound_sys(sys_id);

    // 1. Live TCP session (`!session.address.is_empty()`): zero queued `DoDestinyUpdates` notifications,
    //    leaving the single authoritative `Stop` event in `observer_events` for `macho_listener`.
    let mut tcp_sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id));
    tcp_sess.address = "127.0.0.1:54123".to_string();
    let _ = sys_h
        .set_movement_mode(
            ship_id,
            MovementMode::GotoDirection {
                direction: DVec3::X,
            },
        )
        .await;
    let _ = sys_h.drain_observer_updates(ship_id).await;

    handle_cmd_stop(&ctx, &tcp_sess, Some(&bound))
        .await
        .expect("CmdStop on TCP session");
    let tcp_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !tcp_notifs.iter().any(|n| n.event == "DoDestinyUpdates"),
        "live TCP session must not emit duplicate DoDestinyUpdates alongside DoDestinyUpdate: {tcp_notifs:?}"
    );
    let (_, obs_evs, _) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    assert!(obs_evs
        .iter()
        .any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == ship_id)));

    // 2. Direct in-memory RPC session (`session.address.is_empty()`): keeps `DoDestinyUpdates` for headless tests.
    let direct_sess = make_session(char_id, 98_000_001, Some(sys_id), Some(ship_id));
    handle_cmd_stop(&ctx, &direct_sess, Some(&bound))
        .await
        .expect("CmdStop on direct session");
    let direct_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(direct_notifs.iter().any(|n| n.event == "DoDestinyUpdates"));
}
