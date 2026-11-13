//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_entitlement_membership`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_auth_user::{read_identity_store, with_identity_store};
use eve_db::{
    corp_roles, CorporationRecord, InventoryItemRecord, IskCents, MarketOrderRecord,
    WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse,
    GoogleAny, QuasarDescriptorRegistry, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        if let Some(tmpl) = sde.types.values().next().cloned() {
            let mut sk = tmpl;
            sk.type_id = 3400;
            sk.group_id = 257;
            sk.category_id = 16;
            sk.published = true;
            sk.base_price = 50_000.0;
            sde.types.insert(3400, sk);
        }
        Arc::new(sde)
    }))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(QuasarDescriptorRegistry::open_default_or_empty))
}

fn setup() -> (Arc<WorldRepository>, PublicGatewayRouter) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let desc = shared_descriptors();
    let router = PublicGatewayRouter::new(desc, Arc::clone(&db), sde);
    register_handlers(&router, Arc::clone(&db));
    (db, router)
}

fn sess(uid: Option<u64>, cid: Option<i64>, corp: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = corp;
    s.role = role;
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
            correlation_uuid: vec![3; 16],
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

fn extract_strings(wire: &WireMessage, field: u32) -> Vec<String> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_bytes().and_then(|b| String::from_utf8(b.to_vec()).ok()))
        .collect()
}

fn extract_messages(wire: &WireMessage, field: u32) -> Vec<WireMessage> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_bytes().and_then(|b| WireMessage::decode(b).ok()))
        .collect()
}

fn extract_varints(wire: &WireMessage, field: u32) -> Vec<u64> {
    wire.fields
        .iter()
        .filter(|f| f.field_number == field)
        .filter_map(|f| f.value.as_varint())
        .collect()
}

