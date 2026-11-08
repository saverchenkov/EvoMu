//! Extra unit and mutation-killing tests for `services::quasar_cosmetics_live`.

#![forbid(unsafe_code)]

use super::quasar_cosmetics_live::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, EvePublicResponse, GoogleAny,
    QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
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

fn sess(
    uid: Option<u64>,
    cid: Option<i64>,
    corp: Option<i64>,
    role: u64,
    crole: u64,
) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = corp;
    s.role = role;
    s.corp_role = crole;
    s
}

fn call(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![23; 16],
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

fn assert_code(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
    code: u32,
) {
    assert_eq!(call(router, s, ty, value, now).status_code, code, "{ty}");
}

#[test]
fn macho_notify_sink_alliance_logos_and_ship_skin_state_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let server_ctx = ServerContext::with_repository(Arc::clone(&db));
    install_macho_notify_sink(&server_ctx);

    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;
    let corp_id = 98_000_001i64;
    let ship_id = db.with_snapshot(|s| s.characters[&cid].active_ship_id) as u64;
    let ship_tid = db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);

    db.acquire_online_lock(1, 101, Some(cid), true, 1_760_000_000)
        .unwrap();
    emit_ship_logo_macho_notifications(
        &server_ctx.quasar_gateway.macho_notify_sink,
        &db,
        ship_id,
        0,
        PyRep::Integer(1),
    );
    assert!(!server_ctx.pending_notifications.read().unwrap()[&cid].is_empty());

    db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    emit_ship_logo_macho_notifications(
        &server_ctx.quasar_gateway.macho_notify_sink,
        &db,
        ship_id,
        0,
        PyRep::Integer(2),
    );

    let router = &PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let player = sess(Some(1), Some(cid), Some(corp_id), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let other = sess(Some(2), Some(other_cid), Some(corp_id), service_roles::ROLE_PLAYER_DEFAULT, 0);

    let mut ship_seq = Vec::new();
    encode_varint_field(1, ship_id, &mut ship_seq);
    let mut logo_id = Vec::new();
    encode_bytes_field(1, &ship_seq, &mut logo_id);
    encode_varint_field(2, 0, &mut logo_id);

    let disp_ep = "eve_public.cosmetic.ship.logo.DisplayRequest";
    let mut alliance_attr = Vec::new();
    encode_bytes_field(1, &[], &mut alliance_attr);
    let mut alliance_disp = Vec::new();
    encode_bytes_field(1, &logo_id, &mut alliance_disp);
    encode_bytes_field(2, &alliance_attr, &mut alliance_disp);

    assert_code(router, &player, disp_ep, alliance_disp.clone(), now, STATUS_FORBIDDEN);
    db.with_mut_snapshot(|s| s.characters.get_mut(&cid).unwrap().alliance_id = Some(99_000_001));
    assert_code(router, &player, disp_ep, alliance_disp.clone(), now, STATUS_FORBIDDEN);

    super::quasar_auth_user::with_identity_store(&db, |id| {
        id.char_alliance_logo_ships.entry(cid as u64).or_default().push(ship_tid as u64);
    });
    assert_code(router, &player, disp_ep, alliance_disp.clone(), now, STATUS_OK);
    assert_code(router, &other, disp_ep, alliance_disp, now, STATUS_FORBIDDEN);

    let mut empty_attr_req = Vec::new();
    encode_bytes_field(1, &logo_id, &mut empty_attr_req);
    encode_bytes_field(2, &[], &mut empty_attr_req);
    assert_code(router, &player, disp_ep, empty_attr_req, now, STATUS_BAD_REQUEST);

    // ship.api.GetRequest & GetAllInBubbleRequest with first-party skin and third-party SKINR skin
    let ship_get_ep = "eve_public.cosmetic.ship.api.GetRequest";
    db.with_mut_snapshot(|s| {
        s.licensed_skins.entry(cid).or_default().push(1001);
        s.applied_ship_skins.insert(ship_id as i64, 1001);
    });
    let mut get_ship_req = Vec::new();
    encode_bytes_field(1, &ship_seq, &mut get_ship_req);
    assert_code(router, &player, ship_get_ep, get_ship_req.clone(), now, STATUS_OK);
    let bubble_rsp = call(
        router,
        &player,
        "eve_public.cosmetic.ship.api.GetAllInBubbleRequest",
        Vec::new(),
        now,
    );
    assert!(!bubble_rsp.payload.unwrap().value.is_empty());

    db.with_mut_snapshot(|s| {
        s.applied_ship_skins.remove(&(ship_id as i64));
        let mut skinr_data = super::quasar_cosmetics_skinr::SkinrCosmeticsData::default();
        skinr_data.skin_licenses.insert(
            super::quasar_cosmetics_skinr::SkinrCosmeticsState::skin_lic_key(cid as u32, "custom_hex_123"),
            super::quasar_cosmetics_skinr::ThirdPartySkinLicense {
                character_id: cid as u32,
                skin_hex: "custom_hex_123".into(),
                activated: true,
                unactivated: 0,
                escrowed: 0,
                first_acquired_unix: now,
            },
        );
        skinr_data.applied_thirdparty_skins.insert(ship_id, "custom_hex_123".into());
        s.quasar_state_blobs.insert(
            super::quasar_cosmetics_skinr::SKINR_BLOB_KEY.into(),
            serde_json::to_string(&skinr_data).unwrap(),
        );
    });
    assert_code(router, &player, ship_get_ep, get_ship_req, now, STATUS_OK);
    assert_code(router, &player, ship_get_ep, Vec::new(), now, STATUS_BAD_REQUEST);

    let mut nf_ship = Vec::new();
    let mut nf_seq = Vec::new();
    encode_varint_field(1, 999_999_999, &mut nf_seq);
    encode_bytes_field(1, &nf_seq, &mut nf_ship);
    assert_code(router, &player, ship_get_ep, nf_ship, now, STATUS_NOT_FOUND);
    let _ = sde;
}

