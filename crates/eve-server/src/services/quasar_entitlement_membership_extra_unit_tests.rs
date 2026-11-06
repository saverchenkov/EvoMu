//! Extra branch-coverage and mutation-killing unit tests for `quasar_entitlement_membership`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED, STATUS_UNAUTHORIZED,
};
use crate::services::quasar_auth_user::{read_identity_store, with_identity_store};
use crate::services::quasar_entitlement_membership::{encode_logo_identifier, register_handlers};
use eve_db::{corp_roles, IskCents, MarketOrderRecord, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn test_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let tmpl = sde.types.values().next().expect("seed type").clone();

        let mut sk_pub = tmpl.clone();
        sk_pub.type_id = 3450;
        sk_pub.category_id = 16;
        sk_pub.published = true;
        sk_pub.base_price = 10_000.0;
        sde.types.insert(3450, sk_pub);

        let mut sk_unpub = tmpl.clone();
        sk_unpub.type_id = 3451;
        sk_unpub.category_id = 16;
        sk_unpub.published = false;
        sk_unpub.base_price = 10_000.0;
        sde.types.insert(3451, sk_unpub);

        let mut sk_drop = tmpl;
        sk_drop.type_id = 3452;
        sk_drop.category_id = 16;
        sk_drop.published = true;
        sk_drop.base_price = 0.0;
        sde.types.insert(3452, sk_drop);

        Arc::new(sde)
    }))
}

fn invoke(
    router: &PublicGatewayRouter,
    req_name: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handlers = router.handlers.read().unwrap();
    let h = handlers.get(req_name).expect(req_name);
    h(ctx, &GoogleAny::default(), msg)
}

