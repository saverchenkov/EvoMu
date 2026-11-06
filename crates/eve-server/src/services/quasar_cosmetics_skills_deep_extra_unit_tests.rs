//! Deep branch-coverage and mutation-killing unit tests for `services::quasar_cosmetics_market`,
//! `services::quasar_cosmetics_skinr`, and `services::quasar_skills_clones`.

#![forbid(unsafe_code)]

use super::quasar_cosmetics_market::{
    register_with_state as register_market_with_state, seed_component_listings_if_empty,
    CosmeticsMarketState,
};
use super::quasar_cosmetics_skinr::{
    encode_comp_license_id, encode_component_id, encode_license_kind, parse_component_id,
    parse_component_id_wire, register as register_skinr, SkinDesignRecord, SkinLayoutSlot,
    SkinrCosmeticsState, ThirdPartySkinLicense, SKINR_BLOB_KEY,
};
use super::quasar_skills_clones::{register_skill_handlers, SkillPlanManager};
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK,
};
use eve_db::{IskCents, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    DynamicQuasarMessage, EvePublicRequest, GoogleAny, GoogleTimestamp,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const CORP_AIR_ASSOCIATION: i64 = 1_000_419;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(|| {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let db = dir.join("data/quasar_descriptors.sqlite");
            let manifest = dir.join("data/quasar_descriptor_manifest.json");
            if db.exists() && manifest.exists() {
                if let Ok(reg) = QuasarDescriptorRegistry::open(&db, &manifest) {
                    return Arc::new(reg);
                }
            }
            if !dir.pop() {
                break;
            }
        }
        QuasarDescriptorRegistry::open_default_or_empty()
    }))
}

fn make_session(uid: u64, cid: u32, role: u64) -> SessionState {
    let mut s = SessionState::new(uid * 100 + cid as u64);
    s.user_id = Some(uid);
    s.character_id = Some(cid as i64);
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call_gw(
    router: &PublicGatewayRouter,
    s: &SessionState,
    ty: &str,
    value: Vec<u8>,
    now: u64,
) -> u32 {
    router
        .handle_public_request(
            s,
            &EvePublicRequest {
                issued: Some(GoogleTimestamp {
                    seconds: now as i64,
                    nanos: 0,
                }),
                correlation_uuid: vec![7; 16],
                payload: Some(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ty}"),
                    value,
                }),
                external_origin: "test".into(),
                application_instance_uuid: Vec::new(),
                authoritative_context: None,
                trace_context: None,
            },
            None,
            false,
            now,
        )
        .status_code
}

