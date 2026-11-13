#![forbid(unsafe_code)]

use super::quasar_cosmetics_skinr::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleDuration, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

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
fn test_empty_sde_catalog_fallbacks_tier_boundaries_and_wire_helpers() {
    let empty_sde = SdeRegistry::default();
    let mut cat = SkinrStaticCatalog::load(&empty_sde);
    assert_eq!(cat.point_values.get(&(1, 1)), Some(&25));
    assert_eq!(cat.point_values.get(&(2, 6)), Some(&750));
    assert_eq!(cat.point_values.get(&(3, 6)), Some(&900));
    assert_eq!(cat.tier_thresholds.get(&1), Some(&125));
    assert_eq!(cat.tier_thresholds.get(&5), Some(&850));
    assert_eq!(cat.default_ship_slots, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(cat.slot_allowed_categories.get(&5), Some(&vec![COMP_CAT_PATTERN]));

    cat.ship_slot_overrides.insert(587, vec![1, 2, 5]);
    assert_eq!(cat.allowed_slots_for_ship(587), &[1, 2, 5]);
    assert_eq!(cat.allowed_slots_for_ship(999), &[1, 2, 3, 4, 5, 6, 7, 8]);

    // Test exact upper-bound tier boundaries on frigate table (isisGroupID=8: [125, 250, 400, 625, 850, ..., 5300])
    for (cid, pts) in [
        (101u32, 125u32),
        (102, 126),
        (103, 250),
        (104, 251),
        (105, 400),
        (106, 401),
        (107, 5300),
        (108, 5301),
    ] {
        cat.components.insert(
            cid,
            SdeSkinComponent {
                component_id: cid,
                category: 99,
                rarity: cid,
                disallowed_slots: Vec::new(),
                associated_types: Vec::new(),
                binder_type_id: 0,
                binder_count: 0,
            },
        );
        cat.point_values.insert((99, cid), pts);
    }
    assert_eq!(cat.compute_points_and_tier(&[]), (0, 1));
    assert_eq!(cat.compute_points_and_tier(&[101]), (125, 1));
    assert_eq!(cat.compute_points_and_tier(&[102]), (126, 2));
    assert_eq!(cat.compute_points_and_tier(&[103]), (250, 2));
    assert_eq!(cat.compute_points_and_tier(&[104]), (251, 3));
    assert_eq!(cat.compute_points_and_tier(&[105]), (400, 3));
    assert_eq!(cat.compute_points_and_tier(&[106]), (401, 4));
    assert_eq!(cat.compute_points_and_tier(&[107]), (5300, 19));
    assert_eq!(cat.compute_points_and_tier(&[108]), (5301, 20));

    // SequencingJobRecord::is_active
    let mut job = SequencingJobRecord {
        job_uuid: [1; 16],
        skin_hex: "abc".into(),
        quantity: 1,
        sequencer_id: 100,
        initiated_unix: 1000,
        planned_duration_secs: 60,
        completed_unix: None,
        failed_reason: None,
    };
    assert!(job.is_active());
    job.completed_unix = Some(1060);
    assert!(!job.is_active());
    job.completed_unix = None;
    job.failed_reason = Some(1);
    assert!(!job.is_active());

    // Wire helpers: as_u64, pb::message duration, require_char, sub, seq_of, str_of, parse_component_id
    assert_eq!(as_u64(&DynamicValue::Uint64(42)), Some(42));
    assert_eq!(as_u64(&DynamicValue::Uint32(42)), Some(42));
    assert_eq!(as_u64(&DynamicValue::Int64(42)), Some(42));
    assert_eq!(as_u64(&DynamicValue::Int64(-1)), None);
    assert_eq!(as_u64(&DynamicValue::Int32(42)), Some(42));
    assert_eq!(as_u64(&DynamicValue::Int32(-1)), None);
    assert_eq!(as_u64(&DynamicValue::Enum(3)), Some(3));
    assert_eq!(as_u64(&DynamicValue::Enum(-2)), None);
    assert_eq!(as_u64(&DynamicValue::Bool(true)), None);

    let dur_out = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsResponse",
    )
    .add_with("durations", |d| {
        d.set("tier.level", 1u32)
            .set("duration", GoogleDuration { seconds: 3600, nanos: 0 })
    })
    .must_encode();
    assert!(!dur_out.is_empty());

    let mut ctx = AuthoritativeContext::default();
    assert_eq!(require_char(&ctx).unwrap_err().0, STATUS_FORBIDDEN);
    ctx.active_character = Some(eve_proto::quasar::EvePublicCharacterId { sequential: 90_000_001 });
    assert_eq!(require_char(&ctx).unwrap(), 90_000_001);

    let mut inner_seq = DynamicQuasarMessage::new("Seq");
    inner_seq.fields.insert(1, DynamicValue::Uint64(77));
    let mut msg = DynamicQuasarMessage::new("Msg");
    msg.fields.insert(1, DynamicValue::Message(Box::new(inner_seq.clone())));
    msg.fields.insert(2, DynamicValue::Bytes(b"utf8_str".to_vec()));
    assert_eq!(seq_of(&msg, 1), Some(77));
    assert_eq!(str_of(&msg, 2).as_deref(), Some("utf8_str"));
    assert_eq!(parse_component_id(&msg), Some((77, false)));

    let mut pat_msg = DynamicQuasarMessage::new("Pat");
    pat_msg.fields.insert(2, DynamicValue::Message(Box::new(inner_seq)));
    assert_eq!(parse_component_id(&pat_msg), Some((77, true)));
    assert_eq!(parse_component_id(&DynamicQuasarMessage::new("Empty")), None);
}

