//! Unit tests for `quasar_cosmetics_live`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, WireMessage,
};
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
            correlation_uuid: vec![5; 16],
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
fn logo_kind_mapping_catalogue_pricing_and_pruning() {
    assert_eq!(logo_kind_to_fsd_cosmetic_type(1), 2);
    assert_eq!(logo_kind_to_fsd_cosmetic_type(2), 1);
    assert_eq!(logo_kind_to_fsd_cosmetic_type(9), 9);

    assert_eq!(get_catalogue_price(35832, 2_592_000), Some(1_000));
    assert_eq!(get_catalogue_price(35832, 7_776_000), Some(2_500));
    assert_eq!(get_catalogue_price(35833, 2_592_000), Some(2_000));
    assert_eq!(get_catalogue_price(35834, 2_592_000), Some(5_000));
    assert_eq!(get_catalogue_price(35832, 86_400), None);
    assert_eq!(get_catalogue_price(99999, 2_592_000), None);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let cid = 90_000_001i64;
    let ship_id = 1_000_100i64;
    db.with_mut_snapshot(|s| {
        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: -1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut live = CosmeticsLiveData::default();
        let mut slots = BTreeMap::new();
        slots.insert(0, 1); // alliance
        slots.insert(1, 2); // corp
        live.ship_logos.insert(ship_id as u64, slots);
        s.quasar_state_blobs.insert(
            BLOB_KEY.into(),
            serde_json::to_string(&live).unwrap(),
        );
    });

    super::super::quasar_auth_user::with_identity_store(&db, |id| {
        id.char_alliance_logo_ships.entry(cid as u64).or_default().push(587);
        id.char_corp_logo_ships.entry(cid as u64).or_default().push(587);
    });
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.alliance_id = Some(99_000_001);
        }
    });

    let dict = db.with_snapshot(|s| enabled_cosmetics_dict_from_snapshot(s, ship_id as u64));
    assert_eq!(
        dict,
        PyRep::Dict(vec![
            (PyRep::Integer(0), PyRep::Integer(2)),
            (PyRep::Integer(1), PyRep::Integer(1)),
        ])
    );

    let sink = RwLock::new(None);
    prune_revoked_logo_entitlement(&db, &sink, cid as u64, 587, true);
    let dict_after_alliance = db.with_snapshot(|s| enabled_cosmetics_dict_from_snapshot(s, ship_id as u64));
    assert_eq!(
        dict_after_alliance,
        PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(1))])
    );

    prune_revoked_logo_entitlement(&db, &sink, cid as u64, 587, false);
    let dict_after_corp = db.with_snapshot(|s| enabled_cosmetics_dict_from_snapshot(s, ship_id as u64));
    assert_eq!(dict_after_corp, PyRep::Dict(Vec::new()));
}