#[test]
fn extract_nested_seq_id_and_logo_entitlement_target_cover_numeric_variants_and_shapes() {
    assert_eq!(extract_nested_seq_id(&DynamicValue::Uint64(42)), Some(42));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Int64(99)), Some(99));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Sint64(7)), Some(7));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Uint32(15)), Some(15));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Int32(25)), Some(25));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Sint32(35)), Some(35));
    assert_eq!(extract_nested_seq_id(&DynamicValue::Int64(0)), None);
    assert_eq!(extract_nested_seq_id(&DynamicValue::Int64(-5)), None);
    assert_eq!(extract_nested_seq_id(&DynamicValue::Sint32(-1)), None);

    let mut nested = DynamicQuasarMessage::new("Nested");
    nested.set(1, DynamicValue::Uint64(587));
    assert_eq!(
        extract_nested_seq_id(&DynamicValue::Message(Box::new(nested.clone()))),
        Some(587)
    );

    let ctx = AuthoritativeContext::for_session_at(
        None,
        Some(2110_001),
        None,
        0,
        "tranquility",
        vec![],
        1_760_000_000,
    );

    // Flat target with effective_character_id fallback
    let mut flat_msg = DynamicQuasarMessage::new("Flat");
    flat_msg.set(2, DynamicValue::Uint64(587));
    assert_eq!(
        extract_logo_entitlement_target(&ctx, &flat_msg),
        Ok((2110_001, 587))
    );

    // Nested Message in field 1 containing char_id (1) and ship_type_id (2)
    let mut inner = DynamicQuasarMessage::new("Inner");
    inner.set(1, DynamicValue::Uint64(2110_009));
    inner.set(2, DynamicValue::Message(Box::new(nested)));
    let mut nested_msg = DynamicQuasarMessage::new("Outer");
    nested_msg.set(1, DynamicValue::Message(Box::new(inner)));
    assert_eq!(
        extract_logo_entitlement_target(&ctx, &nested_msg),
        Ok((2110_009, 587))
    );

    // Missing ship_type_id -> 400
    let missing_ship = DynamicQuasarMessage::new("Empty");
    assert_eq!(
        extract_logo_entitlement_target(&ctx, &missing_ship).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn user_entitlement_get_filters_expired_and_deduplicates_omega() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;

    // uid == None -> 401; uid == 0 -> 403
    let unauth = sess(None, None, None, 0);
    assert_eq!(
        call(&router, &unauth, "eve_public.entitlement.user.api.GetRequest", Vec::new(), now)
            .status_code,
        STATUS_UNAUTHORIZED
    );
    let zero_uid = sess(Some(0), None, None, 0);
    assert_eq!(
        call(&router, &zero_uid, "eve_public.entitlement.user.api.GetRequest", Vec::new(), now)
            .status_code,
        STATUS_FORBIDDEN
    );

    // Case A: "omega" in both user_entitlements and omega_subscriptions -> deduplicated; expired entry excluded
    with_identity_store(&db, |st| {
        st.user_entitlements.insert(
            77,
            vec![
                "omega".to_string(),
                "expired_perk".to_string(),
                "alpha_bonus".to_string(),
            ],
        );
        st.user_entitlement_expiries.insert(
            77,
            std::collections::HashMap::from([
                ("omega".to_string(), now + 1_000),
                ("expired_perk".to_string(), now), // <= now -> excluded
                ("alpha_bonus".to_string(), now + 500),
            ]),
        );
        st.omega_subscriptions.insert(77, now + 5_000);
        // Case B: "omega" ONLY in omega_subscriptions
        st.omega_subscriptions.insert(78, now + 3_600);
    });

    let r77 = call(
        &router,
        &sess(Some(77), Some(2110_077), None, 0),
        "eve_public.entitlement.user.api.GetRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r77.status_code, STATUS_OK);
    let w77 = WireMessage::decode(&r77.payload.unwrap().value).unwrap();
    let names77: Vec<String> = extract_messages(&w77, 1)
        .into_iter()
        .flat_map(|e| extract_strings(&e, 1))
        .collect();
    assert_eq!(names77.iter().filter(|n| n.as_str() == "omega").count(), 1);
    assert!(names77.contains(&"alpha_bonus".to_string()));
    assert!(!names77.contains(&"expired_perk".to_string()));

    let r78 = call(
        &router,
        &sess(Some(78), Some(2110_078), None, 0),
        "eve_public.entitlement.user.api.GetRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r78.status_code, STATUS_OK);
    let w78 = WireMessage::decode(&r78.payload.unwrap().value).unwrap();
    let names78: Vec<String> = extract_messages(&w78, 1)
        .into_iter()
        .flat_map(|e| extract_strings(&e, 1))
        .collect();
    assert_eq!(names78, vec!["omega".to_string()]);
}

#[test]
fn character_logo_entitlement_admin_grant_double_grant_revoke_and_is_entitled_checks() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let player = sess(Some(10), Some(2110_010), Some(1_000_009), 0);
    let other_player = sess(Some(11), Some(2110_011), Some(1_000_009), 0);
    let admin = sess(Some(99), Some(2110_099), Some(1_000_009), service_roles::ROLE_ADMIN);

    let corp_ident = encode_logo_identifier(2110_010, 587);
    let grant_corp = pb::message("eve_public.entitlement.character.ship.admin.corplogo.GrantRequest")
        .set_raw_message("entitlement", corp_ident.clone())
        .must_encode();

    let alliance_ident = encode_logo_identifier(2110_010, 588);
    let grant_alliance = pb::message("eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest")
        .set_raw_message("entitlement", alliance_ident.clone())
        .must_encode();

    // Non-admin rejected across Grant, Revoke, and GetAll
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
            grant_corp.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
            grant_alliance.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &player,
            "eve.entitlement.character.admin.GetAllRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Admin double grant is idempotent (corp logo) + alliance logo grant
    assert_eq!(
        call(
            &router,
            &admin,
            "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
            grant_corp.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &admin,
            "eve.entitlement.character.ship.admin.corplogo.GrantRequest",
            grant_corp.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &admin,
            "eve.entitlement.character.ship.admin.alliancelogo.GrantRequest",
            grant_alliance.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    // Admin GetAll encodes both corp and alliance logos for target char
    let get_all = pb::message("eve.entitlement.character.admin.GetAllRequest")
        .set("owner.sequential", 2110_010u32)
        .must_encode();
    let all_resp = call(
        &router,
        &admin,
        "eve.entitlement.character.admin.GetAllRequest",
        get_all,
        now,
    );
    assert_eq!(all_resp.status_code, STATUS_OK);
    let w_all = WireMessage::decode(&all_resp.payload.unwrap().value).unwrap();
    assert_eq!(extract_messages(&w_all, 1).len(), 2);

    // Admin can query IsEntitledRequest for another character; non-admin gets 403/404 as appropriate
    let is_ent_alliance = pb::message("eve.entitlement.character.api.IsEntitledRequest")
        .set_raw_message("alliance_logo", alliance_ident)
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.character.api.IsEntitledRequest", is_ent_alliance.clone(), now)
            .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &other_player, "eve.entitlement.character.api.IsEntitledRequest", is_ent_alliance, now)
            .status_code,
        STATUS_FORBIDDEN
    );

    // Seed active ship cosmetic logo & verify revoke prunes it + emits RevokedNotice
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 587,
                owner_id: 2110_010,
                ..Default::default()
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_cosmetics_live".to_string(),
            r#"{"ship_logos":{"700001":{"0":2}}}"#.to_string(),
        );
    });
    router.routing_cache.write().unwrap().entries.clear();
    assert_eq!(
        call(
            &router,
            &admin,
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
            grant_corp.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    let notices: Vec<String> = router
        .routing_cache
        .read()
        .unwrap()
        .entries
        .iter()
        .filter_map(|e| e.notice.payload.as_ref().map(|a| a.type_url.clone()))
        .collect();
    assert!(notices.iter().any(|u| u.ends_with("RevokedNotice")));
    assert!(!db.with_snapshot(|s| s
        .quasar_state_blobs
        .get("quasar_cosmetics_live")
        .is_some_and(|b| b.contains("700001"))));

    // Re-checking corp logo IsEntitled returns 404 now that it is revoked
    let is_ent_corp = pb::message("eve.entitlement.character.api.IsEntitledRequest")
        .set_raw_message("corp_logo", corp_ident)
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.character.api.IsEntitledRequest", is_ent_corp, now)
            .status_code,
        STATUS_NOT_FOUND
    );
}