#[test]
fn test_component_license_grant_get_revoke_error_and_cross_char_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let state = SkinrCosmeticsState::load(Arc::clone(&db), Arc::clone(&sde));
    let (coat_id, _) = state
        .catalog
        .components
        .iter()
        .find(|(_, c)| !c.is_pattern())
        .map(|(&id, c)| (id, c.clone()))
        .expect("coating component");
    register(&router, Arc::clone(&state));

    let now = 1_710_000_000u64;
    let player_cid = 2_112_000_801i64;
    let other_cid = 2_112_000_802i64;
    let player_sess = sess(Some(10), Some(player_cid), 0);
    let admin_sess = sess(Some(10), Some(player_cid), service_roles::ROLE_ADMIN);

    // 1. Non-admin cannot grant or revoke finite/infinite licenses
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // 2. Admin grant missing component ID / zero capacity / unknown component / category mismatch
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let zero_cap_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
    )
    .set("component", build_component_id(coat_id, false))
    .set("capacity", 0u64)
    .must_encode();
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            zero_cap_req,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let unknown_comp_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
    )
    .set("component", build_component_id(999_999_999, false))
    .set("capacity", 2u64)
    .must_encode();
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            unknown_comp_req,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    // Pass coating component ID marked as pattern (is_pattern = true) -> category mismatch
    let mismatch_req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
    )
    .set("component", build_component_id(coat_id, true))
    .set("capacity", 2u64)
    .must_encode();
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            mismatch_req,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // 3. Cross-character GetRequest forbidden for player, allowed for admin
    state.data.write().unwrap().infinite_licenses.insert(
        SkinrCosmeticsState::comp_key(other_cid as u32, false, coat_id),
        InfiniteComponentLicense {
            character_id: other_cid as u32,
            component_id: coat_id,
            is_pattern: false,
            first_acquired_unix: now,
            is_free: true,
        },
    );
    let get_other = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
    )
    .set(
        "license",
        build_comp_license_id(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.Identifier",
            other_cid as u32,
            coat_id,
            false,
        ),
    )
    .must_encode();
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            get_other.clone(),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
            get_other.clone(),
            now,
        )
        .status_code,
        STATUS_OK
    );

    // 4. Revoke missing license -> STATUS_NOT_FOUND
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
            get_other,
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
}

