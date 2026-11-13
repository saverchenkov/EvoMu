//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_cosmetics_live`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleDuration, QuasarDescriptorRegistry,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        // Ensure a synthetic category-2118 ship emblem license type exists for fallback spec tests
        if let Some(tmpl) = sde.types.get(&587).cloned() {
            let mut corp_emblem = tmpl.clone();
            corp_emblem.type_id = 77_990;
            corp_emblem.category_id = 2118;
            corp_emblem.name = format!("{} Corporation Emblem", tmpl.name);
            sde.types.insert(77_990, corp_emblem);

            let mut all_emblem = tmpl;
            all_emblem.type_id = 77_991;
            all_emblem.category_id = 2118;
            all_emblem.name = "NonexistentHull Alliance Emblem".to_string();
            sde.types.insert(77_991, all_emblem);
        }
        Arc::new(sde)
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
            correlation_uuid: vec![6; 16],
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

fn build_logo_id(ship_id: u64, slot: i64) -> ProtoBuilder {
    pb::message("eve_public.cosmetic.ship.logo.Identifier")
        .set("ship.sequential", ship_id)
        .when(slot != 0, |b| b.set("index", slot as i32))
}

fn build_rgb(r: u32, g: u32, b: u32) -> ProtoBuilder {
    pb::message("eve_public.color.RGB")
        .set("red", r)
        .set("green", g)
        .set("blue", b)
}

#[test]
fn enabled_cosmetics_and_ship_state_edges() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let cid = 90_000_001i64;
    let ship_id = 1_000_555i64;
    let tritanium_id = 1_000_556i64;

    db.with_mut_snapshot(|s| {
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34, // non-ship category
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            1_000_557,
            InventoryItemRecord {
                item_id: 1_000_557,
                type_id: 587,
                owner_id: 0, // invalid owner_id <= 0
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        let mut live = CosmeticsLiveData::default();
        let mut slots = BTreeMap::new();
        slots.insert(0, 1); // alliance
        slots.insert(1, 2); // corp
        slots.insert(2, 99); // unknown kind
        live.ship_logos.insert(ship_id as u64, slots);
        s.quasar_state_blobs
            .insert(BLOB_KEY.into(), serde_json::to_string(&live).unwrap());
    });

    // Missing ship, owner<=0 -> empty dict
    db.with_snapshot(|s| {
        assert_eq!(enabled_cosmetics_dict_from_snapshot(s, 999_999_999), PyRep::Dict(Vec::new()));
        assert_eq!(enabled_cosmetics_dict_from_snapshot(s, 1_000_557), PyRep::Dict(Vec::new()));
        // Without entitlements -> empty dict even though ship_logos exist in blob
        assert_eq!(enabled_cosmetics_dict_from_snapshot(s, ship_id as u64), PyRep::Dict(Vec::new()));
        // Non-existent or non-ship -> ship_state returns None
        assert!(ship_state(s, &sde, 999_999_999).is_none());
        assert!(ship_state(s, &sde, tritanium_id as u64).is_none());
    });

    // Grant alliance logo entitlement WITHOUT alliance membership -> still filtered out; add alliance -> appears
    super::super::quasar_auth_user::with_identity_store(&db, |id| {
        id.char_alliance_logo_ships.entry(cid as u64).or_default().push(587);
    });
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().alliance_id = None;
    });
    assert_eq!(
        db.with_snapshot(|s| enabled_cosmetics_dict_from_snapshot(s, ship_id as u64)),
        PyRep::Dict(Vec::new())
    );
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().alliance_id = Some(99_000_001);
    });
    assert_eq!(
        db.with_snapshot(|s| enabled_cosmetics_dict_from_snapshot(s, ship_id as u64)),
        PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(2))])
    );

    // ship_state: unlicensed applied_ship_skin falls through to activated third-party SKINR skin,
    // and unactivated third-party skin falls through to no_skin (field 3)
    {
        use crate::services::quasar_cosmetics_skinr::{
            SkinrCosmeticsData, SkinrCosmeticsState, ThirdPartySkinLicense,
        };
        let mut skinr = SkinrCosmeticsData::default();
        let lic_key = SkinrCosmeticsState::skin_lic_key(cid as u32, "deadbeef");
        skinr.skin_licenses.insert(
            lic_key.clone(),
            ThirdPartySkinLicense {
                character_id: cid as u32,
                skin_hex: "deadbeef".into(),
                activated: false,
                unactivated: 1,
                escrowed: 0,
                first_acquired_unix: 1_700_000_000,
            },
        );
        skinr.applied_thirdparty_skins.insert(ship_id as u64, "deadbeef".into());
        db.with_mut_snapshot(|s| {
            s.applied_ship_skins.insert(ship_id, 4242); // NOT in licensed_skins!
            s.quasar_state_blobs.insert(
                crate::services::quasar_cosmetics_skinr::SKINR_BLOB_KEY.into(),
                serde_json::to_string(&skinr).unwrap(),
            );
        });
        let raw_no_skin = db.with_snapshot(|s| ship_state(s, &sde, ship_id as u64).unwrap());
        let wm = WireMessage::decode(&raw_no_skin).unwrap();
        let skin_b = wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let skin_wm = WireMessage::decode(skin_b).unwrap();
        assert_eq!(
            skin_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
            Some(1)
        );

        // Now activate the third-party skin -> field 2 (third-party) encoded!
        skinr.skin_licenses.get_mut(&lic_key).unwrap().activated = true;
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(
                crate::services::quasar_cosmetics_skinr::SKINR_BLOB_KEY.into(),
                serde_json::to_string(&skinr).unwrap(),
            );
        });
        let raw_third = db.with_snapshot(|s| ship_state(s, &sde, ship_id as u64).unwrap());
        let wm3 = WireMessage::decode(&raw_third).unwrap();
        let skin_b3 = wm3.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let skin_wm3 = WireMessage::decode(skin_b3).unwrap();
        assert!(skin_wm3.fields.iter().any(|f| f.field_number == 2));

        // Now license the first-party skin -> field 1 (first-party) takes precedence!
        db.with_mut_snapshot(|s| {
            s.licensed_skins.entry(cid).or_default().push(4242);
        });
        let raw_first = db.with_snapshot(|s| ship_state(s, &sde, ship_id as u64).unwrap());
        let wm1 = WireMessage::decode(&raw_first).unwrap();
        let skin_b1 = wm1.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let skin_wm1 = WireMessage::decode(skin_b1).unwrap();
        assert!(skin_wm1.fields.iter().any(|f| f.field_number == 1));
    }
}

