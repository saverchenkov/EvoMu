use crate::router::ServerContext;
use crate::services::pve_abyssal_lifecycle::collapse_abyssal_pocket_and_pod_pilot_at;
use crate::services::pve_special_sites::{read_abyssal_state, with_abyssal_state};
use crate::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace;
use crate::services::pvp_filament_mgr::{
    handle as handle_pvp_filament_mgr, load_pvp_store, PvpEventSchedule,
};
use crate::services::tourney_service::{
    handle as handle_tourney_mgr, load_store as load_tourney_store,
    save_store as save_tourney_store, TourneyMatch, TourneyMatchState, TourneyPilot,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn make_ship_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_combat_state(hp: f64) -> CombatState {
    CombatState {
        shield_hp: hp,
        max_shield_hp: hp,
        armor_hp: hp,
        max_armor_hp: hp,
        structure_hp: hp,
        max_structure_hp: hp,
        structure_resonance: [1.0; 4],
        ..Default::default()
    }
}

#[tokio::test]
async fn r7_tc_p1_445_proving_grounds_ticket_id_collision_and_partial_stack_leave() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;
    let now = ctx.now_unix() as i64;
    let ship_ty = ctx.sde.types.get(&587).unwrap();
    let filament_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            (t.group_id == 4050 || t.group_id == 4743)
                && !t.name.starts_with("Expired")
                && t.name.contains("1v1")
                && {
                    let spec = crate::services::pvp_filament_mgr::get_pvp_filament_match_spec(
                        &ctx.sde,
                        t.type_id,
                    );
                    spec.fleet_size == 1
                        && spec.number_of_fleets == 2
                        && spec
                            .ship_restrictions_list_id
                            .is_none_or(|lid| ctx.sde.type_lists.get(&lid).is_some_and(|tl| tl.contains(ship_ty)))
                }
        })
        .map(|t| t.type_id)
        .unwrap();

    let char_a = 91_445_001_i64;
    let char_b = 91_445_002_i64;
    let ship_a = 92_445_001_i64;
    let ship_b = 92_445_002_i64;
    let item_a = 93_445_001_i64;
    let item_b = 93_445_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_pvp_store(snap);
        store.next_ticket_id = item_b - 1;
        store.events = vec![PvpEventSchedule {
            event_id: 777,
            schedule_id: 777,
            event_name: "Test Arena".into(),
            description: "Test Arena".into(),
            match_type_id: 777,
            start_unix: now - 3_600,
            end_unix: now + 3_600,
            type_ids: vec![i64::from(filament_type_id)],
        }];
        store.seeded_default_events = true;
        crate::services::pvp_filament_mgr::save_pvp_store(snap, &store);

        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, sid, iid) in [(char_a, ship_a, item_a), (char_b, ship_b, item_b)] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.name = format!("Pilot-{cid}");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.home_station_id = 60_003_760;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: filament_type_id,
                    owner_id: cid,
                    location_id: sid,
                    flag: InventoryFlag::Cargo,
                    quantity: 2,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess_a = SessionState {
        session_id: 4451,
        user_id: Some(11),
        character_id: Some(char_a),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let mut sess_b = SessionState {
        session_id: 4452,
        user_id: Some(12),
        character_id: Some(char_b),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_b),
        ..Default::default()
    };
    let kwargs = HashMap::new();

    let res_a = handle_pvp_filament_mgr(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(item_a), PyRep::Integer(i64::from(filament_type_id))],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(res_a.is_ok(), "pilot A queue join with partial stack must succeed");

    let ticket_a = ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_a].quantity, 1, "joining queue consumes 1 filament from stack");
        let store = load_pvp_store(snap);
        let tid = store.queue_tickets[&char_a];
        assert!(store.queue_refund_targets.contains_key(&tid));
        tid
    });

    let leave_a = handle_pvp_filament_mgr(
        &ctx,
        &mut sess_a,
        "pvpFilamentMgr",
        "LeavePVPQueue",
        &[PyRep::Integer(item_a)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(leave_a.is_ok(), "leaving queue from partial stack must succeed");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_a].quantity, 2, "leaving queue refunds 1 filament back to remaining stack");
        let store = load_pvp_store(snap);
        assert!(!store.queue.contains_key(&char_a));
        assert!(!store.queue_refund_targets.contains_key(&ticket_a));
    });

    let res_b = handle_pvp_filament_mgr(
        &ctx,
        &mut sess_b,
        "pvpFilamentMgr",
        "JoinPVPQueue",
        &[PyRep::Integer(item_b), PyRep::Integer(i64::from(filament_type_id))],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(res_b.is_ok(), "pilot B joining with item_b == previous ticket_a must succeed without collision");
}

