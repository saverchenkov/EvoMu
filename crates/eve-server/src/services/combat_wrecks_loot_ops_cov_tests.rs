//! Direct unit tests covering all remaining branches of `combat_wrecks_loot_ops`.

use super::super::combat_wrecks_loot_ops::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, DetailedKillmail, KillmailAttackerEntry, SafetyLevel,
};
use crate::services::inventory_fitting::fitting_containers_ships::{
    set_alsc_item_locked_snap, set_container_password_snap, set_container_unlocked_snap,
};
use eve_db::{FleetRecord, InventoryFlag, KillmailRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::sync::Arc;

#[tokio::test]
async fn loot_all_location_dock_acl_criminal_corp_fleet_locked_and_singleton_branches() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();

    let mut sde = (*ctx.sde).clone();
    sde.types
        .insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    let mut sec_cont = make_fighter_type(17_366, 340, 2, 100.0, vec![]);
    sec_cont.capacity = 500.0;
    sde.types.insert(17_366, sec_cont);
    sde.types
        .insert(34, make_fighter_type(34, 18, 4, 200.0, vec![])); // 200 m3 per unit
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.capacity = 100.0; // smaller than 200 m3 singleton
    }
    ctx.sde = Arc::new(sde);

    // 1. Docked in session (station or structure) -> CantDoThatWhileDocked
    let mut docked = session.clone();
    docked.station_id = Some(60_003_760);
    assert_eq!(
        handle_loot_all(&ctx, &mut docked, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );
    docked.station_id = None;
    docked.structure_id = Some(1_020_000_001);
    assert_eq!(
        handle_loot_all(&ctx, &mut docked, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );

    // 2. No active ship -> NoActiveShip
    let mut no_ship = session.clone();
    no_ship.ship_id = None;
    assert_eq!(
        handle_loot_all(&ctx, &mut no_ship, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // 3. Ship docked in DB while session undocked -> CantDoThatWhileDocked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(60_003_760);
        }
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.location_id = 60_003_760;
        }
    });
    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "CantDoThatWhileDocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
        }
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.location_id = sys_id as i64;
        }
    });

    // 4. Container in wrong solar system -> InvalidContainerLocation
    let wrong_sys_wreck = 910_001i64;
    let sc_id = 910_002i64;
    let locked_child = 910_003i64;
    let alsc_child = 910_004i64;
    let big_singleton = 910_005i64;
    let crim_owner = 90_000_777i64;
    let corp_owner = 90_000_778i64;
    let fleet_owner = 90_000_779i64;
    let corp_wreck = 910_010i64;
    let fleet_wreck = 910_011i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_200_001;
        }
        let template = snap.characters[&char_id].clone();
        let mut c_crim = template.clone();
        c_crim.character_id = crim_owner;
        c_crim.corporation_id = 1_000_009;
        snap.characters.insert(crim_owner, c_crim);
        let mut c_corp = template.clone();
        c_corp.character_id = corp_owner;
        c_corp.corporation_id = 98_200_001;
        snap.characters.insert(corp_owner, c_corp);
        let mut c_fleet = template;
        c_fleet.character_id = fleet_owner;
        c_fleet.corporation_id = 98_200_099;
        snap.characters.insert(fleet_owner, c_fleet);
        snap.fleets.insert(
            777_001,
            FleetRecord {
                fleet_id: 777_001,
                boss_id: char_id,
                members: vec![char_id, fleet_owner],
                ..Default::default()
            },
        );
        snap.items.insert(
            wrong_sys_wreck,
            make_fighter_item(wrong_sys_wreck, 26_557, crim_owner, 30_000_144, InventoryFlag::None, 1),
        );
        snap.items.insert(
            sc_id,
            make_fighter_item(sc_id, 17_366, crim_owner, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            locked_child,
            make_fighter_item(locked_child, 34, crim_owner, sc_id, InventoryFlag::Locked, 1),
        );
        snap.items.insert(
            alsc_child,
            make_fighter_item(alsc_child, 34, crim_owner, sc_id, InventoryFlag::Cargo, 1),
        );
        let mut big = make_fighter_item(big_singleton, 34, crim_owner, sc_id, InventoryFlag::Cargo, 1);
        big.singleton = true;
        snap.items.insert(big_singleton, big);

        snap.items.insert(
            corp_wreck,
            make_fighter_item(corp_wreck, 26_557, corp_owner, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            fleet_wreck,
            make_fighter_item(fleet_wreck, 26_557, fleet_owner, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(wrong_sys_wreck)])
            .await
            .unwrap_err()
            .code,
        "InvalidContainerLocation"
    );

    // 5. ShipNotInSpace, then spawn ship -> ContainerNotInSpace
    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(sc_id)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(sc_id)])
            .await
            .unwrap_err()
            .code,
        "ContainerNotInSpace"
    );

    // 6. Unlocked secure container owned by criminal, with Locked, ALSC-locked, and oversized singleton items
    for bid in [sc_id, corp_wreck, fleet_wreck] {
        sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: 17_366,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }
    ctx.db.with_mut_snapshot(|snap| {
        set_container_password_snap(snap, sc_id, 1, "pw", 0).unwrap();
        set_container_unlocked_snap(snap, sc_id, char_id, true, 0).unwrap();
        set_alsc_item_locked_snap(snap, alsc_child, true, 0).unwrap();
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(crim_owner).criminal_flag_until_unix = now + 900;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });

    let sc_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(sc_id)])
        .await
        .unwrap();
    assert_eq!(sc_res.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(sc_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    assert_eq!(sc_res.dict_get("lootedItemIDs"), Some(&PyRep::List(vec![])));
    assert_eq!(
        sc_res.dict_get("remainingItemIDs"),
        Some(&PyRep::List(vec![
            PyRep::Integer(locked_child),
            PyRep::Integer(alsc_child),
            PyRep::Integer(big_singleton),
        ]))
    );

    // 7. Same player corp (>2M) & same fleet allow looting on SafetyLevel::Green without suspect flag
    let corp_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(corp_wreck)])
        .await
        .unwrap();
    assert_eq!(corp_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    let fleet_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(fleet_wreck)])
        .await
        .unwrap();
    assert_eq!(fleet_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
}

