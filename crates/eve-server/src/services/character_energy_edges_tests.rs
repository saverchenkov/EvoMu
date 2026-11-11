//! Edge-case behavioral unit tests for `character_energy` (`characterEnergyMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_db::{AccountRecord, CharacterRecord};
use eve_proto::PyRep;

fn seed_online_char(ctx: &ServerContext, cid: i64, uid: u64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            format!("energy_{uid}"),
            AccountRecord {
                user_id: uid,
                username: format!("energy_{uid}"),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        snap.characters.insert(
            cid,
            CharacterRecord {
                activity_status: None,
                character_id: cid,
                user_id: uid,
                name: format!("Energy_{cid}"),
                corporation_id: 98_000_001,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 0,
                balance: eve_db::IskCents(0),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
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
            },
        );
    });
    ctx.db
        .acquire_online_lock(uid, uid + 1000, Some(cid), true, ctx.now_unix())
        .unwrap();
}

#[test]
fn get_character_energy_edge_cases_and_notifications() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let cid = 90_010_001_i64;
    seed_online_char(&ctx, cid, 101);
    let now_ft = unix_to_filetime(ctx.now_unix());

    // Initial get creates entry and drains any setup state cleanly.
    let init = get_character_energy(&ctx, cid);
    assert_eq!(init.current_energy, DEFAULT_MAX_ENERGY);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // 1. Sub-minute elapsed with gained == 0: no update, no notification.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 50;
        entry.last_update_time = now_ft - 300_000_000; // 0.5 min
        entry.energy_rem = 0.0;
    }
    let st_submin = get_character_energy(&ctx, cid);
    assert_eq!(st_submin.current_energy, 50);
    assert_eq!(st_submin.last_update_time, now_ft - 300_000_000);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // 2. >= 1 minute elapsed with gained == 0: accumulates remainder without notifying.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 50;
        entry.last_update_time = now_ft - 5 * 600_000_000; // 5 min * (100/1320) ~= 0.378 < 1.0
        entry.energy_rem = 0.0;
    }
    let st_rem = get_character_energy(&ctx, cid);
    assert_eq!(st_rem.current_energy, 50);
    assert!(st_rem.energy_rem > 0.3 && st_rem.energy_rem < 0.4);
    assert_eq!(st_rem.last_update_time, now_ft);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // 3. Positive gain capped at max_energy zeroes remainder and emits OnCharacterEnergyChanged.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 98;
        entry.last_update_time = now_ft - 120 * 600_000_000; // ~9.09 CEQ
        entry.energy_rem = 0.7;
    }
    let st_capped = get_character_energy(&ctx, cid);
    assert_eq!(st_capped.current_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(st_capped.energy_rem, 0.0);
    let notifs = ctx.drain_notifications_for_char(cid);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnCharacterEnergyChanged");

    // 4. Full energy resets stale energy_rem and refreshes last_update_time.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = DEFAULT_MAX_ENERGY;
        entry.last_update_time = now_ft - 600_000_000;
        entry.energy_rem = 0.42;
    }
    let st_full = get_character_energy(&ctx, cid);
    assert_eq!(st_full.energy_rem, 0.0);
    assert_eq!(st_full.last_update_time, now_ft);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // 5. last_update_time <= 0 initializes timestamp without regenerating.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 25;
        entry.last_update_time = 0;
        entry.energy_rem = 0.25;
    }
    let st_zero_ts = get_character_energy(&ctx, cid);
    assert_eq!(st_zero_ts.current_energy, 25);
    assert_eq!(st_zero_ts.last_update_time, now_ft);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    // 6. recharge_rate <= 0 does not regenerate even after long elapsed time.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 10;
        entry.last_update_time = now_ft - 600 * 600_000_000;
        entry.recharge_rate = 0.0;
    }
    let st_no_rate = get_character_energy(&ctx, cid);
    assert_eq!(st_no_rate.current_energy, 10);
    assert!(ctx.drain_notifications_for_char(cid).is_empty());
}

#[test]
fn set_consume_and_rpc_edge_cases() {
    let ctx = test_ctx();
    let fresh_cid = 90_010_002_i64;
    seed_online_char(&ctx, fresh_cid, 102);

    // set_character_energy on uninitialized character creates entry and notifies.
    let created = set_character_energy(&ctx, fresh_cid, 73);
    assert_eq!(created.current_energy, 73);
    assert_eq!(created.max_energy, DEFAULT_MAX_ENERGY);
    let notifs = ctx.drain_notifications_for_char(fresh_cid);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnCharacterEnergyChanged");

    // set_character_energy migrates legacy max_energy == 5_000 even when current_energy != 5_000.
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&fresh_cid).unwrap();
        entry.max_energy = 5_000;
        entry.current_energy = 4_200;
        entry.recharge_rate = 9.0;
        entry.energy_rem = 0.8;
    }
    let migrated = set_character_energy(&ctx, fresh_cid, 150);
    assert_eq!(migrated.max_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(migrated.current_energy, DEFAULT_MAX_ENERGY);
    assert!((migrated.recharge_rate - DEFAULT_RECHARGE_RATE).abs() < 1e-12);
    assert_eq!(migrated.energy_rem, 0.0);
    assert_eq!(ctx.drain_notifications_for_char(fresh_cid).len(), 1);

    // consume_character_energy with negative amount is a no-op and does not notify.
    let neg_res = consume_character_energy(&ctx, fresh_cid, -25).unwrap();
    assert_eq!(neg_res.current_energy, DEFAULT_MAX_ENERGY);
    assert!(ctx.drain_notifications_for_char(fresh_cid).is_empty());

    // RPC handle guards: wrong service, unauthenticated, unknown method.
    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "not_character_energy", "GetMyEnergyState", &[]).is_none());
    assert!(handle(&ctx, &mut unauth, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .is_err());

    let mut sess = auth_session(1, fresh_cid, 98_000_001, 30_000_142);
    assert!(handle(&ctx, &mut sess, "characterEnergyMgr", "unknown_method_fallback", &[]).is_none());
    let kv = handle(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .unwrap();
    assert_eq!(kv.dict_get("currentEnergy"), Some(&PyRep::Integer(DEFAULT_MAX_ENERGY)));
    assert_eq!(kv.dict_get("quiescentEnergyLevel"), Some(&PyRep::Integer(DEFAULT_MAX_ENERGY)));
}
