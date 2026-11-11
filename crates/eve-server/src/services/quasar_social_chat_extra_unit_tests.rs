//! Extra branch-coverage and mutation-killing unit tests for `quasar_social_chat.rs`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    GatewaySubscriber, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use crate::services::quasar_social_chat::{
    check_active_chat_mute, ctx_now_unix, ctx_queue_notice, drain_local_notices,
    encode_chat_member, get_solar_system_members, is_delayed_local_system, is_wormhole_system,
    load_mutes_from_db, load_wormhole_speakers_from_db, on_character_enter_local_chat,
    on_character_leave_local_chat, on_character_local_chat_membership_refreshed, register,
    save_mutes_to_db, save_wormhole_speakers_to_db, ChatModerationState,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{corp_roles, OnlineSessionRecord, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};

fn make_sub(session_id: u64, char_id: u32, corp_id: u32, sys_id: u32) -> GatewaySubscriber {
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    GatewaySubscriber {
        session_id,
        subscribed_at_unix: 100,
        application_instance_uuid: vec![1],
        user_id: Some(session_id as i32),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        alliance_id: None,
        solar_system_id: Some(sys_id),
        fleet_id: None,
        bubble_instance_uuid: None,
        subscribed_notice_types: Vec::new(),
        sender: tx,
        inbox: VecDeque::new(),
    }
}

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router() -> (Arc<WorldRepository>, PublicGatewayRouter) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_777_000_000)));
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    register(&router, Arc::clone(&db));
    (db, router)
}

fn invoke_handler(
    router: &PublicGatewayRouter,
    name: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handlers = router.handlers.read().unwrap();
    let h = handlers.get(name).expect("registered handler");
    h(ctx, &GoogleAny::default(), msg)
}

#[test]
fn ctx_now_unix_and_system_classification_boundaries() {
    let mut ctx = AuthoritativeContext::default();
    assert_eq!(ctx_now_unix(&ctx), 0);
    ctx.received = Some(GoogleTimestamp { seconds: -42, nanos: 0 });
    assert_eq!(ctx_now_unix(&ctx), 0);
    ctx.received = Some(GoogleTimestamp { seconds: 1_750_000_123, nanos: 500 });
    assert_eq!(ctx_now_unix(&ctx), 1_750_000_123);

    // Exact boundaries for is_wormhole_system (31_000_000..32_000_000) and Zarzakh (30_100_000)
    assert!(!is_wormhole_system(30_999_999));
    assert!(is_wormhole_system(31_000_000));
    assert!(is_wormhole_system(31_999_999));
    assert!(!is_wormhole_system(32_000_000));
    assert!(!is_delayed_local_system(30_999_999));
    assert!(is_delayed_local_system(31_000_000));
    assert!(is_delayed_local_system(31_999_999));
    assert!(!is_delayed_local_system(32_000_000));
    assert!(!is_wormhole_system(30_100_000));
    assert!(is_delayed_local_system(30_100_000));
}

#[test]
fn chat_moderation_persistence_db_fallback_and_expiry_boundary() {
    let db = WorldRepository::new_empty();
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs
            .insert("quasar_chat_mutes".into(), "{broken_json".into());
    });
    assert!(load_mutes_from_db(&db).is_empty());

    let mut initial = HashMap::new();
    initial.insert(90_000_011_u32, (1_000_u64, "MuteA".to_string()));
    initial.insert(90_000_012_u32, (2_000_u64, "MuteB".to_string()));
    save_mutes_to_db(&db, &initial);

    let mod_from_db = ChatModerationState::new(&db);
    assert_eq!(mod_from_db.mutes.read().unwrap().len(), 2);

    // Clear in-memory entry for 90_000_011 to force check_active_chat_mute to load from DB
    mod_from_db.mutes.write().unwrap().remove(&90_000_011);
    assert_eq!(
        check_active_chat_mute(&mod_from_db, &db, 90_000_011, 999),
        Some((1_000, "MuteA".into()))
    );

    // Exact boundary: now_unix == expiry (1_000) is expired, purges 1_000 and keeps 2_000
    mod_from_db
        .mutes
        .write()
        .unwrap()
        .insert(90_000_011, (1_000, "MuteA".into()));
    assert_eq!(check_active_chat_mute(&mod_from_db, &db, 90_000_011, 1_000), None);
    let remaining = load_mutes_from_db(&db);
    assert!(!remaining.contains_key(&90_000_011));
    assert_eq!(remaining.get(&90_000_012), Some(&(2_000, "MuteB".into())));
    assert_eq!(check_active_chat_mute(&mod_from_db, &db, 99_999_999, 500), None);
}