#[tokio::test]
async fn r7_tc_p1_446_tournament_ban_phase_and_prematch_referee_gating() {
    let ctx = ServerContext::new();
    let sys_id = 30_000_142_u32;
    let red_capt = 91_446_001_i64;
    let blue_capt = 91_446_002_i64;
    let outsider = 91_446_003_i64;
    let ref_cid = 91_446_004_i64;
    let match_id = 88_446_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_tourney_store(snap);
        let mut m = TourneyMatch::new(match_id, 1, i64::from(sys_id), 100, 600, 0, 0);
        m.state = TourneyMatchState::Setup;
        m.autoban_qty = 2;
        m.red_fleet_boss_id = Some(red_capt);
        m.blue_fleet_boss_id = Some(blue_capt);
        m.referees = vec![ref_cid];
        m.red_players = vec![TourneyPilot {
            character_id: red_capt,
            ship_type_id: 587,
            ship_id: 92_446_001,
            points: 4,
        }];
        m.blue_players = vec![TourneyPilot {
            character_id: blue_capt,
            ship_type_id: 587,
            ship_id: 92_446_002,
            points: 4,
        }];
        store.matches.insert(match_id, m);
        store.active_match_id = Some(match_id);
        save_tourney_store(snap, &store, 1_000);
    });

    let mut sess_out = SessionState {
        session_id: 4461,
        user_id: Some(21),
        character_id: Some(outsider),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let mut sess_ref = SessionState {
        session_id: 4462,
        user_id: Some(22),
        character_id: Some(ref_cid),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let mut sess_red = SessionState {
        session_id: 4463,
        user_id: Some(23),
        character_id: Some(red_capt),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let mut sess_blue = SessionState {
        session_id: 4464,
        user_id: Some(24),
        character_id: Some(blue_capt),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let kwargs = HashMap::new();

    assert!(handle_tourney_mgr(&ctx, &mut sess_out, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(match_id)], &kwargs).await.unwrap().is_err());
    assert!(handle_tourney_mgr(&ctx, &mut sess_out, "tourneyMgr", "GetTeamMembers", &[PyRep::Integer(match_id)], &kwargs).await.unwrap().is_err());
    assert!(handle_tourney_mgr(&ctx, &mut sess_ref, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(match_id)], &kwargs).await.unwrap().is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_tourney_store(snap);
        store.matches.get_mut(&match_id).unwrap().state = TourneyMatchState::InProgress;
        save_tourney_store(snap, &store, 1_000);
    });
    let late_ban = handle_tourney_mgr(
        &ctx,
        &mut sess_red,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(late_ban.is_err(), "BanShipType outside Setup/Banning phase must fail");
    assert!(handle_tourney_mgr(&ctx, &mut sess_out, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(match_id)], &kwargs).await.unwrap().is_ok(), "Public details open in InProgress");

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_tourney_store(snap);
        store.matches.get_mut(&match_id).unwrap().state = TourneyMatchState::Banning;
        save_tourney_store(snap, &store, 1_001);
    });

    assert!(handle_tourney_mgr(&ctx, &mut sess_red, "tourneyMgr", "BanShipType", &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(587)], &kwargs).await.unwrap().is_ok());
    assert!(handle_tourney_mgr(&ctx, &mut sess_red, "tourneyMgr", "BanShipType", &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(588)], &kwargs).await.unwrap().is_ok());
    assert!(handle_tourney_mgr(&ctx, &mut sess_red, "tourneyMgr", "BanShipType", &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(589)], &kwargs).await.unwrap().is_err(), "3rd ban exceeds autoban_qty=2");
    assert!(handle_tourney_mgr(&ctx, &mut sess_blue, "tourneyMgr", "BanShipType", &[PyRep::Integer(match_id), PyRep::Integer(1), PyRep::Integer(590)], &kwargs).await.unwrap().is_ok(), "Blue has separate ban allowance");

    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_tourney_store(snap);
        store.matches.get_mut(&match_id).unwrap().bans_finalized = true;
        save_tourney_store(snap, &store, 1_002);
    });
    assert!(handle_tourney_mgr(&ctx, &mut sess_blue, "tourneyMgr", "BanShipType", &[PyRep::Integer(match_id), PyRep::Integer(1), PyRep::Integer(591)], &kwargs).await.unwrap().is_err(), "Cannot ban after bans_finalized");
}