#[test]
fn get_killmail_error_empty_hash_zero_timestamp_and_attacker_fallbacks() {
    let (ctx, session, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    assert_eq!(
        handle_get_killmail(&ctx, &session, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_get_killmail(&ctx, &session, &[PyRep::Integer(999_999_123)])
            .unwrap_err()
            .code,
        "KillmailNotFound"
    );

    // Basic killmail with killed_unix == 0 falls back to ctx.now_unix(), empty hash allowed
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            600_001,
            KillmailRecord {
                kill_id: 600_001,
                victim_character_id: char_id,
                attacker_character_id: 90_000_088,
                solar_system_id: sys_id,
                ship_type_id: 23_915,
                total_value_isk: 123_456.0,
                killed_unix: 0,
                ..Default::default()
            },
        );
    });
    let basic = handle_get_killmail(&ctx, &session, &[PyRep::Integer(600_001), PyRep::String(String::new())])
        .unwrap();
    assert_eq!(basic.dict_get("killTime"), Some(&PyRep::Integer(ctx.now_unix() as i64)));

    // Detailed killmail with empty attackers -> finalCharacterID falls back to victim_character_id
    ctx.db.with_mut_snapshot(|snap| {
        let cw = snap.combat_crimewatch_state.as_inner_mut();
        cw.detailed_killmails.insert(
            600_002,
            DetailedKillmail {
                kill_id: 600_002,
                kill_time_unix: 1_750_000_000,
                solar_system_id: sys_id,
                killmail_hash: "h2".into(),
                victim_character_id: char_id,
                victim_corporation_id: 1_000_009,
                victim_alliance_id: None,
                victim_ship_type_id: 23_915,
                victim_damage_taken: 100.0,
                victim_position: [0.0, 0.0, 0.0],
                attackers: vec![],
                dropped_items: vec![],
                destroyed_items: vec![],
                total_value_isk: 10.0,
            },
        );
        // Detailed killmail with attacker lacking final_blow=true falls back to first attacker
        cw.detailed_killmails.insert(
            600_003,
            DetailedKillmail {
                kill_id: 600_003,
                kill_time_unix: 1_750_000_000,
                solar_system_id: sys_id,
                killmail_hash: "h3".into(),
                victim_character_id: char_id,
                victim_corporation_id: 1_000_009,
                victim_alliance_id: None,
                victim_ship_type_id: 23_915,
                victim_damage_taken: 100.0,
                victim_position: [0.0, 0.0, 0.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: 90_000_123,
                    corporation_id: 1_000_009,
                    alliance_id: None,
                    ship_type_id: 23_915,
                    weapon_type_id: 2_905,
                    damage_done: 100.0,
                    final_blow: false,
                    security_status: 0.0,
                }],
                dropped_items: vec![],
                destroyed_items: vec![],
                total_value_isk: 20.0,
            },
        );
    });
    let det_empty = handle_get_killmail(&ctx, &session, &[PyRep::Integer(600_002)]).unwrap();
    assert_eq!(det_empty.dict_get("finalCharacterID"), Some(&PyRep::Integer(char_id)));
    let det_first = handle_get_killmail(&ctx, &session, &[PyRep::Integer(600_003), PyRep::String(String::new())]).unwrap();
    assert_eq!(det_first.dict_get("finalCharacterID"), Some(&PyRep::Integer(90_000_123)));
}

