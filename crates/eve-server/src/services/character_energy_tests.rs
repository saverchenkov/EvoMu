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
