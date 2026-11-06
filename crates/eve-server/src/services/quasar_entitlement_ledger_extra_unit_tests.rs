//! Extra branch-coverage and mutation-killing unit tests for `quasar_entitlement_ledger`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use crate::services::quasar_entitlement_ledger::{
    encode_asset_attributes, hex, load, public_identifier, redeemed_skin_licenses_for_char,
    redeemed_titles_for_char, AssetRecord, EntitlementRecord,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

const ROLE_ADMIN: u64 = eve_machonet::service_roles::ROLE_GM_MASK;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn setup_router() -> (Arc<PublicGatewayRouter>, Arc<WorldRepository>, Arc<SdeRegistry>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let router = Arc::new(PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    ));
    (router, db, sde)
}

fn call_msg(
    router: &PublicGatewayRouter,
    ctx: &AuthoritativeContext,
    type_name: &str,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let req = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: Vec::new(),
    };
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(type_name)
        .cloned()
        .expect("handler");
    handler(ctx, &req, msg)
}

fn make_ctx(char_id: Option<u32>, role: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        char_id,
        None,
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        1_760_000_000,
    )
    .with_service_roles(role)
}

fn seed_char(db: &WorldRepository, char_id: i64) {
    db.with_mut_snapshot(|s| {
        let mut rec = s.characters[&90_000_001].clone();
        rec.character_id = char_id;
        s.characters.insert(char_id, rec);
    });
}

fn dmsg(fields: impl IntoIterator<Item = (u32, DynamicValue)>) -> DynamicQuasarMessage {
    let mut m = DynamicQuasarMessage::new("Msg");
    for (k, v) in fields {
        m.fields.insert(k, v);
    }
    m
}

fn dsub(fields: impl IntoIterator<Item = (u32, DynamicValue)>) -> DynamicValue {
    DynamicValue::Message(Box::new(dmsg(fields)))
}

fn stack_item(type_id: u64, qty: u64) -> DynamicValue {
    dsub([
        (1, dsub([(1, DynamicValue::Uint64(type_id))])),
        (2, DynamicValue::Uint64(qty)),
    ])
}

fn ent_id_msg(
    holder: u32,
    asset_uuid: &[u8; 16],
    group_name: &str,
    group_uuid: Option<&[u8; 16]>,
) -> DynamicValue {
    let mut grp = Vec::new();
    if !group_name.is_empty() {
        grp.push((1, DynamicValue::String(group_name.into())));
    }
    if let Some(gu) = group_uuid {
        grp.push((2, DynamicValue::Bytes(gu.to_vec())));
    }
    dsub([
        (1, dsub([(1, dsub([(1, DynamicValue::Uint32(holder))]))])),
        (2, dsub([(1, DynamicValue::Bytes(asset_uuid.to_vec()))])),
        (3, dsub(grp)),
    ])
}

