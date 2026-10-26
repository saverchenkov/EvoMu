//! Integration tests for the 8 Character & Account Feature rows:
//! - feat:account.queue-population-cap
//! - feat:character.character-lock
//! - feat:character.bloodline-change
//! - feat:character.portrait-upload
//! - feat:character.paperdoll-recustomize
//! - feat:character.character-titles-cosmetic
//! - feat:character.emergency-warp-logoff
//! - feat:character.energy-state

use crate::common::*;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

fn make_id_request(id: u32) -> Vec<u8> {
    let (mut out, mut id_bytes, mut uuid_bytes) = (Vec::new(), Vec::new(), [0u8; 16]);
    uuid_bytes[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_admin_grant_char_and_title(char_id: u32, title_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut char_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(1, &char_msg, &mut out);

    let mut title_msg = Vec::new();
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes[12..16].copy_from_slice(&title_id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut title_msg);
    encode_bytes_field(2, &title_msg, &mut out);
    out
}

#[tokio::test]
async fn test_feat_account_queue_pop_cap_and_character_locks() {
    // parity: feat:account.queue-population-cap, feat:character.character-lock, feat:character.bloodline-change, feat:character.portrait-upload
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(10_001);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();

    // ------------------------------------------------------------------------
    // 1. feat:character.character-lock
    // ------------------------------------------------------------------------
    let lock_type = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterLockType", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetCharacterLockType initially");
    assert_eq!(lock_type, PyRep::None);

    // Apply transfer lock.
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("character_lock:{char_id}"), "transfer".into());
    });
    let lock_type = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterLockType", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(lock_type, PyRep::Integer(1));

    // SelectCharacterID is blocked while locked.
    let select_err = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect_err("selection must be blocked when locked");
    assert_eq!(select_err.code, "CharacterLocked");

    // Clear transfer lock and test delete pending lock.
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove(&format!("character_lock:{char_id}"));
        for corp in snap.corporations.values_mut() {
            if corp.ceo_id == char_id {
                corp.ceo_id = 99_999_999;
            }
        }
    });
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "PrepareCharacterForDelete", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("PrepareCharacterForDelete");

    let lock_type = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterLockType", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(lock_type, PyRep::Integer(2));

    let select_err = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect_err("selection blocked on delete pending");
    assert_eq!(select_err.code, "CharacterLocked");

    // Cancel delete preparation and test GM lock.
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "CancelCharacterDeletePrepare", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("CancelCharacterDeletePrepare");

    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("character_lock:{char_id}"), "gm".into());
    });
    let lock_type = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterLockType", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(lock_type, PyRep::Integer(3));

    // Clear GM lock so character can be selected.
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove(&format!("character_lock:{char_id}"));
    });
    let lock_type = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "GetCharacterLockType", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();
    assert_eq!(lock_type, PyRep::None);

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID now succeeds");

    // ------------------------------------------------------------------------
    // 2. feat:account.queue-population-cap
    // ------------------------------------------------------------------------
    let cur_sys = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);

    // Normal load slot: returns {cur_sys: None}.
    let slot = ctx
        .dispatch_rpc(&mut sess, "populationCap", None, "GetCharacterLoadSlot", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetCharacterLoadSlot normal");
    let PyRep::Dict(dict) = slot else { panic!("expected Dict") };
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, PyRep::Integer(cur_sys as i64));
    assert_eq!(dict[0].1, PyRep::None);

    // Overloaded system: gate entry and offer alternative system with slotKey.
    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.system_population_caps.insert(cur_sys, 0);
    });

    let slot = ctx
        .dispatch_rpc(&mut sess, "populationCap", None, "GetCharacterLoadSlot", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("GetCharacterLoadSlot overloaded");
    let PyRep::Dict(dict) = slot else { panic!("expected Dict") };
    assert_eq!(dict.len(), 2, "overloaded system offers alternative system");
    let (alt_sys_rep, slot_key_rep) = &dict[1];
    let alt_sys = alt_sys_rep.as_i64().expect("alt_sys i64") as u32;
    let PyRep::String(slot_key) = slot_key_rep else { panic!("expected slotKey string") };

    // Move character to alternative system using offered slotKey.
    ctx.dispatch_rpc(
        &mut sess,
        "populationCap",
        None,
        "MoveCharacterToNewSystem",
        &[PyRep::Integer(char_id), PyRep::Integer(alt_sys as i64), PyRep::String(slot_key.clone())],
        &kw,
        None,
    )
    .await
    .expect("MoveCharacterToNewSystem");

    let new_sys = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().solar_system_id);
    assert_eq!(new_sys, alt_sys);

    // ------------------------------------------------------------------------
    // 3. feat:character.bloodline-change
    // ------------------------------------------------------------------------
    sess.role = 0; // non-GM player role for resculpt check
    // Invalid bloodline fails.
    let bl_err = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "UpdateCharacterBloodline", &[PyRep::Integer(char_id), PyRep::Integer(999_999)], &kw, None)
        .await
        .expect_err("invalid bloodline must fail");
    assert_eq!(bl_err.code, "InvalidBloodline");

    // Bloodline change across race requires active resculpt token.
    let ch_race = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().race_id);
    let cross_race_bl = if ch_race == 1 { 7u32 } else { 1u32 };

    let bl_req_err = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "UpdateCharacterBloodline", &[PyRep::Integer(char_id), PyRep::Integer(cross_race_bl as i64)], &kw, None)
        .await
        .expect_err("cross-race bloodline requires resculpt");
    assert_eq!(bl_req_err.code, "ResculptRequired");

    // Add resculpt token and change bloodline to cross_race_bl.
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("account_resculpt_tokens:{}", sess.user_id.unwrap()), "1".into());
    });
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "UpdateCharacterBloodline", &[PyRep::Integer(char_id), PyRep::Integer(cross_race_bl as i64)], &kw, None)
        .await
        .expect("UpdateCharacterBloodline with resculpt token");

    let (bl_id, race_id) = ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&char_id).unwrap();
        (ch.bloodline_id, ch.race_id)
    });
    assert_eq!(bl_id, cross_race_bl);
    assert_eq!(race_id, ctx.sde.bloodlines.get(&cross_race_bl).unwrap().race_id);

    // ------------------------------------------------------------------------
    // 4. feat:character.portrait-upload
    // ------------------------------------------------------------------------
    let esi_router = EsiRouter::new(Arc::clone(&ctx));
    // Verify before upload: image server returns 404 Not Found for unuploaded portrait.
    let pre_req = EsiHttpRequest::new("GET", format!("/images/characters/{char_id}/portrait?size=64"));
    let pre_resp = esi_router.handle_request(&pre_req);
    assert_eq!(pre_resp.status_code, 404, "Unuploaded portrait must return 404");

    let portrait_bytes = b"\xFF\xD8\xFF\xE0\x00\x10JFIF_MOCK_PORTRAIT_1001".to_vec();
    ctx.dispatch_rpc(
        &mut sess,
        // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
        "photoUploadSvc",
        None,
        "Upload",
        &[PyRep::Integer(char_id), PyRep::Bytes(portrait_bytes.clone())],
        &kw,
        None,
    )
    .await
    .expect("photoUploadSvc.Upload");

    // Verify snapshot storage.
    ctx.db.with_snapshot(|snap| {
        assert!(snap.service_state_blobs.contains_key(&format!("portrait:{char_id}")));
        for sz in [32, 64, 128, 256, 512, 1024] {
            assert!(snap.service_state_blobs.contains_key(&format!("portrait:{char_id}:{sz}")));
        }
    });

    // Verify ESI portrait metadata endpoint serves standard sizes.
    let esi_req = EsiHttpRequest::new("GET", format!("/characters/{char_id}/portrait"));
    let esi_resp = esi_router.handle_request(&esi_req);
    assert_eq!(esi_resp.status_code, 200);
    let esi_json: serde_json::Value = serde_json::from_slice(&esi_resp.body).expect("valid json");
    assert!(esi_json.get("px64x64").is_some());
    assert!(esi_json.get("px128x128").is_some());
    assert!(esi_json.get("px256x256").is_some());
    assert!(esi_json.get("px512x512").is_some());

    // Verify image server serves standard image sizes with image/jpeg Content-Type.
    for sz in [32, 64, 128, 256, 512, 1024] {
        let img_req = EsiHttpRequest::new("GET", format!("/images/characters/{char_id}/portrait?size={sz}"));
        let img_resp = esi_router.handle_request(&img_req);
        assert_eq!(img_resp.status_code, 200, "Image server must serve size {sz}");
        assert_eq!(img_resp.headers.get("Content-Type").map(|s| s.as_str()), Some("image/jpeg"));
        assert_eq!(img_resp.body, portrait_bytes);
    }
}

