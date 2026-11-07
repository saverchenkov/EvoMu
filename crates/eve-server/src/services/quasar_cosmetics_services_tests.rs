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
        QuasarDescriptorRegistry::open_default_or_empty(),
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
        QuasarDescriptorRegistry::open_default_or_empty(),
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
