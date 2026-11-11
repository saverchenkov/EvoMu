//! Direct unit tests covering all remaining branches of `combat_wrecks_loot_ops`.

use super::super::combat_wrecks_loot_ops::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, DetailedKillmail, KillmailAttackerEntry, SafetyLevel,
};
use crate::services::inventory_fitting::fitting_containers_ships::write_container_ship_state;
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
    write_container_ship_state(&ctx, |st| {
        st.container_passwords.insert(sc_id, "pw".into());
        st.unlocked_containers.insert((sc_id, char_id));
        st.alsc_locked_items.insert(alsc_child);
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
    with_mut_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.insert(
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
        rt.detailed_killmails.insert(
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
}
