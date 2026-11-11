//! Unit tests for `quasar_cosmetics_services`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_OK};
use eve_db::corp_roles;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_string_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(|| {
        if let Ok(reg) = QuasarDescriptorRegistry::open_default() {
            return reg;
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .unwrap();
        let manifest = root.join("data/quasar_descriptor_manifest.json");
        for anc in root.ancestors().skip(1) {
            let db = anc.join("data/quasar_descriptors.sqlite");
            if db.exists() {
                if let Ok(reg) = QuasarDescriptorRegistry::open(&db, &manifest) {
                    return Arc::new(reg);
                }
            }
        }
        QuasarDescriptorRegistry::open_default_or_empty()
    }))
}

fn sess(uid: Option<u64>, cid: Option<i64>, corp: Option<i64>, role: u64, crole: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = corp;
    s.role = role;
    s.corp_role = crole;
    s
}

fn call(router: &PublicGatewayRouter, s: &SessionState, ty: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![7; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                value,
            }),
            ..Default::default()
        },
        None,
        false,
        now,
    )
}

#[test]
fn state_persistence_ship_palette_and_quasar_config_handlers() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let st = QuasarCosmeticsState::from_db(Arc::clone(&db));
    {
        let mut d = st.data.write().unwrap();
        d.ship_logos.insert(1_000_055, (90_000_001, 98_000_001));
        d.corp_palettes.insert(98_000_001, "InitialPalette".to_string());
    }
    st.sync_to_db();

    let reloaded = QuasarCosmeticsState::from_db(Arc::clone(&db));
    assert_eq!(
        reloaded.data.read().unwrap().ship_logos.get(&1_000_055),
        Some(&(90_000_001, 98_000_001))
    );
    assert_eq!(
        reloaded.data.read().unwrap().corp_palettes.get(&98_000_001).map(String::as_str),
        Some("InitialPalette")
    );

    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_000u64;
    let member = sess(Some(2), Some(90_000_002), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let director = sess(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        service_roles::ROLE_PLAYER_DEFAULT,
        corp_roles::ROLE_DIRECTOR,
    );

    // Ship request & bubble cosmetics
    let mut ship_req = Vec::new();
    encode_varint_field(1, 1_000_055, &mut ship_req);
    let r_ship = call(&router, &member, "eve.cosmetic.ship.api.request.GetRequest", ship_req, now);
    assert_eq!(r_ship.status_code, STATUS_OK);
    assert!(r_ship.payload.is_some());

    let r_bub = call(&router, &member, "eve.cosmetic.ship.api.request.GetAllInBubbleRequest", Vec::new(), now);
    assert_eq!(r_bub.status_code, STATUS_OK);
    assert!(r_bub.payload.is_some());

    // Corporation palette (internal eve.cosmetic.corporation.palette.api.*)
    assert_eq!(
        call(&router, &member, "eve.cosmetic.corporation.palette.api.SetRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &director, "eve.cosmetic.corporation.palette.api.SetRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &member, "eve.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    let mut corp_req = Vec::new();
    encode_varint_field(1, 98_000_001, &mut corp_req);
    assert_eq!(
        call(&router, &member, "eve.cosmetic.corporation.palette.api.GetRequest", corp_req, now).status_code,
        STATUS_OK
    );

    // Quasar config
    let r_cfg = call(&router, &member, "eve.quasar.config.api.GetRequest", Vec::new(), now);
    assert_eq!(r_cfg.status_code, STATUS_OK);
    assert!(r_cfg.payload.is_some());
}

#[test]
fn publish_notices_and_handle_app_domain_events() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_100u64;
    let cid = 90_000_001i64;
    let player = sess(Some(1), Some(cid), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let _rx = router.subscribe_session(&player, Vec::new(), now);

    assert_eq!(
        publish_cosmetic_title_granted_notice(&router, cid as u32, 42, now).expect("title notice"),
        1
    );
    assert_eq!(
        publish_cosmetic_market_transaction_notice(&router, cid as u32, 777, now).expect("tx notice"),
        1
    );
    let notices = router.poll_notices(player.session_id);
    assert_eq!(notices.len(), 2);

    // Non-app event is ignored
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.other.Event"),
            value: Vec::new(),
        },
        now,
    );
    assert!(db.with_snapshot(|s| !s.quasar_state_blobs.contains_key("quasar_app_telemetry")));

    // Bookmark added (deduped)
    let mut bm_bytes = Vec::new();
    encode_varint_field(1, 501, &mut bm_bytes);
    for _ in 0..2 {
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &player,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.bookmark.Added"),
                value: bm_bytes.clone(),
            },
            now,
        );
    }

    // Action taken
    let mut act_bytes = Vec::new();
    encode_varint_field(1, 501, &mut act_bytes);
    encode_varint_field(2, 3, &mut act_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.action.Taken"),
            value: act_bytes,
        },
        now,
    );

    // Activity tracker node selected
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.activitytracker.ui.node.Selected"),
            value: bm_bytes,
        },
        now,
    );

    // Map settings changed
    let mut map_bytes = Vec::new();
    encode_string_field(1, "color_by", &mut map_bytes);
    encode_varint_field(2, 12, &mut map_bytes);
    encode_varint_field(2, 34, &mut map_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.map.settings.api.Changed"),
            value: map_bytes,
        },
        now,
    );

    // Feature preview toggled
    let mut fp_bytes = Vec::new();
    encode_string_field(1, "new_map", &mut fp_bytes);
    encode_varint_field(2, 1, &mut fp_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.settings.featurepreview.Toggled"),
            value: fp_bytes,
        },
        now,
    );

    // Helper pointer clicked
    let mut hp_bytes = Vec::new();
    encode_string_field(1, "neocom.inventory", &mut hp_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.helperpointer.ui.link.Clicked"),
            value: hp_bytes,
        },
        now,
    );

    // Launcher event
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &player,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.launcher.eve_client.GameClientStarted"),
            value: Vec::new(),
        },
        now,
    );

    let telemetry: AppTelemetryStore = db.with_snapshot(|s| {
        serde_json::from_str(s.quasar_state_blobs.get("quasar_app_telemetry").expect("telemetry blob"))
            .expect("decode telemetry")
    });
    assert_eq!(telemetry.agency_bookmarks_by_char[&cid], vec![501]);
    assert_eq!(telemetry.agency_actions_by_char[&cid].len(), 1);
    assert_eq!(telemetry.activity_nodes_by_char[&cid], vec![501]);
    assert_eq!(telemetry.feature_previews_by_char[&cid]["new_map"], true);
    assert_eq!(telemetry.helper_pointers_by_char[&cid], vec!["neocom.inventory".to_string()]);
    assert_eq!(telemetry.launcher_events_by_user[&1].len(), 1);
}

