//! Tests for ESI loyalty store offers and final RPC parity closures.

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::ActiveMissionRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{with_misc_state, OfferedLoadSlot};
use eve_server::services::lp_store::{save_lp_store_offers, LPOffer};
use serde_json::json;

use crate::common;

fn validate_and_json(
    router: &EsiRouter,
    expected_status: u16,
    req: &EsiHttpRequest,
) -> (serde_json::Value, HashMap<String, String>) {
    let resp = router.handle_request(req);
    assert_eq!(resp.status_code, expected_status);
    let json_val = if resp.status_code == 304 || resp.body.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&resp.body).expect("Valid JSON response body")
    };
    (json_val, resp.headers)
}

async fn login_and_select(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) {
    let kw = HashMap::new();
    common::login_pilot(ctx, sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
}

#[tokio::test]
// DATA GAP: NPC corporation LP store offer catalogs are server-only in modern EVE and absent from V24 client FSD (SharedCache); all 6 LP store rows remain open until authoritative data is available.
async fn test_esi_loyalty_store_offers_route() {
    // parity: esi:GET /loyalty/stores/{corporation_id}/offers, feat:esi.loyalty.offers
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));

    let npc_corp_id = 1_000_125i64; // CONCORD

    // 1. Unknown corporation returns 404
    let not_found_req = EsiHttpRequest::new("GET", "/v1/loyalty/stores/999999999/offers/");
    let (parsed_err, _) = validate_and_json(&router, 404, &not_found_req);
    assert_eq!(parsed_err["error"], "Corporation 999999999 not found");

    // 2. Known corporation with no offers returns empty array (200)
    let offers_req = EsiHttpRequest::new("GET", format!("/v1/loyalty/stores/{npc_corp_id}/offers/"));
    let (empty_list, headers) = validate_and_json(&router, 200, &offers_req);
    assert_eq!(empty_list, json!([]));
    assert!(headers.contains_key("ETag") || headers.contains_key("etag"));

    // 3. Seed offers via save_lp_store_offers directly
    ctx.db.with_mut_snapshot(|snap| {
        save_lp_store_offers(
            snap,
            &[LPOffer {
                offer_id: 4001,
                corp_id: npc_corp_id,
                type_id: 17_634,
                quantity: 1,
                lp_cost: 50_000,
                isk_cost: eve_db::IskCents::from_isk(20_000_000),
                req_items: vec![(621, 1)],
                ak_cost: 100,
                min_standing: None,
            }],
        );
    });

    let (offers, headers) = validate_and_json(&router, 200, &offers_req);
    let etag = headers.get("ETag").or_else(|| headers.get("etag")).cloned().unwrap();
    assert_eq!(
        offers,
        json!([{
            "offer_id": 4001,
            "type_id": 17634,
            "quantity": 1,
            "lp_cost": 50000,
            "isk_cost": 20000000,
            "ak_cost": 100,
            "required_items": [{
                "type_id": 621,
                "quantity": 1
            }]
        }])
    );

    // 4. ETag caching returns 304 Not Modified
    let mut cond_req = offers_req.clone();
    cond_req.headers.insert("if-none-match".to_string(), etag);
    let (body304, _) = validate_and_json(&router, 304, &cond_req);
    assert_eq!(body304, serde_json::Value::Null);

    // 5. Offers for another corporation in lp_store_offers service blob
    let other_corp_id = 1_000_130i64;
    ctx.db.with_mut_snapshot(|snap| {
        save_lp_store_offers(
            snap,
            &[LPOffer {
                offer_id: 5001,
                corp_id: other_corp_id,
                type_id: 587,
                quantity: 2,
                lp_cost: 15_000,
                isk_cost: eve_db::IskCents(10_000_000),
                req_items: vec![(603, 1)],
                ak_cost: 0,
                min_standing: None,
            }],
        );
    });
    let other_req = EsiHttpRequest::new("GET", format!("/v1/loyalty/stores/{other_corp_id}/offers/"));
    let (offers2, _) = validate_and_json(&router, 200, &other_req);
    assert_eq!(
        offers2,
        json!([{
            "offer_id": 5001,
            "type_id": 587,
            "quantity": 2,
            "lp_cost": 15000,
            "isk_cost": 100000,
            "required_items": [{
                "type_id": 603,
                "quantity": 1
            }]
        }])
    );

    // 6. Legacy fallback to snap.loyalty_store_offers is unified across ESI and LPStoreMgr
    let legacy_corp_id = 1_000_140i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.loyalty_store_offers.insert(
            legacy_corp_id,
            vec![eve_db::LoyaltyStoreOfferRecord {
                offer_id: 6001,
                type_id: 621,
                quantity: 1,
                lp_cost: 20_000,
                isk_cost: 5_000_000,
                ak_cost: None,
                required_items: vec![],
            }],
        );
    });
    let legacy_req = EsiHttpRequest::new("GET", format!("/v1/loyalty/stores/{legacy_corp_id}/offers/"));
    let (legacy_offers, _) = validate_and_json(&router, 200, &legacy_req);
    assert_eq!(
        legacy_offers,
        json!([{
            "offer_id": 6001,
            "type_id": 621,
            "quantity": 1,
            "lp_cost": 20000,
            "isk_cost": 5000000,
            "required_items": []
        }])
    );
}

