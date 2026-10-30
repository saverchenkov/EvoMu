//! Integration tests for SKIN licence bookkeeping (`shipCosmeticsMgr`) and the client UI
//! persistence services in `cosmetics_ui_ops` (overview preset links, shared setting links,
//! station guests with dock/undock broadcasts, achievements, contextual-offer tracking and
//! NES intro state).


use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state;
use std::collections::HashMap;

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    kv_entry(rep, key).and_then(|v| v.as_i64()).unwrap_or(0)
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess
}

fn grant_item(ctx: &ServerContext, owner_id: i64, location_id: i64, type_id: u32, quantity: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id,
                flag: InventoryFlag::Hangar,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

fn take_notifications(sess: &mut SessionState, event: &str) -> Vec<Vec<PyRep>> {
    let (hit, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut sess.notifications).into_iter().partition(|(e, _)| e == event);
    sess.notifications = rest;
    hit.into_iter().map(|(_, a)| a).collect()
}

fn skin_rows(rep: &PyRep) -> Vec<(i64, Option<i64>)> {
    let PyRep::List(rows) = rep else { panic!("list expected, got {rep:?}") };
    rows.iter().map(|r| (kv_i64(r, "skinID"), kv_entry(r, "expires").and_then(|v| v.as_i64()))).collect()
}

#[tokio::test]
async fn test_skin_licences_activation_listing_and_timed_expiry() {
    // parity: feat:cosmetics.skin-licenses, feat:cosmetics.skin-activate, feat:cosmetics.skin-expiry
    eve_testkit::attest(&["feat:cosmetics.skin-licenses", "feat:cosmetics.skin-activate", "feat:cosmetics.skin-expiry"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = login(&ctx, 2001, "demo", "demo-secret", char_id).await;
    let station_id = sess.station_id.expect("docked");
    let ship_id = sess.ship_id.expect("ship");
    let ship_type = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).expect("ship item"));

    // SDE-driven licence types: one permanent (duration -1) and one timed licence for the active hull.
    let perm_lic = 34_748u32;
    let timed_lic = 35_493u32;
    let perm = ctx.sde.skin_licenses.get(&perm_lic).expect("permanent licence in SDE");
    let timed = ctx.sde.skin_licenses.get(&timed_lic).expect("timed licence in SDE");
    assert!(perm.duration <= 0, "34748 must be a permanent licence in the SDE");
    assert!(timed.duration > 0, "35493 must be a timed licence in the SDE");
    for lic in [perm, timed] {
        assert!(ctx.sde.skins.get(&lic.skin_id).expect("skin").types.contains(&ship_type), "licence must fit the active hull");
    }

    // Activation requires the licence item.
    let missing = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(i64::from(perm_lic))]).await.expect_err("no item");
    assert_eq!(missing.code, "SkinLicenseItemNotFound");
    let perm_item = grant_item(&ctx, char_id, station_id, perm_lic, 1);
    let timed_item = grant_item(&ctx, char_id, station_id, timed_lic, 1);
    sess.notifications.clear();

    let perm_act = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(i64::from(perm_lic))]).await.expect("activate permanent");
    assert_eq!(kv_entry(&perm_act, "expiresAtUnix"), Some(&PyRep::None));
    let activated_at = ctx.now_unix();
    let timed_act = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(i64::from(timed_lic))]).await.expect("activate timed");
    let expires = activated_at as i64 + i64::from(timed.duration) * 86_400;
    assert_eq!(kv_i64(&timed_act, "expiresAtUnix"), expires);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&perm_item), "permanent licence item consumed");
        assert!(!snap.items.contains_key(&timed_item), "timed licence item consumed");
    });
    let activated = take_notifications(&mut sess, "OnSkinLicenseActivated");
    assert_eq!(activated, vec![
        vec![PyRep::Integer(i64::from(perm.skin_id)), PyRep::Integer(char_id)],
        vec![PyRep::Integer(i64::from(timed.skin_id)), PyRep::Integer(char_id)],
    ]);

    // Listing: both owned licences with expiry (None = permanent).
    let mut listed = skin_rows(&rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkins", &[]).await.expect("GetLicencedSkins"));
    listed.sort();
    let mut want = vec![(i64::from(perm.skin_id), None), (i64::from(timed.skin_id), Some(expires))];
    want.sort();
    assert_eq!(listed, want);
    let mut for_hull = skin_rows(&rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(i64::from(ship_type))]).await.expect("for hull"));
    for_hull.sort();
    assert_eq!(for_hull, want);
    let other_hull = ctx.sde.types.values().find(|t| t.category_id == 6 && t.published && !ctx.sde.skins.get(&perm.skin_id).unwrap().types.contains(&t.type_id) && !ctx.sde.skins.get(&timed.skin_id).unwrap().types.contains(&t.type_id)).map(|t| t.type_id).expect("another hull");
    let none = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(i64::from(other_hull))]).await.expect("other hull");
    assert_eq!(skin_rows(&none), Vec::new());
    let bad = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[PyRep::Integer(34)]).await.expect_err("Tritanium is not a hull");
    assert_eq!(bad.code, "InvalidShipTypeForSkin");

    // Wear the timed SKIN, then let it expire.
    rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(i64::from(ship_type)), PyRep::Integer(i64::from(timed_lic))]).await.expect("apply timed");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.applied_ship_skins.get(&ship_id), Some(&timed_lic)));
    ctx.advance_virtual_clock(u64::from(timed.duration.unsigned_abs()) * 86_400 - 60);
    let before_end = skin_rows(&rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkins", &[]).await.expect("before end"));
    assert_eq!(before_end.len(), 2, "timed licence still valid one minute before its end date");
    assert!(take_notifications(&mut sess, "OnSkinLicenseRemoved").is_empty());
    ctx.advance_virtual_clock(120);
    let after_end = skin_rows(&rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkins", &[]).await.expect("after end"));
    assert_eq!(after_end, vec![(i64::from(perm.skin_id), None)]);
    assert_eq!(
        take_notifications(&mut sess, "OnSkinLicenseRemoved"),
        vec![vec![
            PyRep::Integer(i64::from(timed.skin_id)),
            PyRep::Integer(ship_id),
            PyRep::Integer(i64::from(ship_type)),
        ]]
    );
    ctx.db.with_snapshot(|snap| assert_eq!(snap.applied_ship_skins.get(&ship_id), None, "ship reverts to the default skin"));
    let reapply = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(i64::from(ship_type)), PyRep::Integer(i64::from(timed_lic))]).await.expect_err("expired");
    assert_eq!(reapply.code, "SkinNotLicensed");
    rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(i64::from(ship_type)), PyRep::Integer(i64::from(perm_lic))]).await.expect("permanent still works");
}

