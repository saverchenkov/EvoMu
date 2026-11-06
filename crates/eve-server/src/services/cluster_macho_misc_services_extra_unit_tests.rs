//! Extra branch-coverage and mutation-killing unit tests for `cluster_macho_misc_services.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::{
    check_and_apply_shipcaster_target_cooldown, default_pirate_fob_pads,
    get_character_settings_dict, get_or_create_offered_load_slot, get_zarzakh_horizon_siege_state,
    handle, is_zarzakh_horizon_siege_active, persist_misc_state, populate_shipcaster_balls,
    populate_shipcaster_balls_from_db, populate_shipcaster_balls_with_sde,
    schedule_zarzakh_horizon_siege, set_zarzakh_horizon_siege_active, tick_zarzakh_horizon_siege,
    trigger_zarzakh_horizon_siege, validate_and_consume_offered_load_slot, with_misc_state,
    ClusterMiscRuntimeState, HorizonSiegePhase, LandingPadRecord, HORIZON_SIEGE_ACTIVE_SECS,
    HORIZON_SIEGE_COOLDOWN_SECS, HORIZON_SIEGE_WARNING_SECS,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};

async fn call_misc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, service, method, args, &kw)
        .await
        .expect("misc RPC handled")
}

#[test]
fn landing_pad_timestamp_boundaries_ballpark_variants_and_persistence_roundtrip() {
    // link_timestamp == 0 and < 0 -> isLinked is false, but canBeLinked is true when built & not disrupted
    for (ts, expected_linked) in [(-5_i64, false), (0, false), (1, true)] {
        let pad = LandingPadRecord {
            item_id: 10,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: 30_000_142,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: ts,
            is_disrupted: false,
            position: glam::DVec3::ZERO,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        };
        let kv = pad.to_keyval();
        assert_eq!(kv.dict_get("isLinked"), Some(&PyRep::Bool(expected_linked)));
        assert_eq!(kv.dict_get("canBeLinked"), Some(&PyRep::Bool(true)));
    }

    // default_pirate_fob_pads with now_unix == 0 clamps link_timestamp to 1
    let pads_zero = default_pirate_fob_pads(0);
    assert_eq!(pads_zero.len(), 3);
    assert_eq!(pads_zero[0].link_timestamp, 1);

    let ctx = test_ctx();
    // populate_shipcaster_balls (no SDE, no DB) populates default pirate FOB pad in 30_002_056
    let mut bp_fob = eve_destiny::Ballpark::new(30_002_056);
    populate_shipcaster_balls(30_002_056, &mut bp_fob);
    assert!(bp_fob.balls.get(&104_000_020).is_some());

    // Seed a custom built Empire pad in 30_000_142, initialize SDE launchers, and persist
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: 104_999_001,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: 30_000_142,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: 100,
            is_disrupted: false,
            position: glam::DVec3::new(50_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
    });
    persist_misc_state(&ctx);

    // populate_shipcaster_balls_from_db reads persisted pads & Turnur entry beacon (30_002_086)
    let mut bp_jita = eve_destiny::Ballpark::new(30_000_142);
    populate_shipcaster_balls_from_db(30_000_142, &mut bp_jita, Some(&ctx.db));
    assert!(bp_jita.balls.get(&104_999_001).is_some());

    let mut bp_turnur = eve_destiny::Ballpark::new(30_002_086);
    populate_shipcaster_balls_with_sde(30_002_086, &mut bp_turnur, Some(&ctx.sde), Some(&ctx.db));
    assert!(bp_turnur.balls.get(&105_000_011).is_some());

    // Verify ClusterMiscRuntimeState::from_db restores persisted_shipcaster_state and re-adds missing Zarzakh beacons
    with_misc_state(&ctx, |st| {
        st.launchers.retain(|l| l.item_id != 105_000_010 && l.item_id != 105_000_011);
    });
    persist_misc_state(&ctx);
    let mut restored = ClusterMiscRuntimeState::from_db(&ctx.db);
    assert!(restored.sde_initialized);
    assert!(restored.launchers.iter().any(|l| l.item_id == 105_000_010));
    assert!(restored.launchers.iter().any(|l| l.item_id == 105_000_011));
    restored.ensure_sde_initialized(&ctx.sde);
}