#[test]
fn ship_logos_ship_skin_state_and_corporation_palette_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().corporation_id = corp_id;
    });
    let ship_id = db.with_snapshot(|s| s.characters[&cid].active_ship_id) as u64;
    let ship_tid = db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);

    let member = sess(Some(1), Some(cid), Some(corp_id), service_roles::ROLE_PLAYER_DEFAULT, 0);
    let director = sess(
        Some(1),
        Some(cid),
        Some(corp_id),
        service_roles::ROLE_PLAYER_DEFAULT,
        corp_roles::ROLE_DIRECTOR,
    );

    // Grant corp logo entitlement for ship_tid
    super::super::quasar_auth_user::with_identity_store(&db, |id| {
        id.char_corp_logo_ships.entry(cid as u64).or_default().push(ship_tid as u64);
    });

    // DisplayRequest: missing id -> 400, valid corp logo -> 200, ClearRequest -> 200
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.ship.logo.DisplayRequest", Vec::new(), now).status_code,
        STATUS_BAD_REQUEST
    );
    let mut ship_seq = Vec::new();
    encode_varint_field(1, ship_id, &mut ship_seq);
    let mut logo_id = Vec::new();
    encode_bytes_field(1, &ship_seq, &mut logo_id);
    encode_varint_field(2, 0, &mut logo_id);
    let mut corp_attr = Vec::new();
    encode_bytes_field(2, &[], &mut corp_attr);
    let mut disp_req = Vec::new();
    encode_bytes_field(1, &logo_id, &mut disp_req);
    encode_bytes_field(2, &corp_attr, &mut disp_req);
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.ship.logo.DisplayRequest", disp_req, now).status_code,
        STATUS_OK
    );

    let mut clear_req = Vec::new();
    encode_bytes_field(1, &logo_id, &mut clear_req);
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.ship.logo.ClearRequest", clear_req, now).status_code,
        STATUS_OK
    );

    // Ship SKIN state GetRequest & GetAllInBubbleRequest + publish_ship_skin_set & publish_arrival_cosmetics
    let mut get_ship_req = Vec::new();
    encode_bytes_field(1, &ship_seq, &mut get_ship_req);
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.ship.api.GetRequest", get_ship_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.ship.api.GetAllInBubbleRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    publish_ship_skin_set(&router, &db, &sde, ship_id as i64, now);
    publish_arrival_cosmetics(&router, &db, &sde, cid, now);

    // Corporation Palette: CanEditRequest, SetRequest (valid + out-of-range > 255), GetOwnRequest, GetRequest
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.corporation.palette.api.CanEditRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    let encode_rgb = |r: u64, g: u64, b: u64| {
        let mut c = Vec::new();
        encode_varint_field(1, r, &mut c);
        encode_varint_field(2, g, &mut c);
        encode_varint_field(3, b, &mut c);
        c
    };
    let mut bad_attrs = Vec::new();
    encode_bytes_field(1, &encode_rgb(300, 10, 20), &mut bad_attrs);
    let mut bad_pal = Vec::new();
    encode_bytes_field(1, &bad_attrs, &mut bad_pal);
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.corporation.palette.api.SetRequest", bad_pal, now).status_code,
        STATUS_BAD_REQUEST
    );

    let mut ok_attrs = Vec::new();
    encode_bytes_field(1, &encode_rgb(10, 20, 30), &mut ok_attrs);
    encode_bytes_field(2, &encode_rgb(40, 50, 60), &mut ok_attrs);
    let mut ok_pal = Vec::new();
    encode_bytes_field(1, &ok_attrs, &mut ok_pal);
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.corporation.palette.api.SetRequest", ok_pal, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    let mut corp_seq = Vec::new();
    encode_varint_field(1, corp_id as u64, &mut corp_seq);
    let mut pal_ident = Vec::new();
    encode_bytes_field(1, &corp_seq, &mut pal_ident);
    let mut get_pal_req = Vec::new();
    encode_bytes_field(1, &pal_ident, &mut get_pal_req);
    assert_eq!(
        call(&router, &member, "eve_public.cosmetic.corporation.palette.api.GetRequest", get_pal_req, now).status_code,
        STATUS_OK
    );
}

#[test]
fn structure_paintwork_catalogue_issue_query_and_revoke() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );
    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    let struct_id = 1_030_000_000_001i64;
    db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().corporation_id = corp_id;
        s.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Astrahus".into(),
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
        s.corp_lp_balances.insert((corp_id, PARAGON_CORP_ID), 10_000);
    });

    let director = sess(
        Some(1),
        Some(cid),
        Some(corp_id),
        service_roles::ROLE_PLAYER_DEFAULT,
        corp_roles::ROLE_DIRECTOR,
    );

    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.license.api.GetCatalogueRequest", Vec::new(), now).status_code,
        STATUS_OK
    );

    // Issue paintwork license (slot 1 = paint 101, duration 2_592_000, structure = struct_id)
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

    let iss_rsp = call(&router, &director, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", issue_req, now);
    assert_eq!(iss_rsp.status_code, STATUS_OK);
    assert_eq!(db.with_snapshot(|s| s.corp_lp_balances[&(corp_id, PARAGON_CORP_ID)]), 9_000);

    // Extract license UUID from IssueResponse
    let iss_wm = WireMessage::decode(&iss_rsp.payload.unwrap().value).unwrap();
    let entry_b = iss_wm.fields[0].value.as_bytes().unwrap();
    let entry_wm = WireMessage::decode(entry_b).unwrap();
    let lic_id_b = entry_wm.fields[0].value.as_bytes().unwrap().to_vec();

    let mut get_st_req = Vec::new();
    encode_bytes_field(1, &st_seq, &mut get_st_req);
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.api.GetRequest", get_st_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.license.api.GetAllOwnedByCorporationRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    let mut lic_req = Vec::new();
    encode_bytes_field(1, &lic_id_b, &mut lic_req);
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.license.api.GetRequest", lic_req.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &director, "eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest", lic_req, now).status_code,
        STATUS_OK
    );
}