#[test]
fn encode_chat_member_and_solar_system_member_filtering_branches() {
    let b_none = encode_chat_member(101, 201, None, None);
    let b_ally = encode_chat_member(101, 201, Some(301), None);
    let b_fact = encode_chat_member(101, 201, None, Some(401));
    let b_both = encode_chat_member(101, 201, Some(301), Some(401));
    assert_ne!(b_none, b_ally);
    assert_ne!(b_none, b_fact);
    assert_ne!(b_ally, b_both);
    assert_ne!(b_fact, b_both);

    let (db, router) = setup_router();
    let k_sys = 30_000_142_u32;
    let wh_sys = 31_000_005_u32;
    let other_wh = 31_000_006_u32;
    let c_session = 90_000_001_u32;
    let c_sub_only = 90_000_021_u32;
    let c_offline = 90_000_022_u32;
    let c_wh_spoken = 90_000_023_u32;
    let c_wh_silent = 90_000_024_u32;

    db.with_mut_snapshot(|snap| {
        let template = snap.characters.get(&(c_session as i64)).unwrap().clone();
        for (cid, sys) in [
            (c_session, k_sys),
            (c_sub_only, k_sys),
            (c_offline, k_sys),
            (c_wh_spoken, wh_sys),
            (c_wh_silent, wh_sys),
        ] {
            let mut ch = template.clone();
            ch.character_id = cid as i64;
            ch.solar_system_id = sys;
            snap.characters.insert(cid as i64, ch);
        }
        snap.sessions.clear();
        for (sid, cid) in [(10, c_session), (11, c_wh_spoken), (12, c_wh_silent)] {
            snap.sessions.insert(
                sid,
                OnlineSessionRecord {
                    session_id: sid,
                    user_id: sid as u64,
                    username: format!("s{sid}"),
                    character_id: Some(cid as i64),
                    connected_at_unix: 100,
                    computer_hash: None,
                },
            );
        }
    });

    router
        .subscribers
        .write()
        .unwrap()
        .insert(500, make_sub(500, c_sub_only, 1_000_009, k_sys));

    let (bytes, ids) = get_solar_system_members(
        &db,
        Some(&router.subscribers),
        k_sys,
        Some(c_session),
        Some(1_000_009),
        Some(&router.chat_moderation),
    );
    assert_eq!(bytes.len(), 2);
    assert!(ids.contains(&c_session) && ids.contains(&c_sub_only) && !ids.contains(&c_offline));

    // Wormhole without moderation state returns 0 members
    assert!(get_solar_system_members(&db, None, wh_sys, Some(c_wh_spoken), None, None)
        .1
        .is_empty());

    router
        .chat_moderation
        .wormhole_speakers
        .write()
        .unwrap()
        .extend([(other_wh, c_wh_spoken), (wh_sys, c_wh_spoken)]);
    let (_, wh_ids) = get_solar_system_members(
        &db,
        Some(&router.subscribers),
        wh_sys,
        Some(c_wh_spoken),
        None,
        Some(&router.chat_moderation),
    );
    assert_eq!(wh_ids, vec![c_wh_spoken]);
}