#[test]
fn horizon_siege_direct_skip_transitions_instability_cap_and_active_preservation() {
    let ctx = test_ctx();
    let now = ctx.now_unix();

    // Schedule twice in Warning -> instability_points goes 50 -> 100 and caps at 100 on 3rd schedule
    let s1 = schedule_zarzakh_horizon_siege(&ctx, Some(10));
    assert_eq!(s1.phase, HorizonSiegePhase::Warning);
    assert_eq!(s1.instability_points, 50);

    let s2 = schedule_zarzakh_horizon_siege(&ctx, Some(11));
    assert_eq!(s2.instability_points, 100);
    let s3 = schedule_zarzakh_horizon_siege(&ctx, Some(12));
    assert_eq!(s3.instability_points, 100);

    // Jump directly from Warning to Stabilizing (now >= active_until && now < cooldown_until)
    let st_warn_to_stab = tick_zarzakh_horizon_siege(
        &ctx,
        now + HORIZON_SIEGE_WARNING_SECS + HORIZON_SIEGE_ACTIVE_SECS + 10,
    );
    assert_eq!(st_warn_to_stab.phase, HorizonSiegePhase::Stabilizing);
    assert!(!st_warn_to_stab.fulcrum_restricted_to_pirates);

    // Re-schedule to Warning, then jump directly from Warning to Normal (now >= cooldown_until)
    let _ = schedule_zarzakh_horizon_siege(&ctx, Some(13));
    let st_warn_to_norm = tick_zarzakh_horizon_siege(
        &ctx,
        now + HORIZON_SIEGE_WARNING_SECS
            + HORIZON_SIEGE_ACTIVE_SECS
            + HORIZON_SIEGE_COOLDOWN_SECS
            + 500,
    );
    assert_eq!(st_warn_to_norm.phase, HorizonSiegePhase::Normal);
    assert_eq!(st_warn_to_norm.instability_points, 0);

    // Trigger Active with duration 0 (clamped to 1), then verify schedule_zarzakh_horizon_siege does NOT downgrade Active to Warning
    let st_act = trigger_zarzakh_horizon_siege(&ctx, Some(77), 0);
    assert_eq!(st_act.phase, HorizonSiegePhase::Active);
    assert_eq!(st_act.active_until_unix, now + 1);
    let st_still_act = schedule_zarzakh_horizon_siege(&ctx, Some(88));
    assert_eq!(st_still_act.phase, HorizonSiegePhase::Active);
    assert_eq!(st_still_act.triggered_by_campaign_id, Some(77));

    // Jump directly from Active to Normal (skipping Stabilizing when now >= cooldown_until)
    let st_act_to_norm =
        tick_zarzakh_horizon_siege(&ctx, now + 1 + HORIZON_SIEGE_COOLDOWN_SECS + 10);
    assert_eq!(st_act_to_norm.phase, HorizonSiegePhase::Normal);
    assert_eq!(st_act_to_norm.instability_points, 0);

    // set_zarzakh_horizon_siege_active(true) & DB fallback check
    set_zarzakh_horizon_siege_active(&ctx, true);
    assert!(is_zarzakh_horizon_siege_active(&ctx));
    assert_eq!(
        get_zarzakh_horizon_siege_state(&ctx).phase,
        HorizonSiegePhase::Active
    );
    set_zarzakh_horizon_siege_active(&ctx, false);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));
}

#[tokio::test]
async fn load_slot_expiry_cooldown_recharge_char_settings_500_cap_and_shipcaster_fallthrough() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let wh_sys = 31_000_005_u32; // Wormhole system has no stargate jumps in SDE
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);

    // Cap = 0 in wormhole system with no adjacent stargates -> returns None
    with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(wh_sys, 0);
        st.system_population_caps.insert(sys_id, 0);
    });
    assert!(get_or_create_offered_load_slot(&ctx, cid, wh_sys).is_none());

    let (slot_key, alt_sys) = get_or_create_offered_load_slot(&ctx, cid, sys_id).unwrap();
    // Wrong char_id and wrong selected_sys fail without consuming the offer
    assert!(!validate_and_consume_offered_load_slot(
        &ctx,
        cid + 1,
        alt_sys,
        &slot_key
    ));
    assert!(!validate_and_consume_offered_load_slot(
        &ctx, cid, sys_id, &slot_key
    ));

    // Expired slot fails
    let now = ctx.now_unix();
    with_misc_state(&ctx, |st| {
        st.offered_load_slots.get_mut(&cid).unwrap().expires_unix = now - 1;
    });
    assert!(!validate_and_consume_offered_load_slot(
        &ctx, cid, alt_sys, &slot_key
    ));
    // Exact boundary expires_unix == now succeeds
    with_misc_state(&ctx, |st| {
        st.offered_load_slots.get_mut(&cid).unwrap().expires_unix = now;
    });
    assert!(validate_and_consume_offered_load_slot(
        &ctx, cid, alt_sys, &slot_key
    ));

    // Shipcaster target cooldown expiration recharges target_charges to 1 and allows next jump
    let pad_id = 104_000_021_i64;
    assert!(check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 60).is_ok());
    with_misc_state(&ctx, |st| {
        // Set expires to exact now so `now < expires` is false -> recharges and succeeds
        st.target_cooldowns.insert(pad_id, now);
    });
    assert!(check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 60).is_ok());

    // Unauthenticated SaveCharacterSetting & DeleteCharacterSetting fail
    let mut unauth = SessionState::default();
    assert!(
        call_misc(&ctx, &mut unauth, "charMgr", "SaveCharacterSetting", &[])
            .await
            .is_err()
    );
    assert!(
        call_misc(&ctx, &mut unauth, "charMgr", "DeleteCharacterSetting", &[])
            .await
            .is_err()
    );
    assert!(
        call_misc(&ctx, &mut sess, "charMgr", "DeleteCharacterSetting", &[])
            .await
            .is_err()
    );

    // Key of exact MAX_SETTING_KEY_LEN (128) with omitted value arg defaults to PyRep::Bool(true)
    let key_128 = "a".repeat(128);
    assert_eq!(
        call_misc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String(key_128.clone())]
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let dict = get_character_settings_dict(&ctx, cid);
    assert_eq!(dict.dict_get(&key_128), Some(&PyRep::Bool(true)));

    // Populate 500 settings for cid -> updating existing key succeeds, inserting 501st key fails
    with_misc_state(&ctx, |st| {
        let map = st.char_settings.entry(cid).or_default();
        map.clear();
        for i in 0..500 {
            map.insert(format!("k_{i}"), PyRep::Integer(i));
        }
    });
    assert_eq!(
        call_misc(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::String("k_0".into()), PyRep::Integer(999)]
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
    let err_501 = call_misc(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::String("k_500".into()), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_501.code, "CustomError");

    // Fallthrough to shipcaster_ops::handle
    let all_pads = call_misc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetAllLandingPads",
        &[],
    )
    .await
    .unwrap();
    assert!(!matches!(all_pads, PyRep::None));
}
