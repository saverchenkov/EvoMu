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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutant_killers_load_slot_online_sort_db_blob_horizon_fallback_and_full_persistence() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_jita = 30_000_142_u32;
    let kw = HashMap::new();
    let mut sess = auth_session(1, cid, 98_000_001, sys_jita);

    // 1. Adjacent systems sorted by online count (not total DB characters!) in get_or_create_offered_load_slot
    let mut neighbors: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == sys_jita)
        .map(|j| j.to_solar_system_id)
        .collect();
    neighbors.sort_unstable();
    neighbors.dedup();
    assert!(neighbors.len() >= 2, "Jita should have >=2 adjacent systems");
    let busy_neighbor = neighbors[0];
    let other_neighbors = &neighbors[1..];

    // Put 2 online chars in busy_neighbor and 4 offline chars in other_neighbors[0]
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&cid].clone();
        let base_acc = snap
            .accounts
            .values()
            .next()
            .expect("seed account")
            .clone();
        for i in 0..2 {
            let mut acc = base_acc.clone();
            acc.user_id = 1_100 + i as u64;
            acc.username = format!("load_user_{i}");
            snap.accounts.insert(acc.username.clone(), acc);

            let new_cid = 91_000_100 + i;
            let mut c = base_ch.clone();
            c.character_id = new_cid;
            c.user_id = 1_100 + i as u64;
            c.solar_system_id = busy_neighbor;
            snap.characters.insert(new_cid, c);
        }
        for i in 0..4 {
            let off_cid = 91_000_200 + i;
            let mut c = base_ch.clone();
            c.character_id = off_cid;
            c.user_id = 1_200 + i as u64;
            c.solar_system_id = other_neighbors[0];
            snap.characters.insert(off_cid, c);
        }
    });
    for i in 0..2 {
        let new_cid = 91_000_100 + i;
        ctx.db
            .acquire_online_lock_with_computer_hash(
                (1_100 + i) as u64,
                11_000 + i as u64,
                Some(new_cid),
                None,
                true,
                ctx.now_unix(),
            )
            .unwrap();
    }
    // Cap sys_jita to 0 so offer triggers immediately and must NOT choose busy_neighbor
    with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(sys_jita, 0);
    });
    let (slot_key, chosen_alt) = get_or_create_offered_load_slot(&ctx, cid, sys_jita).unwrap();
    assert_ne!(chosen_alt, busy_neighbor);
    assert!(other_neighbors.contains(&chosen_alt));
    // Single-use consume: first succeeds, second returns false
    assert!(validate_and_consume_offered_load_slot(&ctx, cid, chosen_alt, &slot_key));
    assert!(!validate_and_consume_offered_load_slot(&ctx, cid, chosen_alt, &slot_key));

    // 2. is_zarzakh_horizon_siege_active falls back to DB entity_kv / blob `zarzakh_horizon_siege_active` when in-memory phase is Normal
    set_zarzakh_horizon_siege_active(&ctx, false);
    assert!(!is_zarzakh_horizon_siege_active(&ctx));
    ctx.db.with_mut_snapshot(|snap| {
        let _ = snap
            .kv_mut(eve_db::Ns::ZarzakhHorizonSiegeActive)
            .set(0, "", &true);
    });
    assert!(is_zarzakh_horizon_siege_active(&ctx));
    ctx.db.with_mut_snapshot(|snap| {
        let _ = snap
            .kv_mut(eve_db::Ns::ZarzakhHorizonSiegeActive)
            .set(0, "", &false);
    });
    assert!(!is_zarzakh_horizon_siege_active(&ctx));

    // 3. Full ClusterMiscRuntimeState::from_db round-trip for char_energy, pads with materials, target cooldown/charges, used starters, toll quotes
    let pad_sys = 30_002_537_u32;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: 104_888_001,
            type_id: 79_018,
            built_type_id: 79_018,
            solar_system_id: pad_sys,
            faction_id: 500_002,
            is_built: true,
            link_timestamp: 123,
            is_disrupted: false,
            position: glam::DVec3::new(10_000.0, 20_000.0, 30_000.0),
            required_materials: [(34, 1000)].into_iter().collect(),
            contributed_materials: [(34, 400)].into_iter().collect(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: 104_888_002,
            type_id: 999_777, // unknown type exercises (4547, 2) fallback when sde = None
            built_type_id: 999_777,
            solar_system_id: pad_sys,
            faction_id: 500_003,
            is_built: false,
            link_timestamp: 0,
            is_disrupted: false,
            position: glam::DVec3::new(15_000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.used_starter_shipcaster_chars.insert(cid);
        st.char_energy.insert(
            cid,
            crate::services::character_energy::CharacterEnergyState {
                current_energy: 42,
                max_energy: 100,
                last_update_time: 1_800_000_000,
                recharge_rate: 1.5,
                energy_rem: 0.25,
            },
        );
        st.target_cooldowns.insert(104_888_001, 1_800_000_500);
        st.target_charges.insert(104_888_001, 2);
        st.zarzakh_toll_quotes.insert(
            cid,
            PendingZarzakhTollQuote {
                from_gate_id: 105_000_011,
                to_gate_id: 30_100_000,
                ship_id: 1_000_000,
                quoted_toll_cents: eve_db::IskCents(500_000),
                expires_unix: 1_800_000_600,
            },
        );
    });
    persist_misc_state(&ctx);

    let mut bp_pad = eve_destiny::Ballpark::new(pad_sys);
    populate_shipcaster_balls_with_sde(pad_sys, &mut bp_pad, None, Some(&ctx.db));
    assert_eq!(bp_pad.ball_slim_meta[&104_888_001].group_id, 4549);
    assert_eq!(bp_pad.ball_slim_meta[&104_888_001].category_id, 65);
    assert_eq!(bp_pad.ball_slim_meta[&104_888_001].owner_id, 500_002);
    assert_eq!(bp_pad.ball_slim_meta[&104_888_002].group_id, 4547);
    assert_eq!(bp_pad.ball_slim_meta[&104_888_002].category_id, 2);
    assert_eq!(bp_pad.ball_slim_meta[&104_888_002].owner_id, 500_003);

    let rehydrated = ClusterMiscRuntimeState::from_db(&ctx.db);
    assert!(rehydrated.used_starter_shipcaster_chars.contains(&cid));
    assert_eq!(rehydrated.char_energy[&cid].current_energy, 42);
    assert_eq!(rehydrated.target_charges.get(&104_888_001), Some(&2));
    assert_eq!(
        rehydrated.zarzakh_toll_quotes[&cid].quoted_toll_cents,
        eve_db::IskCents(500_000)
    );
    let p1 = rehydrated
        .landing_pads
        .iter()
        .find(|p| p.item_id == 104_888_001)
        .unwrap();
    assert_eq!(p1.required_materials.get(&34), Some(&1000));
    assert_eq!(p1.contributed_materials.get(&34), Some(&400));

    // 4. Waypoints save/delete mutates DB autopilot_waypoints, non-string setting key fails
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "SaveCharacterSetting",
        &[PyRep::Integer(42), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    let _ = handle(
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
    .unwrap();
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&cid].autopilot_waypoints.clone()),
        vec![30_000_144]
    );
    let _ = handle(
        &ctx,
        &mut sess,
        "charMgr",
        "DeleteCharacterSetting",
        &[PyRep::String("waypoints".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx
        .db
        .with_snapshot(|s| s.characters[&cid].autopilot_waypoints.is_empty()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_misc_sde_ballpark_horizon_edges_load_slot_sorting_and_dispatch() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(1_850_000_000),
    ));
    let now = ctx.now_unix();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    // 1. Empire War HQ launcher ball population uses SDE type metadata (non-79018 branch)
    let launchers = sde_launchers(&ctx.sde);
    if let Some(empire_launcher) = launchers.iter().find(|l| matches!(l.faction_id, 500_001..=500_004)) {
        let mut bp = eve_destiny::Ballpark::new(empire_launcher.solar_system_id);
        populate_shipcaster_balls_with_sde(
            empire_launcher.solar_system_id,
            &mut bp,
            Some(&ctx.sde),
            None,
        );
        assert!(bp.balls.get(&empire_launcher.item_id).is_some());
    }

    // 2. advance_horizon_siege_at no-op & boundary transitions
    let mut siege = ZarzakhHorizonSiegeState {
        phase: HorizonSiegePhase::Warning,
        phase_started_unix: now,
        active_start_unix: now + 300,
        active_until_unix: now + 3900,
        cooldown_until_unix: now + 11_100,
        instability_points: 50,
        triggered_by_campaign_id: Some(1),
        fulcrum_restricted_to_pirates: false,
    };
    assert!(!advance_horizon_siege_at(&mut siege, now + 299));
    assert_eq!(siege.phase, HorizonSiegePhase::Warning);

    assert!(advance_horizon_siege_at(&mut siege, now + 300));
    assert_eq!(siege.phase, HorizonSiegePhase::Active);
    assert_eq!(siege.instability_points, 100);
    assert!(siege.fulcrum_restricted_to_pirates);

    assert!(!advance_horizon_siege_at(&mut siege, now + 3899));
    assert_eq!(siege.phase, HorizonSiegePhase::Active);

    assert!(advance_horizon_siege_at(&mut siege, now + 3900));
    assert_eq!(siege.phase, HorizonSiegePhase::Stabilizing);
    assert!(!siege.fulcrum_restricted_to_pirates);

    assert!(!advance_horizon_siege_at(&mut siege, now + 11_099));
    assert_eq!(siege.phase, HorizonSiegePhase::Stabilizing);

    assert!(advance_horizon_siege_at(&mut siege, now + 11_100));
    assert_eq!(siege.phase, HorizonSiegePhase::Normal);
    assert_eq!(siege.instability_points, 0);

    // Normal -> Normal is no-op
    assert!(!advance_horizon_siege_at(&mut siege, now + 12_000));

    // is_zarzakh_horizon_siege_active falls back to DB service_state_blobs when state.phase != Active
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("zarzakh_horizon_siege_active".into(), "true".into());
    });
    assert!(is_zarzakh_horizon_siege_active(&ctx));

    // 3. get_or_create_offered_load_slot picks adjacent system with fewest online chars
    let adj: Vec<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == sys_id)
        .map(|j| j.to_solar_system_id)
        .collect();
    if adj.len() >= 2 {
        let busy_adj = adj[0];
        // Put cid online in sys_id and another character online in busy_adj
        let uid1 = ctx.db.with_snapshot(|snap| snap.characters[&cid].user_id);
        let uid2 = ctx.db.with_mut_snapshot(|snap| {
            let acc_uid = 99_u64;
            snap.accounts.insert(
                "adj_test_user".into(),
                eve_db::AccountRecord {
                    user_id: acc_uid,
                    username: "adj_test_user".into(),
                    password_token: "x".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut ch2 = snap.characters[&cid].clone();
            ch2.character_id = 90_000_099;
            ch2.user_id = acc_uid;
            ch2.solar_system_id = busy_adj;
            snap.characters.insert(90_000_099, ch2);
            acc_uid
        });
        ctx.db
            .acquire_online_lock(uid1, 501, Some(cid), true, now)
            .unwrap();
        ctx.db
            .acquire_online_lock(uid2, 599, Some(90_000_099), true, now)
            .unwrap();
        with_misc_state(&ctx, |st| {
            st.system_population_caps.insert(sys_id, 1);
        });
        let (_k, alt) = get_or_create_offered_load_slot(&ctx, cid, sys_id).expect("offered slot");
        assert_ne!(alt, busy_adj, "should prefer adjacent system with 0 online chars");
    }

    // 4. check_and_apply_shipcaster_target_cooldown with 0s cooldown immediately recharges
    let pad_id = 104_000_030_i64;
    assert!(check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 0).is_ok());
    assert!(check_and_apply_shipcaster_target_cooldown(&ctx, pad_id, 0).is_ok());

    // 5. SaveCharacterSetting non-string key & DeleteCharacterSetting missing key & unknown service returns None
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "SaveCharacterSetting",
            &[PyRep::Integer(42), PyRep::Bool(true)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CustomError"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "DeleteCharacterSetting",
            &[PyRep::String("never_existed".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "__unknown_svc__",
        "__unknown_method__",
        &[],
        &kw,
    )
    .await
    .is_none());

    // 6. Direct roundtrip checks for `pad_to_db`/`pad_from_db`, `launcher_to_db`/`launcher_from_db`,
    // `starter_launcher_to_db`/`starter_launcher_from_db`
    let pad = LandingPadRecord {
        item_id: 901,
        type_id: 77_001,
        built_type_id: 77_002,
        solar_system_id: 30_000_142,
        faction_id: 500_001,
        is_built: true,
        link_timestamp: 1_700_000_000,
        is_disrupted: false,
        position: glam::DVec3::new(10.0, 20.0, 30.0),
        required_materials: BTreeMap::from([(34, 100)]),
        contributed_materials: BTreeMap::from([(34, 50)]),
    };
    let pad_rt = super::pad_from_db(super::pad_to_db(&pad));
    assert_eq!(pad_rt.item_id, 901);
    assert_eq!(pad_rt.position, glam::DVec3::new(10.0, 20.0, 30.0));
    assert_eq!(pad_rt.contributed_materials.get(&34), Some(&50));

    let launcher = LauncherRecord {
        item_id: 902,
        type_id: 77_010,
        solar_system_id: 30_000_142,
        faction_id: 500_001,
        linked_pad_ids: vec![901],
        position: glam::DVec3::new(-5.0, 15.0, 25.0),
    };
    let launcher_rt = super::launcher_from_db(super::launcher_to_db(&launcher));
    assert_eq!(launcher_rt.item_id, 902);
    assert_eq!(launcher_rt.linked_pad_ids, vec![901]);
    assert_eq!(launcher_rt.position, glam::DVec3::new(-5.0, 15.0, 25.0));

    let starter = StarterLauncherRecord {
        item_id: 903,
        type_id: 77_020,
        solar_system_id: 30_000_142,
        faction_id: 500_001,
        staging_system_id: 30_000_144,
        staging_station_id: 60_003_760,
        position: glam::DVec3::new(1.0, 2.0, 3.0),
    };
    let starter_rt = super::starter_launcher_from_db(super::starter_launcher_to_db(&starter));
    assert_eq!(starter_rt.item_id, 903);
    assert_eq!(starter_rt.staging_system_id, 30_000_144);
    assert_eq!(starter_rt.position, glam::DVec3::new(1.0, 2.0, 3.0));
}
