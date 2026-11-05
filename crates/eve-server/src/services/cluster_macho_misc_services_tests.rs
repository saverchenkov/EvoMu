//! Unit tests for `cluster_macho_misc_services` (landing pads, shipcaster balls, load slots, char settings, Zarzakh Horizon Siege).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn landing_pads_sde_launchers_ballpark_and_horizon_siege_state_machine() {
    assert_eq!(HorizonSiegePhase::Normal.as_str(), "Normal");
    assert_eq!(HorizonSiegePhase::Warning.as_str(), "Warning");
    assert_eq!(HorizonSiegePhase::Active.as_str(), "Active");
    assert_eq!(HorizonSiegePhase::Stabilizing.as_str(), "Stabilizing");

    let pad_unbuilt = LandingPadRecord {
        item_id: 1,
        type_id: 75932,
        built_type_id: 75928,
        solar_system_id: 30_000_142,
        faction_id: 500_001,
        is_built: false,
        link_timestamp: 100,
        is_disrupted: false,
        position: glam::DVec3::ZERO,
        required_materials: BTreeMap::new(),
        contributed_materials: BTreeMap::new(),
    };
    let kv_unbuilt = pad_unbuilt.to_keyval();
    assert_eq!(kv_unbuilt.dict_get("isLinked"), Some(&PyRep::Bool(false)));
    assert_eq!(kv_unbuilt.dict_get("canBeLinked"), Some(&PyRep::Bool(false)));

    let mut pad_built = pad_unbuilt.clone();
    pad_built.is_built = true;
    let kv_built = pad_built.to_keyval();
    assert_eq!(kv_built.dict_get("isLinked"), Some(&PyRep::Bool(true)));
    assert_eq!(kv_built.dict_get("canBeLinked"), Some(&PyRep::Bool(true)));

    pad_built.is_disrupted = true;
    let kv_disrupted = pad_built.to_keyval();
    assert_eq!(kv_disrupted.dict_get("isLinked"), Some(&PyRep::Bool(false)));
    assert_eq!(
        kv_disrupted.dict_get("canBeLinked"),
        Some(&PyRep::Bool(false))
    );

    let fob_pads = default_pirate_fob_pads(1_700_000_000);
    assert_eq!(fob_pads.len(), 3);
    assert_eq!(starter_type_and_faction_for_race(1), Some((93980, 500_001)));
    assert_eq!(starter_type_and_faction_for_race(2), Some((94018, 500_002)));
    assert_eq!(starter_type_and_faction_for_race(4), Some((93968, 500_003)));
    assert_eq!(starter_type_and_faction_for_race(8), Some((93984, 500_004)));
    assert_eq!(starter_type_and_faction_for_race(99), None);

    let ctx = test_ctx();
    let launchers = sde_launchers(&ctx.sde);
    assert!(!launchers.is_empty());
    let starters = sde_starter_launchers(&ctx.sde);
    assert!(!starters.is_empty());

    // Populate shipcaster balls into a Zarzakh ballpark (30_100_000) and a starter system
    let mut bp_zarzakh = eve_destiny::Ballpark::new(30_100_000);
    populate_shipcaster_balls_with_sde(30_100_000, &mut bp_zarzakh, Some(&ctx.sde), Some(&ctx.db));
    assert!(bp_zarzakh.balls.get(&105_000_010).is_some());
    assert!(bp_zarzakh.balls.get(&104_000_030).is_some());

    let starter_sys = starters[0].solar_system_id;
    let mut bp_starter = eve_destiny::Ballpark::new(starter_sys);
    populate_shipcaster_balls_with_sde(starter_sys, &mut bp_starter, Some(&ctx.sde), None);
    assert!(bp_starter.balls.get(&starters[0].item_id).is_some());

    // Zarzakh Horizon Siege state transitions: Schedule (Warning) -> Active -> Stabilizing -> Normal
    let now = ctx.now_unix();
    let st_warn = schedule_zarzakh_horizon_siege(&ctx, Some(42));
    assert_eq!(st_warn.phase, HorizonSiegePhase::Warning);
    assert_eq!(st_warn.instability_points, 50);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));

    let st_active = tick_zarzakh_horizon_siege(&ctx, now + HORIZON_SIEGE_WARNING_SECS + 1);
    assert_eq!(st_active.phase, HorizonSiegePhase::Active);
    assert!(st_active.fulcrum_restricted_to_pirates);
    assert!(is_zarzakh_horizon_siege_active(&ctx));

    let st_stab = tick_zarzakh_horizon_siege(
        &ctx,
        now + HORIZON_SIEGE_WARNING_SECS + HORIZON_SIEGE_ACTIVE_SECS + 1,
    );
    assert_eq!(st_stab.phase, HorizonSiegePhase::Stabilizing);
    assert!(!st_stab.fulcrum_restricted_to_pirates);

    let st_norm = tick_zarzakh_horizon_siege(
        &ctx,
        now + HORIZON_SIEGE_WARNING_SECS
            + HORIZON_SIEGE_ACTIVE_SECS
            + HORIZON_SIEGE_COOLDOWN_SECS
            + 1,
    );
    assert_eq!(st_norm.phase, HorizonSiegePhase::Normal);
    assert_eq!(st_norm.instability_points, 0);

    // Direct trigger & toggle
    let st_trig = trigger_zarzakh_horizon_siege(&ctx, Some(99), 600);
    assert_eq!(st_trig.phase, HorizonSiegePhase::Active);
    set_zarzakh_horizon_siege_active(&ctx, false);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));
}