#[test]
fn market_admin_and_custom_endpoint_persistence_palette_defaults_and_telemetry_caps() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    // Invalid JSON in quasar_cosmetics blob falls back to default state cleanly
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert("quasar_cosmetics".into(), "{not-valid-json".into());
    });
    let st = QuasarCosmeticsState::from_db(Arc::clone(&db));
    assert!(st.data.read().unwrap().ship_logos.is_empty());

    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_200u64;
    let player = sess(
        Some(2),
        Some(90_000_002),
        Some(98_000_001),
        service_roles::ROLE_PLAYER_DEFAULT,
        0,
    );
    let admin = sess(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        service_roles::ROLE_ADMIN,
        0,
    );

    // GetRequest with empty payload is rejected with STATUS_BAD_REQUEST
    let r_ship_default = call(
        &router,
        &player,
        "eve.cosmetic.ship.api.request.GetRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_ship_default.status_code, STATUS_BAD_REQUEST);

    // Corporation palette SetRequest allows corp role bit 0x02, rejects 0x04
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&90_000_002) {
            ch.corporation_id = 98_000_001;
        }
        if let Some(corp) = s.corporations.get_mut(&98_000_001) {
            corp.member_roles.insert(90_000_002, 0x04);
        }
    });
    assert_eq!(
        call(
            &router,
            &player,
            "eve.cosmetic.corporation.palette.api.SetRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&98_000_001) {
            corp.member_roles.insert(90_000_002, 0x02);
        }
    });
    assert_eq!(
        call(
            &router,
            &player,
            "eve.cosmetic.corporation.palette.api.SetRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    db.with_mut_snapshot(|s| {
        if let Some(corp) = s.corporations.get_mut(&98_000_001) {
            corp.member_roles.insert(90_000_002, 0);
        }
    });

    // Corp palette GetRequest (unknown corp defaults empty) and GetOwnRequest
    let r_corp_own = call(
        &router,
        &admin,
        "eve.cosmetic.corporation.palette.api.GetOwnRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_corp_own.status_code, STATUS_OK);
    let mut unk_corp = Vec::new();
    encode_varint_field(1, 98_999_999, &mut unk_corp);
    let r_corp_unk = call(
        &router,
        &player,
        "eve.cosmetic.corporation.palette.api.GetRequest",
        unk_corp,
        now,
    );
    assert_eq!(r_corp_unk.status_code, STATUS_OK);

    // Admin market endpoint rejects non-admin and validates UUID for admin
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &admin,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            Vec::new(),
            now,
        )
        .status_code,
        crate::quasar_gateway::STATUS_BAD_REQUEST
    );

    // Read-only / listing market endpoints return STATUS_OK on empty payloads
    for req_ty in [
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
        "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
    ] {
        assert_eq!(
            call(&router, &player, req_ty, Vec::new(), now).status_code,
            STATUS_OK,
            "expected STATUS_OK for {req_ty}"
        );
    }
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            Vec::new(),
            now,
        )
        .status_code,
        crate::quasar_gateway::STATUS_BAD_REQUEST
    );

    // Telemetry: bad protobuf bytes for known eve_public.app.* event doesn't panic or write
    let cid = 90_000_001_i64;
    let char_sess = sess(
        Some(1),
        Some(cid),
        Some(98_000_001),
        service_roles::ROLE_PLAYER_DEFAULT,
        0,
    );
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &char_sess,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.bookmark.Added"
            ),
            value: vec![0xFF, 0xFF, 0xFF],
        },
        now,
    );

    // No character_id -> char-scoped event ignored, launcher event with user_id=None ignored
    let no_char_no_uid = sess(None, None, None, 0, 0);
    let mut bm_bytes = Vec::new();
    encode_varint_field(1, 901, &mut bm_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &no_char_no_uid,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.bookmark.Added"
            ),
            value: bm_bytes,
        },
        now,
    );
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &no_char_no_uid,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.launcher.eve_client.GameClientStarted"
            ),
            value: Vec::new(),
        },
        now,
    );

    // Map settings & feature preview false toggle
    let mut map_bytes = Vec::new();
    encode_string_field(1, "star_mode", &mut map_bytes);
    encode_varint_field(2, 7, &mut map_bytes);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &char_sess,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.map.settings.api.Changed"
            ),
            value: map_bytes,
        },
        now,
    );
    let mut fp_off = Vec::new();
    encode_string_field(1, "new_map", &mut fp_off);
    encode_varint_field(2, 0, &mut fp_off);
    handle_app_domain_event(
        &db,
        &router.descriptors,
        &char_sess,
        &GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.settings.featurepreview.Toggled"
            ),
            value: fp_off,
        },
        now,
    );

    // Cap enforcement at 128 entries for bookmarks, actions, activity nodes, helper pointers, launcher events
    for i in 1_u64..=135 {
        let mut id_buf = Vec::new();
        encode_varint_field(1, i, &mut id_buf);
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &char_sess,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.bookmark.Added"
                ),
                value: id_buf.clone(),
            },
            now,
        );
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &char_sess,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.activitytracker.ui.node.Selected"
                ),
                value: id_buf.clone(),
            },
            now,
        );
        encode_varint_field(2, i, &mut id_buf);
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &char_sess,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.agency.ui.action.Taken"
                ),
                value: id_buf,
            },
            now,
        );
        let mut ptr_buf = Vec::new();
        encode_string_field(1, &format!("ptr_{i}"), &mut ptr_buf);
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &char_sess,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.helperpointer.ui.link.Clicked"
                ),
                value: ptr_buf,
            },
            now,
        );
        handle_app_domain_event(
            &db,
            &router.descriptors,
            &char_sess,
            &GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.launcher.eve_client.GameClientStarted"
                ),
                value: Vec::new(),
            },
            now + i,
        );
    }

    let telemetry: AppTelemetryStore = db.with_snapshot(|s| {
        serde_json::from_str(
            s.quasar_state_blobs
                .get("quasar_app_telemetry")
                .expect("telemetry blob"),
        )
        .expect("decode telemetry")
    });
    assert_eq!(telemetry.agency_bookmarks_by_char[&cid].len(), 128);
    assert_eq!(telemetry.agency_bookmarks_by_char[&cid][0], 1);
    assert_eq!(telemetry.agency_actions_by_char[&cid].len(), 128);
    assert_eq!(telemetry.agency_actions_by_char[&cid][0].0, 8);
    assert_eq!(telemetry.activity_nodes_by_char[&cid].len(), 128);
    assert_eq!(telemetry.activity_nodes_by_char[&cid][0], 8);
    assert_eq!(telemetry.helper_pointers_by_char[&cid].len(), 128);
    assert_eq!(telemetry.helper_pointers_by_char[&cid][0], "ptr_8");
    assert_eq!(telemetry.launcher_events_by_user[&1].len(), 128);
    assert_eq!(telemetry.launcher_events_by_user[&1][0].1, now + 8);
    assert_eq!(telemetry.feature_previews_by_char[&cid]["new_map"], false);
    assert!(telemetry.map_settings_by_char[&cid].contains_key("star_mode"));
}