#[tokio::test]
async fn loot_all_abandoned_fleet_safety_stale_dock_and_war_ally_killmail_mutants() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let now = ctx.now_unix();
    let mut sde = (*ctx.sde).clone();
    sde.types
        .insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    sde.types
        .insert(34, make_fighter_type(34, 18, 4, 0.01, vec![]));
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.capacity = 500.0;
    }
    ctx.sde = Arc::new(sde);

    let station_id = 60_003_760_i64;
    let outsider_id = 90_000_881_i64;
    let other_ship_id = 920_099_i64;
    let own_wreck = 920_001_i64;
    let zero_owner_wreck = 920_002_i64;
    let rt_abandoned_wreck = 920_003_i64;
    let foreign_wreck = 920_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 1_000_009; // NPC corp -> same_corp is false even vs corpmates
            c.station_id = Some(station_id); // stale DB station_id while ship is undocked in sys_id
        }
        let template = snap.characters[&char_id].clone();
        let mut out = template;
        out.character_id = outsider_id;
        out.corporation_id = 98_300_001;
        out.station_id = None;
        snap.characters.insert(outsider_id, out);
        // Pilot in a fleet WITHOUT outsider_id (kills `&& -> ||` on FleetRecord::members)
        snap.fleets.insert(
            777_099,
            FleetRecord {
                fleet_id: 777_099,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        for (wid, owner) in [
            (own_wreck, char_id),
            (zero_owner_wreck, 0),
            (rt_abandoned_wreck, outsider_id),
            (foreign_wreck, outsider_id),
        ] {
            snap.items.insert(
                wid,
                make_fighter_item(wid, 26_557, owner, sys_id as i64, InventoryFlag::None, 1),
            );
        }
        snap.items.insert(
            920_101,
            make_fighter_item(920_101, 34, char_id, own_wreck, InventoryFlag::None, 5),
        );
        snap.items.insert(
            920_102,
            make_fighter_item(920_102, 34, 0, zero_owner_wreck, InventoryFlag::None, 5),
        );
        snap.items.insert(
            920_103,
            make_fighter_item(920_103, 34, outsider_id, rt_abandoned_wreck, InventoryFlag::None, 5),
        );
        snap.items.insert(
            920_104,
            make_fighter_item(920_104, 34, outsider_id, foreign_wreck, InventoryFlag::None, 5),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for bid in [own_wreck, zero_owner_wreck, rt_abandoned_wreck, foreign_wreck] {
        sys_handle
            .spawn_ball(Ball {
                id: bid,
                type_id: 26_557,
                position: DVec3::new(400.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
    }

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
        rt.container_lifetimes.insert(
            rt_abandoned_wreck,
            crate::services::combat_crimewatch::ContainerLifetimeEntry {
                item_id: rt_abandoned_wreck,
                solar_system_id: sys_id,
                type_id: 26_557,
                owner_id: outsider_id,
                created_unix: now.saturating_sub(60),
                lifetime_secs: 7_200,
                is_abandoned: true,
            },
        );
    });

    // 1. Stale DB station_id while ship_item.location_id != station_id must NOT fail CantDoThatWhileDocked (kills L34 `== -> !=`);
    //    own wreck + active_ball_id == ship_id on Green succeeds without suspect (kills L210-213 mutants).
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 23_915,
                position: glam::DVec3::ZERO,
                velocity: glam::DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        )
        .await
        .unwrap();

    let own_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(own_wreck)])
        .await
        .unwrap();
    assert_eq!(own_res.dict_get("abandoned"), Some(&PyRep::Bool(false)));
    assert_eq!(own_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));
    assert_eq!(own_res.dict_get("lootedItemIDs"), Some(&PyRep::List(vec![PyRep::Integer(920_101)])));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&920_101].location_id), ship_id);

    // 2. owner_id == 0 alone makes wreck abandoned on Green (kills L201 `== -> !=` & second `|| -> &&`)
    let zero_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(zero_owner_wreck)])
        .await
        .unwrap();
    assert_eq!(zero_res.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(zero_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));

    // 3. is_abandoned_rt == true alone makes wreck abandoned on Green
    let rt_ab_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(rt_abandoned_wreck)])
        .await
        .unwrap();
    assert_eq!(rt_ab_res.dict_get("abandoned"), Some(&PyRep::Bool(true)));
    assert_eq!(rt_ab_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(false)));

    // 4. Foreign non-abandoned non-fleetmate wreck on Green MUST be rejected (kills L201 `!= 0`, L208 `&& -> ||`, L210 `|| -> &&`, L211/L212 `!`, L213 `== -> !=`)
    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(foreign_wreck)])
            .await
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    // 5. Same foreign wreck on Yellow succeeds & triggers suspect
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });
    let yellow_res = handle_loot_all(&ctx, &mut session, &[PyRep::Integer(foreign_wreck)])
        .await
        .unwrap();
    assert_eq!(yellow_res.dict_get("abandoned"), Some(&PyRep::Bool(false)));
    assert_eq!(yellow_res.dict_get("suspectFlagTriggered"), Some(&PyRep::Bool(true)));

    // 6. Own wreck when active_ball_id != ship_id on Yellow also triggers suspect (`active_ball_id != ship_id` arm)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.active_ship_id = other_ship_id;
        }
        snap.items.insert(
            920_005,
            make_fighter_item(920_005, 26_557, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    sys_handle
        .spawn_ball(Ball {
            id: 920_005,
            type_id: 26_557,
            position: DVec3::new(400.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert_eq!(
        handle_loot_all(&ctx, &mut session, &[PyRep::Integer(920_005)])
            .await
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    // 7. War ally negotiation status & effective_unix timing boundaries in authorize_killmail_read (kills L376, L460 mutants)
    let agg_corp = 98_400_001_i64;
    let def_corp = 98_400_002_i64;
    let ally_corp = 98_400_003_i64;
    let caller_id = 90_000_991_i64;
    let mut ally_sess = eve_machonet::SessionState::new(991);
    ally_sess.user_id = Some(991);
    ally_sess.character_id = Some(caller_id);
    ally_sess.corporation_id = Some(ally_corp);

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            810_001,
            eve_db::types::WarRecord {
                war_id: 810_001,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: 1_000_000,
                active: true,
            },
        );
        snap.war_state.negotiations.push(eve_db::PersistedWarNegotiation {
            id: 1,
            war_id: 810_001,
            kind: eve_db::PersistedNegKind::Ally,
            from_corp: ally_corp,
            isk: eve_db::IskCents(0),
            message: String::new(),
            status: "Pending".into(),
            effective_unix: Some(2_000_000),
        });
    });

    ctx.db.with_snapshot(|snap| {
        // Pending ally negotiation -> denied even after effective_unix
        assert_eq!(
            authorize_killmail_read(
                &ally_sess,
                caller_id,
                snap,
                "secret",
                None,
                700_001,
                90_000_992,
                agg_corp,
                None,
                &[(90_000_993, def_corp, None)],
                2_000_500,
            )
            .unwrap_err()
            .code,
            "KillmailAccessDenied"
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state.negotiations[0].status = "Accepted".into();
    });
    ctx.db.with_snapshot(|snap| {
        // Before ally effective_unix -> denied (kills L460 `&& -> ||` & `>= -> <`)
        assert_eq!(
            authorize_killmail_read(
                &ally_sess,
                caller_id,
                snap,
                "secret",
                None,
                700_001,
                90_000_992,
                agg_corp,
                None,
                &[(90_000_993, def_corp, None)],
                1_999_999,
            )
            .unwrap_err()
            .code,
            "KillmailAccessDenied"
        );
        // Exact boundary kill_time == effective_unix -> authorized
        authorize_killmail_read(
            &ally_sess,
            caller_id,
            snap,
            "secret",
            None,
            700_001,
            90_000_992,
            agg_corp,
            None,
            &[(90_000_993, def_corp, None)],
            2_000_000,
        )
        .unwrap();
        // kill_time == 0 bypasses effective_unix check when Accepted (kills L460 `|| -> &&`)
        authorize_killmail_read(
            &ally_sess,
            caller_id,
            snap,
            "secret",
            None,
            700_001,
            90_000_992,
            agg_corp,
            None,
            &[(90_000_993, def_corp, None)],
            0,
        )
        .unwrap();
    });

    // Expired ally contract (kill_time >= ally_expires_unix) denies read, while exp - 1 allows.
    ctx.db.with_mut_snapshot(|snap| {
        snap.war_state
            .ally_expires_unix
            .insert((810_001, ally_corp), 2_005_000);
    });
    ctx.db.with_snapshot(|snap| {
        authorize_killmail_read(
            &ally_sess,
            caller_id,
            snap,
            "secret",
            None,
            700_001,
            90_000_992,
            agg_corp,
            None,
            &[(90_000_993, def_corp, None)],
            2_004_999,
        )
        .unwrap();
        for expired_ts in [2_005_000, 2_006_000] {
            assert_eq!(
                authorize_killmail_read(
                    &ally_sess,
                    caller_id,
                    snap,
                    "secret",
                    None,
                    700_001,
                    90_000_992,
                    agg_corp,
                    None,
                    &[(90_000_993, def_corp, None)],
                    expired_ts,
                )
                .unwrap_err()
                .code,
                "KillmailAccessDenied"
            );
        }
    });
}

#[tokio::test]
async fn loot_all_emits_on_item_change_for_whole_and_split_moves_to_looter() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let foreign_owner_id = 90_000_002_i64;
    session.session_id = 91_206;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, session.session_id, Some(char_id), true, ctx.now_unix())
        .unwrap();

    let mut sde = (*ctx.sde).clone();
    // 26_557 = wreck (group 186, cat 2), 34/35/36 = mineral cargo items
    sde.types
        .insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    sde.types
        .insert(34, make_fighter_type(34, 18, 4, 100.0, vec![]));
    sde.types
        .insert(35, make_fighter_type(35, 18, 4, 250.0, vec![]));
    sde.types
        .insert(36, make_fighter_type(36, 18, 4, 1_000.0, vec![]));
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.capacity = 5_000.0;
    }
    ctx.sde = Arc::new(sde);

    let wreck_whole_id = 930_001_i64;
    let wreck_split_id = 930_002_i64;
    let item_whole_stack_id = 930_101_i64;
    let item_whole_single_id = 930_102_i64;
    let item_split_stack_id = 930_103_i64;
    let item_locked_id = 930_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_whole_id,
            make_fighter_item(
                wreck_whole_id,
                26_557,
                foreign_owner_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            wreck_split_id,
            make_fighter_item(
                wreck_split_id,
                26_557,
                foreign_owner_id,
                sys_id as i64,
                InventoryFlag::None,
                1,
            ),
        );
        snap.items.insert(
            item_whole_stack_id,
            make_fighter_item(
                item_whole_stack_id,
                34,
                foreign_owner_id,
                wreck_whole_id,
                InventoryFlag::None,
                5, // 5 * 100 m3 = 500 m3
            ),
        );
        let mut single_rec = make_fighter_item(
            item_whole_single_id,
            35,
            foreign_owner_id,
            wreck_whole_id,
            InventoryFlag::None,
            1, // 1 * 250 m3 = 250 m3 (singleton)
        );
        single_rec.singleton = true;
        snap.items.insert(item_whole_single_id, single_rec);
        snap.items.insert(
            item_split_stack_id,
            make_fighter_item(
                item_split_stack_id,
                36,
                foreign_owner_id,
                wreck_split_id,
                InventoryFlag::None,
                6, // 6 * 1000 m3 = 6000 m3; ship cap 5000 - 750 = 4250 -> fits 4 units, leaves 2
            ),
        );
        snap.items.insert(
            item_locked_id,
            make_fighter_item(
                item_locked_id,
                35,
                foreign_owner_id,
                wreck_split_id,
                InventoryFlag::Locked,
                1,
            ),
        );
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        snap.item_positions
            .insert(wreck_whole_id, [500.0, 0.0, 0.0]);
        snap.item_positions
            .insert(wreck_split_id, [600.0, 0.0, 0.0]);
    });

    let now = ctx.now_unix();
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        for wid in [wreck_whole_id, wreck_split_id] {
            rt.container_lifetimes.insert(
                wid,
                crate::services::combat_crimewatch::ContainerLifetimeEntry {
                    item_id: wid,
                    solar_system_id: sys_id,
                    type_id: 26_557,
                    owner_id: foreign_owner_id,
                    created_unix: now.saturating_sub(60),
                    lifetime_secs: 7_200,
                    is_abandoned: true,
                },
            );
        }
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, btype, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (wreck_whole_id, 26_557_u32, DVec3::new(500.0, 0.0, 0.0)),
        (wreck_split_id, 26_557_u32, DVec3::new(600.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: btype,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }

    let kw = std::collections::HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Whole moves from wreck_whole_id -> ship Cargo emit OnItemChange before OnWreckLootAll.
    let res_whole = super::super::handle_wrecks_killmails(
        &ctx,
        &mut session,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(wreck_whole_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(looted_whole_list) = res_whole
        .dict_get("lootedItemIDs")
        .expect("lootedItemIDs present")
    else {
        panic!("expected List for lootedItemIDs");
    };
    assert_eq!(looted_whole_list.len(), 2);
    assert!(looted_whole_list.contains(&PyRep::Integer(item_whole_stack_id)));
    assert!(looted_whole_list.contains(&PyRep::Integer(item_whole_single_id)));
    assert_eq!(
        res_whole.dict_get("remainingItemIDs"),
        Some(&PyRep::List(Vec::new()))
    );

    let notifs_whole = ctx.drain_notifications_for_char(char_id);
    let pos_loot_all_whole = notifs_whole
        .iter()
        .position(|n| n.event == "OnWreckLootAll")
        .expect("OnWreckLootAll emitted");
    let item_changes_whole: Vec<_> = notifs_whole
        .iter()
        .enumerate()
        .filter(|(_, n)| n.event == "OnItemChange")
        .collect();
    assert_eq!(item_changes_whole.len(), 2);
    for (idx, notif) in &item_changes_whole {
        assert!(
            *idx < pos_loot_all_whole,
            "OnItemChange must precede OnWreckLootAll"
        );
        let PyRep::Dict(changes) = &notif.args[1] else {
            panic!("expected Dict in OnItemChange changes, got {:?}", notif.args[1]);
        };
        assert!(
            changes.iter().all(|(k, _)| k.as_i64() != Some(5)),
            "legacy ixQuantity (5) must never be emitted: {changes:?}"
        );
        assert!(changes.contains(&(PyRep::Integer(2), PyRep::Integer(foreign_owner_id))));
        assert!(changes.contains(&(PyRep::Integer(3), PyRep::Integer(wreck_whole_id))));
        assert!(changes.contains(&(
            PyRep::Integer(4),
            PyRep::Integer(InventoryFlag::None as i64)
        )));
        assert_eq!(
            notif.args[0].dbrow_get("locationID"),
            Some(&PyRep::Integer(ship_id))
        );
        assert_eq!(
            notif.args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Cargo as i64))
        );
        assert_eq!(
            notif.args[0].dbrow_get("ownerID"),
            Some(&PyRep::Integer(char_id))
        );
    }
    let whole_ids: Vec<i64> = item_changes_whole
        .iter()
        .filter_map(|(_, n)| n.args[0].dbrow_get("itemID").and_then(PyRep::as_i64))
        .collect();
    assert!(whole_ids.contains(&item_whole_stack_id));
    assert!(whole_ids.contains(&item_whole_single_id));

    // 2. Partial split + skipped locked item from wreck_split_id:
    //    emits OnItemChange for both remaining stack (ixStackSize=9, no ixQuantity=5)
    //    and newly created Cargo stack (ixLocationID=0), and nothing for locked item.
    let res_split = super::super::handle_wrecks_killmails(
        &ctx,
        &mut session,
        "invbroker",
        "LootAll",
        &[PyRep::Integer(wreck_split_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(looted_split_list) = res_split
        .dict_get("lootedItemIDs")
        .expect("lootedItemIDs present")
    else {
        panic!("expected List for lootedItemIDs");
    };
    assert_eq!(looted_split_list.len(), 1);
    let split_cargo_id = looted_split_list[0].as_i64().expect("split cargo item id");
    assert_ne!(split_cargo_id, item_split_stack_id);
    assert_ne!(split_cargo_id, item_locked_id);
    let PyRep::List(rem_split_list) = res_split
        .dict_get("remainingItemIDs")
        .expect("remainingItemIDs present")
    else {
        panic!("expected List for remainingItemIDs");
    };
    assert!(rem_split_list.contains(&PyRep::Integer(item_split_stack_id)));
    assert_eq!(
        ctx.db.get_item(item_locked_id).unwrap().location_id,
        wreck_split_id
    );

    let notifs_split = ctx.drain_notifications_for_char(char_id);
    let pos_loot_all_split = notifs_split
        .iter()
        .position(|n| n.event == "OnWreckLootAll")
        .expect("OnWreckLootAll emitted on split");
    let item_changes_split: Vec<_> = notifs_split
        .iter()
        .enumerate()
        .filter(|(_, n)| n.event == "OnItemChange")
        .collect();
    assert_eq!(
        item_changes_split.len(),
        2,
        "partial split must emit OnItemChange for remaining stack and new cargo stack"
    );
    for (idx, _) in &item_changes_split {
        assert!(*idx < pos_loot_all_split);
    }
    assert!(
        item_changes_split
            .iter()
            .all(|(_, n)| n.args[0].dbrow_get("itemID") != Some(&PyRep::Integer(item_locked_id))),
        "locked item must not emit OnItemChange"
    );

    let (_, rem_notif) = item_changes_split
        .iter()
        .find(|(_, n)| n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(item_split_stack_id)))
        .expect("OnItemChange for remaining stack");
    assert_eq!(
        rem_notif.args[0].dbrow_get("locationID"),
        Some(&PyRep::Integer(wreck_split_id))
    );
    assert_eq!(
        rem_notif.args[0].dbrow_get("quantity"),
        Some(&PyRep::Integer(2))
    );
    let PyRep::Dict(rem_changes) = &rem_notif.args[1] else {
        panic!("expected Dict in remaining stack OnItemChange");
    };
    assert!(rem_changes.iter().all(|(k, _)| k.as_i64() != Some(5)));
    assert!(rem_changes.contains(&(PyRep::Integer(9), PyRep::Integer(6))));

    let (_, cargo_notif) = item_changes_split
        .iter()
        .find(|(_, n)| n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(split_cargo_id)))
        .expect("OnItemChange for split cargo stack");
    assert_eq!(
        cargo_notif.args[0].dbrow_get("locationID"),
        Some(&PyRep::Integer(ship_id))
    );
    assert_eq!(
        cargo_notif.args[0].dbrow_get("flagID"),
        Some(&PyRep::Integer(InventoryFlag::Cargo as i64))
    );
    assert_eq!(
        cargo_notif.args[0].dbrow_get("ownerID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        cargo_notif.args[0].dbrow_get("quantity"),
        Some(&PyRep::Integer(4))
    );
    let PyRep::Dict(cargo_changes) = &cargo_notif.args[1] else {
        panic!("expected Dict in split cargo stack OnItemChange");
    };
    assert!(cargo_changes.iter().all(|(k, _)| k.as_i64() != Some(5)));
    assert!(cargo_changes.contains(&(PyRep::Integer(3), PyRep::Integer(0))));
}

#[tokio::test]
async fn loot_all_partial_split_does_not_duplicate_on_item_change_for_non_character_owner() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let npc_corp_owner_id = 1_000_009_i64;
    let foreign_corp_owner_id = 98_777_001_i64;
    let foreign_char_owner_id = 90_000_002_i64;
    session.session_id = 91_207;
    let (uid, foreign_char_uid) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&char_id].user_id,
            s.characters[&foreign_char_owner_id].user_id,
        )
    });
    let lock_now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(uid, session.session_id, Some(char_id), true, lock_now)
        .unwrap();
    ctx.db
        .acquire_online_lock(foreign_char_uid, 91_208, Some(foreign_char_owner_id), true, lock_now)
        .unwrap();

    let mut sde = (*ctx.sde).clone();
    sde.types
        .insert(26_557, make_fighter_type(26_557, 186, 2, 100.0, vec![]));
    sde.types
        .insert(36, make_fighter_type(36, 18, 4, 100.0, vec![]));
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.capacity = 400.0; // fits 4 units of qty=10 partial stacks cleanly
    }
    ctx.sde = Arc::new(sde);

    let cases = [
        (npc_corp_owner_id, 931_001_i64, 931_101_i64, false),
        (foreign_corp_owner_id, 931_002_i64, 931_102_i64, false),
        (foreign_char_owner_id, 931_003_i64, 931_103_i64, true),
    ];

    ctx.db.with_mut_snapshot(|snap| {
        assert!(!snap.characters.contains_key(&npc_corp_owner_id));
        assert!(!snap.characters.contains_key(&foreign_corp_owner_id));
        assert!(snap.characters.contains_key(&foreign_char_owner_id));
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        for &(owner_id, wreck_id, stack_id, _) in &cases {
            snap.items.insert(
                wreck_id,
                make_fighter_item(wreck_id, 26_557, owner_id, sys_id as i64, InventoryFlag::None, 1),
            );
            snap.items.insert(
                stack_id,
                make_fighter_item(stack_id, 36, owner_id, wreck_id, InventoryFlag::None, 10),
            );
            snap.item_positions.insert(wreck_id, [500.0, 0.0, 0.0]);
        }
    });

    let now = ctx.now_unix();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (bid, btype, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (931_001_i64, 26_557_u32, DVec3::new(500.0, 0.0, 0.0)),
        (931_002_i64, 26_557_u32, DVec3::new(500.0, 0.0, 0.0)),
        (931_003_i64, 26_557_u32, DVec3::new(500.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: bid,
                    type_id: btype,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 120.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState::default(),
            )
            .await
            .unwrap();
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        for &(owner_id, wreck_id, _, _) in &cases {
            rt.container_lifetimes.insert(
                wreck_id,
                crate::services::combat_crimewatch::ContainerLifetimeEntry {
                    item_id: wreck_id,
                    solar_system_id: sys_id,
                    type_id: 26_557,
                    owner_id,
                    created_unix: now.saturating_sub(60),
                    lifetime_secs: 7_200,
                    is_abandoned: false,
                },
            );
        }
    });

    let kw = std::collections::HashMap::new();
    for &(owner_id, wreck_id, stack_id, is_foreign_char) in &cases {
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx.drain_notifications_for_char(owner_id);

        let res = super::super::handle_wrecks_killmails(
            &ctx,
            &mut session,
            "invbroker",
            "LootAll",
            &[PyRep::Integer(wreck_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::List(looted_ids) = res.dict_get("lootedItemIDs").unwrap() else {
            panic!("expected lootedItemIDs list");
        };
        assert_eq!(looted_ids.len(), 1);
        let split_cargo_id = looted_ids[0].as_i64().unwrap();
        assert_eq!(
            res.dict_get("remainingItemIDs"),
            Some(&PyRep::List(vec![PyRep::Integer(stack_id)]))
        );

        let looter_item_changes: Vec<_> = ctx
            .drain_notifications_for_char(char_id)
            .into_iter()
            .filter(|n| n.event == "OnItemChange")
            .collect();
        let rem_for_looter: Vec<_> = looter_item_changes
            .iter()
            .filter(|n| n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(stack_id)))
            .collect();
        let split_for_looter: Vec<_> = looter_item_changes
            .iter()
            .filter(|n| n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(split_cargo_id)))
            .collect();
        assert_eq!(
            rem_for_looter.len(),
            1,
            "looter must receive exactly one OnItemChange for remaining stack (owner={owner_id})"
        );
        assert_eq!(
            split_for_looter.len(),
            1,
            "looter must receive exactly one OnItemChange for split cargo stack (owner={owner_id})"
        );

        if is_foreign_char {
            let owner_rem: Vec<_> = ctx
                .drain_notifications_for_char(owner_id)
                .into_iter()
                .filter(|n| {
                    n.event == "OnItemChange"
                        && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(stack_id))
                })
                .collect();
            assert_eq!(
                owner_rem.len(),
                1,
                "foreign character owner must also receive one OnItemChange for remaining stack"
            );
        }

        // Clear looted split stack from ship cargo before the next wreck case.
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&split_cargo_id);
        });
    }
}