#[tokio::test]
async fn r7_tc_p1_447_abyssal_reentry_restores_saved_attrs_and_collapse_pods_all_coop() {
    let ctx = ServerContext::new();
    let origin_sys = 30_000_142_u32;
    let ship_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.published && t.category_id == 6 && t.group_id == 26)
        .map(|t| t.type_id)
        .unwrap_or(620);
    let filament_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.published && t.name.contains("Electrical") && t.name.contains("Filament"))
        .map(|t| t.type_id)
        .unwrap_or(47_762);

    let c1 = 91_447_001_i64;
    let c2 = 91_447_002_i64;
    let s1 = 92_447_001_i64;
    let s2 = 92_447_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters.values().next().unwrap().clone();
        for (cid, sid) in [(c1, s1), (c2, s2)] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.name = format!("Abyss-{cid}");
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.home_station_id = 60_003_760;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: ship_tid,
                    owner_id: cid,
                    location_id: i64::from(origin_sys),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess1 = SessionState {
        session_id: 4471,
        user_id: Some(31),
        character_id: Some(c1),
        solar_system_id: Some(origin_sys),
        ship_id: Some(s1),
        ..Default::default()
    };

    enter_abyssal_pocket_with_trace(&ctx, &mut sess1, c1, filament_tid, 1, origin_sys, None).await;
    let first_dyn = ctx.db.with_snapshot(|s| s.items[&s1].dynamic_attributes.clone());

    sess1.solar_system_id = Some(origin_sys);
    sess1.ship_id = Some(s1);
    enter_abyssal_pocket_with_trace(&ctx, &mut sess1, c1, filament_tid, 1, origin_sys, None).await;
    let (pid, abyss_sys) = read_abyssal_state(&ctx, |st| {
        let p = st.pockets.get(&c1).unwrap();
        (p.pocket_id, p.abyssal_solar_system_id)
    });
    let second_dyn = ctx.db.with_snapshot(|s| s.items[&s1].dynamic_attributes.clone());
    assert_eq!(first_dyn, second_dyn, "re-entry must not mutate baseline ship dynamic_attributes");

    with_abyssal_state(&ctx, |st| {
        let mut c2_session = st.pockets[&c1].clone();
        c2_session.character_id = c2;
        c2_session.ship_id = s2;
        st.pockets.insert(c2, c2_session);
    });
    let _ = pid;
    let sys_h = ctx.systems.get_or_spawn(abyss_sys);
    let _ = sys_h.spawn_ball_with_combat(make_ship_ball(s1, ship_tid, DVec3::ZERO), make_combat_state(1000.0)).await;
    let _ = sys_h.spawn_ball_with_combat(make_ship_ball(s2, ship_tid, DVec3::new(1000.0, 0.0, 0.0)), make_combat_state(1000.0)).await;

    collapse_abyssal_pocket_and_pod_pilot_at(&ctx, c1, s1, abyss_sys, 2_300);

    read_abyssal_state(&ctx, |st| {
        assert!(st.pockets[&c1].collapsed_destroyed);
        assert!(st.pockets[&c2].collapsed_destroyed, "co-op pilot must also be collapsed");
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&s1));
        assert!(!snap.items.contains_key(&s2), "co-op pilot ship must also be destroyed/podded");
    });
}