#[test]
fn admin_user_entitlement_and_subscription_grant_validation_and_clamping() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let player = sess(Some(10), Some(2110_010), None, 0);
    let admin = sess(Some(99), Some(2110_099), None, service_roles::ROLE_ADMIN);

    // 1. eve.entitlement.user.api.GrantRequest: whitespace name -> 400
    let ws_req = pb::message("eve.entitlement.user.api.GrantRequest")
        .set_with("user_entitlement_fulfillment", |e| {
            e.set("user.sequential", 55i32)
                .set("entitlement_name", "   ")
        })
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.user.api.GrantRequest", ws_req, now).status_code,
        STATUS_BAD_REQUEST
    );

    // 2. Grant "omega" with explicit expiry (field 5) -> updates omega_subscriptions
    let omega_exp_req = pb::message("eve.entitlement.user.api.GrantRequest")
        .set_with("user_entitlement_fulfillment", |e| {
            e.set("user.sequential", 55i32)
                .set("entitlement_name", "omega")
                .set("expiry_date.seconds", (now + 10_000) as i64)
        })
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.user.api.GrantRequest", omega_exp_req, now).status_code,
        STATUS_OK
    );
    assert_eq!(
        read_identity_store(&db, |st| st.omega_subscriptions.get(&55).copied()),
        Some(now + 10_000)
    );

    // 3. Grant expired "omega" (expiry <= now) defaults to 30 days minimum
    let expired_req = pb::message("eve.entitlement.user.api.GrantRequest")
        .set_with("user_entitlement_fulfillment", |e| {
            e.set("user.sequential", 56i32)
                .set("entitlement_name", "omega")
                .set("expiry_date.seconds", (now - 10) as i64)
        })
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.user.api.GrantRequest", expired_req, now).status_code,
        STATUS_OK
    );
    assert!(
        read_identity_store(&db, |st| st.omega_subscriptions.get(&56).copied().unwrap_or(0) > now)
    );

    // 4. eve.entitlement.user.subscription.api.GrantRequest guards & defaults
    assert_eq!(
        call(&router, &player, "eve.entitlement.user.subscription.api.GrantRequest", Vec::new(), now)
            .status_code,
        STATUS_FORBIDDEN
    );
    // Missing field 1 -> 400
    assert_eq!(
        call(&router, &admin, "eve.entitlement.user.subscription.api.GrantRequest", Vec::new(), now)
            .status_code,
        STATUS_BAD_REQUEST
    );
    // uid == 0 -> 400
    let zero_admin = sess(Some(0), Some(2110_099), None, service_roles::ROLE_ADMIN);
    let zero_uid = pb::message("eve.entitlement.user.subscription.api.GrantRequest")
        .set("subscription_fulfillment.user.sequential", 0i32)
        .must_encode();
    assert_eq!(
        call(&router, &zero_admin, "eve.entitlement.user.subscription.api.GrantRequest", zero_uid, now)
            .status_code,
        STATUS_BAD_REQUEST
    );
    // days == 0 clamped to 1 day & empty field 6 defaults to "omega"
    let sub_grant = pb::message("eve.entitlement.user.subscription.api.GrantRequest")
        .set_with("subscription_fulfillment", |s| {
            s.set("user.sequential", 57i32)
                .set("recurring_interval_in_days", 0i32)
        })
        .must_encode();
    assert_eq!(
        call(&router, &admin, "eve.entitlement.user.subscription.api.GrantRequest", sub_grant, now)
            .status_code,
        STATUS_OK
    );
    let exp57 = read_identity_store(&db, |st| st.omega_subscriptions.get(&57).copied().unwrap_or(0));
    assert!(exp57 >= now + 86_400);
}