#[tokio::test]
// parity: rpc:XmppChatMgr.GetDeprecatedPrefsFallback
// parity: rpc:communityTrackMgr.change_qa_mode
// parity: rpc:populationCap.MoveCharacterToNewSystem
async fn test_chat_community_track_and_population_cap() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(31_001);
    login_and_select(&ctx, &mut sess, 90_000_001).await;

    let char_id = sess.character_id.expect("char_id");

    // 1. XmppChatMgr.GetDeprecatedPrefsFallback returns Dict with joined channel settings
    let prefs = ctx
        .dispatch_rpc(&mut sess, "XmppChatMgr", None, "GetDeprecatedPrefsFallback", &[], &kw, None)
        .await
        .expect("GetDeprecatedPrefsFallback");
    let PyRep::Dict(prefs_map) = prefs else { panic!("Expected Dict from GetDeprecatedPrefsFallback") };
    assert!(prefs_map.iter().any(|(k, _)| k == &PyRep::String("solarsystemid2:30000142".into())));

    // 2. communityTrackMgr.change_qa_mode requires QA/GM role
    let normal_role_err = ctx
        .dispatch_rpc(
            &mut sess,
            "communityTrackMgr",
            None,
            "change_qa_mode",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(normal_role_err.code, "AccessDenied"); // rpc-lint: allow error code assertion

    // Elevate session to GM role
    sess.role = eve_machonet::service_roles::ROLE_GMH;
    let qa_res = ctx
        .dispatch_rpc(
            &mut sess,
            "communityTrackMgr",
            None,
            "change_qa_mode",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("change_qa_mode");
    assert_eq!(qa_res, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs.get("community_track_qa_mode"),
            Some(&"true".to_string())
        );
    });

    // 3. populationCap.MoveCharacterToNewSystem
    // Verify unowned character fails
    let fake_char_id = 99_999_999i64;
    let unowned_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(fake_char_id),
                PyRep::Integer(30_000_142),
                PyRep::String("invalid_key".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(unowned_err.code, "CharacterOwnershipError");

    // Verify unoffered slot key fails
    let target_sys = 30_000_142u32;
    let unoffered_err = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(char_id),
                PyRep::Integer(target_sys as i64),
                PyRep::String("nonexistent_slot_key".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(unoffered_err.code, "SystemNotOffered");

    // Offer a valid load slot
    let valid_slot_key = "test_slot_key_123".to_string();
    with_misc_state(&ctx, |st| {
        st.offered_load_slots.insert(
            char_id,
            OfferedLoadSlot {
                slot_key: valid_slot_key.clone(),
                original_system_id: 30_000_144,
                offered_system_ids: vec![target_sys],
                expires_unix: ctx.now_unix() + 300,
            },
        );
    });

    // Perform valid MoveCharacterToNewSystem
    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "populationCap",
            None,
            "MoveCharacterToNewSystem",
            &[
                PyRep::Integer(char_id),
                PyRep::Integer(target_sys as i64),
                PyRep::String(valid_slot_key.clone()),
            ],
            &kw,
            None,
        )
        .await
        .expect("MoveCharacterToNewSystem");
    assert_eq!(move_res, PyRep::None);

    // Verify character solar system moved and slot consumed
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).expect("character in snapshot");
        assert_eq!(ch.solar_system_id, target_sys);
    });
    with_misc_state(&ctx, |st| {
        assert!(!st.offered_load_slots.contains_key(&char_id));
    });
}

