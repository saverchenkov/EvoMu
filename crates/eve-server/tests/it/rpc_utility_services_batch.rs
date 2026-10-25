//! Parity integration test suite for 11 utility and management RPCs.
//!
//! Validates live-server return shapes and state mutations for:
//! - `calendarProxy.GetEventList` & `calendarProxy.GetEventDetails`
//! - `characterEnergyMgr.GetMyEnergyState`
//! - `characterMiningLedger.GetCharacterLogs`
//! - `jumpTimers.GetTimers`
//! - `onlineStatus.GetInitialState` & `onlineStatus.GetOnlineStatus`
//! - `overviewPresetMgr.StoreLinkAndGetID` & `overviewPresetMgr.GetStoredPreset`
//! - `sharedSettingsMgr.StoreSettingLinkAndGetID` & `sharedSettingsMgr.GetStoredSharedSetting`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_i64()),
        _ => None,
    }
}

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_str()),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .and_then(|(_, v)| v.as_str()),
        _ => None,
    }
}

#[tokio::test]
async fn test_rpc_utility_services_batch_complete() {
    // parity: rpc:calendarProxy.GetEventList, rpc:calendarProxy.GetEventDetails
    // parity: rpc:characterEnergyMgr.GetMyEnergyState, rpc:characterMiningLedger.GetCharacterLogs
    // parity: rpc:jumpTimers.GetTimers, rpc:onlineStatus.GetInitialState, rpc:onlineStatus.GetOnlineStatus
    // parity: rpc:overviewPresetMgr.StoreLinkAndGetID, rpc:overviewPresetMgr.GetStoredPreset
    // parity: rpc:sharedSettingsMgr.StoreSettingLinkAndGetID, rpc:sharedSettingsMgr.GetStoredSharedSetting
    eve_testkit::attest(&[
        "rpc:calendarProxy.GetEventList",
        "rpc:calendarProxy.GetEventDetails",
        "rpc:characterEnergyMgr.GetMyEnergyState",
        "rpc:characterMiningLedger.GetCharacterLogs",
        "rpc:jumpTimers.GetTimers",
        "rpc:onlineStatus.GetInitialState",
        "rpc:onlineStatus.GetOnlineStatus",
        "rpc:overviewPresetMgr.StoreLinkAndGetID",
        "rpc:overviewPresetMgr.GetStoredPreset",
        "rpc:sharedSettingsMgr.StoreSettingLinkAndGetID",
        "rpc:sharedSettingsMgr.GetStoredSharedSetting",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    let mut sess2 = SessionState::new(1002);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT2)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    // 1. Overview preset manager: store YAML preset, retrieve by ID and hash
    let overview_yaml = "presets:\n  - tab: Default\n    bracket: true\n";
    let store_preset_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "overviewPresetMgr",
            None,
            "StoreLinkAndGetID",
            &[PyRep::String(overview_yaml.into())],
            &kw,
            None,
        )
        .await
        .expect("StoreLinkAndGetID overview");
    let PyRep::KeyVal(preset_kv) = &store_preset_res else {
        panic!("StoreLinkAndGetID must return KeyVal");
    };
    let sq_id = preset_kv
        .get("sqID")
        .and_then(|v| v.as_i64())
        .expect("sqID in preset KeyVal");
    let hashvalue = preset_kv
        .get("hashvalue")
        .and_then(|v| v.as_str())
        .expect("hashvalue in preset KeyVal");
    assert!(!hashvalue.is_empty(), "hashvalue must be non-empty");

    // Round-trip retrieval: get preset by integer sqID
    let got_by_sqid = ctx
        .dispatch_rpc(
            &mut sess2,
            "overviewPresetMgr",
            None,
            "GetStoredPreset",
            &[PyRep::Integer(sq_id)],
            &kw,
            None,
        )
        .await
        .expect("GetStoredPreset by sqID");
    assert_eq!(got_by_sqid, PyRep::String(overview_yaml.into()));

    // Round-trip retrieval: get preset by string hashvalue
    let got_by_hash = ctx
        .dispatch_rpc(
            &mut sess2,
            "overviewPresetMgr",
            None,
            "GetStoredPreset",
            &[PyRep::String(hashvalue.into())],
            &kw,
            None,
        )
        .await
        .expect("GetStoredPreset by hashvalue");
    assert_eq!(got_by_hash, PyRep::String(overview_yaml.into()));

    // 2. Shared settings manager: store setting link, retrieve by linkID and key
    let setting_type = "inventory_filter";
    let setting_yaml = "filters:\n  - ore\n  - ice\n";
    let store_setting_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "sharedSettingsMgr",
            None,
            "StoreSettingLinkAndGetID",
            &[
                PyRep::String(setting_type.into()),
                PyRep::String(setting_yaml.into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("StoreSettingLinkAndGetID");
    let PyRep::KeyVal(setting_kv) = &store_setting_res else {
        panic!("StoreSettingLinkAndGetID must return KeyVal");
    };
    let link_id = setting_kv
        .get("linkID")
        .and_then(|v| v.as_i64())
        .expect("linkID");
    let setting_key = setting_kv
        .get("key")
        .and_then(|v| v.as_str())
        .expect("key");
    assert_eq!(
        setting_kv.get("settingType"),
        Some(&PyRep::String(setting_type.into()))
    );

    // Round-trip retrieval: get shared setting by linkID
    let got_setting_by_id = ctx
        .dispatch_rpc(
            &mut sess2,
            "sharedSettingsMgr",
            None,
            "GetStoredSharedSetting",
            &[PyRep::Integer(link_id)],
            &kw,
            None,
        )
        .await
        .expect("GetStoredSharedSetting by linkID");
    assert_eq!(got_setting_by_id, PyRep::String(setting_yaml.into()));

    // Round-trip retrieval: get shared setting by string key
    let got_setting_by_key = ctx
        .dispatch_rpc(
            &mut sess2,
            "sharedSettingsMgr",
            None,
            "GetStoredSharedSetting",
            &[PyRep::String(setting_key.into())],
            &kw,
            None,
        )
        .await
        .expect("GetStoredSharedSetting by key");
    assert_eq!(got_setting_by_key, PyRep::String(setting_yaml.into()));

    // Also verify 1-argument signature and payload bound validation
    let single_yaml = "mode: test_single\n";
    let store_1arg_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "sharedSettingsMgr",
            None,
            "StoreSettingLinkAndGetID",
            &[PyRep::String(single_yaml.into())],
            &kw,
            None,
        )
        .await
        .expect("StoreSettingLinkAndGetID with 1 argument");
    let PyRep::KeyVal(single_kv) = &store_1arg_res else {
        panic!("StoreSettingLinkAndGetID 1-arg must return KeyVal");
    };
    let single_link_id = single_kv.get("linkID").and_then(|v| v.as_i64()).unwrap();
    let got_single = ctx
        .dispatch_rpc(
            &mut sess2,
            "sharedSettingsMgr",
            None,
            "GetStoredSharedSetting",
            &[PyRep::Integer(single_link_id)],
            &kw,
            None,
        )
        .await
        .expect("GetStoredSharedSetting single");
    assert_eq!(got_single, PyRep::String(single_yaml.into()));

    let oversized_payload = "a".repeat(65_537);
    let oversize_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "sharedSettingsMgr",
            None,
            "StoreSettingLinkAndGetID",
            &[PyRep::String(oversized_payload)],
            &kw,
            None,
        )
        .await;
    assert!(oversize_err.is_err(), "Payload > 64 KiB must be rejected");

    // 3. Jump timers: query for active character
    let init_jump_timers = ctx
        .dispatch_rpc(
            &mut sess1,
            "jumpTimers",
            None,
            "GetTimers",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("jumpTimers.GetTimers");
    let PyRep::KeyVal(jt_kv) = &init_jump_timers else {
        panic!("jumpTimers.GetTimers must return KeyVal");
    };
    assert!(jt_kv.contains_key("jumpActivation"));
    assert!(jt_kv.contains_key("jumpFatigue"));
    assert_eq!(jt_kv.get("jumpActivation"), Some(&PyRep::Integer(0)));
    assert_eq!(jt_kv.get("jumpFatigue"), Some(&PyRep::Integer(0)));

    // State mutation: advance character jump fatigue in DB and verify updated timers
    let future_unix = ctx.now_unix() + 1800;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT1) {
            c.jump_cooldown_until_unix = future_unix;
            c.jump_fatigue_until_unix = future_unix + 3600;
        }
    });
    let updated_jump_timers = ctx
        .dispatch_rpc(
            &mut sess1,
            "jumpTimers",
            None,
            "GetTimers",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("jumpTimers.GetTimers updated");
    let act_ft = get_i64(&updated_jump_timers, "jumpActivation").unwrap();
    let fat_ft = get_i64(&updated_jump_timers, "jumpFatigue").unwrap();
    assert!(act_ft > 0, "jumpActivation must be positive FILETIME");
    assert!(fat_ft > act_ft, "jumpFatigue must exceed activation");

    // 4. Character energy state: GetMyEnergyState
    let init_energy = ctx
        .dispatch_rpc(
            &mut sess1,
            "characterEnergyMgr",
            None,
            "GetMyEnergyState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEnergyState");
    assert_eq!(get_i64(&init_energy, "currentEnergy"), Some(5000));
    assert_eq!(get_i64(&init_energy, "maxEnergy"), Some(5000));
    assert!(get_i64(&init_energy, "lastUpdateTime").unwrap_or(0) > 0);

    // State mutation: mutate energy state via set_character_energy and verify updated state
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let mutated_state = eve_server::services::character_energy::set_character_energy(&ctx, PILOT1, 2840);
    assert_eq!(mutated_state.current_energy, 2840);
    let energy_notifs = ctx.drain_notifications_for_char(PILOT1);
    assert!(
        energy_notifs
            .iter()
            .any(|n| n.event == "OnCharacterEnergyChanged"),
        "Mutating energy must emit OnCharacterEnergyChanged"
    );

    // Query again via canonical GetMyEnergyState to verify persistent state
    let after_consume = ctx
        .dispatch_rpc(
            &mut sess1,
            "characterEnergyMgr",
            None,
            "GetMyEnergyState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEnergyState after consume");
    assert_eq!(get_i64(&after_consume, "currentEnergy"), Some(2840));

    // 5. Mining ledger: record entry and query GetCharacterLogs
    let date_str = "2027-01-15";
    let mined_type = 1230u32; // Veldspar
    let mined_qty = 5000i64;
    let sys_id = 30000142u32;
    let _ = ctx.db.record_mining_ledger_entry(
        PILOT1,
        sys_id,
        mined_type,
        mined_qty,
        ctx.now_unix(),
    );

    let logs_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "characterMiningLedger",
            None,
            "GetCharacterLogs",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetCharacterLogs");
    let PyRep::List(logs) = &logs_rep else {
        panic!("GetCharacterLogs must return PyRep::List");
    };
    assert!(!logs.is_empty(), "Mining logs must contain entry");
    let entry = logs
        .iter()
        .find(|item| {
            get_str(item, "date") == Some(date_str)
                && get_i64(item, "typeID") == Some(mined_type as i64)
        })
        .expect("Must find recorded mining log entry");
    assert_eq!(get_i64(entry, "quantity"), Some(mined_qty));
    assert_eq!(get_i64(entry, "solarSystemID"), Some(sys_id as i64));

    // 6. Online status: GetInitialState and GetOnlineStatus
    // Add Pilot 2 as watched contact of Pilot 1 in ESI contacts
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .entry(PILOT1)
        .or_default()
        .push(eve_server::esi::runtime_state::EsiContactEntry {
            contact_id: PILOT2,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![],
        });

    let init_online = ctx
        .dispatch_rpc(
            &mut sess1,
            "onlineStatus",
            None,
            "GetInitialState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetInitialState");
    let PyRep::Rowset { rows, columns, .. } = init_online else {
        panic!("GetInitialState must return Rowset");
    };
    assert_eq!(columns, vec!["contactID", "online"]);
    assert!(
        rows.iter()
            .any(|r| r[0] == PyRep::Integer(PILOT2) && r[1] == PyRep::Bool(true)),
        "Pilot 2 must be listed as online in initial state"
    );

    // Query GetOnlineStatus with list of character IDs -> returns Dict mapping charID -> bool
    let status_dict = ctx
        .dispatch_rpc(
            &mut sess1,
            "onlineStatus",
            None,
            "GetOnlineStatus",
            &[PyRep::List(vec![
                PyRep::Integer(PILOT2),
                PyRep::Integer(99_999_999),
            ])],
            &kw,
            None,
        )
        .await
        .expect("GetOnlineStatus with ID list");
    let PyRep::Dict(status_map) = &status_dict else {
        panic!("GetOnlineStatus with list must return Dict");
    };
    let p2_online = status_map
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(PILOT2))
        .map(|(_, v)| v);
    assert_eq!(p2_online, Some(&PyRep::Bool(true)));
    let offline_check = status_map
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(99_999_999))
        .map(|(_, v)| v);
    assert_eq!(offline_check, Some(&PyRep::Bool(false)));

    // Also verify single integer argument returns Bool for backward compatibility
    let single_p2 = ctx
        .dispatch_rpc(
            &mut sess1,
            "onlineStatus",
            None,
            "GetOnlineStatus",
            &[PyRep::Integer(PILOT2)],
            &kw,
            None,
        )
        .await
        .expect("GetOnlineStatus single");
    assert_eq!(single_p2, PyRep::Bool(true));

    // 7. Calendar proxy: create event, query GetEventList (year, month & month, year), GetEventDetails
    let future_time = ctx.now_blueos_filetime_ticks() + 86_400 * 10_000_000;
    let create_ev = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "CreatePersonalEvent",
            &[
                PyRep::Integer(future_time),
                PyRep::Integer(60),
                PyRep::String("Utility Fleet Op".into()),
                PyRep::String("Deep Space Expedition".into()),
                PyRep::Integer(2),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePersonalEvent");
    let ev_id = get_i64(&create_ev, "eventID").expect("eventID");

    // Query GetEventList with (year, month) = (2027, 1)
    let ev_list_ym = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarProxy",
            None,
            "GetEventList",
            &[PyRep::Integer(2027), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("calendarProxy.GetEventList (year, month)");
    let PyRep::List(rowsets_ym) = &ev_list_ym else {
        panic!("GetEventList must return List of rowsets");
    };
    assert!(!rowsets_ym.is_empty(), "Must return event rowset");
    let PyRep::Rowset { rows: ev_rows, .. } = &rowsets_ym[0] else {
        panic!("Inner item must be Rowset");
    };
    assert!(
        ev_rows.iter().any(|r| r[0] == PyRep::Integer(ev_id)),
        "Event must be found in calendarProxy.GetEventList(2027, 1)"
    );

    // Query GetEventList with (month, year) = (1, 2027)
    let ev_list_my = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarProxy",
            None,
            "GetEventList",
            &[PyRep::Integer(1), PyRep::Integer(2027)],
            &kw,
            None,
        )
        .await
        .expect("calendarProxy.GetEventList (month, year)");
    let PyRep::List(rowsets_my) = &ev_list_my else {
        panic!("GetEventList must return List");
    };
    let PyRep::Rowset { rows: my_rows, .. } = &rowsets_my[0] else {
        panic!("Inner item must be Rowset");
    };
    assert!(
        my_rows.iter().any(|r| r[0] == PyRep::Integer(ev_id)),
        "Event must be found in calendarProxy.GetEventList(1, 2027)"
    );

    // Query calendarProxy.GetEventDetails
    let ev_details = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarProxy",
            None,
            "GetEventDetails",
            &[PyRep::Integer(ev_id), PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("calendarProxy.GetEventDetails");
    assert_eq!(get_i64(&ev_details, "eventID"), Some(ev_id));
    assert_eq!(get_i64(&ev_details, "ownerID"), Some(PILOT1));
    assert_eq!(get_str(&ev_details, "eventTitle"), Some("Utility Fleet Op"));
    assert_eq!(get_i64(&ev_details, "eventDateTime"), Some(future_time));
    assert_eq!(get_i64(&ev_details, "duration"), Some(60));
    assert_eq!(get_i64(&ev_details, "importance"), Some(2));
    assert_eq!(get_i64(&ev_details, "response"), Some(4));

    // 8. Server context reload persistence: verify character energy state survives reloaded context
    let ctx_reloaded = ServerContext::with_sde_and_repository(
        std::sync::Arc::clone(&ctx.sde),
        std::sync::Arc::clone(&ctx.db),
    );
    common::pin_clock(&ctx_reloaded);
    let mut sess_reloaded = SessionState::new(1003);
    common::login_pilot(&ctx_reloaded, &mut sess_reloaded, "demo", "demo-secret").await;
    ctx_reloaded
        .dispatch_rpc(
            &mut sess_reloaded,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let reloaded_energy = ctx_reloaded
        .dispatch_rpc(
            &mut sess_reloaded,
            "characterEnergyMgr",
            None,
            "GetMyEnergyState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEnergyState on reloaded context");
    assert_eq!(
        get_i64(&reloaded_energy, "currentEnergy"),
        Some(2840),
        "Character energy state must persist across server context reload"
    );
}