#[test]
fn membership_roles_and_missing_character_fallback() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let corp_id = 98_500_001_i64;
    let alliance_id = 99_500_001_i64;
    let ceo_id = 2110_501_i64;
    let dir_id = 2110_502_i64;
    let mem_id = 2110_503_i64;

    db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Mem Corp".to_string(); __corp.ticker = "MEMC".to_string(); __corp.ceo_id = ceo_id; __corp.alliance_id = Some(alliance_id); __corp.tax_rate = 0.05; __corp.members = [ceo_id, dir_id, mem_id].into_iter().collect(); __corp.member_roles = [(dir_id, corp_roles::ROLE_DIRECTOR)].into_iter().collect(); __corp.applications = Default::default(); __corp.wallet_divisions = Vec::new(); __corp.description = String::new(); __corp.url = String::new(); __corp.hq_station_id = None; let __mw = IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp };
        corp.member_roles.insert(mem_id, 0);
        snap.corporations.insert(corp_id, corp);

        let tmpl = snap.characters.get(&90_000_001).unwrap().clone();
        for cid in [ceo_id, dir_id, mem_id] {
            let mut ch = tmpl.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }
    });

    for (cid, expected_title) in [
        (ceo_id, "Chief Executive Officer"),
        (dir_id, "Director"),
        (mem_id, "Member"),
    ] {
        let req = pb::message("eve.membership.corporation.GetCharacterRequest")
            .set("character_id.sequential", cid as u32)
            .must_encode();
        let s = sess(Some(1), Some(cid), Some(corp_id), 0);
        let resp = call(&router, &s, "eve.membership.corporation.GetCharacterRequest", req.clone(), now);
        assert_eq!(resp.status_code, STATUS_OK);
        let w = WireMessage::decode(&resp.payload.unwrap().value).unwrap();
        let all_strs: Vec<String> = (1..=6).flat_map(|f| extract_strings(&w, f)).collect();
        let msg_strs: Vec<String> = (1..=6)
            .flat_map(|f| extract_messages(&w, f))
            .flat_map(|m| (1..=6).flat_map(|f| extract_strings(&m, f)).collect::<Vec<_>>())
            .collect();
        assert!(
            all_strs.iter().chain(msg_strs.iter()).any(|t| t == expected_title),
            "expected title {expected_title} for cid {cid}, got {all_strs:?} / {msg_strs:?}"
        );
    }

    // Missing character -> (1000170, "Member")
    let missing_req = pb::message("eve.membership.corporation.GetCharacterRequest")
        .set("character_id.sequential", 2110_999u32)
        .must_encode();
    let miss_resp = call(
        &router,
        &sess(Some(1), Some(2110_999), None, 0),
        "eve.membership.corporation.GetCharacterRequest",
        missing_req,
        now,
    );
    assert_eq!(miss_resp.status_code, STATUS_OK);
    let w_miss = WireMessage::decode(&miss_resp.payload.unwrap().value).unwrap();
    let varints: Vec<u64> = (1..=4)
        .flat_map(|f| extract_messages(&w_miss, f))
        .flat_map(|m| extract_varints(&m, 1))
        .collect();
    assert!(varints.contains(&1_000_170));
}