#[test]
fn palette_roles_and_structure_paintwork_error_and_admin_paths() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    let other_corp = 98_000_002i64;
    let struct_id = 1_030_000_000_055i64;
    let other_struct_id = 1_030_000_000_056i64;

    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().corporation_id = corp_id;
        if let Some(c) = s.corporations.get_mut(&corp_id) {
            c.ceo_id = 90_000_099;
        }
        for (sid, tid, owner) in [
            (struct_id, 35832, corp_id),
            (other_struct_id, 35833, other_corp),
        ] {
            s.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: owner,
                    solar_system_id: 30_000_142,
                    name: format!("Struct {sid}"),
                    state: "ShieldVulnerable".into(),
                    position: [0.0; 3],
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
    });

    let member = sess(Some(1), Some(cid), Some(corp_id), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let brand_mgr = sess(
        Some(1),
        Some(cid),
        Some(corp_id),
        service_roles::ROLE_PLAYER_DEFAULT,
        CORP_ROLE_BRAND_MANAGER,
    );
    let station_mgr = sess(
        Some(1),
        Some(cid),
        Some(corp_id),
        service_roles::ROLE_PLAYER_DEFAULT,
        corp_roles::ROLE_STATION_MANAGER,
    );
    let gm = sess(Some(1), Some(cid), Some(corp_id), service_roles::ROLE_GM_DEFAULT, 0);

    assert_code(
        &router,
        &member,
        "eve_public.cosmetic.corporation.palette.api.GetOwnRequest",
        Vec::new(),
        now,
        STATUS_NOT_FOUND,
    );
    assert_code(
        &router,
        &member,
        "eve_public.cosmetic.corporation.palette.api.GetRequest",
        Vec::new(),
        now,
        STATUS_BAD_REQUEST,
    );
    let pal_set_ep = "eve_public.cosmetic.corporation.palette.api.SetRequest";
    assert_code(&router, &member, pal_set_ep, Vec::new(), now, STATUS_FORBIDDEN);

    let encode_rgb = |r: u64, g: u64, b: u64| {
        let mut c = Vec::new();
        encode_varint_field(1, r, &mut c);
        encode_varint_field(2, g, &mut c);
        encode_varint_field(3, b, &mut c);
        c
    };
    let mut full_pal_attrs = Vec::new();
    encode_bytes_field(1, &encode_rgb(1, 2, 3), &mut full_pal_attrs);
    encode_bytes_field(2, &encode_rgb(4, 5, 6), &mut full_pal_attrs);
    encode_bytes_field(4, &encode_rgb(7, 8, 9), &mut full_pal_attrs);
    let mut full_pal_req = Vec::new();
    encode_bytes_field(1, &full_pal_attrs, &mut full_pal_req);
    assert_code(&router, &brand_mgr, pal_set_ep, full_pal_req, now, STATUS_OK);

    // Paintwork IssueRequest validation & character LP fallback:
    let mut slot1 = Vec::new();
    encode_varint_field(1, 101, &mut slot1);
    let mut slots_cfg = Vec::new();
    encode_bytes_field(1, &slot1, &mut slots_cfg);
    let mut dur_msg = Vec::new();
    encode_varint_field(1, 2_592_000, &mut dur_msg);
    let mut st_seq = Vec::new();
    encode_varint_field(1, struct_id as u64, &mut st_seq);

    let mut issue_req = Vec::new();
    encode_bytes_field(1, &slots_cfg, &mut issue_req);
    encode_bytes_field(2, &dur_msg, &mut issue_req);
    encode_bytes_field(3, &st_seq, &mut issue_req);

    let issue_ep = "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest";
    assert_code(&router, &member, issue_ep, issue_req.clone(), now, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|s| {
        s.corp_lp_balances.clear();
        s.characters.get_mut(&cid).unwrap().loyalty_points.clear();
    });
    assert_code(&router, &station_mgr, issue_ep, issue_req.clone(), now, STATUS_FORBIDDEN);

    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().loyalty_points.insert(PARAGON_CORP_ID, 1_500);
    });
    assert_code(&router, &station_mgr, issue_ep, issue_req, now, STATUS_OK);
    assert_eq!(db.with_snapshot(|s| s.characters[&cid].loyalty_points[&PARAGON_CORP_ID]), 500);

    // Admin IssueRequest & Admin RevokeRequest on other_struct_id
    let mut other_st_seq = Vec::new();
    encode_varint_field(1, other_struct_id as u64, &mut other_st_seq);
    let mut admin_issue_req = Vec::new();
    encode_bytes_field(1, &slots_cfg, &mut admin_issue_req);
    encode_bytes_field(2, &dur_msg, &mut admin_issue_req);
    encode_bytes_field(3, &other_st_seq, &mut admin_issue_req);

    assert_code(&router, &station_mgr, issue_ep, admin_issue_req.clone(), now, STATUS_FORBIDDEN);
    let adm_iss_rsp = call(
        &router,
        &gm,
        "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest",
        admin_issue_req,
        now,
    );
    assert_eq!(adm_iss_rsp.status_code, STATUS_OK);
    let iss_wm = WireMessage::decode(&adm_iss_rsp.payload.unwrap().value).unwrap();
    let entry_wm = WireMessage::decode(iss_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let lic_id_b = entry_wm.fields[0].value.as_bytes().unwrap().to_vec();

    let mut adm_rev_req = Vec::new();
    encode_bytes_field(1, &lic_id_b, &mut adm_rev_req);
    assert_code(
        &router,
        &gm,
        "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest",
        adm_rev_req,
        now,
        STATUS_OK,
    );
}

