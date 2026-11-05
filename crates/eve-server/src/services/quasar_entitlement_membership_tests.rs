//! Unit tests for `quasar_entitlement_membership`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use eve_db::{corp_roles, IskCents, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let tmpl = sde.types.values().next().expect("seed type").clone();
        let mut s_buy = tmpl.clone();
        s_buy.type_id = 3400;
        s_buy.group_id = 257;
        s_buy.category_id = 16;
        s_buy.published = true;
        s_buy.base_price = 50_000.0;
        sde.types.insert(3400, s_buy);

        let mut s_drop = tmpl;
        s_drop.type_id = 3401;
        s_drop.group_id = 257;
        s_drop.category_id = 16;
        s_drop.published = true;
        s_drop.base_price = 0.0;
        sde.types.insert(3401, s_drop);
        Arc::new(sde)
    }))
}

fn sess(uid: Option<u64>, cid: Option<i64>, corp: Option<i64>, role: u64, crole: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(99) + cid.unwrap_or(0) as u64);
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

#[test]
fn user_entitlements_logo_grants_membership_and_direct_skill_purchases() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_handlers(&router, Arc::clone(&db));
    let now = 1_760_000_000u64;
    let cid = 90_000_001u64;

    let gm = sess(Some(1), Some(cid as i64), Some(98_000_001), service_roles::ROLE_GM_DEFAULT, 0);
    let player = sess(Some(1), Some(cid as i64), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT, corp_roles::ROLE_DIRECTOR);
    let no_char = sess(Some(1), None, None, service_roles::ROLE_PLAYER_DEFAULT, 0);
    let other = sess(Some(2), Some(90_000_002), Some(1_000_170), service_roles::ROLE_PLAYER_DEFAULT, 0);

    // 1. User entitlements: grant named + expiry + duration, plus omega & subscription
    assert_eq!(call(&router, &player, "eve.entitlement.user.api.GrantRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &gm, "eve.entitlement.user.api.GrantRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    let mut u_sub = Vec::new();
    encode_varint_field(1, 1, &mut u_sub);
    let mut dur_sub = Vec::new();
    encode_varint_field(1, 3600, &mut dur_sub);
    let mut ent1 = Vec::new();
    encode_bytes_field(2, &u_sub, &mut ent1);
    encode_string_field(3, "beta-tester", &mut ent1);
    encode_bytes_field(4, &dur_sub, &mut ent1);
    let mut grant1 = Vec::new();
    encode_bytes_field(1, &ent1, &mut grant1);
    assert_eq!(call(&router, &gm, "eve.entitlement.user.api.GrantRequest", grant1, now).status_code, STATUS_OK);

    let mut ent2 = Vec::new();
    encode_string_field(3, "omega", &mut ent2);
    let mut grant2 = Vec::new();
    encode_bytes_field(1, &ent2, &mut grant2);
    assert_eq!(call(&router, &gm, "eve.entitlement.user.api.GrantRequest", grant2, now).status_code, STATUS_OK);

    let mut sub_body = Vec::new();
    encode_bytes_field(1, &u_sub, &mut sub_body);
    let mut days_sub = Vec::new();
    encode_varint_field(1, 15, &mut days_sub);
    encode_bytes_field(3, &days_sub, &mut sub_body);
    encode_string_field(6, "omega", &mut sub_body);
    let mut sub_req = Vec::new();
    encode_bytes_field(1, &sub_body, &mut sub_req);
    assert_eq!(call(&router, &gm, "eve.entitlement.user.subscription.api.GrantRequest", sub_req, now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.entitlement.user.api.GetRequest", Vec::new(), now).status_code, STATUS_OK);

    // 2. Character ship corp/alliance logo entitlements: grant, get, is-entitled, revoke
    let logo_ident = encode_logo_identifier(cid, 587);
    let mut logo_req = Vec::new();
    encode_bytes_field(1, &logo_ident, &mut logo_req);
    assert_eq!(call(&router, &gm, "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", logo_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &gm, "eve.entitlement.character.ship.admin.alliancelogo.GrantRequest", logo_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.entitlement.character.GetAllRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &gm, "eve.entitlement.character.admin.GetAllRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    assert_eq!(call(&router, &no_char, "eve_public.entitlement.character.GetAllRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);

    assert_eq!(call(&router, &player, "eve.entitlement.character.api.IsEntitledRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    assert_eq!(call(&router, &other, "eve.entitlement.character.api.IsEntitledRequest", logo_req.clone(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &player, "eve.entitlement.character.api.IsEntitledRequest", logo_req.clone(), now).status_code, STATUS_OK);

    assert_eq!(call(&router, &gm, "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest", logo_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &gm, "eve.entitlement.character.ship.admin.alliancelogo.RevokeRequest", logo_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve.entitlement.character.api.IsEntitledRequest", logo_req, now).status_code, STATUS_NOT_FOUND);

    // 3. Membership queries (director, CEO, unaffiliated fallback)
    assert_eq!(call(&router, &player, "eve.membership.GetCharacterAllRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve.membership.corporation.GetCharacterRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &other, "eve.membership.GetCharacterAllRequest", Vec::new(), now).status_code, STATUS_OK);

    // 4. Direct skill purchase validation & injection
    assert_eq!(call(&router, &no_char, "eve.character.skill.PurchaseSkillsRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    assert_eq!(call(&router, &player, "eve.character.skill.PurchaseSkillsRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);

    let mut bad_sk = Vec::new();
    encode_varint_field(1, 3401, &mut bad_sk); // drop-only skill
    assert_eq!(call(&router, &player, "eve.character.skill.PurchaseSkillsRequest", bad_sk, now).status_code, STATUS_PRECONDITION_FAILED);

    db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid as i64).or_default().remove(&3400);
        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
            ch.balance = IskCents(10_000_000_00);
        }
        let mut ord = snap
            .market_orders
            .values()
            .next()
            .expect("seed market order")
            .clone();
        ord.order_id = 999_001;
        ord.type_id = 3400;
        ord.price = IskCents(50_000_00);
        ord.vol_remaining = 100;
        ord.is_bid = false;
        ord.is_npc = true;
        snap.market_orders.insert(999_001, ord);
    });
    let mut ok_sk = Vec::new();
    encode_varint_field(1, 3400, &mut ok_sk);
    assert_eq!(call(&router, &player, "eve.character.skill.PurchaseSkillsRequest", ok_sk.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve.character.skill.PurchaseSkillsRequest", ok_sk, now).status_code, STATUS_CONFLICT);
}
