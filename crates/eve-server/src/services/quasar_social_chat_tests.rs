//! Unit tests for `quasar_social_chat` (Local Chat, Wormhole Delayed Local, Moderation Mutes, Presence).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{corp_roles, OnlineSessionRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn chat_moderation_wormhole_rules_and_helpers() {
    assert!(is_wormhole_system(31_000_005));
    assert!(!is_wormhole_system(30_000_142));
    assert!(is_delayed_local_system(31_000_005));
    assert!(is_delayed_local_system(30_100_000));
    assert!(!is_delayed_local_system(30_000_142));

    assert!(!encode_chat_member(90_000_001, 1_000_009, None, None).is_empty());
    assert!(!encode_chat_member(90_000_001, 1_000_009, Some(99_000_001), Some(500_001)).is_empty());

    assert_eq!(extract_identifier_val(Some(&DynamicValue::Uint32(77))), Some(77));
    assert_eq!(extract_duration_secs(Some(&DynamicValue::Uint64(300))), Some(300));
    assert_eq!(extract_duration_secs(Some(&DynamicValue::Int64(0))), None);

    let db = WorldRepository::with_demo_seed();
    let moderation = ChatModerationState::new(&db);
    moderation
        .mutes
        .write()
        .unwrap()
        .insert(90_000_001, (2_000, "Spam".into()));
    save_mutes_to_db(&db, &moderation.mutes.read().unwrap());

    assert_eq!(
        check_active_chat_mute(&moderation, &db, 90_000_001, 1_500),
        Some((2_000, "Spam".into()))
    );
    // Expired at 2_500 -> returns None and purges
    assert_eq!(check_active_chat_mute(&moderation, &db, 90_000_001, 2_500), None);
    assert!(load_mutes_from_db(&db).is_empty());

    let _ = drain_local_notices();
    ctx_queue_notice(NoticeScope::Broadcast, GoogleAny::default(), 123);
    assert_eq!(drain_local_notices().len(), 1);
    assert!(drain_local_notices().is_empty());
}

#[test]
fn chat_rpc_handlers_and_lifecycle_notices() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register(&router, Arc::clone(&db));

    let cid = 90_000_001_u32;
    let wh_cid = 90_000_002_u32;
    let abyss_cid = 90_000_003_u32;
    let kspace_sys = 30_000_142_u32;
    let wh_sys = 31_000_005_u32;
    let abyss_sys = 32_000_100_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
            ch.solar_system_id = kspace_sys;
            ch.alliance_id = Some(99_000_001);
            ch.war_faction_id = Some(500_001);
        }
        let mut wh_ch = snap.characters.get(&(cid as i64)).unwrap().clone();
        wh_ch.character_id = wh_cid as i64;
        wh_ch.solar_system_id = wh_sys;
        wh_ch.alliance_id = None;
        wh_ch.war_faction_id = None;
        snap.characters.insert(wh_cid as i64, wh_ch.clone());

        let mut ab_ch = wh_ch;
        ab_ch.character_id = abyss_cid as i64;
        ab_ch.solar_system_id = abyss_sys;
        snap.characters.insert(abyss_cid as i64, ab_ch);

        snap.sessions.insert(
            1,
            OnlineSessionRecord {
                session_id: 1,
                user_id: 1,
                username: "pilot1".into(),
                character_id: Some(cid as i64),
                connected_at_unix: 1_000,
                computer_hash: None,
            },
        );
        snap.sessions.insert(
            2,
            OnlineSessionRecord {
                session_id: 2,
                user_id: 2,
                username: "pilot2".into(),
                character_id: Some(wh_cid as i64),
                connected_at_unix: 1_000,
                computer_hash: None,
            },
        );
    });

    // Notice lifecycle functions for k-space and wormhole
    assert!(on_character_enter_local_chat(&router, &db, cid, kspace_sys, 1_000).is_ok());
    assert!(on_character_local_chat_membership_refreshed(&router, &db, cid, 1_000).is_ok());
    assert!(on_character_leave_local_chat(&router, &db, cid, kspace_sys, 1_000).is_ok());

    // Wormhole unrevealed character yields 0 notices on leave/refresh
    assert_eq!(on_character_local_chat_membership_refreshed(&router, &db, wh_cid, 1_000).unwrap(), 0);
    assert_eq!(on_character_leave_local_chat(&router, &db, wh_cid, wh_sys, 1_000).unwrap(), 0);

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();

    let mut k_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    k_ctx.service_role_mask = u64::MAX;

    let wh_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(wh_cid),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let abyss_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(abyss_cid),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );

    // GetMembershipListRequest
    let mem_resp = handlers["eve_public.chat.local.api.GetMembershipListRequest"](
        &k_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("k-space membership list");
    assert!(mem_resp
        .type_url
        .ends_with("eve_public.chat.local.api.GetMembershipListResponse"));

    // Abyssal deadspace rejected
    assert_eq!(
        handlers["eve_public.chat.local.api.GetMembershipListRequest"](
            &abyss_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Req"),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // BroadcastMessageRequest in k-space and wormhole (revealing speaker)
    let mut bcast = DynamicQuasarMessage::new("Broadcast");
    bcast.fields.insert(1, DynamicValue::String("o7 local".into()));
    let b_resp = handlers["eve_public.chat.local.api.BroadcastMessageRequest"](&k_ctx, &dummy_any, &bcast)
        .expect("broadcast k-space");
    assert!(b_resp
        .type_url
        .ends_with("eve_public.chat.local.api.BroadcastMessageResponse"));

    handlers["eve_public.chat.local.api.BroadcastMessageRequest"](&wh_ctx, &dummy_any, &bcast)
        .expect("broadcast wormhole reveals speaker");
    assert!(router
        .chat_moderation
        .wormhole_speakers
        .read()
        .unwrap()
        .contains(&(wh_sys, wh_cid)));
    assert!(on_character_leave_local_chat(&router, &db, wh_cid, wh_sys, 1_000).is_ok());

    // Admin MuteRequest mutes wh_cid, then wh_cid cannot broadcast
    let mut mute_msg = DynamicQuasarMessage::new("Mute");
    mute_msg.fields.insert(1, DynamicValue::Uint32(wh_cid));
    mute_msg.fields.insert(2, DynamicValue::Uint64(3_600));
    mute_msg.fields.insert(3, DynamicValue::String("Rule violation".into()));
    let mute_resp = handlers["eve_public.chat.local.api.admin.MuteRequest"](&k_ctx, &dummy_any, &mute_msg)
        .expect("admin mute");
    assert!(mute_resp
        .type_url
        .ends_with("eve_public.chat.local.api.admin.MuteResponse"));

    // Presence endpoints
    for (req, suffix) in [
        (
            "eve_public.presence.api.GetCorporationRequest",
            "eve_public.presence.api.GetCorporationResponse",
        ),
        (
            "eve_public.presence.api.GetAllianceRequest",
            "eve_public.presence.api.GetAllianceResponse",
        ),
        (
            "eve_public.presence.api.GetFactionRequest",
            "eve_public.presence.api.GetFactionResponse",
        ),
    ] {
        let r1 = handlers[req](&k_ctx, &dummy_any, &DynamicQuasarMessage::new("Req")).expect(req);
        assert!(r1.type_url.ends_with(suffix));
        let r2 = handlers[req](&wh_ctx, &dummy_any, &DynamicQuasarMessage::new("Req")).expect(req);
        assert!(r2.type_url.ends_with(suffix));
    }
}

#[cfg(test)]
#[path = "quasar_social_chat_extra_unit_tests.rs"]
mod quasar_social_chat_extra_unit_tests;
