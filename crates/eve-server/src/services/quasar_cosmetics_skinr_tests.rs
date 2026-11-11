//! Unit tests for `quasar_cosmetics_skinr`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_OK};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, QuasarDescriptorRegistry,
};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "quasar_cosmetics_skinr_wire_edge_tests.rs"]
mod quasar_cosmetics_skinr_wire_edge_tests;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1) * 100 + cid.unwrap_or(0) as u64);
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.role = role;
    s
}

fn call(router: &PublicGatewayRouter, s: &SessionState, ty: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    router.handle_public_request(
        s,
        &EvePublicRequest {
            correlation_uuid: vec![11; 16],
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
fn static_catalog_tier_math_and_wire_encoders() {
    let sde = shared_sde();
    let mut cat = SkinrStaticCatalog::load(&sde);
    cat.components.insert(
        9001,
        SdeSkinComponent {
            component_id: 9001,
            category: COMP_CAT_MATERIAL,
            rarity: 5,
            disallowed_slots: vec![5],
            associated_types: vec![(79001, 5)],
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    cat.components.insert(
        9002,
        SdeSkinComponent {
            component_id: 9002,
            category: COMP_CAT_PATTERN,
            rarity: 6,
            disallowed_slots: Vec::new(),
            associated_types: vec![(79002, -1)],
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    assert!(!cat.components[&9001].is_pattern());
    assert!(cat.components[&9002].is_pattern());
    assert!(!cat.allowed_slots_for_ship(587).is_empty());

    let (pts, tier) = cat.compute_points_and_tier(&[9001, 9002]);
    assert_eq!(pts, 1_150);
    assert_eq!(tier, 7);
    assert_eq!(cat.tier_thresholds_by_isis_group.len(), 49);
    // Rifter (587, isisGroupID=8, tier 5) vs Avatar (11567, isisGroupID=34, tier 11)
    assert_eq!(cat.compute_points_and_tier_for_ship(587, &[9001]).1, 5);
    assert_eq!(cat.compute_points_and_tier_for_ship(11567, &[9001]).1, 11);

    assert_eq!(tier_base_plex_cents(1), 2_500);
    assert_eq!(tier_base_plex_cents(2), 3_000);
    assert_eq!(tier_base_plex_cents(5), 5_000);
    assert_eq!(tier_base_plex_cents(9), 11_000);
    assert_eq!(tier_base_plex_cents(10), 13_000);

    assert_eq!(tier_base_duration_secs(1), 3 * 3_600);
    assert_eq!(tier_base_duration_secs(5), 28 * 3_600);
    assert_eq!(tier_base_duration_secs(18), 532 * 3_600);
    assert_eq!(tier_base_duration_secs(19), 612 * 3_600);

    assert_eq!(SkinrCosmeticsState::comp_key(90_000_001, false, 12), "90000001:0:12");
    assert_eq!(SkinrCosmeticsState::comp_key(90_000_001, true, 12), "90000001:1:12");
    assert_eq!(SkinrCosmeticsState::skin_lic_key(90_000_001, "skin_abc"), "90000001:skin_abc");

    let coat_b = encode_component_id(44, false);
    let pat_b = encode_component_id(55, true);
    assert_eq!(parse_component_id_wire(&coat_b), Some((44, false)));
    assert_eq!(parse_component_id_wire(&pat_b), Some((55, true)));
    assert!(!encode_license_kind(44, false, Some(3)).is_empty());
    assert!(!encode_license_kind(55, true, None).is_empty());

    let pat_slot = SkinLayoutSlot {
        slot_id: 5,
        component_id: 55,
        is_pattern: true,
        primary_coating_id: Some(44),
        secondary_coating_id: Some(45),
    };
    let coat_slot = SkinLayoutSlot {
        slot_id: 1,
        component_id: 44,
        is_pattern: false,
        primary_coating_id: None,
        secondary_coating_id: None,
    };
    assert!(!encode_layout_slot(&pat_slot).is_empty());
    assert!(!encode_layout_slot(&coat_slot).is_empty());
}

#[test]
fn component_licenses_grant_get_revoke_consume_and_title_definitions() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let state = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    // Pick a coating and a pattern component from the loaded SDE catalog
    let (coat_id, _) = state
        .catalog
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern())
        .map(|(&id, c)| (id, c.clone()))
        .expect("coating component in SDE");
    let (pat_id, _) = state
        .catalog
        .components
        .iter()
        .find(|(_, c)| c.is_pattern())
        .map(|(&id, c)| (id, c.clone()))
        .expect("pattern component in SDE");
    let (&consumable_tid, _) = state
        .catalog
        .item_type_to_component
        .iter()
        .next()
        .expect("consumable component license type in SDE");

    let now = 1_760_000_000u64;
    let cid = 90_000_001i64;
    let player = sess(Some(1), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = sess(Some(1), Some(cid), service_roles::ROLE_GM_DEFAULT);
    let other = sess(Some(2), Some(90_000_002), service_roles::ROLE_PLAYER_DEFAULT);

    // 1. Finite component license grant, get, cross-char forbidden, revoke
    let mut fin_grant = Vec::new();
    encode_bytes_field(1, &encode_component_id(coat_id, false), &mut fin_grant);
    encode_varint_field(2, 5, &mut fin_grant);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            fin_grant,
            now
        )
        .status_code,
        STATUS_OK
    );

    let mut fin_id_req = Vec::new();
    encode_bytes_field(1, &encode_comp_license_id(cid as u32, coat_id, false), &mut fin_id_req);
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
            fin_id_req.clone(),
            now
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &other,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
            fin_id_req.clone(),
            now
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // 2. Infinite component license grant, get, revoke
    let mut inf_grant = Vec::new();
    encode_bytes_field(1, &encode_component_id(pat_id, true), &mut inf_grant);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            inf_grant,
            now
        )
        .status_code,
        STATUS_OK
    );

    let mut inf_id_req = Vec::new();
    encode_bytes_field(1, &encode_comp_license_id(cid as u32, pat_id, true), &mut inf_id_req);
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            inf_id_req.clone(),
            now
        )
        .status_code,
        STATUS_OK
    );

    // GetOwnedRequest returns both finite and infinite
    let owned_rsp = call(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest",
        Vec::new(),
        now,
    );
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    assert!(!owned_rsp.payload.unwrap().value.is_empty());

    // Revoke both
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
            fin_id_req,
            now
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
            inf_id_req,
            now
        )
        .status_code,
        STATUS_OK
    );

    // 3. Consume component license item from inventory
    let item_id = 999_888i64;
    let station_id = db.with_snapshot(|s| s.characters[&cid].station_id.unwrap_or(60_003_760));
    db.with_mut_snapshot(|s| {
        s.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: consumable_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut item_seq = Vec::new();
    encode_varint_field(1, item_id as u64, &mut item_seq);
    let mut consume_req = Vec::new();
    encode_bytes_field(1, &item_seq, &mut consume_req);
    encode_varint_field(2, 1, &mut consume_req);
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            consume_req,
            now
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(db.with_snapshot(|s| s.items[&item_id].quantity), 1);

    // 4. Title definition & version endpoints
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.definition.api.GetLatestRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve_public.cosmetic.character.title.definition.api.GetAllRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve.cosmetic.character.title.definition.api.ListDefinitionsRequest", Vec::new(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &player, "eve.cosmetic.character.title.definition.version.api.GetDefaultVersionRequest", Vec::new(), now).status_code,
        STATUS_OK
    );

    let mut ver_req = Vec::new();
    encode_bytes_field(2, &encode_semver([24, 2, 1]), &mut ver_req);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest",
            ver_req,
            now
        )
        .status_code,
        STATUS_OK
    );
    let mut ovr_req = Vec::new();
    encode_string_field(1, "singularity", &mut ovr_req);
    encode_bytes_field(2, &encode_semver([24, 3, 0]), &mut ovr_req);
    assert_eq!(
        call(
            &router,
            &gm,
            "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantRequest",
            ovr_req,
            now
        )
        .status_code,
        STATUS_OK
    );

    // 5. SKINR 1c catalog boundary tests: compute_points_and_tier_for_ship exact <= boundary, tier 20, and non-frigate isis_group_id
    use crate::services::quasar_cosmetics_skinr::SdeSkinComponent;
    let mut custom_cat = SkinrStaticCatalog::load(&SdeRegistry::default());
    custom_cat.components.insert(
        990_001,
        SdeSkinComponent {
            component_id: 990_001,
            category: 3,
            rarity: 1,
            disallowed_slots: vec![],
            associated_types: vec![],
            binder_type_id: 79992,
            binder_count: 1,
        },
    );
    custom_cat.point_values.insert((1, 99), 125);
    custom_cat.point_values.insert((1, 98), 126);
    custom_cat.point_values.insert((1, 97), 5300);
    custom_cat.point_values.insert((1, 96), 5301);
    for (cid, rarity) in [(990_002, 99), (990_003, 98), (990_004, 97), (990_005, 96)] {
        custom_cat.components.insert(
            cid,
            SdeSkinComponent {
                component_id: cid,
                category: 1,
                rarity,
                disallowed_slots: vec![],
                associated_types: vec![],
                binder_type_id: 79992,
                binder_count: 1,
            },
        );
    }
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(587, &[990_001]), (100, 1));
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(587, &[990_002]), (125, 1));
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(587, &[990_003]), (126, 2));
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(587, &[990_004]), (5300, 19));
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(587, &[990_005]), (5301, 20));
    custom_cat.ship_isis_groups.insert(11_567, 45);
    custom_cat
        .tier_thresholds_by_isis_group
        .insert(45, [(1, 50), (19, 500)].into_iter().collect());
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(11_567, &[990_001]), (100, 19));
    assert_eq!(custom_cat.compute_points_and_tier_for_ship(11_567, &[990_004]), (5300, 20));
}