#[test]
fn user_entitlement_expiry_filtering_and_grant_shapes() {
    let sde = test_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db));

    let anon_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    let user_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    let now = user_ctx.now_unix();
    let admin_ctx = user_ctx.clone().with_service_roles(u64::MAX);

    // Unauthenticated user GetRequest -> STATUS_FORBIDDEN
    assert_eq!(
        invoke(
            &router,
            "eve_public.entitlement.user.api.GetRequest",
            &anon_ctx,
            &DynamicQuasarMessage::new("Get")
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // GrantRequest shape B (field 2 Message with field 1 String, and target user on outer field 1 Sint64)
    let mut ent_b = DynamicQuasarMessage::new("Ent");
    ent_b.fields.insert(1, DynamicValue::String("  custom-pack  ".into()));
    let mut grant_b = DynamicQuasarMessage::new("Grant");
    grant_b.fields.insert(1, DynamicValue::Sint64(1));
    grant_b.fields.insert(2, DynamicValue::Message(Box::new(ent_b)));
    assert!(invoke(&router, "eve.entitlement.user.api.GrantRequest", &admin_ctx, &grant_b).is_ok());

    // GrantRequest shape A with field 1 String name and explicit future expiry (field 5 Int32 / Uint64)
    let mut ent_a = DynamicQuasarMessage::new("Ent");
    ent_a.fields.insert(1, DynamicValue::String("timed-pack".into()));
    ent_a.fields.insert(2, DynamicValue::Int32(1));
    ent_a.fields.insert(5, DynamicValue::Uint64(now + 500));
    let mut grant_a = DynamicQuasarMessage::new("Grant");
    grant_a.fields.insert(1, DynamicValue::Message(Box::new(ent_a)));
    assert!(invoke(&router, "eve.entitlement.user.api.GrantRequest", &admin_ctx, &grant_a).is_ok());

    // Re-granting same entitlement name is idempotent and does not duplicate
    assert!(invoke(&router, "eve.entitlement.user.api.GrantRequest", &admin_ctx, &grant_a).is_ok());

    // Seed an expired entitlement to verify GetRequest filters out `exp <= now_unix`
    with_identity_store(&db, |st| {
        let list = st.user_entitlements.entry(1).or_default();
        list.push("expired-pack".into());
        st.user_entitlement_expiries
            .entry(1)
            .or_default()
            .insert("expired-pack".into(), now);
        st.omega_subscriptions.insert(1, now + 3_600);
    });

    let get_res = invoke(
        &router,
        "eve_public.entitlement.user.api.GetRequest",
        &user_ctx,
        &DynamicQuasarMessage::new("Get"),
    )
    .unwrap();
    let get_str = String::from_utf8_lossy(&get_res.value);
    assert!(get_str.contains("timed-pack") && !get_str.contains("expired-pack"));

    // Grant omega via expiry_opt in shape A
    let mut ent_om = DynamicQuasarMessage::new("Ent");
    ent_om.fields.insert(3, DynamicValue::String("OMEGA".into()));
    ent_om.fields.insert(5, DynamicValue::Uint64(now + 7_200));
    let mut grant_om = DynamicQuasarMessage::new("Grant");
    grant_om.fields.insert(1, DynamicValue::Message(Box::new(ent_om)));
    assert!(invoke(&router, "eve.entitlement.user.api.GrantRequest", &admin_ctx, &grant_om).is_ok());

    // Subscription GrantRequest error branches: missing field 1, missing uid, blank sub_name default
    assert_eq!(
        invoke(
            &router,
            "eve.entitlement.user.subscription.api.GrantRequest",
            &admin_ctx,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let anon_admin = anon_ctx.clone().with_service_roles(u64::MAX);
    let mut sub_no_uid = DynamicQuasarMessage::new("Sub");
    sub_no_uid.fields.insert(3, DynamicValue::Uint32(10));
    let mut req_no_uid = DynamicQuasarMessage::new("Req");
    req_no_uid
        .fields
        .insert(1, DynamicValue::Message(Box::new(sub_no_uid)));
    assert_eq!(
        invoke(
            &router,
            "eve.entitlement.user.subscription.api.GrantRequest",
            &anon_admin,
            &req_no_uid
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let mut sub_ok = DynamicQuasarMessage::new("Sub");
    sub_ok.fields.insert(1, DynamicValue::Sint32(1));
    sub_ok.fields.insert(3, DynamicValue::Int64(7));
    sub_ok.fields.insert(6, DynamicValue::String("   ".into()));
    let mut req_sub_ok = DynamicQuasarMessage::new("Req");
    req_sub_ok
        .fields
        .insert(1, DynamicValue::Message(Box::new(sub_ok)));
    assert!(invoke(
        &router,
        "eve.entitlement.user.subscription.api.GrantRequest",
        &admin_ctx,
        &req_sub_ok
    )
    .is_ok());
}

#[test]
fn character_logo_entitlements_all_endpoints_and_admin_override() {
    let sde = test_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db));

    let cid = 90_000_001_u64;
    let ship_type = 587_u64;
    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    let other_admin_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(1_000_170),
        0,
        "tranquility",
        vec![1],
    )
    .with_service_roles(u64::MAX);

    let encoded_ident = encode_logo_identifier(cid, ship_type);
    assert!(!encoded_ident.is_empty());

    // Test direct field 1 / field 2 target extraction (without nested Message on field 1)
    let mut flat_target = DynamicQuasarMessage::new("FlatLogo");
    flat_target.fields.insert(1, DynamicValue::Uint64(cid));
    flat_target.fields.insert(2, DynamicValue::Uint32(ship_type as u32));

    for req_name in [
        "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
        "eve.entitlement.character.ship.admin.alliancelogo.GrantRequest",
        "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
        "eve.entitlement.character.ship.admin.corplogo.GrantRequest",
    ] {
        assert!(invoke(&router, req_name, &other_admin_ctx, &flat_target).is_ok());
    }

    // Missing ship_type_id in GrantRequest -> STATUS_BAD_REQUEST
    let mut missing_stid = DynamicQuasarMessage::new("BadLogo");
    missing_stid.fields.insert(1, DynamicValue::Uint64(cid));
    assert_eq!(
        invoke(
            &router,
            "eve.entitlement.character.ship.admin.corplogo.GrantRequest",
            &other_admin_ctx,
            &missing_stid
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Admin GetAllRequest with explicit character ID succeeds
    let mut get_all_msg = DynamicQuasarMessage::new("GetAll");
    get_all_msg.fields.insert(1, DynamicValue::Int64(cid as i64));
    let all_res = invoke(
        &router,
        "eve.entitlement.character.admin.GetAllRequest",
        &other_admin_ctx,
        &get_all_msg,
    )
    .unwrap();
    assert!(!all_res.value.is_empty());

    // IsEntitledRequest for alliance logo (field 2) queried by another character with admin role
    let mut ident_msg = DynamicQuasarMessage::new("Ident");
    ident_msg.fields.insert(1, DynamicValue::Uint64(cid));
    ident_msg.fields.insert(2, DynamicValue::Uint64(ship_type));
    let mut is_ent_alliance = DynamicQuasarMessage::new("IsEntitled");
    is_ent_alliance
        .fields
        .insert(2, DynamicValue::Message(Box::new(ident_msg.clone())));
    assert!(invoke(
        &router,
        "eve.entitlement.character.api.IsEntitledRequest",
        &other_admin_ctx,
        &is_ent_alliance
    )
    .is_ok());

    // Revoke all 4 revoke endpoints
    for req_name in [
        "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
        "eve.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
        "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
        "eve.entitlement.character.ship.admin.corplogo.RevokeRequest",
    ] {
        assert!(invoke(&router, req_name, &other_admin_ctx, &flat_target).is_ok());
    }

    assert_eq!(
        invoke(
            &router,
            "eve.entitlement.character.api.IsEntitledRequest",
            &player_ctx,
            &is_ent_alliance
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    let (corp_left, all_left) = read_identity_store(&db, |st| {
        (
            st.char_corp_logo_ships.get(&cid).cloned().unwrap_or_default(),
            st.char_alliance_logo_ships.get(&cid).cloned().unwrap_or_default(),
        )
    });
    assert!(corp_left.is_empty() && all_left.is_empty());
}

#[test]
fn membership_titles_alliances_and_quasar_skill_purchase_branches() {
    let sde = test_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db));

    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(99_000_001);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = cid;
        }
    });

    let member_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        0,
        "tranquility",
        vec![1],
    );
    // GetCharacterAllRequest with Some(alliance_id) and missing character fallback
    let all_mem = invoke(
        &router,
        "eve.membership.GetCharacterAllRequest",
        &member_ctx,
        &DynamicQuasarMessage::new("GetAll"),
    )
    .unwrap();
    assert!(!all_mem.value.is_empty());

    let missing_char_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(888_777_666),
        None,
        0,
        "tranquility",
        vec![1],
    );
    assert!(invoke(
        &router,
        "eve.membership.GetCharacterAllRequest",
        &missing_char_ctx,
        &DynamicQuasarMessage::new("GetAll")
    )
    .is_ok());

    // Corporation GetCharacterRequest: CEO branch, Director branch, Member branch, and missing character branch
    let mut cid_sub = DynamicQuasarMessage::new("CharId");
    cid_sub.fields.insert(1, DynamicValue::Uint32(cid as u32));
    let mut corp_req = DynamicQuasarMessage::new("CorpGet");
    corp_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(cid_sub)));

    let ceo_resp = invoke(
        &router,
        "eve.membership.corporation.GetCharacterRequest",
        &member_ctx,
        &corp_req,
    )
    .unwrap();
    let ceo_str = String::from_utf8_lossy(&ceo_resp.value);
    assert!(ceo_str.contains("Chief Executive Officer"));

    db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = 90_000_999;
        }
    });
    let dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let dir_resp = invoke(
        &router,
        "eve.membership.corporation.GetCharacterRequest",
        &dir_ctx,
        &corp_req,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&dir_resp.value).contains("Director"));

    let mem_resp = invoke(
        &router,
        "eve.membership.corporation.GetCharacterRequest",
        &member_ctx,
        &corp_req,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&mem_resp.value).contains("Member"));

    let ghost_resp = invoke(
        &router,
        "eve.membership.corporation.GetCharacterRequest",
        &missing_char_ctx,
        &DynamicQuasarMessage::new("CorpGet"),
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&ghost_resp.value).contains("Member"));

    // PurchaseSkillsRequest error branches:
    // 1) Unauthenticated -> STATUS_UNAUTHORIZED
    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &unauth_ctx,
            &DynamicQuasarMessage::new("Buy")
        )
        .unwrap_err()
        .0,
        STATUS_UNAUTHORIZED
    );

    // 2) Unknown skill type -> STATUS_NOT_FOUND
    let mut buy_unknown = DynamicQuasarMessage::new("Buy");
    buy_unknown
        .fields
        .insert(1, DynamicValue::Repeated(vec![DynamicValue::Uint32(999_888_777)]));
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &member_ctx,
            &buy_unknown
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // 3) Unpublished skill (3451) or non-skill (34) -> STATUS_BAD_REQUEST
    let mut buy_unpub = DynamicQuasarMessage::new("Buy");
    buy_unpub.fields.insert(2, DynamicValue::Uint32(3451));
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &member_ctx,
            &buy_unpub
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 4) Published skill (3450) without NPC market order -> STATUS_PRECONDITION_FAILED
    db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().remove(&3450);
        snap.market_orders.retain(|_, o| o.type_id != 3450);
    });
    let mut buy_3450 = DynamicQuasarMessage::new("Buy");
    buy_3450.fields.insert(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Uint32(3450),
            DynamicValue::Uint32(3450), // duplicate deduplicated
        ]),
    );
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &member_ctx,
            &buy_3450
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    // 5) Seed NPC market order for 3450, test insufficient ISK -> STATUS_PRECONDITION_FAILED,
    //    missing character -> STATUS_NOT_FOUND, then sufficient ISK -> Ok, then duplicate -> STATUS_CONFLICT
    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            995_001,
            MarketOrderRecord {
                order_id: 995_001,
                owner_id: 1,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 3450,
                is_bid: false,
                price: IskCents(10_000_00),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(1_000_00);
        }
    });
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &member_ctx,
            &buy_3450
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &missing_char_ctx,
            &buy_3450
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.balance = IskCents(13_000_00);
        }
    });
    assert!(invoke(
        &router,
        "eve.character.skill.PurchaseSkillsRequest",
        &member_ctx,
        &buy_3450
    )
    .is_ok());
    assert_eq!(
        invoke(
            &router,
            "eve.character.skill.PurchaseSkillsRequest",
            &member_ctx,
            &buy_3450
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );
}