#[test]
fn mutant_kill_rehydrate_sync_and_positive_id_filter_boundaries() {
    use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue};

    let db = Arc::new(WorldRepository::with_demo_seed());
    let st = QuasarCosmeticsState::from_db(Arc::clone(&db));
    {
        let mut d = st.data.write().unwrap();
        d.ship_logos.insert(888_001, (90_000_001, 98_000_001));
    }
    st.sync_to_db();
    db.with_snapshot(|snap| {
        let blob = snap.quasar_state_blobs.get("quasar_cosmetics").unwrap();
        assert!(blob.contains("888001"));
    });

    db.with_mut_snapshot(|snap| {
        let mut replacement = QuasarCosmeticsStateData::default();
        replacement
            .corp_palettes
            .insert(98_555_001, "RehydratedPalette".into());
        snap.quasar_state_blobs.insert(
            "quasar_cosmetics".into(),
            serde_json::to_string(&replacement).unwrap(),
        );
    });
    st.rehydrate_from_db();
    {
        let d = st.data.read().unwrap();
        assert!(!d.ship_logos.contains_key(&888_001));
        assert_eq!(
            d.corp_palettes.get(&98_555_001).map(String::as_str),
            Some("RehydratedPalette")
        );
    }

    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        shared_sde(),
    );
    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");

    let zero_corp_dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(0),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let zero_char_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(0),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    let ok_dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // Ship GetRequest: 0 and -5 rejected (BAD_REQUEST); positive accepted
    let ship_get = &handlers["eve.cosmetic.ship.api.request.GetRequest"];
    for bad in [0_i64, -5] {
        let mut m = DynamicQuasarMessage::new("ShipGet");
        m.fields.insert(1, DynamicValue::Int64(bad));
        assert_eq!(
            ship_get(&ok_dir_ctx, &dummy_any, &m).unwrap_err().0,
            crate::quasar_gateway::STATUS_BAD_REQUEST
        );
    }
    let mut good_ship = DynamicQuasarMessage::new("ShipGet");
    good_ship.fields.insert(1, DynamicValue::Int64(1_000_055));
    assert!(ship_get(&ok_dir_ctx, &dummy_any, &good_ship).is_ok());

    // Corporation palette GetRequest: both field 1 and ctx corp_id require > 0
    let pal_get = &handlers["eve.cosmetic.corporation.palette.api.GetRequest"];
    for bad in [0_i64, -7] {
        let mut m = DynamicQuasarMessage::new("PalGet");
        m.fields.insert(1, DynamicValue::Int64(bad));
        assert_eq!(
            pal_get(&zero_corp_dir_ctx, &dummy_any, &m).unwrap_err().0,
            crate::quasar_gateway::STATUS_BAD_REQUEST
        );
    }
    assert_eq!(
        pal_get(&zero_corp_dir_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        crate::quasar_gateway::STATUS_BAD_REQUEST
    );
    let mut explicit_corp = DynamicQuasarMessage::new("PalGet");
    explicit_corp
        .fields
        .insert(1, DynamicValue::Int64(98_000_001));
    assert!(pal_get(&zero_corp_dir_ctx, &dummy_any, &explicit_corp).is_ok());
    assert!(pal_get(&ok_dir_ctx, &dummy_any, &empty_msg).is_ok());

    // Corporation palette GetOwnRequest & SetRequest: corp_id == 0 rejected with STATUS_UNAUTHORIZED
    for ep in [
        "eve.cosmetic.corporation.palette.api.GetOwnRequest",
        "eve.cosmetic.corporation.palette.api.SetRequest",
    ] {
        let h = &handlers[ep];
        assert_eq!(
            h(&zero_corp_dir_ctx, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            crate::quasar_gateway::STATUS_UNAUTHORIZED
        );
        assert!(h(&ok_dir_ctx, &dummy_any, &empty_msg).is_ok());
    }

    // Market SKINR endpoints: char_id == 0 rejected with STATUS_UNAUTHORIZED
    let market_tax = &handlers["eve_public.cosmetic.market.api.GetTaxRateRequest"];
    assert_eq!(
        market_tax(&zero_char_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        crate::quasar_gateway::STATUS_UNAUTHORIZED
    );
    assert!(market_tax(&ok_dir_ctx, &dummy_any, &empty_msg).is_ok());
}