#[test]
fn ship_cosmetics_stripping_clears_first_third_party_skins_and_logos_across_caches() {
    use crate::services::quasar_auth_user::with_identity_store;
    use crate::services::quasar_cosmetics_skinr::{
        register as register_skinr, SkinDesignRecord, SkinrCosmeticsState, ThirdPartySkinLicense,
    };

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let live_state = CosmeticsLiveState::load(Arc::clone(&db), Arc::clone(&sde));
    let skinr_state = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    register_skinr(&router, Arc::clone(&skinr_state));
    register(&router, Arc::clone(&live_state), Arc::clone(&skinr_state));

    let owner = 90_000_001_u32;
    let ship_id = 100_000_001_u64;
    let ship_tid = db.with_snapshot(|s| s.items[&(ship_id as i64)].type_id);
    with_identity_store(&db, |id| {
        id.char_corp_logo_ships.entry(u64::from(owner)).or_default().push(u64::from(ship_tid));
    });
    db.with_mut_snapshot(|s| {
        s.licensed_skins.entry(i64::from(owner)).or_default().push(1001);
        s.applied_ship_skins.insert(ship_id as i64, 1001);
    });
    {
        let mut ld = live_state.data.write().unwrap();
        ld.ship_logos.entry(ship_id).or_default().insert(0, 2);
    }
    live_state.save();
    {
        let mut sd = skinr_state.data.write().unwrap();
        sd.skins.insert(
            "cafebabe".into(),
            SkinDesignRecord {
                skin_hex: "cafebabe".into(),
                name: "TestStrip".into(),
                line_name: "Line".into(),
                creator_id: owner,
                ship_type_id: ship_tid,
                pattern_blend_mode: 0,
                slots: Vec::new(),
                tier_level: 1,
                first_sequenced_unix: None,
            },
        );
        sd.skin_licenses.insert(
            SkinrCosmeticsState::skin_lic_key(owner, "cafebabe"),
            ThirdPartySkinLicense {
                character_id: owner,
                skin_hex: "cafebabe".into(),
                activated: true,
                unactivated: 0,
                escrowed: 0,
                first_acquired_unix: 1_700_000_000,
            },
        );
        sd.applied_thirdparty_skins.insert(ship_id, "cafebabe".into());
    }
    skinr_state.save();

    db.with_snapshot(|s| {
        assert!(matches!(enabled_cosmetics_dict_from_snapshot(s, ship_id), PyRep::Dict(ref d) if !d.is_empty()));
        assert!(ship_state(s, &sde, ship_id).is_some());
    });

    strip_ship_cosmetics_for_ships(&db, &[ship_id as i64]);

    db.with_snapshot(|s| {
        assert_eq!(enabled_cosmetics_dict_from_snapshot(s, ship_id), PyRep::Dict(Vec::new()));
        let raw_state = ship_state(s, &sde, ship_id).expect("ship_state encodes no_skin for valid ship");
        let wm = eve_proto::quasar::WireMessage::decode(&raw_state).unwrap();
        let skin_bytes = wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap();
        let skin_wm = eve_proto::quasar::WireMessage::decode(skin_bytes).unwrap();
        assert!(!skin_wm.fields.iter().any(|f| f.field_number == 1 || f.field_number == 2));
        assert_eq!(skin_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(1));
        assert!(!s.applied_ship_skins.contains_key(&(ship_id as i64)));
    });
    assert!(!live_state.data.read().unwrap().ship_logos.contains_key(&ship_id));
    assert!(!skinr_state.data.read().unwrap().applied_thirdparty_skins.contains_key(&ship_id));
}