#[test]
fn logo_handlers_error_guards_and_macho_notify_sink_delivery() {
    let sde = shared_sde();
    let ctx = Arc::new(crate::router::ServerContext::with_repository(Arc::new(
        WorldRepository::with_demo_seed(),
    )));
    install_macho_notify_sink(&ctx);
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let other_cid = 90_000_002i64;

    // Make character 1 online and in space so MachoNotifySink delivers to SolarSystem
    ctx.db
        .acquire_online_lock(1, 81_001, Some(cid), true, now)
        .unwrap();
    let ship_id = ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.alliance_id = None;
        ch.active_ship_id
    }) as u64;
    let ship_tid = ctx.db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);

    let router = &ctx.quasar_gateway;
    let no_char = sess(Some(1), None, None, service_roles::ROLE_PLAYER_DEFAULT, 0);
    let player = sess(Some(1), Some(cid), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let other = sess(Some(2), Some(other_cid), Some(98_000_002), service_roles::ROLE_PLAYER_DEFAULT, 0);

    // 1. Missing active character -> 403
    assert_eq!(
        call(router, &no_char, "eve_public.cosmetic.ship.logo.DisplayRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(router, &no_char, "eve_public.cosmetic.ship.logo.ClearRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );

    // 2. Negative slot index -> 400; missing ship -> 404; foreign ship -> 403
    let neg_req = pb::message("eve_public.cosmetic.ship.logo.DisplayRequest")
        .set("id", build_logo_id(ship_id, -1))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", neg_req, now).status_code,
        STATUS_BAD_REQUEST
    );

    let missing_ship_req = pb::message("eve_public.cosmetic.ship.logo.ClearRequest")
        .set("logo", build_logo_id(888_888_888, 0))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.ClearRequest", missing_ship_req, now).status_code,
        STATUS_NOT_FOUND
    );

    let foreign_req = pb::message("eve_public.cosmetic.ship.logo.ClearRequest")
        .set("logo", build_logo_id(ship_id, 0))
        .must_encode();
    assert_eq!(
        call(router, &other, "eve_public.cosmetic.ship.logo.ClearRequest", foreign_req, now).status_code,
        STATUS_FORBIDDEN
    );

    // 3. Missing attr -> 400; empty attr (neither alliance nor corp) -> 400
    let no_attr = pb::message("eve_public.cosmetic.ship.logo.DisplayRequest")
        .set("id", build_logo_id(ship_id, 0))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", no_attr, now).status_code,
        STATUS_BAD_REQUEST
    );

    let empty_attr_req = pb::message("eve_public.cosmetic.ship.logo.DisplayRequest")
        .set("id", build_logo_id(ship_id, 0))
        .set("attr", pb::message("eve_public.cosmetic.ship.logo.Attributes"))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", empty_attr_req, now).status_code,
        STATUS_BAD_REQUEST
    );

    // 4. Alliance logo: not in alliance -> 403; in alliance without entitlement -> 403; with entitlement -> 200
    let all_req = pb::message("eve_public.cosmetic.ship.logo.DisplayRequest")
        .set("id", build_logo_id(ship_id, 0))
        .set("attr.alliance", pb::message("eve_public.cosmetic.ship.logo.Alliance"))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", all_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );

    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().alliance_id = Some(99_000_001);
    });
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", all_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );

    // Corp logo without entitlement -> 403
    let corp_req = pb::message("eve_public.cosmetic.ship.logo.DisplayRequest")
        .set("id", build_logo_id(ship_id, 0))
        .set("attr.corporation", pb::message("eve_public.cosmetic.ship.logo.Corporation"))
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", corp_req, now).status_code,
        STATUS_FORBIDDEN
    );

    // Grant alliance entitlement and display -> 200 and queues Macho notifications
    super::super::quasar_auth_user::with_identity_store(&ctx.db, |id| {
        id.char_alliance_logo_ships.entry(cid as u64).or_default().push(u64::from(ship_tid));
    });
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.logo.DisplayRequest", all_req, now).status_code,
        STATUS_OK
    );
    let pending_len = ctx.pending_notifications.read().unwrap().get(&cid).map_or(0, |q| q.len());
    assert!(pending_len >= 2, "expected OnShipCosmeticChanged & OnShipCosmeticsChanged");

    // 5. ship_get missing id -> 400; non-existent ship -> 404; docked GetAllInBubble -> empty value
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.api.GetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let missing_get = pb::message("eve_public.cosmetic.ship.api.GetRequest")
        .set("ship.sequential", 777_777_777u64)
        .must_encode();
    assert_eq!(
        call(router, &player, "eve_public.cosmetic.ship.api.GetRequest", missing_get, now).status_code,
        STATUS_NOT_FOUND
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(60_003_760);
    });
    let docked_bubble = call(router, &player, "eve_public.cosmetic.ship.api.GetAllInBubbleRequest", Vec::new(), now);
    assert_eq!(docked_bubble.status_code, STATUS_OK);
    assert!(docked_bubble.payload.unwrap().value.is_empty());
    let _ = sde;
}