#[test]
fn get_membership_and_broadcast_message_error_and_notice_branches() {
    let (db, router) = setup_router();
    let k_cid = 90_000_001_u32;
    let wh_cid = 90_000_031_u32;
    let abyss_start_cid = 90_000_032_u32;
    let abyss_end_cid = 90_000_033_u32;
    let offline_cid = 90_000_034_u32;
    let missing_cid = 90_999_999_u32;
    let k_sys = 30_000_142_u32;
    let zarzakh_sys = 30_100_000_u32;

    db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&(k_cid as i64)).unwrap().clone();
        for (cid, sys) in [
            (k_cid, k_sys),
            (wh_cid, zarzakh_sys),
            (abyss_start_cid, 32_000_000),
            (abyss_end_cid, 32_999_999),
            (offline_cid, k_sys),
        ] {
            let mut ch = base.clone();
            ch.character_id = cid as i64;
            ch.solar_system_id = sys;
            snap.characters.insert(cid as i64, ch);
        }
        snap.sessions.clear();
        for (sid, cid) in [(1, k_cid), (2, wh_cid), (3, abyss_start_cid), (4, abyss_end_cid)] {
            snap.sessions.insert(
                sid,
                OnlineSessionRecord {
                    session_id: sid,
                    user_id: sid as u64,
                    username: format!("u{sid}"),
                    character_id: Some(cid as i64),
                    connected_at_unix: 100,
                    computer_hash: None,
                },
            );
        }
    });

    let anon_ctx = AuthoritativeContext::default();
    let empty_req = DynamicQuasarMessage::new("Req");
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.GetMembershipListRequest", &anon_ctx, &empty_req)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &anon_ctx, &empty_req)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let ctx_for = |cid: u32| {
        AuthoritativeContext::for_session(Some(1), Some(cid), Some(1_000_009), 0, "tq", vec![1])
    };
    let mut msg = DynamicQuasarMessage::new("Bcast");
    msg.fields.insert(1, DynamicValue::String("hello".into()));

    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.GetMembershipListRequest", &ctx_for(missing_cid), &empty_req)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(missing_cid), &msg)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    for ab_cid in [abyss_start_cid, abyss_end_cid] {
        assert_eq!(
            invoke_handler(&router, "eve_public.chat.local.api.GetMembershipListRequest", &ctx_for(ab_cid), &empty_req)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
        assert_eq!(
            invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(ab_cid), &msg)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(k_cid), &empty_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(offline_cid), &msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Delayed local (Zarzakh 30_100_000): first broadcast emits JoinNotice + MessageBroadcastNotice, second only MessageBroadcastNotice
    let _ = drain_local_notices();
    assert!(invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(wh_cid), &msg).is_ok());
    let notices_first = drain_local_notices();
    assert_eq!(notices_first.len(), 2);
    assert!(notices_first[0].1.type_url.ends_with("JoinNotice"));
    assert!(notices_first[1].1.type_url.ends_with("MessageBroadcastNotice"));
    assert_eq!(notices_first[0].2, 1_777_000_000);

    assert!(invoke_handler(&router, "eve_public.chat.local.api.BroadcastMessageRequest", &ctx_for(wh_cid), &msg).is_ok());
    let notices_second = drain_local_notices();
    assert_eq!(notices_second.len(), 1);
    assert!(notices_second[0].1.type_url.ends_with("MessageBroadcastNotice"));
}