#[tokio::test]
async fn load_slots_target_cooldowns_and_char_settings_rpc() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    // Under population cap -> None
    assert!(get_or_create_offered_load_slot(&ctx, cid, sys_id).is_none());

    // Set population cap to 0 so current online count (0) >= cap (0) offers an adjacent system
    with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(sys_id, 0);
    });
    let (slot_key, alt_sys) = get_or_create_offered_load_slot(&ctx, cid, sys_id)
        .expect("should offer adjacent system when cap is 0");
    assert!(!validate_and_consume_offered_load_slot(
        &ctx,
        cid,
        alt_sys,
        "wrong-key"
    ));
    assert!(validate_and_consume_offered_load_slot(
        &ctx, cid, alt_sys, &slot_key
    ));
    // Consumed slot cannot be reused
    assert!(!validate_and_consume_offered_load_slot(
        &ctx, cid, alt_sys, &slot_key
    ));

    // Shipcaster target cooldowns and charges
    let pad_id = 104_000_020_i64;
    assert!(check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 120).is_ok());
    assert_eq!(
        check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 120)
            .unwrap_err()
            .code,
        "ShipcasterTargetOnCooldown"
    );
    with_misc_state(&ctx, |st| {
        st.target_cooldowns.remove(&pad_id);
        st.target_charges.insert(pad_id, 0);
    });
    assert_eq!(
        check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 120)
            .unwrap_err()
            .code,
        "ShipcasterTargetNoCharges"
    );

    // SaveCharacterSetting & DeleteCharacterSetting validation and persistence
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String(String::new()), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    let long_key = "k".repeat(MAX_SETTING_KEY_LEN + 1);
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String(long_key), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    let huge_val = "v".repeat(MAX_SETTING_VAL_SERIALIZED_BYTES + 100);
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("big".into()), PyRep::String(huge_val)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("ui_scale".into()), PyRep::Real(1.25)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let dict = get_character_settings_dict(&ctx, cid);
    assert_eq!(dict.dict_get("ui_scale"), Some(&PyRep::Real(1.25)));

    // Waypoints setting syncs autopilot_waypoints and DeleteCharacterSetting clears it
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[
                PyRep::String("waypoints".into()),
                PyRep::List(vec![PyRep::Integer(30_000_144)]),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("waypoints".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("ui_scale".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Verify ClusterMiscRuntimeState::from_db round-trip
    let rehydrated = ClusterMiscRuntimeState::from_db(&ctx.db);
    assert!(rehydrated.char_settings.get(&cid).is_none_or(|m| !m.contains_key("ui_scale")));
}