#[tokio::test]
async fn test_feat_character_paperdoll_titles_emergency_warp_and_energy() {
    // parity: feat:character.paperdoll-recustomize, feat:character.character-titles-cosmetic, feat:character.emergency-warp-logoff, feat:character.energy-state
    let ctx = Arc::new(ServerContext::new());
    let mut sess = SessionState::new(10_002);
    sess.role = 0; // non-GM session
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    let kw = HashMap::new();

    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    // ------------------------------------------------------------------------
    // 5. feat:character.paperdoll-recustomize
    // ------------------------------------------------------------------------
    let doll_payload = PyRep::keyval([
        ("hair", PyRep::String("short_dark".into())),
        ("beard", PyRep::String("trimmed".into())),
    ]);
    let portrait_payload = PyRep::keyval([("pose", PyRep::String("front".into()))]);

    // UpdateExistingCharacterLimited succeeds without resculpt token.
    let limited_res = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(char_id), doll_payload.clone(), portrait_payload.clone()],
            &kw,
            None,
        )
        .await
        .expect("UpdateExistingCharacterLimited allows clothing/hair changes");
    assert!(matches!(limited_res, PyRep::KeyVal(_)));

    // UpdateExistingCharacterFull requires a resculpt token.
    let full_err = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "UpdateExistingCharacterFull",
            &[PyRep::Integer(char_id), doll_payload.clone(), portrait_payload.clone()],
            &kw,
            None,
        )
        .await
        .expect_err("UpdateExistingCharacterFull without token must fail");
    assert_eq!(full_err.code, "UserError");
    assert_eq!(full_err.message, "NoResculptAvailable");

    // Grant resculpt token.
    let user_id = sess.user_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(format!("account_resculpt_tokens:{user_id}"), "1".into());
    });

    let full_res = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "UpdateExistingCharacterFull",
            &[PyRep::Integer(char_id), doll_payload, portrait_payload],
            &kw,
            None,
        )
        .await
        .expect("UpdateExistingCharacterFull with token");
    assert!(matches!(full_res, PyRep::KeyVal(_)));

    // Token must be consumed.
    ctx.db.with_snapshot(|snap| {
        let tok = snap.service_state_blobs.get(&format!("account_resculpt_tokens:{user_id}")).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        assert_eq!(tok, 0, "token consumed on successful full resculpt");
    });

    // ------------------------------------------------------------------------
    // 6. feat:character.character-titles-cosmetic
    // ------------------------------------------------------------------------
    let title_id = 701u32;
    // Temporarily grant GM role to grant cosmetic title.
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let grant_req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"title-grant-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.character.title.api.admin.GrantRequest"),
            value: make_admin_grant_char_and_title(char_id as u32, title_id),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let grant_rep = ctx
        .dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(grant_req.encode_proto())], &kw, None)
        .await
        .expect("GrantRequest");
    let PyRep::KeyVal(kvs) = grant_rep else { panic!("expected KeyVal") };
    let proto_bytes = kvs.iter().find(|(k, _)| k.as_str() == "response_proto").and_then(|(_, v)| match v {
        PyRep::Bytes(b) => Some(b.clone()),
        _ => None,
    }).unwrap();
    let grant_resp = EvePublicResponse::decode_proto(&proto_bytes).unwrap();
    assert_eq!(grant_resp.status_code, STATUS_OK);
    sess.role = 0; // return to normal player session

    // EquipRequest equips the title and updates character title.
    let eq_req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"title-eq-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.EquipRequest"),
            value: make_id_request(title_id),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let eq_rep = ctx
        .dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(eq_req.encode_proto())], &kw, None)
        .await
        .expect("EquipRequest");
    let PyRep::KeyVal(kvs) = eq_rep else { panic!("expected KeyVal") };
    let proto_bytes = kvs.iter().find(|(k, _)| k.as_str() == "response_proto").and_then(|(_, v)| match v {
        PyRep::Bytes(b) => Some(b.clone()),
        _ => None,
    }).unwrap();
    let eq_resp = EvePublicResponse::decode_proto(&proto_bytes).unwrap();
    assert_eq!(eq_resp.status_code, STATUS_OK);

    // Title is reflected on character.
    let ch_title = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().title.clone());
    assert_eq!(ch_title, format!("Title {title_id}"));

    // UnequipRequest unequips and clears title.
    let un_req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"title-uneq-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.UnequipRequest"),
            value: Vec::new(),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let un_rep = ctx
        .dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(un_req.encode_proto())], &kw, None)
        .await
        .expect("UnequipRequest");
    let PyRep::KeyVal(kvs) = un_rep else { panic!("expected KeyVal") };
    let proto_bytes = kvs.iter().find(|(k, _)| k.as_str() == "response_proto").and_then(|(_, v)| match v {
        PyRep::Bytes(b) => Some(b.clone()),
        _ => None,
    }).unwrap();
    let un_resp = EvePublicResponse::decode_proto(&proto_bytes).unwrap();
    assert_eq!(un_resp.status_code, STATUS_OK);
    let ch_title = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().title.clone());
    assert!(ch_title.is_empty(), "character title cleared on unequip");

    // ------------------------------------------------------------------------
    // 7. feat:character.energy-state
    // ------------------------------------------------------------------------
    let energy_rep = ctx
        .dispatch_rpc(&mut sess, "characterEnergyMgr", None, "GetMyEnergyState", &[], &kw, None)
        .await
        .expect("GetMyEnergyState");
    let PyRep::KeyVal(e_entries) = energy_rep else { panic!("expected KeyVal energy state") };
    let cur_energy = e_entries.iter().find(|(k, _)| k.as_str() == "currentEnergy").and_then(|(_, v)| v.as_i64());
    let max_energy = e_entries.iter().find(|(k, _)| k.as_str() == "maxEnergy").and_then(|(_, v)| v.as_i64());
    let recharge = e_entries.iter().find(|(k, _)| k.as_str() == "rechargeRate");
    assert!(cur_energy.is_some() && cur_energy.unwrap() > 0);
    assert!(max_energy.is_some() && max_energy.unwrap() > 0);
    assert!(recharge.is_some());

    // ------------------------------------------------------------------------
    // 8. feat:character.emergency-warp-logoff
    // ------------------------------------------------------------------------
    // Put character into space (not in station).
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });

    let disc_entry = eve_server::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("handle_character_logoff_or_disconnect must produce DisconnectedShipEntry");
    assert!(disc_entry.emergency_warp_dest.is_some(), "disconnect in space without timers triggers emergency warp");
    assert!(disc_entry.remove_at_unix > disc_entry.disconnected_at_unix);

    // Advance time past remove_at_unix to simulate timer expiry and ship removal.
    let diff = disc_entry.remove_at_unix.saturating_sub(ctx.now_unix()) + 1;
    ctx.advance_time_seconds(diff);

    let is_removed = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships.get(&char_id).map(|d| d.removed_from_space).unwrap_or(true)
    });
    assert!(is_removed, "ship must be marked removed from space after logoff timer expires");
}