#[test]
fn admin_mute_request_validation_and_delayed_local_notice_suppression() {
    let (db, router) = setup_router();
    let admin_cid = 90_000_001_u32;
    let wh_cid = 90_000_041_u32;
    let wh_sys = 31_000_009_u32;

    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&(admin_cid as i64)).unwrap().clone();
        ch.character_id = wh_cid as i64;
        ch.solar_system_id = wh_sys;
        snap.characters.insert(wh_cid as i64, ch);
    });

    let non_admin = AuthoritativeContext::for_session(Some(1), Some(admin_cid), Some(1_000_009), 0, "tq", vec![1]);
    let mut admin = non_admin.clone();
    admin.service_role_mask = u64::MAX;
    admin.received = Some(GoogleTimestamp { seconds: 5_000, nanos: 0 });

    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &non_admin, &DynamicQuasarMessage::new("Mute"))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    let mut bad_char = DynamicQuasarMessage::new("Mute");
    bad_char.fields.insert(1, DynamicValue::Int32(0));
    bad_char.fields.insert(2, DynamicValue::Int64(60));
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &admin, &bad_char).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let mut bad_dur = DynamicQuasarMessage::new("Mute");
    bad_dur.fields.insert(1, DynamicValue::Uint32(wh_cid));
    bad_dur.fields.insert(2, DynamicValue::Int64(-5));
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &admin, &bad_dur).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    let mut missing_target = DynamicQuasarMessage::new("Mute");
    let mut sub_char = DynamicQuasarMessage::new("CharId");
    sub_char.fields.insert(1, DynamicValue::Int64(99_888_777));
    missing_target.fields.insert(1, DynamicValue::Message(Box::new(sub_char)));
    let mut sub_dur = DynamicQuasarMessage::new("Dur");
    sub_dur.fields.insert(1, DynamicValue::Int64(120));
    missing_target.fields.insert(2, DynamicValue::Message(Box::new(sub_dur)));
    assert_eq!(
        invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &admin, &missing_target).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Muting unrevealed wormhole pilot succeeds with default reason and emits NO MembershipRefreshedNotice
    let _ = drain_local_notices();
    let mut mute_wh = DynamicQuasarMessage::new("Mute");
    let mut sub_c = DynamicQuasarMessage::new("CharId");
    sub_c.fields.insert(1, DynamicValue::Int32(wh_cid as i32));
    mute_wh.fields.insert(1, DynamicValue::Message(Box::new(sub_c)));
    let mut sub_d = DynamicQuasarMessage::new("Dur");
    sub_d.fields.insert(1, DynamicValue::Uint64(300));
    mute_wh.fields.insert(2, DynamicValue::Message(Box::new(sub_d)));
    assert!(invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &admin, &mute_wh).is_ok());
    assert!(drain_local_notices().is_empty());
    assert_eq!(
        check_active_chat_mute(&router.chat_moderation, &db, wh_cid, 5_100),
        Some((5_300, "Chat moderation mute".into()))
    );

    // Reveal wormhole pilot and mute again -> emits MembershipRefreshedNotice
    router.chat_moderation.wormhole_speakers.write().unwrap().insert((wh_sys, wh_cid));
    assert!(invoke_handler(&router, "eve_public.chat.local.api.admin.MuteRequest", &admin, &mute_wh).is_ok());
    let refreshed = drain_local_notices();
    assert_eq!(refreshed.len(), 1);
    assert!(refreshed[0].1.type_url.ends_with("MembershipRefreshedNotice"));
}

#[test]
fn presence_handlers_and_local_chat_lifecycle_edge_cases() {
    let (db, router) = setup_router();
    let cid = 90_000_001_u32;
    let wh_sys = 31_000_005_u32;

    assert_eq!(on_character_enter_local_chat(&router, &db, 99_999_999, 30_000_142, 100).unwrap(), 0);
    assert_eq!(on_character_local_chat_membership_refreshed(&router, &db, 99_999_999, 100).unwrap(), 0);

    router
        .subscribers
        .write()
        .unwrap()
        .insert(1, make_sub(1, cid, 1_000_009, wh_sys));
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
            ch.solar_system_id = wh_sys;
        }
    });
    assert_eq!(on_character_enter_local_chat(&router, &db, cid, wh_sys, 1_000).unwrap(), 1);
    router.chat_moderation.wormhole_speakers.write().unwrap().insert((wh_sys, cid));
    assert_eq!(on_character_local_chat_membership_refreshed(&router, &db, cid, 1_000).unwrap(), 1);
    assert_eq!(on_character_leave_local_chat(&router, &db, cid, wh_sys, 1_000).unwrap(), 1);

    let mut explicit_msg = DynamicQuasarMessage::new("Presence");
    explicit_msg.fields.insert(1, DynamicValue::Uint64(cid as u64));
    let anon = AuthoritativeContext::default();
    for endpoint in [
        "eve_public.presence.api.GetCorporationRequest",
        "eve_public.presence.api.GetAllianceRequest",
        "eve_public.presence.api.GetFactionRequest",
    ] {
        assert!(invoke_handler(&router, endpoint, &anon, &explicit_msg).is_ok());
        assert_eq!(
            invoke_handler(&router, endpoint, &anon, &DynamicQuasarMessage::new("Empty"))
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND
        );
    }
    let _ = (NoticeScope::Broadcast, ctx_queue_notice, corp_roles::ROLE_DIRECTOR);

    // Direct check for `save_wormhole_speakers_to_db` and `load_wormhole_speakers_from_db`
    let mut speakers = std::collections::HashSet::from([(wh_sys, cid)]);
    save_wormhole_speakers_to_db(&db, &speakers);
    assert!(load_wormhole_speakers_from_db(&db).contains(&(wh_sys, cid)));
    speakers.clear();
    save_wormhole_speakers_to_db(&db, &speakers);
    assert!(load_wormhole_speakers_from_db(&db).is_empty());
}