#[tokio::test]
async fn test_ui_links_station_guests_achievements_offers_and_nes_intro() {
    // parity: feat:ui.overview-presets, feat:ui.shared-settings, feat:ui.station-guests, feat:ui.achievements, feat:ui.contextual-offers, feat:ui.nes-intro
    eve_testkit::attest(&[
        "feat:ui.overview-presets",
        "feat:ui.shared-settings",
        "feat:ui.station-guests",
        "feat:ui.achievements",
        "feat:ui.contextual-offers",
        "feat:ui.nes-intro",
    ]);

    let ctx = ServerContext::new();
    let a_id = 90_000_001;
    let b_id = 90_000_002;
    let mut a = login(&ctx, 2101, "demo", "demo-secret", a_id).await;
    let mut b = login(&ctx, 2102, "other", "other-secret", b_id).await;

    // Overview preset links: stored by A, retrievable by anyone with the link.
    let preset = PyRep::String("groups:\n  - 25\n  - 420\n".into());
    let empty = rpc(&ctx, &mut a, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String(" ".into()), preset.clone()]).await.expect_err("blank name");
    assert_eq!(empty.code, "InvalidLinkName");
    let preset_rep = rpc(&ctx, &mut a, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String("PvP".into()), preset.clone()]).await.expect("store preset");
    let PyRep::KeyVal(p_kv) = &preset_rep else { panic!("expected KeyVal") };
    let preset_id = p_kv.get("sqID").and_then(|v| v.as_i64()).unwrap();
    let p_hash = p_kv.get("hashvalue").and_then(|v| v.as_str()).unwrap();
    assert!(!p_hash.is_empty());
    let got = rpc(&ctx, &mut b, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(preset_id)]).await.expect("B reads A's preset");
    assert_eq!(got, preset);
    let got_by_hash = rpc(&ctx, &mut b, "overviewPresetMgr", "GetStoredPreset", &[PyRep::String(p_hash.into())]).await.expect("B reads A's preset by hash");
    assert_eq!(got_by_hash, preset);
    let missing = rpc(&ctx, &mut b, "overviewPresetMgr", "GetStoredPreset", &[PyRep::Integer(preset_id + 999)]).await.expect_err("unknown link");
    assert_eq!(missing.code, "StoredLinkNotFound");

    // Shared setting links are a separate namespace.
    let setting = PyRep::String("filter: ore\ncompact: true\n".into());
    let setting_rep = rpc(&ctx, &mut a, "sharedSettingsMgr", "StoreSettingLinkAndGetID", &[PyRep::String("inventory filter".into()), setting.clone()]).await.expect("store setting");
    let PyRep::KeyVal(s_kv) = &setting_rep else { panic!("expected KeyVal") };
    let setting_id = s_kv.get("linkID").and_then(|v| v.as_i64()).unwrap();
    let s_key = s_kv.get("key").and_then(|v| v.as_str()).unwrap();
    assert_ne!(setting_id, preset_id);
    let got_setting = rpc(&ctx, &mut b, "sharedSettingsMgr", "GetStoredSharedSetting", &[PyRep::Integer(setting_id)]).await.expect("B reads setting");
    assert_eq!(got_setting, setting);
    let got_setting_by_key = rpc(&ctx, &mut b, "sharedSettingsMgr", "GetStoredSharedSetting", &[PyRep::String(s_key.into())]).await.expect("B reads setting by key");
    assert_eq!(got_setting_by_key, setting);
    let cross = rpc(&ctx, &mut b, "sharedSettingsMgr", "GetStoredSharedSetting", &[PyRep::Integer(preset_id)]).await.expect_err("preset id is not a setting link");
    assert_eq!(cross.code, "StoredLinkNotFound");
    let empty_payload = rpc(&ctx, &mut a, "sharedSettingsMgr", "StoreSettingLinkAndGetID", &[PyRep::String("x".into()), PyRep::None]).await.expect_err("no payload");
    assert_eq!(empty_payload.code, "EmptyLinkPayload");

    // Station guests: both characters docked at the same station.
    let station = a.station_id.expect("A docked");
    assert_eq!(b.station_id, Some(station), "fixture characters share a home station");
    let guest_ids = |rep: &PyRep| -> Vec<i64> {
        let PyRep::List(rows) = rep else { panic!("list") };
        let mut ids: Vec<i64> = rows.iter().filter_map(|r| if let PyRep::Tuple(t) = r { t.first().and_then(|v| v.as_i64()) } else { None }).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(guest_ids(&rpc(&ctx, &mut a, "station", "GetGuests", &[]).await.expect("GetGuests")), vec![a_id, b_id]);
    ctx.drain_notifications_for_char(a_id);
    rpc(&ctx, &mut b, "stationSvc", "Undock", &[]).await.expect("B undocks");
    let left: Vec<_> = ctx.drain_notifications_for_char(a_id).into_iter().filter(|n| n.event == "OnCharNoLongerInStation").collect();
    assert_eq!(left.len(), 1);
    assert!(matches!(&left[0].args[0], PyRep::Tuple(t) if t.first() == Some(&PyRep::Integer(b_id))));
    assert_eq!(guest_ids(&rpc(&ctx, &mut a, "station", "GetGuests", &[]).await.expect("GetGuests")), vec![a_id]);
    let undocked_err = rpc(&ctx, &mut b, "station", "GetGuests", &[]).await.expect_err("B is in space");
    assert_eq!(undocked_err.code, "NotDockedInStation");
    ctx.advance_secs(12);
    rpc(&ctx, &mut b, "stationSvc", "Dock", &[PyRep::Integer(station)]).await.expect("B docks again");
    let arrived: Vec<_> = ctx.drain_notifications_for_char(a_id).into_iter().filter(|n| n.event == "OnCharNowInStation").collect();
    assert_eq!(arrived.len(), 1);
    assert!(matches!(&arrived[0].args[0], PyRep::Tuple(t) if t.first() == Some(&PyRep::Integer(b_id))));
    assert_eq!(guest_ids(&rpc(&ctx, &mut a, "station", "GetGuests", &[]).await.expect("GetGuests")), vec![a_id, b_id]);

    // Achievements and client event counters persist (counters never go backwards).
    let ach_dict = |done: Vec<i64>, ev: PyRep| {
        PyRep::Dict(vec![
            (
                PyRep::String("completedDict".into()),
                PyRep::Dict(done.into_iter().map(|id| (PyRep::Integer(id), PyRep::Bool(true))).collect()),
            ),
            (PyRep::String("eventDict".into()), ev),
        ])
    };
    let empty_state = rpc(&ctx, &mut a, "monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount", &[]).await.expect("initial");
    assert_eq!(empty_state, ach_dict(vec![], PyRep::Dict(vec![])));
    let counters = |n: i64| PyRep::Dict(vec![(PyRep::String("warps".into()), PyRep::Integer(n))]);
    rpc(&ctx, &mut a, "monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters", &[PyRep::List(vec![PyRep::Integer(202), PyRep::Integer(101)]), counters(3)]).await.expect("update");
    rpc(&ctx, &mut a, "monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters", &[PyRep::List(vec![PyRep::Integer(202)]), counters(1)]).await.expect("update again");
    let state = rpc(&ctx, &mut a, "monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount", &[]).await.expect("state");
    assert_eq!(state, ach_dict(vec![101, 202], counters(3)));
    let b_state = rpc(&ctx, &mut b, "monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount", &[]).await.expect("B state");
    assert_eq!(b_state, ach_dict(vec![], PyRep::Dict(vec![])), "achievements are per character");

    // Contextual offer tracking calls are accepted and recorded.
    for m in ["TrackOfferSeen", "TrackOfferBuyPress", "TrackOfferPurchased", "TrackOfferDelivered", "TrackOfferPurchasedSeen", "TrackOfferClosed"] {
        assert_eq!(rpc(&ctx, &mut a, "contextualOfferMgr", m, &[PyRep::Integer(77)]).await.expect(m), PyRep::None);
    }
    let log = ctx.db.with_snapshot(|snap| load_cosmetics_ui_state(snap).contextual_offer_events.get(&a_id).cloned().unwrap_or_default());
    assert_eq!(log.iter().map(|(m, o, _)| (m.as_str(), *o)).collect::<Vec<_>>(), vec![
        ("TrackOfferSeen", 77),
        ("TrackOfferBuyPress", 77),
        ("TrackOfferPurchased", 77),
        ("TrackOfferDelivered", 77),
        ("TrackOfferPurchasedSeen", 77),
        ("TrackOfferClosed", 77),
    ]);

    // NES intro state persists per character.
    assert_eq!(rpc(&ctx, &mut a, "nes_intro", "get_nes_intro_state", &[]).await.expect("get"), PyRep::Bool(false));
    assert_eq!(rpc(&ctx, &mut a, "nes_intro", "skip_nes_intro", &[]).await.expect("skip"), PyRep::None);
    assert_eq!(rpc(&ctx, &mut a, "nes_intro", "get_nes_intro_state", &[]).await.expect("get"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut b, "nes_intro", "get_nes_intro_state", &[]).await.expect("get B"), PyRep::Bool(false));
}