#[test]
fn spawn_hold_and_create_validation_error_branches() {
    let (router, _db, _sde) = setup_router();
    let user = make_ctx(Some(90_000_001), 0);
    let admin = make_ctx(Some(90_000_001), ROLE_ADMIN);
    let empty = dmsg([]);

    for t in [
        "eve.assetholding.asset.api.HoldRequest",
        "eve.assetholding.asset.api.CreateRequest",
    ] {
        assert_eq!(call_msg(&router, &user, t, &empty).unwrap_err().0, STATUS_FORBIDDEN);
    }

    // SpawnRequest: empty itemstack container -> STATUS_BAD_REQUEST
    let spawn_t = "eve.assetholding.asset.api.SpawnRequest";
    let empty_stacks = dmsg([(6, dsub([(1, DynamicValue::Repeated(vec![]))]))]);
    assert_eq!(
        call_msg(&router, &admin, spawn_t, &empty_stacks).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // SpawnRequest: itemstack with qty == 0 -> STATUS_BAD_REQUEST
    let zero_qty_stack = dmsg([(6, dsub([(1, DynamicValue::Repeated(vec![stack_item(34, 0)]))]))]);
    assert_eq!(
        call_msg(&router, &admin, spawn_t, &zero_qty_stack).unwrap_err().0,
        STATUS_BAD_REQUEST
    );

    // SpawnRequest: character_title with title id 0 -> STATUS_BAD_REQUEST; varint title id 55 -> Ok
    let zero_title = dmsg([(13, dsub([(1, dsub([(1, DynamicValue::Uint64(0))]))]))]);
    assert_eq!(
        call_msg(&router, &admin, spawn_t, &zero_title).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let varint_title = dmsg([
        (2, DynamicValue::String("title_55".into())),
        (
            13,
            dsub([
                (1, dsub([(1, DynamicValue::Int64(55))])),
                (3, DynamicValue::Uint64(4)),
            ]),
        ),
    ]);
    assert!(call_msg(&router, &admin, spawn_t, &varint_title).is_ok());

    // HoldRequest: blank skin hex, empty itemstack, and invalid itemstack -> STATUS_BAD_REQUEST
    let hold_t = "eve.assetholding.asset.api.HoldRequest";
    let hold_blank_skin = dmsg([(
        11,
        dsub([(1, dsub([(1, dsub([(1, DynamicValue::String("   ".into()))]))]))]),
    )]);
    let hold_empty_st = dmsg([(7, dsub([(1, DynamicValue::Repeated(vec![]))]))]);
    let hold_bad_st = dmsg([(7, dsub([(1, DynamicValue::Repeated(vec![stack_item(34, 0)]))]))]);
    for bad_hold in [&hold_blank_skin, &hold_empty_st, &hold_bad_st] {
        assert_eq!(
            call_msg(&router, &admin, hold_t, bad_hold).unwrap_err().0,
            STATUS_BAD_REQUEST
        );
    }

    // HoldRequest with explicit creator (field 2.1) and capacity (field 7.7)
    let hold_creator_ok = dmsg([
        (2, dsub([(1, DynamicValue::Int32(90_000_001))])),
        (
            7,
            dsub([
                (1, DynamicValue::Repeated(vec![stack_item(34, 25)])),
                (7, DynamicValue::Uint64(3)),
            ]),
        ),
    ]);
    assert!(call_msg(&router, &admin, hold_t, &hold_creator_ok).is_ok());

    // CreateRequest: unsupported unit, unknown SDE type in unit 8, blank hex in unit 10, zero title in unit 12
    let create_t = "eve.assetholding.asset.api.CreateRequest";
    let create_unsupported = dmsg([(4, dsub([(99, DynamicValue::Uint64(1))]))]);
    let create_bad_u8 = dmsg([(4, dsub([(8, dsub([(1, stack_item(999_999_999, 1))]))]))]);
    let create_bad_u10 = dmsg([(
        4,
        dsub([(10, dsub([(1, dsub([(1, DynamicValue::String("  ".into()))]))]))]),
    )]);
    let create_bad_u12 = dmsg([(4, dsub([(12, dsub([(1, dsub([(1, DynamicValue::Uint64(0))]))]))]))]);
    for bad_create in [&create_unsupported, &create_bad_u8, &create_bad_u10, &create_bad_u12] {
        assert_eq!(
            call_msg(&router, &admin, create_t, bad_create).unwrap_err().0,
            STATUS_BAD_REQUEST
        );
    }

    // CreateRequest: valid unit 8, unit 10, unit 12
    let create_ok_u8 = dmsg([
        (4, dsub([(8, dsub([(1, stack_item(34, 10))]))])),
        (6, DynamicValue::Uint64(5)),
    ]);
    let create_ok_u10 = dmsg([(
        4,
        dsub([(
            10,
            dsub([(1, dsub([(1, DynamicValue::String("skin_hex_ok".into()))]))]),
        )]),
    )]);
    let create_ok_u12 = dmsg([(
        4,
        dsub([(12, dsub([(1, dsub([(1, DynamicValue::Uint64(88))]))]))]),
    )]);
    for ok_create in [&create_ok_u8, &create_ok_u10, &create_ok_u12] {
        assert!(call_msg(&router, &admin, create_t, ok_create).is_ok());
    }
}

#[test]
fn direct_delivery_hangar_volume_group_uuid_delete_unredeemed_and_admin_ops() {
    let (router, db, _sde) = setup_router();
    let holder = 211_300_101_u32;
    let holder2 = 211_300_102_u32;
    seed_char(&db, holder as i64);
    seed_char(&db, holder2 as i64);

    let holder_ctx = make_ctx(Some(holder), 0);
    let no_char_ctx = make_ctx(None, 0);
    let admin = make_ctx(Some(holder), ROLE_ADMIN);

    let empty_asset = AssetRecord::default();
    assert!(!encode_asset_attributes(&empty_asset, true).is_empty());
    assert!(!encode_asset_attributes(&empty_asset, false).is_empty());
    let ent_with_guuid = EntitlementRecord {
        holder,
        asset: [7u8; 16],
        group_name: "g".into(),
        group_uuid: Some([9u8; 16]),
        total: 2,
        redeemed: 1,
    };
    assert!(!public_identifier(&ent_with_guuid).is_empty());

    // Spawn an itemstack asset (Tritanium type 34, qty 10)
    let sp_stack = dmsg([(6, dsub([(1, DynamicValue::Repeated(vec![stack_item(34, 10)]))]))]);
    let spawn_resp = call_msg(
        &router,
        &admin,
        "eve.assetholding.asset.api.SpawnRequest",
        &sp_stack,
    )
    .unwrap();
    let asset_uuid = load(&db).assets.values().next().unwrap().uuid;
    assert_eq!(hex(&asset_uuid).len(), 32);
    assert!(!spawn_resp.value.is_empty());

    let make_dd = |ent_opt: Option<DynamicValue>, qty: u64| -> DynamicQuasarMessage {
        let mut entry = vec![(10, DynamicValue::Uint64(qty))];
        if let Some(ent) = ent_opt {
            entry.push((2, ent));
        }
        dmsg([(2, DynamicValue::Repeated(vec![dsub(entry)]))])
    };
    let dd_t = "eve.assetholding.entitlement.api.DirectDeliveryRequest";
    let group_uuid = [0xABu8; 16];
    let valid_ent = ent_id_msg(holder, &asset_uuid, "grp", Some(&group_uuid));
    assert_eq!(call_msg(&router, &admin, dd_t, &make_dd(None, 1)).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(
        call_msg(&router, &admin, dd_t, &make_dd(Some(valid_ent.clone()), 0))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let unknown_char_ent = ent_id_msg(999_888_777, &asset_uuid, "grp", None);
    let unknown_asset_ent = ent_id_msg(holder, &[0xFFu8; 16], "grp", None);
    for nf_ent in [unknown_char_ent, unknown_asset_ent] {
        assert_eq!(
            call_msg(&router, &admin, dd_t, &make_dd(Some(nf_ent), 1))
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND
        );
    }

    call_msg(&router, &admin, dd_t, &make_dd(Some(valid_ent.clone()), 2)).unwrap();
    call_msg(&router, &admin, dd_t, &make_dd(Some(valid_ent.clone()), 3)).unwrap();

    let req_wrap = dmsg([(1, valid_ent.clone())]);
    let get_pub_t = "eve_public.assetholding.entitlement.api.GetRequest";
    assert_eq!(
        call_msg(&router, &no_char_ctx, get_pub_t, &req_wrap)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let missing_wrap = dmsg([(1, ent_id_msg(holder, &asset_uuid, "no_such_group", None))]);
    assert_eq!(
        call_msg(&router, &holder_ctx, get_pub_t, &missing_wrap)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // Non-admin DeleteRequest on unredeemed entitlement (redeemed == 0) is allowed
    assert!(call_msg(
        &router,
        &holder_ctx,
        "eve.assetholding.entitlement.api.DeleteRequest",
        &req_wrap
    )
    .is_ok());
    assert_eq!(
        call_msg(&router, &holder_ctx, get_pub_t, &req_wrap)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    call_msg(&router, &admin, dd_t, &make_dd(Some(valid_ent), 2)).unwrap();
    let holder2_ent = ent_id_msg(holder2, &asset_uuid, "grp2", None);
    call_msg(&router, &admin, dd_t, &make_dd(Some(holder2_ent.clone()), 4)).unwrap();

    let holder_filter = dmsg([(1, DynamicValue::Uint64(holder as u64))]);
    let redeliv_t = "eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest";
    assert_eq!(
        call_msg(&router, &holder_ctx, redeliv_t, &holder_filter)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert!(call_msg(&router, &admin, redeliv_t, &holder_filter).is_ok());

    let adm_red_t = "eve.assetholding.entitlement.api.admin.RedeemRequest";
    assert_eq!(
        call_msg(&router, &holder_ctx, adm_red_t, &req_wrap)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let adm_red = call_msg(&router, &admin, adm_red_t, &req_wrap).unwrap();
    assert_eq!(
        adm_red.normalized_type_name(),
        "eve.assetholding.entitlement.api.admin.RedeemResponse"
    );

    let auto_filter = dmsg([(1, DynamicValue::Uint64(holder2 as u64))]);
    let auto_res = call_msg(
        &router,
        &admin,
        "eve.assetholding.entitlement.api.admin.AutoRedeemRequest",
        &auto_filter,
    )
    .unwrap();
    assert_eq!(
        auto_res.normalized_type_name(),
        "eve.assetholding.entitlement.api.admin.AutoRedeemResponse"
    );

    let h2_wrap = dmsg([(1, holder2_ent)]);
    let exp_pub = call_msg(
        &router,
        &admin,
        "eve_public.assetholding.entitlement.api.admin.ExpireRequest",
        &h2_wrap,
    )
    .unwrap();
    assert_eq!(
        exp_pub.normalized_type_name(),
        "eve_public.assetholding.entitlement.api.admin.ExpireResponse"
    );
}

#[test]
fn redeemed_titles_dedup_skin_license_aggregation_and_hangar_capacity_limit() {
    let (router, db, _sde) = setup_router();
    let holder = 211_300_201_u32;
    seed_char(&db, holder as i64);
    let admin = make_ctx(Some(holder), ROLE_ADMIN);

    let mut ledger = load(&db);
    let (u1, u2, u3) = ([1u8; 16], [2u8; 16], [3u8; 16]);
    ledger.assets.insert(
        hex(&u1),
        AssetRecord {
            uuid: u1,
            name: "t1".into(),
            character_title_id: Some(777),
            ..Default::default()
        },
    );
    ledger.assets.insert(
        hex(&u2),
        AssetRecord {
            uuid: u2,
            name: "t2_dup".into(),
            character_title_id: Some(777),
            ..Default::default()
        },
    );
    ledger.assets.insert(
        hex(&u3),
        AssetRecord {
            uuid: u3,
            name: "sk".into(),
            ship_skin_license_hex: Some("hex_alpha".into()),
            ..Default::default()
        },
    );
    for (k, asset, total, redeemed) in [("k1", u1, 1, 1), ("k2", u2, 2, 2), ("k3", u3, 2, 2), ("k4", u3, 5, 3)] {
        ledger.entitlements.insert(
            k.into(),
            EntitlementRecord {
                holder,
                asset,
                group_name: k.into(),
                group_uuid: None,
                total,
                redeemed,
            },
        );
    }
    let json = serde_json::to_string(&ledger).unwrap();
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert("quasar_entitlements".into(), json);
    });

    assert_eq!(redeemed_titles_for_char(&db, holder), vec![777]);
    assert_eq!(
        redeemed_skin_licenses_for_char(&db, holder),
        vec![("hex_alpha".to_string(), 5)]
    );

    let (small_cap_tid, bulky_tid) = {
        let sde = shared_sde();
        let sc = sde.types.values().find(|t| t.capacity > 0.0 && t.capacity <= 50.0).unwrap().type_id;
        let bt = sde.types.values().find(|t| t.volume >= 100.0).unwrap().type_id;
        (sc, bt)
    };
    let custom_station_id = 61_999_001_i64;
    db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(holder as i64)) {
            ch.station_id = Some(custom_station_id);
        }
        s.items.insert(
            custom_station_id,
            InventoryItemRecord {
                item_id: custom_station_id,
                type_id: small_cap_tid,
                owner_id: 1,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sp_bulky = dmsg([(
        6,
        dsub([(1, DynamicValue::Repeated(vec![stack_item(bulky_tid as u64, 10)]))]),
    )]);
    call_msg(&router, &admin, "eve.assetholding.asset.api.SpawnRequest", &sp_bulky).unwrap();
    let bulky_uuid = load(&db)
        .assets
        .values()
        .find(|a| !a.stacks.is_empty())
        .unwrap()
        .uuid;
    let ent_id = ent_id_msg(holder, &bulky_uuid, "bulky", None);
    let dd_msg = dmsg([(
        2,
        DynamicValue::Repeated(vec![dsub([(2, ent_id), (10, DynamicValue::Uint64(1))])]),
    )]);
    let err = call_msg(
        &router,
        &admin,
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        &dd_msg,
    )
    .unwrap_err();
    assert_eq!(err.0, STATUS_BAD_REQUEST);
    assert!(err.1.contains("capacity exceeded"));
}