#[test]
fn purchase_skills_validation_dedup_markup_and_history_edges() {
    let (db, router) = setup();
    let now = 1_760_000_000_u64;
    let cid = 2110_600_i64;
    let s = sess(Some(60), Some(cid), Some(1_000_009), 0);

    // Find an actual published skill type with positive base_price from SDE (category 16)
    let skill_id = shared_sde()
        .types
        .values()
        .find(|t| t.category_id == 16 && t.published && t.base_price > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(3400);

    // 1. Unknown type -> 404
    let unk = pb::message("eve.character.skill.PurchaseSkillsRequest")
        .add_with("type", |t| t.set("sequential", 999_999_888u64))
        .must_encode();
    assert_eq!(
        call(&router, &s, "eve.character.skill.PurchaseSkillsRequest", unk, now).status_code,
        STATUS_NOT_FOUND
    );

    // 2. Non-skill type (34 Tritanium) -> 400
    let non_skill = pb::message("eve.character.skill.PurchaseSkillsRequest")
        .add_with("type", |t| t.set("sequential", 34u64))
        .must_encode();
    assert_eq!(
        call(&router, &s, "eve.character.skill.PurchaseSkillsRequest", non_skill, now).status_code,
        STATUS_BAD_REQUEST
    );

    // 3. No NPC sell order -> 412
    let no_order = pb::message("eve.character.skill.PurchaseSkillsRequest")
        .add_with("type", |t| t.set("sequential", u64::from(skill_id)))
        .must_encode();
    assert_eq!(
        call(&router, &s, "eve.character.skill.PurchaseSkillsRequest", no_order.clone(), now)
            .status_code,
        STATUS_PRECONDITION_FAILED
    );

    // 4. Add NPC sell order with price 10_000 cents (1.30x = 13_000 cents), char has only 5_000 -> 412
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&90_000_001).unwrap().clone();
        ch.character_id = cid;
        ch.balance = IskCents(5_000);
        snap.characters.insert(cid, ch);
        snap.market_orders.insert(
            90_001,
            MarketOrderRecord {
                order_id: 90_001,
                owner_id: 1_000_002,
                station_id: 6000_3760,
                region_id: 1000_0002,
                type_id: skill_id,
                is_bid: false,
                price: IskCents(10_000),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
    });
    assert_eq!(
        call(&router, &s, "eve.character.skill.PurchaseSkillsRequest", no_order, now).status_code,
        STATUS_PRECONDITION_FAILED
    );

    // 5. Duplicate skill ID across repeated `type` -> charged only once at 1.30x (13_000), updates skill + journal + skill_history
    db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(50_000);
    });
    let dup_req = pb::message("eve.character.skill.PurchaseSkillsRequest")
        .add_with("type", |t| t.set("sequential", u64::from(skill_id)))
        .add_with("type", |t| t.set("sequential", u64::from(skill_id)))
        .must_encode();
    let ok = call(&router, &s, "eve.character.skill.PurchaseSkillsRequest", dup_req, now);
    assert_eq!(ok.status_code, STATUS_OK);
    db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&cid).unwrap().balance.0, 50_000 - 13_000);
        assert!(snap.skills.get(&cid).and_then(|m| m.get(&skill_id)).is_some());
        assert!(!snap.wallet_journal.is_empty());
        assert!(!snap.skill_history.get(&cid).map_or(true, |v| v.is_empty()));
    });

    // 6. Missing character in snap.characters -> 404
    let missing_s = sess(Some(61), Some(2110_699), Some(1_000_009), 0);
    let req_miss_char = pb::message("eve.character.skill.PurchaseSkillsRequest")
        .add_with("type", |t| t.set("sequential", u64::from(skill_id)))
        .must_encode();
    assert_eq!(
        call(&router, &missing_s, "eve.character.skill.PurchaseSkillsRequest", req_miss_char, now)
            .status_code,
        STATUS_NOT_FOUND
    );
}