fn put_uuid_msg(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

#[test]
fn cosmetics_market_fees_component_purchase_and_skin_listing_roundtrip() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let skin_hex = "deadbeefcafebabe0102030405060708";
    let seller_cid: u32 = 90_000_001;
    let buyer_cid: u32 = 90_000_002;
    let now: u64 = 1_700_000_000;

    db.with_mut_snapshot(|snap| {
        if let Some(ch1) = snap.characters.get_mut(&(seller_cid as i64)) {
            ch1.balance = IskCents(10_000_000_00);
            ch1.plex_balance = 50_000;
            ch1.loyalty_points.insert(CORP_AIR_ASSOCIATION, 50_000);
        }
        let mut ch2 = snap.characters[&(seller_cid as i64)].clone();
        ch2.character_id = buyer_cid as i64;
        ch2.user_id = 2;
        ch2.balance = IskCents(50_000_000_00);
        ch2.plex_balance = 50_000;
        ch2.loyalty_points.insert(CORP_AIR_ASSOCIATION, 100_000);
        snap.characters.insert(buyer_cid as i64, ch2);

        let mut skinr_data = super::quasar_cosmetics_skinr::SkinrCosmeticsData::default();
        skinr_data.skins.insert(
            skin_hex.into(),
            SkinDesignRecord {
                skin_hex: skin_hex.into(),
                name: "Seller SKIN".into(),
                line_name: "Line".into(),
                creator_id: seller_cid,
                ship_type_id: 587,
                pattern_blend_mode: 1,
                slots: vec![SkinLayoutSlot {
                    slot_id: 1,
                    component_id: 1,
                    is_pattern: false,
                    primary_coating_id: None,
                    secondary_coating_id: None,
                }],
                tier_level: 1,
                first_sequenced_unix: Some(now),
            },
        );
        skinr_data.skin_licenses.insert(
            format!("{seller_cid}:{skin_hex}"),
            ThirdPartySkinLicense {
                character_id: seller_cid,
                skin_hex: skin_hex.into(),
                activated: false,
                unactivated: 2,
                escrowed: 0,
                first_acquired_unix: now,
            },
        );
        snap.quasar_state_blobs.insert(
            SKINR_BLOB_KEY.into(),
            serde_json::to_string(&skinr_data).unwrap(),
        );
    });

    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let market = CosmeticsMarketState::load(Arc::clone(&db), Arc::clone(&sde), Arc::clone(&skinr));
    {
        let mut guard = market.data.write().unwrap();
        seed_component_listings_if_empty(&mut guard, &sde);
    }

    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_skinr(&router, Arc::clone(&skinr));
    register_market_with_state(&router, Arc::clone(&market));

    let seller_sess = make_session(1, seller_cid, 0);
    let buyer_sess = make_session(2, buyer_cid, 0);
    let gm_sess = make_session(1, seller_cid, service_roles::ROLE_GM_MASK);

    // 1. Market fee schedule endpoints
    for fee_req in [
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest",
    ] {
        assert_eq!(call_gw(&router, &seller_sess, fee_req, Vec::new(), now), STATUS_OK);
    }
    let mut buyer_fee_req = Vec::new();
    let mut buyer_fee_skin = Vec::new();
    encode_string_field(1, skin_hex, &mut buyer_fee_skin);
    encode_bytes_field(2, &buyer_fee_skin, &mut buyer_fee_req);
    assert_eq!(
        call_gw(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            buyer_fee_req,
            now,
        ),
        STATUS_OK
    );

    // 2. Component listing browse + missing listing 404 + valid component purchase
    assert_eq!(
        call_gw(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.component.listing.api.GetAllRequest",
            Vec::new(),
            now,
        ),
        STATUS_OK
    );

    let mut bad_purchase = Vec::new();
    put_uuid_msg(1, &[0x99; 16], &mut bad_purchase);
    encode_varint_field(2, 1, &mut bad_purchase);
    assert_eq!(
        call_gw(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            bad_purchase,
            now,
        ),
        STATUS_NOT_FOUND
    );

    let comp_entry = market
        .data
        .read()
        .unwrap()
        .component_listings
        .values()
        .next()
        .cloned()
        .expect("seeded component listing exists");
    let mut ok_purchase = Vec::new();
    put_uuid_msg(1, &comp_entry.uuid, &mut ok_purchase);
    encode_varint_field(2, 1, &mut ok_purchase);
    assert_eq!(
        call_gw(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
            ok_purchase,
            now,
        ),
        STATUS_OK
    );

    // 3. Create SKIN listing -> GetAll -> GetAllOwned -> Get -> admin.SetExpiration -> Cancel -> Re-list & Purchase
    let mut lic_id = Vec::new();
    let mut seq_b = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seq_b);
    encode_bytes_field(1, &seq_b, &mut lic_id);
    let mut hex_b = Vec::new();
    encode_string_field(1, skin_hex, &mut hex_b);
    encode_bytes_field(2, &hex_b, &mut lic_id);

    let mut isk_price = Vec::new();
    encode_varint_field(1, encode_zigzag64(100_000), &mut isk_price);
    encode_varint_field(2, 0, &mut isk_price);

    let mut dur_msg = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg);

    let mut create_req = Vec::new();
    encode_bytes_field(1, &isk_price, &mut create_req);
    encode_bytes_field(3, &lic_id, &mut create_req);
    encode_bytes_field(4, &dur_msg, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);

    assert_eq!(
        call_gw(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            create_req.clone(),
            now,
        ),
        STATUS_OK
    );

    for browse_req in [
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
    ] {
        assert_eq!(call_gw(&router, &seller_sess, browse_req, Vec::new(), now), STATUS_OK);
    }

    let listed_uuid_hex = market
        .data
        .read()
        .unwrap()
        .skin_listings
        .keys()
        .next()
        .cloned()
        .expect("skin listing created");
    let mut listed_uuid = [0u8; 16];
    let hb = listed_uuid_hex.as_bytes();
    for (i, b) in listed_uuid.iter_mut().enumerate() {
        let hi = char::from(hb[i * 2]).to_digit(16).unwrap() as u8;
        let lo = char::from(hb[i * 2 + 1]).to_digit(16).unwrap() as u8;
        *b = (hi << 4) | lo;
    }

    let mut get_single = Vec::new();
    put_uuid_msg(1, &listed_uuid, &mut get_single);
    assert_eq!(
        call_gw(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.GetRequest",
            get_single.clone(),
            now,
        ),
        STATUS_OK
    );

    let mut set_exp = Vec::new();
    put_uuid_msg(1, &listed_uuid, &mut set_exp);
    let mut ts_msg = Vec::new();
    encode_varint_field(1, now + 172_800, &mut ts_msg);
    encode_bytes_field(2, &ts_msg, &mut set_exp);
    assert_eq!(
        call_gw(
            &router,
            &gm_sess,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            set_exp,
            now,
        ),
        STATUS_OK
    );

    assert_eq!(
        call_gw(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
            get_single,
            now,
        ),
        STATUS_OK
    );
    assert_eq!(
        call_gw(
            &router,
            &seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            create_req,
            now,
        ),
        STATUS_OK
    );
    let uuid2_hex = market
        .data
        .read()
        .unwrap()
        .skin_listings
        .keys()
        .find(|k| *k != &listed_uuid_hex)
        .cloned()
        .unwrap();
    let mut uuid2 = [0u8; 16];
    let hb2 = uuid2_hex.as_bytes();
    for (i, b) in uuid2.iter_mut().enumerate() {
        let hi = char::from(hb2[i * 2]).to_digit(16).unwrap() as u8;
        let lo = char::from(hb2[i * 2 + 1]).to_digit(16).unwrap() as u8;
        *b = (hi << 4) | lo;
    }
    let mut buy_skin = Vec::new();
    put_uuid_msg(1, &uuid2, &mut buy_skin);
    encode_varint_field(2, 1, &mut buy_skin);
    assert_eq!(
        call_gw(
            &router,
            &buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_skin,
            now,
        ),
        STATUS_OK
    );
}