#[test]
fn palette_paintwork_and_license_authorization_and_boundary_guards() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_000u64;
    let ceo_cid = 90_000_001i64;
    let member_cid = 90_000_002i64;
    let bm_cid = 90_000_003i64;
    let corp_a = 98_000_111i64;
    let corp_b = 98_000_222i64;
    let struct_a = 1_030_000_000_111i64;
    let struct_b = 1_030_000_000_222i64;

    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&ceo_cid).unwrap().corporation_id = corp_a;
        s.characters.get_mut(&member_cid).unwrap().corporation_id = corp_a;
        let mut bm_char = s.characters[&member_cid].clone();
        bm_char.character_id = bm_cid;
        bm_char.user_id = 3;
        bm_char.name = "BrandManager".into();
        bm_char.corporation_id = corp_a;
        s.characters.insert(bm_cid, bm_char);
        s.corporations.insert(
            corp_a,
            {
                let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_a;
                __corp.name = "Alpha".into();
                __corp.ticker = "ALPH".into();
                __corp.ceo_id = ceo_cid;
                __corp.members.insert(ceo_cid);
                __corp.members.insert(member_cid);
                __corp.members.insert(bm_cid);
                __corp.member_roles.insert(member_cid, 0);
                __corp.member_roles.insert(bm_cid, CORP_ROLE_BRAND_MANAGER);
                __corp.ensure_wallet_divisions();
                __corp
            },
        );
        s.structures.insert(
            struct_a,
            UpwellStructureRecord {
                structure_id: struct_a,
                type_id: 35832,
                owner_corp_id: corp_a,
                solar_system_id: 30_000_142,
                name: "Astra A".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        s.structures.insert(
            struct_b,
            UpwellStructureRecord {
                structure_id: struct_b,
                type_id: 35832,
                owner_corp_id: corp_b,
                solar_system_id: 30_000_142,
                name: "Astra B".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let ceo = sess(Some(1), Some(ceo_cid), Some(corp_a), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let brand_mgr = sess(Some(3), Some(bm_cid), Some(corp_a), service_roles::ROLE_PLAYER_DEFAULT, CORP_ROLE_BRAND_MANAGER);
    let grunt = sess(Some(2), Some(member_cid), Some(corp_a), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let admin = sess(Some(1), Some(ceo_cid), Some(corp_a), service_roles::ROLE_ADMIN, 0);
    let ghost = sess(Some(99), Some(99_999_999), Some(corp_a), service_roles::ROLE_PLAYER_DEFAULT, 0);

    // 1. Palette: missing palette -> 404; grunt Set -> 403; grunt CanEdit -> 0; brand_mgr CanEdit -> 1;
    //    missing attrs / missing main / secondary > 255 -> 400; CEO Set with tertiary -> 200
    assert_eq!(
        call(&router, &grunt, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new(), now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &ghost, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new(), now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &grunt, "eve_public.cosmetic.corporation.palette.api.GetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let can_grunt = call(&router, &grunt, "eve_public.cosmetic.corporation.palette.api.CanEditRequest", Vec::new(), now);
    assert_eq!(can_grunt.status_code, STATUS_OK);
    assert_eq!(
        WireMessage::decode(&can_grunt.payload.unwrap().value).unwrap().fields.first().and_then(|f| f.value.as_varint()),
        Some(0)
    );
    let can_bm = call(&router, &brand_mgr, "eve_public.cosmetic.corporation.palette.api.CanEditRequest", Vec::new(), now);
    assert_eq!(
        WireMessage::decode(&can_bm.payload.unwrap().value).unwrap().fields[0].value.as_varint(),
        Some(1)
    );

    assert_eq!(
        call(&router, &grunt, "eve_public.cosmetic.corporation.palette.api.SetRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &brand_mgr, "eve_public.cosmetic.corporation.palette.api.SetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let no_main = pb::message("eve_public.cosmetic.corporation.palette.api.SetRequest")
        .set("attributes.secondary_color", build_rgb(1, 2, 3))
        .must_encode();
    assert_eq!(
        call(&router, &brand_mgr, "eve_public.cosmetic.corporation.palette.api.SetRequest", no_main, now).status_code,
        STATUS_BAD_REQUEST
    );
    let bad_sec = pb::message("eve_public.cosmetic.corporation.palette.api.SetRequest")
        .set("attributes.main_color", build_rgb(1, 2, 3))
        .set("attributes.secondary_color", build_rgb(1, 256, 3))
        .must_encode();
    assert_eq!(
        call(&router, &brand_mgr, "eve_public.cosmetic.corporation.palette.api.SetRequest", bad_sec, now).status_code,
        STATUS_BAD_REQUEST
    );

    let full_pal = pb::message("eve_public.cosmetic.corporation.palette.api.SetRequest")
        .set("attributes.main_color", build_rgb(10, 20, 30))
        .set("attributes.secondary_color", build_rgb(40, 50, 60))
        .set("attributes.tertiary_color", build_rgb(70, 80, 90))
        .must_encode();
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.corporation.palette.api.SetRequest", full_pal, now).status_code,
        STATUS_OK
    );

    // 2. Paintwork & License issue/revoke error guards & personal Paragon LP fallback
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.api.GetRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let get_missing_pw = pb::message("eve_public.cosmetic.structure.paintwork.api.GetRequest")
        .set("structure.sequential", struct_a as u64)
        .must_encode();
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.api.GetRequest", get_missing_pw, now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &ghost, "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest", Vec::new(), now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &ghost, "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest", Vec::new(), now).status_code,
        STATUS_NOT_FOUND
    );

    // Empty slots, duration 0, missing structures -> 400
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let bad_slots_req = pb::message("eve_public.cosmetic.structure.paintwork.license.api.IssueRequest")
        .set("paintwork.first.empty", true)
        .must_encode();
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", bad_slots_req, now).status_code,
        STATUS_BAD_REQUEST
    );

    let mk_issue = |st_id: u64, dur: u64| {
        pb::message("eve_public.cosmetic.structure.paintwork.license.api.IssueRequest")
            .set("paintwork.first.paint", 101u32)
            .set(
                "duration",
                GoogleDuration {
                    seconds: dur as i64,
                    nanos: 0,
                },
            )
            .when(st_id > 0, |b| {
                b.add_with("structures", |s| s.set("sequential", st_id))
            })
            .must_encode()
    };

    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_a as u64, 0), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(0, 2_592_000), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(9_999_999, 2_592_000), now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_b as u64, 2_592_000), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_a as u64, 12_345), now).status_code,
        STATUS_BAD_REQUEST
    );
    // Grunt lacks director/station manager/CEO role -> 403
    assert_eq!(
        call(&router, &grunt, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_a as u64, 2_592_000), now).status_code,
        STATUS_FORBIDDEN
    );
    // CEO with 0 corp LP and 0 char LP -> 403 insufficient EverMarks
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_a as u64, 2_592_000), now).status_code,
        STATUS_FORBIDDEN
    );

    // Give CEO personal Paragon LP -> fallback deducts personal LP!
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&ceo_cid).unwrap().loyalty_points.insert(PARAGON_CORP_ID, 1_500);
    });
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", mk_issue(struct_a as u64, 2_592_000), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        db.with_snapshot(|s| s.characters[&ceo_cid].loyalty_points[&PARAGON_CORP_ID]),
        500
    );

    // Admin issue on foreign structure struct_b without LP deduction -> 200; non-admin -> 403
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest", mk_issue(struct_b as u64, 2_592_000), now).status_code,
        STATUS_FORBIDDEN
    );
    let adm_iss = call(&router, &admin, "eve_public.cosmetic.structure.paintwork.license.api.admin.IssueRequest", mk_issue(struct_b as u64, 2_592_000), now);
    assert_eq!(adm_iss.status_code, STATUS_OK);
    let adm_wm = WireMessage::decode(&adm_iss.payload.unwrap().value).unwrap();
    let adm_entry = WireMessage::decode(adm_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let lic_b_id = adm_entry.fields[0].value.as_bytes().unwrap().to_vec();

    // CEO cannot view or revoke corp_b's license
    let lic_b_get_req = pb::message("eve_public.cosmetic.structure.paintwork.license.api.GetRequest")
        .set("id", pb::raw_message(&lic_b_id))
        .must_encode();
    let lic_b_rev_req = pb::message("eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest")
        .set("license", pb::raw_message(&lic_b_id))
        .must_encode();
    let lic_b_adm_rev_req =
        pb::message("eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest")
            .set("license", pb::raw_message(&lic_b_id))
            .must_encode();
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.GetRequest", lic_b_get_req, now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(&router, &grunt, "eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest", lic_b_rev_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest", lic_b_rev_req, now).status_code,
        STATUS_FORBIDDEN
    );
    // Non-admin cannot call admin.RevokeRequest; admin can revoke any corp's license!
    assert_eq!(
        call(&router, &ceo, "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest", lic_b_adm_rev_req.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &admin, "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest", lic_b_adm_rev_req.clone(), now).status_code,
        STATUS_OK
    );
    // Revoking again -> 404
    assert_eq!(
        call(&router, &admin, "eve_public.cosmetic.structure.paintwork.license.api.admin.RevokeRequest", lic_b_adm_rev_req, now).status_code,
        STATUS_NOT_FOUND
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emblem_activation_and_broadcast_ship_skin_cosmetic_change_edges() {
    let sde = shared_sde();
    let mut srv_ctx = crate::router::ServerContext::with_repository(Arc::new(
        WorldRepository::with_demo_seed(),
    ));
    srv_ctx.sde = Arc::clone(&sde);
    let ctx = Arc::new(srv_ctx);

    let cid = 90_000_001i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let mut session = SessionState::new(77_100);
    session.character_id = Some(cid);
    session.station_id = Some(60_003_760);
    session.ship_id = Some(ship_id);

    // 1. ship_emblem_license_spec fallback & unknown
    assert_eq!(ship_emblem_license_spec(&sde, 34), None);
    assert_eq!(ship_emblem_license_spec(&sde, 77_990), Some((587, 1)));
    assert_eq!(ship_emblem_license_spec(&sde, 77_991), None);

    // 2. activate_ship_emblem_license error guards
    assert_eq!(
        activate_ship_emblem_license(&ctx, &mut session, cid, 34, None).unwrap_err().code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        activate_ship_emblem_license(&ctx, &mut session, cid, 77_990, Some(602)).unwrap_err().code,
        "InvalidShipTypeForSkin"
    );
    assert_eq!(
        activate_ship_emblem_license(&ctx, &mut session, cid, 77_990, Some(587)).unwrap_err().code,
        "SkinLicenseItemNotFound"
    );

    // 3. Seed quantity 2 license item -> first activation decrements to 1, second removes item
    let lic_item_id = 9_500_001i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            lic_item_id,
            InventoryItemRecord {
                item_id: lic_item_id,
                type_id: 77_990,
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
    let res1 = activate_ship_emblem_license(&ctx, &mut session, cid, 77_990, Some(587)).unwrap();
    assert!(matches!(res1, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&lic_item_id].quantity), 1);

    let _res2 = activate_ship_emblem_license(&ctx, &mut session, cid, 77_990, None).unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&lic_item_id)));

    // 4. broadcast_ship_skin_cosmetic_change: docked -> Character OnShipCosmeticsChanged; in-space -> SolarSystem + OnSlimItemChange
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().station_id = Some(60_003_760);
    });
    broadcast_ship_skin_cosmetic_change(&ctx, cid, ship_id, 587, None, Some(1001)).await;

    ctx.db
        .acquire_online_lock(1, 77_100, Some(cid), true, ctx.now_unix())
        .unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    broadcast_ship_skin_cosmetic_change(&ctx, cid, ship_id, 587, Some(1001), None).await;
    let notifs = ctx.pending_notifications.read().unwrap();
    let char_notifs = &notifs[&cid];
    assert!(char_notifs.iter().any(|n| n.event == "OnShipCosmeticsChanged"));
    assert!(char_notifs.iter().any(|n| n.event == "OnSlimItemChange"));
}

