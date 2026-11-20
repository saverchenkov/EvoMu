//! Unit tests for `character_energy` (`characterEnergyMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_machonet::SessionState;
use eve_proto::PyRep;

#[test]
fn energy_state_default_keyval_regen_consume_and_rpc() {
    let def = CharacterEnergyState::default();
    assert_eq!(def.current_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(def.max_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);

    let kv = energy_to_keyval(&def);
    assert_eq!(
        kv.dict_get("currentEnergy"),
        Some(&PyRep::Integer(DEFAULT_MAX_ENERGY))
    );
    assert_eq!(
        kv.dict_get("maxEnergy"),
        Some(&PyRep::Integer(DEFAULT_MAX_ENERGY))
    );
    assert_eq!(kv.dict_get("rechargeTickPeriod"), Some(&PyRep::Integer(3_600)));
    assert_eq!(kv.dict_get("minEnergyLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(
        kv.dict_get("energyIncreasePerRechargeTick"),
        Some(&PyRep::Real(DEFAULT_RECHARGE_RATE * 60.0))
    );
    assert_eq!(kv.dict_get("isAtQuiescentLevel"), Some(&PyRep::Bool(true)));
    assert_eq!(
        kv.dict_get("energyRange"),
        Some(&PyRep::Integer(DEFAULT_MAX_ENERGY))
    );
    assert_eq!(kv.dict_get("normalisedEnergyLevel"), Some(&PyRep::Real(1.0)));

    let ctx = test_ctx();
    let cid = 90_000_001_i64;

    // Initial get initializes to DEFAULT_MAX_ENERGY
    let st0 = get_character_energy(&ctx, cid);
    assert_eq!(st0.current_energy, DEFAULT_MAX_ENERGY);

    // Legacy 5_000 max migration
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        g.char_energy.insert(
            cid,
            CharacterEnergyState {
                current_energy: 5_000,
                max_energy: 5_000,
                last_update_time: 100,
                recharge_rate: 1.0,
                energy_rem: 0.5,
            },
        );
    }
    let st_migrated = get_character_energy(&ctx, cid);
    assert_eq!(st_migrated.max_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(st_migrated.current_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(st_migrated.energy_rem, 0.0);

    // Set energy clamping (negative -> 0, > max -> max)
    let st_low = set_character_energy(&ctx, cid, -50);
    assert_eq!(st_low.current_energy, 0);
    let st_high = set_character_energy(&ctx, cid, 999);
    assert_eq!(st_high.current_energy, DEFAULT_MAX_ENERGY);

    // Consume energy: <= 0 no-op, valid deduction, insufficient error
    let st_set = set_character_energy(&ctx, cid, 40);
    assert_eq!(st_set.current_energy, 40);
    let kv_partial = energy_to_keyval(&st_set);
    assert_eq!(
        kv_partial.dict_get("isAtQuiescentLevel"),
        Some(&PyRep::Bool(false))
    );
    assert_eq!(
        kv_partial.dict_get("energyRange"),
        Some(&PyRep::Integer(DEFAULT_MAX_ENERGY))
    );
    assert_eq!(
        kv_partial.dict_get("normalisedEnergyLevel"),
        Some(&PyRep::Real(40.0 / (DEFAULT_MAX_ENERGY as f64)))
    );
    let st_noop = consume_character_energy(&ctx, cid, 0).unwrap();
    assert_eq!(st_noop.current_energy, 40);
    let st_after = consume_character_energy(&ctx, cid, 15).unwrap();
    assert_eq!(st_after.current_energy, 25);
    assert_eq!(
        consume_character_energy(&ctx, cid, 30).unwrap_err().code,
        "InsufficientEnergy"
    );

    // Regeneration over elapsed FILETIME ticks (60 minutes = 60 * 600_000_000 ticks)
    let now_ft = unix_to_filetime(ctx.now_unix());
    {
        let mut g = ctx.cluster_misc_state.lock().unwrap();
        let entry = g.char_energy.get_mut(&cid).unwrap();
        entry.current_energy = 50;
        entry.last_update_time = now_ft - 60 * 600_000_000;
        entry.energy_rem = 0.0;
    }
    let st_regen = get_character_energy(&ctx, cid);
    // 60 minutes * (100 / 1320) = 4.545 -> +4 CEQ and ~0.545 remainder
    assert_eq!(st_regen.current_energy, 54);
    assert!(st_regen.energy_rem > 0.5 && st_regen.energy_rem < 0.6);

    // RPC dispatch
    let mut unauth = SessionState::default();
    let other_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle(&ctx, &mut unauth, other_svc, "GetMyEnergyState", &[]).is_none());
    assert!(handle(&ctx, &mut unauth, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .is_err());

    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    assert!(handle(&ctx, &mut sess, "characterEnergyMgr", unknown_method, &[]).is_none());
    let rpc_res = handle(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .unwrap();
    assert_eq!(
        rpc_res.dict_get("currentEnergy"),
        Some(&PyRep::Integer(54))
    );
}

#[test]
fn get_my_energy_state_rpc_is_read_pure() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let uninit_cid = 90_099_001_i64;
    let mut sess = auth_session(1, uninit_cid, 98_000_001, 30_000_142);

    let epoch_before = ctx.db.state_epoch();
    let commits_before = ctx.db.tx_commit_count();
    let res = handle(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .unwrap();
    assert_eq!(
        res.dict_get("currentEnergy"),
        Some(&PyRep::Integer(DEFAULT_MAX_ENERGY))
    );
    assert!(
        !ctx.cluster_misc_state
            .lock()
            .unwrap()
            .char_energy
            .contains_key(&uninit_cid),
        "read path must not insert default energy into cluster_misc_state"
    );
    assert!(ctx.drain_notifications_for_char(uninit_cid).is_empty());
    assert_eq!(ctx.db.state_epoch(), epoch_before);
    assert_eq!(ctx.db.tx_commit_count(), commits_before);
}

#[test]
fn consume_character_energy_emits_single_post_deduction_notif_and_is_pure_on_insufficient() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let cid = 90_099_002_i64;
    let uninit_cid = 90_099_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "energy_777".into(),
            eve_db::AccountRecord {
                user_id: 777,
                username: "energy_777".into(),
                password_token: "pw".into(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch = snap
            .characters
            .values()
            .next()
            .cloned()
            .expect("demo character");
        ch.character_id = cid;
        ch.user_id = 777;
        snap.characters.insert(cid, ch);
    });
    ctx.db
        .acquire_online_lock(777, 1777, Some(cid), true, ctx.now_unix())
        .unwrap();

    // Non-positive consume on uninitialized character is read-pure.
    let e_noop = ctx.db.state_epoch();
    let c_noop = ctx.db.tx_commit_count();
    let noop = consume_character_energy(&ctx, uninit_cid, 0).expect("zero consume");
    assert_eq!(noop.current_energy, DEFAULT_MAX_ENERGY);
    assert_eq!(ctx.db.state_epoch(), e_noop);
    assert_eq!(ctx.db.tx_commit_count(), c_noop);
    assert!(ctx.drain_notifications_for_char(uninit_cid).is_empty());

    // Set energy to 10, advance 60 min (+4 -> 14 CEQ), then fail consuming 30 CEQ without mutating or notifying.
    let _ = set_character_energy(&ctx, cid, 10);
    let _ = ctx.drain_notifications_for_char(cid);
    ctx.clock().advance_secs(3_600);
    let e_fail = ctx.db.state_epoch();
    let c_fail = ctx.db.tx_commit_count();
    let err = consume_character_energy(&ctx, cid, 30).unwrap_err();
    assert_eq!(err.code, "InsufficientEnergy");
    assert_eq!(ctx.db.state_epoch(), e_fail);
    assert_eq!(ctx.db.tx_commit_count(), c_fail);
    assert!(
        ctx.drain_notifications_for_char(cid).is_empty(),
        "failed InsufficientEnergy must not emit OnCharacterEnergyChanged"
    );

    // Advance 25 hours (full regen to 100 CEQ), read purely via GetMyEnergyState, then consume 30 -> 70 CEQ.
    ctx.clock().advance_secs(90_000);
    let mut sess = auth_session(777, cid, 98_000_001, 30_000_142);
    let read_rep = handle(&ctx, &mut sess, "characterEnergyMgr", "GetMyEnergyState", &[])
        .unwrap()
        .unwrap();
    assert_eq!(read_rep.dict_get("currentEnergy"), Some(&PyRep::Integer(100)));
    assert!(ctx.drain_notifications_for_char(cid).is_empty());

    let after = consume_character_energy(&ctx, cid, 30).expect("consume 30 after regen");
    assert_eq!(after.current_energy, 70);
    let notifs = ctx.drain_notifications_for_char(cid);
    let energy_notifs: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnCharacterEnergyChanged")
        .collect();
    assert_eq!(
        energy_notifs.len(),
        1,
        "consume after elapsed regen must emit exactly one OnCharacterEnergyChanged (got {energy_notifs:?})"
    );
    assert_eq!(energy_notifs[0].args[0], PyRep::Integer(cid));
    assert_eq!(
        energy_notifs[0].args[1].dict_get("currentEnergy"),
        Some(&PyRep::Integer(70))
    );
    assert_eq!(
        energy_notifs[0].args[1].dict_get("maxEnergy"),
        Some(&PyRep::Integer(100))
    );
}