#[test]
fn skinr_licenses_title_defs_and_character_skill_plan_milestone_roundtrip() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let cid: u32 = 90_000_001;
    let now: u64 = 1_700_000_000;

    let cid_wire = encode_component_id(42, false);
    assert_eq!(parse_component_id_wire(&cid_wire), Some((42, false)));
    let dyn_cid = DynamicQuasarMessage::new("ComponentId");
    assert_eq!(parse_component_id(&dyn_cid), None);
    assert!(!encode_comp_license_id(cid, 42, true).is_empty());
    assert!(!encode_license_kind(42, false, Some(3)).is_empty());

    let skinr = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let skill_mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    let router = PublicGatewayRouter::new(
        shared_descriptors(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_skinr(&router, Arc::clone(&skinr));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&skill_mgr));

    let player_sess = make_session(1, cid, 0);
    let gm_sess = make_session(1, cid, service_roles::ROLE_GM_MASK);

    for title_req in [
        "eve_public.cosmetic.character.title.definition.api.GetAllRequest",
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
    ] {
        assert_eq!(call_gw(&router, &player_sess, title_req, Vec::new(), now), STATUS_OK);
    }

    let first_comp = skinr
        .catalog
        .components
        .values()
        .next()
        .expect("seeded SKINR component");
    let first_comp_pat = first_comp.is_pattern();
    let comp_wire = encode_component_id(first_comp.component_id, first_comp_pat);
    let mut comp_req = Vec::new();
    encode_bytes_field(1, &comp_wire, &mut comp_req);

    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            comp_req.clone(),
            now,
        ),
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(
            &router,
            &gm_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            comp_req.clone(),
            now,
        ),
        STATUS_OK
    );
    let mut inf_lic_req = Vec::new();
    encode_bytes_field(
        1,
        &encode_comp_license_id(cid, first_comp.component_id, first_comp_pat),
        &mut inf_lic_req,
    );
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            inf_lic_req.clone(),
            now,
        ),
        STATUS_OK
    );
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest",
            Vec::new(),
            now,
        ),
        STATUS_OK
    );
    assert_eq!(
        call_gw(
            &router,
            &gm_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
            inf_lic_req,
            now,
        ),
        STATUS_OK
    );

    // Skill & implant Quasar endpoints + Character Skill Plan Create/SetActive/GetActive/Delete lifecycle
    for ok_req in [
        "eve.skillcategory.GetAllRequest",
        "eve.implanttype.GetAllRequest",
        "eve.character.implant.GetActiveImplantsByCharacterRequest",
    ] {
        assert_eq!(call_gw(&router, &player_sess, ok_req, Vec::new(), now), STATUS_OK);
    }
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve.skill.injector.InjectSkillPointsRequest",
            Vec::new(),
            now,
        ),
        STATUS_NOT_IMPLEMENTED
    );

    let mut plan_attrs = Vec::new();
    encode_string_field(1, "PvP Core", &mut plan_attrs);
    let mut create_plan = Vec::new();
    encode_bytes_field(1, &plan_attrs, &mut create_plan);
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.character.skill.plan.CreateRequest",
            create_plan,
            now,
        ),
        STATUS_OK
    );

    let plans = skill_mgr.personal_plans.read().unwrap()[&cid].clone();
    assert_eq!(plans.len(), 1);
    let sp_uuid = plans[0].uuid;

    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.character.skill.plan.GetAllRequest",
            Vec::new(),
            now,
        ),
        STATUS_OK
    );

    let mut sp_id_only = Vec::new();
    put_uuid_msg(1, &sp_uuid, &mut sp_id_only);

    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.character.skill.plan.SetActiveRequest",
            sp_id_only.clone(),
            now,
        ),
        STATUS_OK
    );
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.character.skill.plan.GetActiveRequest",
            Vec::new(),
            now,
        ),
        STATUS_OK
    );
    assert_eq!(
        call_gw(
            &router,
            &player_sess,
            "eve_public.character.skill.plan.DeleteRequest",
            sp_id_only,
            now,
        ),
        STATUS_OK
    );
}