#[test]
fn test_consume_item_branches_and_title_definition_versions() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mut state_inner = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), Arc::clone(&sde));
    // Find both a finite-use item and an infinite-use item (or inject one into catalog)
    let (&finite_tid, &(finite_comp, _)) = state_inner
        .catalog
        .item_type_to_component
        .iter()
        .find(|(_, &(_, uses))| uses > 0)
        .expect("finite consumable type");
    let inf_tid = 999_888_777u32;
    Arc::get_mut(&mut state_inner)
        .unwrap()
        .catalog
        .item_type_to_component
        .insert(inf_tid, (finite_comp, -1));
    let state = state_inner;
    register(&router, Arc::clone(&state));

    let now = 1_710_000_000u64;
    let cid = 2_112_000_901i64;
    let station_id = 60_003_760i64;
    db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().expect("demo char").clone();
        ch.character_id = cid;
        ch.station_id = Some(station_id);
        s.characters.insert(cid, ch);
    });
    let player_sess = sess(Some(11), Some(cid), 0);
    let admin_sess = sess(Some(11), Some(cid), service_roles::ROLE_ADMIN);

    let encode_consume = |item_id: u64, qty: u64| -> Vec<u8> {
        pb::message(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
        )
        .set("item.item.sequential", item_id)
        .set("quantity", qty as i32)
        .must_encode()
    };

    // 1. Missing item_id or qty == 0 -> STATUS_BAD_REQUEST
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(0, 1),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    // 2. Item not found -> STATUS_NOT_FOUND
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_001, 1),
            now,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // 3. Wrong owner, unreachable location, non-component item type, partial vs exact stack, infinite uses
    db.with_mut_snapshot(|s| {
        // Wrong owner
        s.items.insert(
            888_000_002,
            InventoryItemRecord {
                item_id: 888_000_002,
                type_id: finite_tid,
                owner_id: cid + 1,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        // Unreachable station
        s.items.insert(
            888_000_003,
            InventoryItemRecord {
                item_id: 888_000_003,
                type_id: finite_tid,
                owner_id: cid,
                location_id: station_id + 999,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        // Non-component item (Tritanium 34)
        s.items.insert(
            888_000_004,
            InventoryItemRecord {
                item_id: 888_000_004,
                type_id: 34,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        // Infinite uses item (exact stack = 1)
        s.items.insert(
            888_000_005,
            InventoryItemRecord {
                item_id: 888_000_005,
                type_id: inf_tid,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_002, 1),
            now,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_003, 1),
            now,
        )
        .status_code,
        STATUS_PRECONDITION_FAILED
    );
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_004, 1),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    // Insufficient stack (request 10 when only 1 exists)
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_005, 10),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    // Exact stack consume of infinite license item removes item and grants infinite license
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
            encode_consume(888_000_005, 1),
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert!(db.with_snapshot(|s| !s.items.contains_key(&888_000_005)));
    assert!(!state.data.read().unwrap().infinite_licenses.is_empty());

    // 4. Title definitions & version endpoints
    state.data.write().unwrap().title_definitions.push(TitleDefinitionRecord {
        title_id: 7,
        name: "Grand Master".into(),
        internal_name: "title_gm".into(),
    });
    let latest_req =
        pb::message("eve_public.cosmetic.character.title.definition.api.GetLatestRequest")
            .set("current_version.major", 24u32)
            .set("current_version.minor", 2u32)
            .set("current_version.patch", 1u32)
            .add("current_version.prerelease_tags", "rc1")
            .set("language.code", "de")
            .must_encode();
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve.cosmetic.character.title.definition.api.GetLatestRequest",
            latest_req,
            now,
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        call(
            &router,
            &player_sess,
            "eve.cosmetic.character.title.definition.api.ListDefinitionsRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_OK
    );
    // Admin version error branches: empty payload -> STATUS_BAD_REQUEST, empty tenant -> STATUS_BAD_REQUEST
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve.cosmetic.character.title.definition.version.api.admin.SetDefaultVersionRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let empty_tenant = pb::message(
        "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantRequest",
    )
    .set("context.comment", "")
    .set("version.major", 24u32)
    .set("version.minor", 2u32)
    .set("version.patch", 1u32)
    .add("version.prerelease_tags", "rc1")
    .must_encode();
    assert_eq!(
        call(
            &router,
            &admin_sess,
            "eve.cosmetic.character.title.definition.version.api.admin.OverrideVersionForTenantRequest",
            empty_tenant,
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn catalog_load_isis_groups_custom_tiers_persistence_and_active_ship_consume_mutants() {
    // 1. SkinrStaticCatalog::load populates ship_isis_groups & custom hull thresholds (kills L88, L89, L247, L261, L274, L279, L284)
    let mut sde = (*shared_sde()).clone();
    sde.type_isis_groups.insert(648, 28);
    let mut cat = SkinrStaticCatalog::load(&sde);
    assert_eq!(cat.ship_isis_groups.get(&648), Some(&28));
    assert_eq!(cat.thresholds_for_ship(587).get(&1), Some(&125));
    cat.tier_thresholds_by_isis_group.insert(
        28,
        std::collections::BTreeMap::from([(1_u32, 100_u32), (5_u32, 500_u32)]),
    );
    assert_eq!(cat.thresholds_for_ship(648).get(&5), Some(&500));

    cat.components.insert(
        701,
        SdeSkinComponent {
            component_id: 701,
            category: 99,
            rarity: 1,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    cat.components.insert(
        702,
        SdeSkinComponent {
            component_id: 702,
            category: 99,
            rarity: 2,
            disallowed_slots: Vec::new(),
            associated_types: Vec::new(),
            binder_type_id: 0,
            binder_count: 0,
        },
    );
    cat.point_values.insert((99, 1), 100);
    cat.point_values.insert((99, 2), 401);
    assert_eq!(cat.compute_points_and_tier_for_ship(648, &[701]), (100, 1));
    assert_eq!(cat.compute_points_and_tier_for_ship(648, &[702]), (401, 5));
    // Exceeding max_tier = 5 yields (5 + 1).min(20) = 6 (kills `+ -> - / *` on L279)
    assert_eq!(cat.compute_points_and_tier_for_ship(648, &[701, 702]), (501, 6));

    // 2. SkinrCosmeticsState::save & rehydrate_from_db round-trip + DB skin merge (kills L534, L567)
    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let state1 = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), Arc::clone(&sde));
    let preserved_skin = SkinDesignRecord {
        skin_hex: "deadbeef01".into(),
        name: "Persisted Crimson".into(),
        line_name: "LineX".into(),
        creator_id: 90_000_001,
        ship_type_id: 587,
        pattern_blend_mode: 2,
        slots: vec![SkinLayoutSlot {
            slot_id: 1,
            component_id: 701,
            is_pattern: false,
            primary_coating_id: None,
            secondary_coating_id: None,
        }],
        tier_level: 3,
        first_sequenced_unix: Some(1_710_000_050),
    };
    state1
        .data
        .write()
        .unwrap()
        .skins
        .insert("deadbeef01".into(), preserved_skin.clone());
    state1.save();

    let state2 = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), Arc::clone(&sde));
    assert_eq!(state2.data.read().unwrap().skins.get("deadbeef01"), Some(&preserved_skin));
    // Clear in-memory skins, seed a second design in state2, save() merges existing DB skin back
    let second_skin = SkinDesignRecord {
        skin_hex: "cafe02".into(),
        name: "Second Coat".into(),
        ..preserved_skin.clone()
    };
    state2.data.write().unwrap().skins.clear();
    state2
        .data
        .write()
        .unwrap()
        .skins
        .insert("cafe02".into(), second_skin.clone());
    state2.save();
    let state3 = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), Arc::clone(&sde));
    {
        let mut g = state3.data.write().unwrap();
        g.skins.clear();
    }
    state3.rehydrate_from_db();
    assert_eq!(state3.data.read().unwrap().skins.get("deadbeef01"), Some(&preserved_skin));
    assert_eq!(state3.data.read().unwrap().skins.get("cafe02"), Some(&second_skin));

    // 3. handle_item_consume forwards active_ship_id so boarded active ship hull cannot be consumed (kills L854, L938, L1201)
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mut state_arc = SkinrCosmeticsState::load_unregistered(Arc::clone(&db), Arc::clone(&sde));
    Arc::get_mut(&mut state_arc)
        .unwrap()
        .catalog
        .item_type_to_component
        .insert(587, (701, 5));
    register(&router, state_arc);

    let cid = 2_112_930_001_i64;
    let station_id = 60_003_760_i64;
    let active_ship_item = 888_930_001_i64;
    db.with_mut_snapshot(|s| {
        let mut ch = s.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.station_id = Some(station_id);
        ch.active_ship_id = active_ship_item;
        s.characters.insert(cid, ch);
        s.items.insert(
            active_ship_item,
            InventoryItemRecord {
                item_id: active_ship_item,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let player_sess = sess(Some(77), Some(cid), 0);
    let req = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
    )
    .set("item.item.sequential", active_ship_item as u64)
    .set("quantity", 1i32)
    .must_encode();
    let rsp = call(
        &router,
        &player_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
        req,
        1_710_000_100,
    );
    assert_eq!(rsp.status_code, STATUS_PRECONDITION_FAILED);
    assert!(db.with_snapshot(|s| s.items.contains_key(&active_ship_item)));
}