#[tokio::test]
// parity: rpc:epicArcStatus.AgentHasEpicMissionsForCharacter
// parity: rpc:rewardMgr.GetDelayedRewardsByGroupIDs
async fn test_epic_arc_status_and_delayed_rewards() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(31_002);
    login_and_select(&ctx, &mut sess, 90_000_001).await;

    let char_id = sess.character_id.expect("char_id");
    let non_epic_id = 999_999_999i64;
    let epic_agent_id = 3_019_356i64;

    // 1. epicArcStatus.AgentHasEpicMissionsForCharacter on non-epic agent returns false
    let has_none = ctx
        .dispatch_rpc(
            &mut sess,
            "epicArcStatus",
            None,
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(non_epic_id)],
            &kw,
            None,
        )
        .await
        .expect("AgentHasEpicMissionsForCharacter");
    assert_eq!(has_none, PyRep::Bool(false));

    // Seed active mission for epic agent
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 99_001),
            ActiveMissionRecord {
                mission_id: 99_001,
                character_id: char_id,
                agent_id: epic_agent_id,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![], bookmark_id: None,
            },
        );
    });

    let has_mission = ctx
        .dispatch_rpc(
            &mut sess,
            "epicArcStatus",
            None,
            "AgentHasEpicMissionsForCharacter",
            &[PyRep::Integer(epic_agent_id)],
            &kw,
            None,
        )
        .await
        .expect("AgentHasEpicMissionsForCharacter with mission");
    assert_eq!(has_mission, PyRep::Bool(true));

    // 2. rewardMgr.GetDelayedRewardsByGroupIDs
    let default_groups = ctx
        .dispatch_rpc(
            &mut sess,
            "rewardMgr",
            None,
            "GetDelayedRewardsByGroupIDs",
            &[PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)])],
            &kw,
            None,
        )
        .await
        .expect("GetDelayedRewardsByGroupIDs");
    let PyRep::Dict(groups_dict) = &default_groups else { panic!("Expected Dict") };
    assert_eq!(groups_dict.len(), 2);
    assert_eq!(groups_dict[0].1, PyRep::List(vec![]));
    assert_eq!(groups_dict[1].1, PyRep::List(vec![]));

    // Seed delayed rewards in service_state_blobs
    ctx.db.with_mut_snapshot(|snap| {
        let key = format!("delayed_rewards:{char_id}");
        let val = json!({
            "101": [{
                "isk": 5_000_000,
                "lp": 2_500
            }]
        });
        snap.service_state_blobs.insert(key, val.to_string());
    });

    let seeded_groups = ctx
        .dispatch_rpc(
            &mut sess,
            "rewardMgr",
            None,
            "GetDelayedRewardsByGroupIDs",
            &[PyRep::List(vec![PyRep::Integer(101), PyRep::Integer(102)])],
            &kw,
            None,
        )
        .await
        .expect("GetDelayedRewardsByGroupIDs seeded");
    let PyRep::Dict(s_dict) = &seeded_groups else { panic!("Expected Dict") };
    assert_eq!(s_dict.len(), 2);
    let entry_101 = s_dict.iter().find(|(k, _)| k == &PyRep::Integer(101)).unwrap();
    let PyRep::List(items_101) = &entry_101.1 else { panic!("Expected List") };
    assert_eq!(items_101.len(), 1);
    assert_eq!(items_101[0].dict_get("isk"), Some(&PyRep::Integer(5_000_000)));
    assert_eq!(items_101[0].dict_get("lp"), Some(&PyRep::Integer(2_500)));
}

#[tokio::test]
// parity: rpc:keeper.PlayDungeon
// parity: rpc:keeper.GotoRoom
async fn test_keeper_play_and_goto_room() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(31_003);
    login_and_select(&ctx, &mut sess, 90_000_001).await;

    let sys_id = 30_000_142u32;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(900_000_001);
    sess.role = eve_machonet::service_roles::ROLE_GMH;

    // 1. keeper.PlayDungeon for dungeon 49 (which has rooms 8652, 8653, 8654)
    let play_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "PlayDungeon",
            &[PyRep::Integer(49)],
            &kw,
            None,
        )
        .await
        .expect("keeper.PlayDungeon");
    assert_eq!(play_rep.dict_get("dungeonID"), Some(&PyRep::Integer(49)));
    assert_eq!(play_rep.dict_get("roomID"), Some(&PyRep::Integer(8652)));
    assert_eq!(play_rep.dict_get("godmode"), Some(&PyRep::Bool(false)));

    // 2. keeper.GotoRoom to room 8653
    let goto_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "GotoRoom",
            &[PyRep::Integer(8653)],
            &kw,
            None,
        )
        .await
        .expect("keeper.GotoRoom");
    assert_eq!(goto_rep.dict_get("dungeonID"), Some(&PyRep::Integer(49)));
    assert_eq!(goto_rep.dict_get("roomID"), Some(&PyRep::Integer(8653)));
}

#[tokio::test]
// DATA GAP: requires server-only season schedule and challenges (rpc:seasonManager.get_season_end_time, select_season, get_season_data_for_character)
async fn test_season_manager_rpc_suite() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(31_004);
    login_and_select(&ctx, &mut sess, 90_000_001).await;

    let char_id = sess.character_id.expect("char_id");

    // 1. get_season_end_time returns valid filetime ticks > 0
    let end_time = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "get_season_end_time", &[], &kw, None)
        .await
        .expect("get_season_end_time");
    let PyRep::Integer(ft) = end_time else { panic!("Expected Integer") };
    assert!(ft > 0, "Season end time filetime ticks should be positive");

    // 2. select_season: invalid ID returns error
    let invalid_err = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "select_season",
            &[PyRep::Integer(9999)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(invalid_err.code, "InvalidSeasonId"); // rpc-lint: allow error code assertion

    // select_season: valid season 2 returns true and updates active_season
    let sel = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "select_season",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("select_season 2");
    assert_eq!(sel, PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        let key = format!("active_season:{char_id}");
        assert_eq!(snap.service_state_blobs.get(&key), Some(&"2".to_string()));
    });

    // 3. get_season_data_for_character returns season keyval
    let data = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "get_season_data_for_character",
            &[],
            &kw,
            None,
        )
        .await
        .expect("get_season_data_for_character");
    assert_eq!(data.dict_get("season_id"), Some(&PyRep::Integer(2)));
}