#[test]
fn cosmetics_live_save_rehydrate_register_sink_and_active_ship_cargo_emblem_mutants() {
    let sde = shared_sde();
    let mut srv_ctx =
        crate::router::ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    srv_ctx.sde = Arc::clone(&sde);
    let ctx = Arc::new(srv_ctx);

    let state = CosmeticsLiveState::load(Arc::clone(&ctx.db), Arc::clone(&ctx.sde));
    state
        .data
        .write()
        .unwrap()
        .ship_logos
        .insert(1_234_567, BTreeMap::from([(0, 2)]));
    state.save();
    assert!(ctx.db.with_snapshot(|s| {
        s.quasar_state_blobs
            .get(BLOB_KEY)
            .is_some_and(|raw| raw.contains("1234567"))
    }));

    // Mutate persisted DB blob directly and verify rehydrate_from_db replaces in-memory map
    let mut replacement = CosmeticsLiveData::default();
    replacement
        .ship_logos
        .insert(7_654_321, BTreeMap::from([(1, 1)]));
    ctx.db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert(BLOB_KEY.into(), serde_json::to_string(&replacement).unwrap());
    });
    state.rehydrate_from_db();
    {
        let data = state.data.read().unwrap();
        assert!(!data.ship_logos.contains_key(&1_234_567));
        assert_eq!(data.ship_logos.get(&7_654_321), Some(&BTreeMap::from([(1, 1)])));
    }

    // Verify register wires eve_public.cosmetic.ship.api.GetRequest cleanly
    let cid = 90_000_001i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.sde),
    );
    let skinr = super::super::quasar_cosmetics_skinr::SkinrCosmeticsState::load(
        Arc::clone(&ctx.db),
        Arc::clone(&ctx.sde),
    );
    register(&router, Arc::clone(&state), skinr);
    let get_req = pb::message("eve_public.cosmetic.ship.api.GetRequest")
        .set("ship.sequential", ship_id as u64)
        .must_encode();
    let player = sess(Some(1), Some(cid), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let get_res = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.api.GetRequest",
        get_req,
        1_760_000_000,
    );
    assert_eq!(get_res.status_code, STATUS_OK);

    // Verify install_macho_notify_sink populates ctx.quasar_gateway.macho_notify_sink and emits notifications
    install_macho_notify_sink(&ctx);
    ctx.db
        .acquire_online_lock(1, 77_200, Some(cid), true, ctx.now_unix())
        .unwrap();
    ctx.pending_notifications.write().unwrap().clear();
    emit_ship_logo_macho_notifications(
        &ctx.quasar_gateway.macho_notify_sink,
        &ctx.db,
        ship_id as u64,
        0,
        PyRep::Integer(2),
    );
    assert!(ctx
        .pending_notifications
        .read()
        .unwrap()
        .get(&cid)
        .is_some_and(|q| q.iter().any(|n| n.event == "OnShipCosmeticChanged")));

    // Verify activate_ship_emblem_license works when the license is ONLY in active ship Cargo (station_id = None),
    // killing `delete field active_ship_id` and `Ok(Default::default())`.
    let cargo_lic_id = 9_500_099i64;
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        let sys_id = ch.solar_system_id as i64;
        if let Some(ship) = s.items.get_mut(&ship_id) {
            ship.location_id = sys_id;
            ship.flag = InventoryFlag::None;
        }
        s.items.insert(
            cargo_lic_id,
            InventoryItemRecord {
                item_id: cargo_lic_id,
                type_id: 77_990,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut space_sess = SessionState::new(77_200);
    space_sess.user_id = Some(1);
    space_sess.character_id = Some(cid);
    space_sess.station_id = None;
    space_sess.structure_id = None;
    space_sess.ship_id = Some(ship_id);
    let act_res = activate_ship_emblem_license(&ctx, &mut space_sess, cid, 77_990, Some(587)).unwrap();
    assert!(matches!(
        act_res,
        PyRep::KeyVal(ref entries)
            if entries.iter().any(|(k, v)| k == "licensed" && *v == PyRep::Bool(true))
    ));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&cargo_lic_id)));
}