#[test]
fn activate_ship_emblem_license_and_broadcast_skin_cosmetic_change() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_repository(Arc::clone(&db));
    let cid = 90_000_001i64;
    let mut session = sess(Some(1), Some(cid), Some(98_000_001), 0, 0);
    session.station_id = Some(60_003_760);

    assert_eq!(
        activate_ship_emblem_license(&ctx, &mut session, cid, 34, None)
            .unwrap_err()
            .code,
        "InvalidSkinLicenseType"
    );

    let (lic_tid, expected_ship_tid) = sde
        .types
        .keys()
        .find_map(|&tid| ship_emblem_license_spec(&sde, tid).map(|(st, _)| (tid, st)))
        .expect("at least one emblem license in SDE");

    assert_eq!(
        activate_ship_emblem_license(
            &ctx,
            &mut session,
            cid,
            lic_tid,
            Some(expected_ship_tid + 1)
        )
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );
    assert_eq!(
        activate_ship_emblem_license(&ctx, &mut session, cid, lic_tid, Some(expected_ship_tid))
            .unwrap_err()
            .code,
        "SkinLicenseItemNotFound"
    );

    db.with_mut_snapshot(|s| {
        s.items.insert(
            888_001,
            InventoryItemRecord {
                item_id: 888_001,
                type_id: lic_tid,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert!(
        activate_ship_emblem_license(&ctx, &mut session, cid, lic_tid, Some(expected_ship_tid))
            .is_ok()
    );
    assert_eq!(db.with_snapshot(|s| s.items[&888_001].quantity), 1);
    assert!(activate_ship_emblem_license(&ctx, &mut session, cid, lic_tid, None).is_ok());
    assert!(db.with_snapshot(|s| !s.items.contains_key(&888_001)));

    let ship_id = db.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        broadcast_ship_skin_cosmetic_change(&ctx, cid, ship_id, 587, None, Some(1001)).await;
        db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&cid).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
        });
        broadcast_ship_skin_cosmetic_change(&ctx, cid, ship_id, 587, Some(1001), None).await;
    });
    let _ = NotificationTarget::Character(cid);
}
